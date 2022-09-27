use crate::pinger::PingReply;
use crate::{Config, Utils};
use log::info;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

#[derive(Copy, Clone)]
pub struct ReflectorStats {
    pub(crate) down_ewma: f64,
    pub(crate) up_ewma: f64,
    pub(crate) last_receive_time_s: f64,
}

pub struct Baseliner {
    pub(crate) config: Config,
    pub(crate) owd_baseline: Arc<Mutex<HashMap<IpAddr, ReflectorStats>>>,
    pub(crate) owd_recent: Arc<Mutex<HashMap<IpAddr, ReflectorStats>>>,
    pub(crate) reselect_trigger: Sender<bool>,
    pub(crate) stats_receiver: Receiver<PingReply>,
}

impl Baseliner {
    pub fn run(&self) {
        /*
         * 135 seconds to decay to 50% for the slow factor and
         * 0.4 seconds to decay to 50% for the fast factor.
         * The fast one can be adjusted to tune, try anything from 0.01 to 3.0 to get more or less sensitivity
         * with more sensitivity we respond faster to bloat, but are at risk from triggering due to lag spikes that
         * aren't bloat related, with less sensitivity (bigger numbers) we smooth through quick spikes
         * but take longer to respond to real bufferbloat
         */
        let slow_factor = Utils::ewma_factor(self.config.tick_interval, 135.0);
        let fast_factor = Utils::ewma_factor(self.config.tick_interval, 0.4);

        loop {
            let time_data = self.stats_receiver.recv().unwrap();

            let mut owd_baseline_map = self.owd_baseline.lock().unwrap();
            let mut owd_recent_map = self.owd_recent.lock().unwrap();

            let owd_baseline_new = ReflectorStats {
                down_ewma: time_data.down_time,
                up_ewma: time_data.down_time,
                last_receive_time_s: time_data.last_receive_time_s,
            };

            let owd_recent_new = ReflectorStats {
                down_ewma: time_data.down_time,
                up_ewma: time_data.down_time,
                last_receive_time_s: time_data.last_receive_time_s,
            };

            let mut owd_baseline = owd_baseline_map
                .entry(time_data.reflector)
                .or_insert(owd_baseline_new);

            let mut owd_recent = owd_recent_map
                .entry(time_data.reflector)
                .or_insert(owd_recent_new);

            if time_data.last_receive_time_s - owd_baseline.last_receive_time_s > 30.0
                || time_data.last_receive_time_s - owd_recent.last_receive_time_s > 30.0
            {
                owd_baseline.down_ewma = time_data.down_time;
                owd_baseline.up_ewma = time_data.up_time;
                owd_baseline.last_receive_time_s = time_data.last_receive_time_s;
                owd_recent.down_ewma = time_data.down_time;
                owd_recent.up_ewma = time_data.up_time;
                owd_recent.last_receive_time_s = time_data.last_receive_time_s;
            }

            owd_baseline.last_receive_time_s = time_data.last_receive_time_s;
            owd_recent.last_receive_time_s = time_data.last_receive_time_s;

            // if this reflection is more than 5 seconds higher than baseline... mark it no good and trigger a reselection
            if time_data.up_time > owd_baseline.up_ewma + 5000.0
                || time_data.down_time > owd_baseline.down_ewma + 5000.0
            {
                owd_baseline.last_receive_time_s = time_data.last_receive_time_s - 60.0;
                owd_recent.last_receive_time_s = time_data.last_receive_time_s - 60.0;
                info!(
                    "Reflector {} has OWD > 5 seconds more than baseline, triggering reselection",
                    time_data.reflector
                );
                let _ = self.reselect_trigger.send(true);
            } else {
                owd_baseline.down_ewma = owd_baseline.down_ewma * slow_factor
                    + (1.0 - slow_factor) * time_data.down_time;
                owd_baseline.up_ewma =
                    owd_baseline.up_ewma * slow_factor + (1.0 - slow_factor) * time_data.up_time;

                owd_recent.down_ewma =
                    owd_recent.down_ewma * fast_factor + (1.0 - fast_factor) * time_data.down_time;
                owd_recent.up_ewma =
                    owd_recent.up_ewma * fast_factor + (1.0 - fast_factor) * time_data.up_time;

                if owd_baseline.down_ewma > owd_recent.down_ewma {
                    owd_baseline.down_ewma = owd_recent.down_ewma;
                }

                if owd_baseline.up_ewma > owd_recent.up_ewma {
                    owd_baseline.up_ewma = owd_recent.up_ewma;
                }
            }

            info!(
                "Reflector {} up baseline = {} down baseline = {}",
                time_data.reflector, owd_baseline.up_ewma, owd_baseline.down_ewma
            );
            info!(
                "Reflector {} up recent = {} down recent = {}",
                time_data.reflector, owd_recent.up_ewma, owd_recent.down_ewma
            );
        }
    }
}
