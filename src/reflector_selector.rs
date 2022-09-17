use crate::{Config, ReflectorStats};
use rand::seq::SliceRandom;
use rand::{thread_rng, Rng};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

pub struct ReflectorSelector {
    pub(crate) config: Config,
    pub(crate) owd_baseline: Arc<Mutex<HashMap<IpAddr, ReflectorStats>>>,
    pub(crate) owd_recent: Arc<Mutex<HashMap<IpAddr, ReflectorStats>>>,
    pub(crate) reflector_peers_lock: Arc<Mutex<Vec<IpAddr>>>,
    pub(crate) reflector_pool: Vec<IpAddr>,
    pub(crate) trigger_channel: Receiver<bool>,
}

impl ReflectorSelector {
    pub fn run(&self) {
        let mut selector_sleep_time = Duration::new(30, 0);
        let mut reselection_count = 0;
        let baseline_sleep_time = Duration::new(
            (self.config.tick_duration * std::f64::consts::PI) as u64,
            (((self.config.tick_duration * std::f64::consts::PI) % 1.0) * 1e9) as u32,
        );

        let mut rng = thread_rng();

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

            // After 40 reselections, slow down to every 15 minutes
            if reselection_count > 40 {
                selector_sleep_time = Duration::new(15 * 60, 0);
            }

            let mut next_peers: Vec<IpAddr> = Vec::new();
            let mut reflectors_peers = self.reflector_peers_lock.lock().unwrap();

            // Include all current peers
            for reflector in reflectors_peers.iter() {
                next_peers.push(*reflector);
            }

            for i in 1..20 {
                let next_candidate = self.reflector_pool.choose(&mut rng).unwrap();
                next_peers.push(*next_candidate);
            }

            // Put all the pool members back into the peers for some re-baselining...
            *reflectors_peers = next_peers.clone();

            // Drop the MutexGuard explicitly, as Rust won't unlock the mutex by default
            // until the guard goes out of scope
            drop(reflectors_peers);

            // Wait for several seconds to allow all reflectors to be re-baselined
            sleep(baseline_sleep_time);

            // Re-acquire the lock when we wake up again
            reflectors_peers = self.reflector_peers_lock.lock().unwrap();
            reflectors_peers.len();

            let mut candidates = Vec::new();
            let owd_baseline = self.owd_baseline.lock().unwrap();
            let owd_recent = self.owd_recent.lock().unwrap();

            for peer in next_peers {
                if owd_recent.contains_key(&peer) {
                    let rtt = (owd_recent[&peer].down_ewma + owd_recent[&peer].up_ewma) as u64;
                    candidates.push((peer, rtt));
                    println!("Candidate reflector: {} RTT: {}", peer.to_string(), rtt);
                } else {
                    println!(
                        "No data found from candidate reflector: {} - skipping",
                        peer.to_string()
                    );
                }
            }

            // Sort the candidates table now by ascending RTT
            candidates.sort_by(|a, b| a.1.cmp(&b.1));

            // Now we will just limit the candidates down to 2 * num_reflectors
            let mut num_reflectors = self.config.num_reflectors.clone();
            let candidate_pool_num = (2 * num_reflectors) as usize;
            candidates = candidates[0..candidate_pool_num - 1].to_vec();

            for (candidate, rtt) in candidates.iter() {
                println!("Fastest candidate {}: {}", candidate, rtt);
            }

            // Shuffle the deck so we avoid overwhelming good reflectors (Fisher-Yates)
            for i in (1 as usize..candidates.len()).rev() {
                let j = rng.gen_range(0..(i + 1));
                candidates.swap(i, j);
            }

            if (candidates.len() as u8) < num_reflectors {
                num_reflectors = candidates.len() as u8;
            }

            let mut new_peers = Vec::new();
            for i in 1..num_reflectors {
                new_peers.push(candidates[i as usize].0);
                println!(
                    "New selected peer: {}",
                    candidates[i as usize].0.to_string()
                );
            }

            *reflectors_peers = new_peers;
        }
    }
}
