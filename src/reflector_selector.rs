use crate::{Config, ReflectorStats};
use log::{debug, info};
use rand::seq::IndexedRandom;
use rand::{rng, RngExt};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, RwLock};
use std::thread::sleep;
use std::time::Duration;

pub struct ReflectorSelector {
    pub config: Config,
    pub owd_recent: Arc<Mutex<HashMap<IpAddr, ReflectorStats>>>,
    pub reflector_peers_lock: Arc<RwLock<Vec<IpAddr>>>,
    pub reflector_pool: Vec<IpAddr>,
    pub trigger_channel: Receiver<bool>,
}

impl ReflectorSelector {
    pub fn run(&self) -> anyhow::Result<()> {
        let mut selector_sleep_time = Duration::new(30, 0);
        let mut reselection_count = 0;
        let baseline_sleep_time =
            Duration::from_secs_f64(self.config.tick_interval * std::f64::consts::PI);

        let mut rng = rng();

        // Initial wait of several seconds to allow some OWD data to build up
        sleep(baseline_sleep_time);

        loop {
            /*
             * Selection is triggered either by some other thread triggering it through the channel,
             * or it passes the timeout. In any case we don't care about the result of this function,
             * so we ignore the result of it.
             */
            let _ = self
                .trigger_channel
                .recv_timeout(selector_sleep_time)
                .unwrap_or(true);
            reselection_count += 1;
            info!("Starting reselection [#{}]", reselection_count);

            // After 40 reselections, slow down to every 15 minutes
            if reselection_count > 40 {
                selector_sleep_time = Duration::new(15 * 60, 0);
            }

            let mut next_peers: Vec<IpAddr> = Vec::new();
            let mut reflectors_peers = self.reflector_peers_lock.write().unwrap();

            // Include all current peers
            for reflector in reflectors_peers.iter() {
                debug!("Current peer: {}", reflector.to_string());
                next_peers.push(*reflector);
            }

            for _ in 1..20 {
                let next_candidate = self.reflector_pool.choose(&mut rng).unwrap();
                debug!("Next candidate: {}", next_candidate.to_string());
                next_peers.push(*next_candidate);
            }

            // Put all the pool members back into the peers for some re-baselining...
            *reflectors_peers = next_peers.clone();

            // Drop the MutexGuard explicitly, as Rust won't unlock the mutex by default
            // until the guard goes out of scope
            drop(reflectors_peers);

            debug!("Waiting for candidates to be baselined");
            // Wait for several seconds to allow all reflectors to be re-baselined
            sleep(baseline_sleep_time);

            // Re-acquire the lock when we wake up again
            reflectors_peers = self.reflector_peers_lock.write().unwrap();
            reflectors_peers.len();

            let mut candidates = Vec::new();
            let owd_recent = self.owd_recent.lock().unwrap();

            for peer in next_peers {
                if owd_recent.contains_key(&peer) {
                    let rtt = (owd_recent[&peer].down_ewma + owd_recent[&peer].up_ewma) as u64;
                    candidates.push((peer, rtt));
                    info!("Candidate reflector: {} RTT: {}", peer.to_string(), rtt);
                } else {
                    info!(
                        "No data found from candidate reflector: {} - skipping",
                        peer.to_string()
                    );
                }
            }

            // Sort the candidates table now by ascending RTT
            candidates.sort_by(|a, b| a.1.cmp(&b.1));

            // Now we will just limit the candidates down to 2 * num_reflectors
            let mut num_reflectors = self.config.num_reflectors;
            let candidate_pool_num = (2 * num_reflectors) as usize;
            candidates = candidates[0..candidate_pool_num - 1].to_vec();

            for (candidate, rtt) in candidates.iter() {
                info!("Fastest candidate {}: {}", candidate, rtt);
            }

            // Shuffle the deck so we avoid overwhelming good reflectors (Fisher-Yates)
            for i in (1_usize..candidates.len()).rev() {
                let j = rng.random_range(0..(i + 1));
                candidates.swap(i, j);
            }

            if (candidates.len() as u8) < num_reflectors {
                num_reflectors = candidates.len() as u8;
            }

            let mut new_peers = Vec::new();
            for i in 0..num_reflectors {
                new_peers.push(candidates[i as usize].0);
                info!(
                    "New selected peer: {}",
                    candidates[i as usize].0.to_string()
                );
            }

            *reflectors_peers = new_peers;
        }
    }
}
