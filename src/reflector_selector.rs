use crate::component::Baseliner;
use crate::config::PingSourceConfig;
use log::{debug, info};
use std::net::IpAddr;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, RwLock};
use std::thread::sleep;
use std::time::Duration;

pub struct ReflectorSelector {
    pub config: PingSourceConfig,
    pub baseliner: Arc<RwLock<dyn Baseliner>>,
    pub reflector_peers_lock: Arc<RwLock<Vec<IpAddr>>>,
    /// Static pool of candidate reflectors (from config file).
    pub reflector_pool: Vec<IpAddr>,
    pub trigger_channel: Receiver<bool>,
    /// Dynamic candidates discovered via eBPF TCP flow monitoring (Phase D).
    /// When `Some`, new IPs are drained each reselection cycle and merged with
    /// the static pool before scoring.
    #[cfg(feature = "ebpf")]
    pub dynamic_candidates: Option<Receiver<IpAddr>>,
}

impl ReflectorSelector {
    pub fn run(&self) -> anyhow::Result<()> {
        let mut selector_sleep_time = Duration::new(30, 0);
        let mut reselection_count = 0;
        let baseline_sleep_time = Duration::from_secs_f64(
            self.config.tick_interval * std::f64::consts::PI,
        );

        // Initial wait for some OWD data to accumulate
        sleep(baseline_sleep_time);

        loop {
            // Trigger via timeout or explicit channel signal
            let _ = self
                .trigger_channel
                .recv_timeout(selector_sleep_time)
                .unwrap_or(true);
            reselection_count += 1;
            info!("Starting reselection [#{}]", reselection_count);

            // After 40 reselections slow down to the configured interval
            if reselection_count > 40 {
                selector_sleep_time =
                    Duration::new(self.config.peer_reselection_time * 60, 0);
            }

            let mut next_peers: Vec<IpAddr> = Vec::new();
            let mut reflectors_peers = self
                .reflector_peers_lock
                .write()
                .expect("reflectors RwLock poisoned");

            // Keep all current active peers
            for &ip in reflectors_peers.iter() {
                debug!("Current peer: {ip}");
                next_peers.push(ip);
            }

            // Drain any eBPF-discovered dynamic candidates (Phase D).
            #[cfg(feature = "ebpf")]
            if let Some(ref dyn_rx) = self.dynamic_candidates {
                let max_dynamic = 10; // cap to avoid overwhelming the scorer
                let mut added = 0;
                while added < max_dynamic {
                    match dyn_rx.try_recv() {
                        Ok(ip) if !next_peers.contains(&ip) => {
                            debug!("Dynamic candidate (eBPF): {ip}");
                            next_peers.push(ip);
                            added += 1;
                        }
                        Ok(_) => {} // already in list
                        Err(_) => break,
                    }
                }
            }

            // Add random candidates from the static pool (if non-empty).
            if !self.reflector_pool.is_empty() {
                for _ in 0..20 {
                    let candidate =
                        self.reflector_pool[fastrand::usize(..self.reflector_pool.len())];
                    if !next_peers.contains(&candidate) {
                        debug!("Candidate: {candidate}");
                        next_peers.push(candidate);
                    }
                }
            }

            *reflectors_peers = next_peers.clone();
            drop(reflectors_peers);

            // Wait for the candidates to accumulate baseline data
            debug!("Waiting for candidates to be baselined…");
            sleep(baseline_sleep_time);

            // Re-acquire lock and score candidates by RTT
            let mut reflectors_peers = self
                .reflector_peers_lock
                .write()
                .expect("reflectors RwLock poisoned");

            let rtts: std::collections::HashMap<IpAddr, f64> = {
                let b = self.baseliner.read().expect("baseliner RwLock poisoned");
                b.reflector_rtts().into_iter().collect()
            };

            let mut candidates: Vec<(IpAddr, u64)> = next_peers
                .into_iter()
                .filter_map(|ip| {
                    let rtt = rtts.get(&ip).copied()?;
                    let rtt_rounded = rtt as u64;
                    info!("Candidate {ip} RTT={rtt_rounded}");
                    Some((ip, rtt_rounded))
                })
                .collect();

            // Sort ascending by RTT
            candidates.sort_by(|a, b| a.1.cmp(&b.1));

            // Keep top 2×N candidates, then shuffle to avoid overwhelming best peers
            let mut num_reflectors = self.config.num_reflectors;
            let pool_size = (2 * num_reflectors) as usize;
            candidates.truncate(pool_size);

            // Fisher-Yates shuffle
            for i in (1..candidates.len()).rev() {
                let j = fastrand::usize(0..=i);
                candidates.swap(i, j);
            }

            if (candidates.len() as u8) < num_reflectors {
                num_reflectors = candidates.len() as u8;
            }

            let mut new_peers = Vec::new();
            for i in 0..num_reflectors as usize {
                info!("Selected peer: {}", candidates[i].0);
                new_peers.push(candidates[i].0);
            }

            *reflectors_peers = new_peers;
        }
    }
}
