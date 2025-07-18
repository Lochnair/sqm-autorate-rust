use crate::pinger::PingReply;
use crate::Config;
use log::info;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Copy, Clone)]
pub struct ReflectorStats {
    pub down_ewma: f64,
    pub up_ewma: f64,
    pub last_receive_time_s: Instant,
}

pub struct Baseliner {
    pub config: Config,
    pub owd_baseline: Arc<Mutex<HashMap<IpAddr, ReflectorStats>>>,
    pub owd_recent: Arc<Mutex<HashMap<IpAddr, ReflectorStats>>>,
    pub reselect_trigger: Sender<bool>,
    pub start_time: Instant,
    pub stats_receiver: Receiver<PingReply>,
}

fn ewma_factor(tick: f64, dur: f64) -> f64 {
    ((0.5_f64).ln() / (dur / tick)).exp()
}

impl Baseliner {
    pub fn run(&self) -> anyhow::Result<()> {
        /*
         * 135 seconds to decay to 50% for the slow factor and
         * 0.4 seconds to decay to 50% for the fast factor.
         * The fast one can be adjusted to tune, try anything from 0.01 to 3.0 to get more or less sensitivity
         * with more sensitivity we respond faster to bloat, but are at risk from triggering due to lag spikes that
         * aren't bloat related, with less sensitivity (bigger numbers) we smooth through quick spikes
         * but take longer to respond to real bufferbloat
         */
        let slow_factor = ewma_factor(self.config.tick_interval, 135.0);
        let fast_factor = ewma_factor(self.config.tick_interval, 0.4);

        loop {
            let time_data = self.stats_receiver.recv()?;

            let mut owd_baseline_map = self.owd_baseline.lock().unwrap();
            let mut owd_recent_map = self.owd_recent.lock().unwrap();

            let owd_baseline_new = ReflectorStats {
                down_ewma: time_data.down_time,
                up_ewma: time_data.up_time,
                last_receive_time_s: time_data.last_receive_time_s,
            };

            let owd_recent_new = ReflectorStats {
                down_ewma: time_data.down_time,
                up_ewma: time_data.up_time,
                last_receive_time_s: time_data.last_receive_time_s,
            };

            let owd_baseline = owd_baseline_map
                .entry(time_data.reflector)
                .or_insert(owd_baseline_new);

            let owd_recent = owd_recent_map
                .entry(time_data.reflector)
                .or_insert(owd_recent_new);

            if time_data
                .last_receive_time_s
                .duration_since(owd_baseline.last_receive_time_s)
                .as_secs_f64()
                > 30.0
                || time_data
                    .last_receive_time_s
                    .duration_since(owd_recent.last_receive_time_s)
                    .as_secs_f64()
                    > 30.0
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
                // mark the data as bad by setting the receive time to the time autorate was started
                owd_baseline.last_receive_time_s = self.start_time;
                owd_recent.last_receive_time_s = self.start_time;
                info!(
                    "Reflector {} has OWD > 5 seconds more than baseline, triggering reselection",
                    time_data.reflector
                );
                // If reselection is disabled this would trigger an error
                // so just ignore the result
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
