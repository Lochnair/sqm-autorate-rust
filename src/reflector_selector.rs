// SPDX-FileCopyrightText: 2022-Present Charles Corrigan mailto:chas-iot@runegate.org (github @chas-iot)
// SPDX-FileCopyrightText: 2022-Present Daniel Lakeland mailto:dlakelan@street-artists.org (github @dlakelan)
// SPDX-FileCopyrightText: 2022-Present Mark Baker mailto:mark@vpost.net (github @Fail-Safe)
// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use crate::law::ControlSnapshot;
use crate::measurement::model::MeasurementStream;
use crate::metrics::{Metric, MetricsSender};
use crate::settings::Settings;
use crate::util::RwLockExt;
use log::{debug, info};
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::watch;

fn recent_rtt_for_peer(
    snapshot: Option<&ControlSnapshot>,
    stream: Option<&MeasurementStream>,
    now: Instant,
    max_age: Duration,
) -> Option<u64> {
    snapshot
        .and_then(|snapshot| stream.and_then(|stream| snapshot.reflectors.get(stream)))
        .filter(|state| now.saturating_duration_since(state.last_receive_at) < max_age)
        .map(|state| (state.recent.down + state.recent.up) as u64)
}

pub struct ReflectorSelector {
    pub settings: Settings,
    pub snapshot_rx: watch::Receiver<Option<ControlSnapshot>>,
    pub reflector_peers_lock: Arc<RwLock<Vec<SocketAddr>>>,
    pub reflector_tx: UnboundedSender<Vec<SocketAddr>>,
    pub reflector_pool: Vec<SocketAddr>,
    pub endpoint_streams: Arc<RwLock<std::collections::HashMap<SocketAddr, MeasurementStream>>>,
    pub trigger_channel: mpsc::Receiver<bool>,
    pub metrics: MetricsSender,
}

impl ReflectorSelector {
    pub async fn run(mut self) -> anyhow::Result<()> {
        let mut selector_sleep_time = Duration::new(30, 0);
        let mut reselection_count = 0;
        let baseline_sleep_time = Duration::from_secs_f64(
            self.settings.advanced_settings.tick_interval * std::f64::consts::PI,
        );
        // Initial wait of several seconds to allow some OWD data to build up
        tokio::time::sleep(baseline_sleep_time).await;

        loop {
            let triggered = tokio::select! {
                trigger = self.trigger_channel.recv() => {
                    match trigger {
                        Some(_) => true,
                        None => return Ok(()),
                    }
                }
                _ = tokio::time::sleep(selector_sleep_time) => false,
            };
            reselection_count += 1;
            info!("Starting reselection [#{}]", reselection_count);
            self.metrics.send(Metric::Event {
                name: "reflector_selection",
                reason: if triggered { "triggered" } else { "timeout" },
                reflector: None,
                tags: &[],
            });

            // After 40 reselections, slow down to every 15 minutes
            if reselection_count > 40 {
                selector_sleep_time =
                    Duration::new(self.settings.reflectors.peer_reselection_time * 60, 0);
            }

            let mut next_peers: Vec<SocketAddr> = Vec::new();
            let current_peers = {
                let mut reflectors_peers = self.reflector_peers_lock.write_anyhow()?;
                let current_peers = reflectors_peers.clone();

                // Include all current peers
                for reflector in reflectors_peers.iter() {
                    debug!("Current peer: {}", reflector);
                    next_peers.push(*reflector);
                }

                if self.reflector_pool.is_empty() {
                    continue;
                }
                for _ in 0..20 {
                    let next_candidate =
                        &self.reflector_pool[fastrand::usize(..self.reflector_pool.len())];
                    if next_peers.contains(next_candidate) {
                        continue;
                    }
                    debug!("Next candidate: {}", next_candidate);
                    next_peers.push(*next_candidate);
                }

                // Clone next_peers because we need it again after the baseline sleep
                // to iterate over candidates for RTT measurement.
                *reflectors_peers = next_peers.clone();
                self.reflector_tx.send(next_peers.clone())?;

                current_peers
            };

            debug!("Waiting for candidates to be baselined");
            // Wait for several seconds to allow all reflectors to be re-baselined
            tokio::time::sleep(baseline_sleep_time).await;

            // Re-acquire the lock when we wake up again
            let mut reflectors_peers = self.reflector_peers_lock.write_anyhow()?;

            let mut candidates = Vec::new();

            let snapshot = self.snapshot_rx.borrow();
            for peer in next_peers {
                let stream = self.endpoint_streams.read_anyhow()?.get(&peer).copied();
                if let Some(rtt) = recent_rtt_for_peer(
                    snapshot.as_ref(),
                    stream.as_ref(),
                    Instant::now(),
                    Duration::from_secs_f64(self.settings.advanced_settings.tick_interval * 2.0),
                ) {
                    candidates.push((peer, rtt));
                    info!("Candidate reflector: {} RTT: {}", peer, rtt);
                } else {
                    info!(
                        "No data found from candidate reflector: {} - skipping",
                        peer
                    );
                }
            }

            // Sort the candidates table now by ascending RTT
            candidates.sort_by_key(|candidate| candidate.1);

            // Now we will just limit the candidates down to 2 * num_reflectors
            let num_reflectors = self.settings.reflectors.num_reflectors;
            let candidate_pool_num = (2 * num_reflectors) as usize;
            candidates.truncate(candidate_pool_num);

            for (candidate, rtt) in candidates.iter() {
                info!("Fastest candidate {}: {}", candidate, rtt);
            }

            // Shuffle the deck so we avoid overwhelming good reflectors (Fisher-Yates)
            for i in (1_usize..candidates.len()).rev() {
                let j = fastrand::usize(0..=i);
                candidates.swap(i, j);
            }

            if candidates.is_empty() {
                info!("No usable reflectors, retaining current set");
                self.reflector_tx.send(current_peers.clone())?;
                *reflectors_peers = current_peers;
                continue;
            }

            if (candidates.len() as u8) < num_reflectors {
                info!(
                    "Only {} usable reflectors, selecting available candidates",
                    candidates.len()
                );
            }
            let num_reflectors = num_reflectors.min(candidates.len() as u8);

            let mut new_peers = Vec::new();
            for i in 0..num_reflectors {
                let peer = candidates[i as usize].0;
                new_peers.push(peer);
                info!("New selected peer: {}", peer);
                self.metrics.send(Metric::Event {
                    name: "reflector_selected",
                    reason: "",
                    reflector: Some(peer.ip()),
                    tags: &[],
                });
            }

            self.reflector_tx.send(new_peers.clone())?;
            *reflectors_peers = new_peers;
        }
    }
}
