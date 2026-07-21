// SPDX-FileCopyrightText: 2022-Present Charles Corrigan mailto:chas-iot@runegate.org (github @chas-iot)
// SPDX-FileCopyrightText: 2022-Present Daniel Lakeland mailto:dlakelan@street-artists.org (github @dlakelan)
// SPDX-FileCopyrightText: 2022-Present Mark Baker mailto:mark@vpost.net (github @Fail-Safe)
// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use crate::Config;
use crate::SHUTDOWN;
use crate::baseliner::ControlSnapshot;
use crate::metrics::{Metric, MetricsSender};
use crate::util::RwLockExt;
use flume::{Receiver, RecvError, RecvTimeoutError, Selector};
use log::{debug, info};
use std::net::IpAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};
use std::thread::sleep;
use std::time::{Duration, Instant};

enum SelectorEvent {
    Trigger(Result<bool, RecvError>),
    Snapshot(Result<ControlSnapshot, RecvError>),
}

#[derive(Debug, Eq, PartialEq)]
enum WaitOutcome {
    Triggered,
    DeadlineReached,
    ChannelsClosed,
}

fn wait_for_trigger_or_deadline(
    trigger_rx: &Receiver<bool>,
    snapshot_rx: &Receiver<ControlSnapshot>,
    latest_snapshot: &mut Option<ControlSnapshot>,
    deadline: Instant,
) -> WaitOutcome {
    let mut trigger_connected = true;
    let mut snapshot_connected = true;

    loop {
        if Instant::now() >= deadline {
            return WaitOutcome::DeadlineReached;
        }

        let event = match (trigger_connected, snapshot_connected) {
            (true, true) => Selector::new()
                .recv(trigger_rx, SelectorEvent::Trigger)
                .recv(snapshot_rx, SelectorEvent::Snapshot)
                .wait_deadline(deadline)
                .ok(),
            (true, false) => match trigger_rx.recv_deadline(deadline) {
                Ok(trigger) => Some(SelectorEvent::Trigger(Ok(trigger))),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    Some(SelectorEvent::Trigger(Err(RecvError::Disconnected)))
                }
            },
            (false, true) => match snapshot_rx.recv_deadline(deadline) {
                Ok(snapshot) => Some(SelectorEvent::Snapshot(Ok(snapshot))),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    Some(SelectorEvent::Snapshot(Err(RecvError::Disconnected)))
                }
            },
            (false, false) => return WaitOutcome::ChannelsClosed,
        };

        match event {
            Some(SelectorEvent::Trigger(Ok(_))) => return WaitOutcome::Triggered,
            Some(SelectorEvent::Trigger(Err(_))) => trigger_connected = false,
            Some(SelectorEvent::Snapshot(Ok(snapshot))) => *latest_snapshot = Some(snapshot),
            Some(SelectorEvent::Snapshot(Err(_))) => snapshot_connected = false,
            None => return WaitOutcome::DeadlineReached,
        }
    }
}

fn receive_snapshots_until(
    snapshot_rx: &Receiver<ControlSnapshot>,
    latest_snapshot: &mut Option<ControlSnapshot>,
    deadline: Instant,
) {
    loop {
        if Instant::now() >= deadline {
            return;
        }

        match snapshot_rx.recv_deadline(deadline) {
            Ok(snapshot) => *latest_snapshot = Some(snapshot),
            Err(RecvTimeoutError::Timeout) => return,
            Err(RecvTimeoutError::Disconnected) => {
                sleep(deadline.saturating_duration_since(Instant::now()));
                return;
            }
        }
    }
}

fn recent_rtt_for_peer(snapshot: Option<&ControlSnapshot>, peer: &IpAddr) -> Option<u64> {
    snapshot
        .and_then(|snapshot| snapshot.reflectors.get(peer))
        .map(|state| (state.recent.down + state.recent.up) as u64)
}

pub struct ReflectorSelector {
    pub config: Config,
    pub snapshot_rx: Receiver<ControlSnapshot>,
    pub reflector_peers_lock: Arc<RwLock<Vec<IpAddr>>>,
    pub reflector_pool: Vec<IpAddr>,
    pub trigger_channel: Receiver<bool>,
    pub metrics: MetricsSender,
}

impl ReflectorSelector {
    pub fn run(&self) -> anyhow::Result<()> {
        let mut selector_sleep_time = Duration::new(30, 0);
        let mut reselection_count = 0;
        let mut latest_snapshot = None;
        let baseline_sleep_time =
            Duration::from_secs_f64(self.config.tick_interval * std::f64::consts::PI);
        // Initial wait of several seconds to allow some OWD data to build up
        receive_snapshots_until(
            &self.snapshot_rx,
            &mut latest_snapshot,
            Instant::now() + baseline_sleep_time,
        );

        loop {
            if SHUTDOWN.load(Ordering::Relaxed) {
                info!("Reflector selector shutting down");
                return Ok(());
            }

            let triggered = match wait_for_trigger_or_deadline(
                &self.trigger_channel,
                &self.snapshot_rx,
                &mut latest_snapshot,
                Instant::now() + selector_sleep_time,
            ) {
                WaitOutcome::Triggered => true,
                WaitOutcome::DeadlineReached => false,
                WaitOutcome::ChannelsClosed => {
                    info!("Reflector selector channels closed, shutting down");
                    return Ok(());
                }
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
                selector_sleep_time = Duration::new(self.config.peer_reselection_time * 60, 0);
            }

            let mut next_peers: Vec<IpAddr> = Vec::new();
            let mut reflectors_peers = self.reflector_peers_lock.write_anyhow()?;

            // Include all current peers
            for reflector in reflectors_peers.iter() {
                debug!("Current peer: {}", reflector);
                next_peers.push(*reflector);
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

            // Drop the MutexGuard explicitly, as Rust won't unlock the mutex by default
            // until the guard goes out of scope
            drop(reflectors_peers);

            debug!("Waiting for candidates to be baselined");
            // Wait for several seconds to allow all reflectors to be re-baselined
            receive_snapshots_until(
                &self.snapshot_rx,
                &mut latest_snapshot,
                Instant::now() + baseline_sleep_time,
            );

            // Re-acquire the lock when we wake up again
            reflectors_peers = self.reflector_peers_lock.write_anyhow()?;

            let mut candidates = Vec::new();

            for peer in next_peers {
                if let Some(rtt) = recent_rtt_for_peer(latest_snapshot.as_ref(), &peer) {
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
            candidates.sort_by(|a, b| a.1.cmp(&b.1));

            // Now we will just limit the candidates down to 2 * num_reflectors
            let mut num_reflectors = self.config.num_reflectors;
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

            if (candidates.len() as u8) < num_reflectors {
                num_reflectors = candidates.len() as u8;
            }

            let mut new_peers = Vec::new();
            for i in 0..num_reflectors {
                let peer = candidates[i as usize].0;
                new_peers.push(peer);
                info!("New selected peer: {}", peer);
                self.metrics.send(Metric::Event {
                    name: "reflector_selected",
                    reason: "",
                    reflector: Some(peer),
                    tags: &[],
                });
            }

            *reflectors_peers = new_peers;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseliner::{EwmaStats, ReflectorState};
    use std::collections::HashMap;
    use std::thread;

    fn snapshot(generated_at: Instant) -> ControlSnapshot {
        ControlSnapshot {
            generated_at,
            reflectors: HashMap::new(),
        }
    }

    fn snapshot_with_rtt(generated_at: Instant, peer: IpAddr, rtt: f64) -> ControlSnapshot {
        let mut reflectors = HashMap::new();
        reflectors.insert(
            peer,
            ReflectorState {
                baseline: EwmaStats {
                    down: rtt / 2.0,
                    up: rtt / 2.0,
                },
                recent: EwmaStats {
                    down: rtt / 2.0,
                    up: rtt / 2.0,
                },
                last_receive_at: generated_at,
            },
        );
        ControlSnapshot {
            generated_at,
            reflectors,
        }
    }

    #[test]
    fn wait_retains_the_newest_snapshot() {
        let (_trigger_tx, trigger_rx) = flume::unbounded();
        let (snapshot_tx, snapshot_rx) = flume::unbounded();
        let generated_at = Instant::now();
        snapshot_tx.send(snapshot(generated_at)).unwrap();
        snapshot_tx
            .send(snapshot(generated_at + Duration::from_millis(1)))
            .unwrap();
        snapshot_tx
            .send(snapshot(generated_at + Duration::from_millis(2)))
            .unwrap();
        let mut latest_snapshot = None;

        let result = wait_for_trigger_or_deadline(
            &trigger_rx,
            &snapshot_rx,
            &mut latest_snapshot,
            Instant::now() + Duration::from_millis(20),
        );

        assert_eq!(result, WaitOutcome::DeadlineReached);
        assert_eq!(
            latest_snapshot.unwrap().generated_at,
            generated_at + Duration::from_millis(2)
        );
    }

    #[test]
    fn snapshots_do_not_extend_the_deadline() {
        let (_trigger_tx, trigger_rx) = flume::unbounded();
        let (snapshot_tx, snapshot_rx) = flume::unbounded();
        let producer = thread::spawn(move || {
            let stop_at = Instant::now() + Duration::from_millis(500);
            while Instant::now() < stop_at {
                if snapshot_tx.send(snapshot(Instant::now())).is_err() {
                    break;
                }
                sleep(Duration::from_millis(1));
            }
        });
        let mut latest_snapshot = None;
        let started_at = Instant::now();

        let result = wait_for_trigger_or_deadline(
            &trigger_rx,
            &snapshot_rx,
            &mut latest_snapshot,
            started_at + Duration::from_millis(30),
        );
        let elapsed = started_at.elapsed();
        drop(snapshot_rx);
        producer.join().unwrap();

        assert_eq!(result, WaitOutcome::DeadlineReached);
        assert!(latest_snapshot.is_some());
        assert!(elapsed < Duration::from_millis(250), "elapsed: {elapsed:?}");
    }

    #[test]
    fn trigger_interrupts_the_wait() {
        let (trigger_tx, trigger_rx) = flume::unbounded();
        let (_snapshot_tx, snapshot_rx) = flume::unbounded();
        trigger_tx.send(true).unwrap();
        let mut latest_snapshot = None;
        let started_at = Instant::now();

        let result = wait_for_trigger_or_deadline(
            &trigger_rx,
            &snapshot_rx,
            &mut latest_snapshot,
            started_at + Duration::from_secs(1),
        );

        assert_eq!(result, WaitOutcome::Triggered);
        assert!(started_at.elapsed() < Duration::from_millis(250));
    }

    #[test]
    fn baseline_wait_retains_snapshot_for_evaluation() {
        let (snapshot_tx, snapshot_rx) = flume::unbounded();
        let peer = "192.0.2.1".parse().unwrap();
        let generated_at = Instant::now();
        snapshot_tx
            .send(snapshot_with_rtt(generated_at, peer, 20.0))
            .unwrap();
        snapshot_tx
            .send(snapshot_with_rtt(
                generated_at + Duration::from_millis(1),
                peer,
                42.0,
            ))
            .unwrap();
        let mut latest_snapshot = None;

        receive_snapshots_until(
            &snapshot_rx,
            &mut latest_snapshot,
            Instant::now() + Duration::from_millis(20),
        );

        assert_eq!(
            recent_rtt_for_peer(latest_snapshot.as_ref(), &peer),
            Some(42)
        );
    }
}
