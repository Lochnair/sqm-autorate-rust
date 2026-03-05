use super::{Baseliner, TcpExcessSample};
use crate::pinger::PingReply;
use log::info;
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Instant;

#[derive(Copy, Clone)]
struct ReflectorStats {
    down_ewma: f64,
    up_ewma: f64,
    last_receive_time: Instant,
}

fn ewma_factor(tick_interval: f64, half_life_secs: f64) -> f64 {
    ((0.5_f64).ln() / (half_life_secs / tick_interval)).exp()
}

/// EWMA-based OWD baseliner.
///
/// Maintains two EWMA series per reflector:
/// - **Slow** (135 s half-life): long-term baseline
/// - **Fast** (0.4 s half-life): recent delay detector
///
/// The baseline is fast-tracked downward (baseline can never exceed recent)
/// so improvements are captured quickly while increases only register after
/// sustained delay.
pub struct SqmEwmaBaseliner {
    start_time: Instant,
    owd_baseline: HashMap<IpAddr, ReflectorStats>,
    owd_recent: HashMap<IpAddr, ReflectorStats>,
    slow_factor: f64,
    fast_factor: f64,
    tick_interval: f64,
}

impl SqmEwmaBaseliner {
    pub fn new(tick_interval: f64) -> Self {
        Self {
            start_time: Instant::now(),
            owd_baseline: HashMap::new(),
            owd_recent: HashMap::new(),
            slow_factor: ewma_factor(tick_interval, 135.0),
            fast_factor: ewma_factor(tick_interval, 0.4),
            tick_interval,
        }
    }
}

impl Baseliner for SqmEwmaBaseliner {
    fn on_ping(&mut self, ping: PingReply) -> bool {
        let initial = ReflectorStats {
            down_ewma: ping.down_time,
            up_ewma: ping.up_time,
            last_receive_time: ping.last_receive_time_s,
        };

        let baseline = self.owd_baseline.entry(ping.reflector).or_insert(initial);
        let recent = self.owd_recent.entry(ping.reflector).or_insert(initial);

        // Reset both EWMAs if there's been a >30 s data gap (path change, etc.)
        let gap_b = ping
            .last_receive_time_s
            .duration_since(baseline.last_receive_time)
            .as_secs_f64();
        let gap_r = ping
            .last_receive_time_s
            .duration_since(recent.last_receive_time)
            .as_secs_f64();

        if gap_b > 30.0 || gap_r > 30.0 {
            baseline.down_ewma = ping.down_time;
            baseline.up_ewma = ping.up_time;
            baseline.last_receive_time = ping.last_receive_time_s;
            recent.down_ewma = ping.down_time;
            recent.up_ewma = ping.up_time;
            recent.last_receive_time = ping.last_receive_time_s;
        }

        baseline.last_receive_time = ping.last_receive_time_s;
        recent.last_receive_time = ping.last_receive_time_s;

        // Outlier: >5 s above baseline → mark stale and trigger reselection
        if ping.up_time > baseline.up_ewma + 5000.0
            || ping.down_time > baseline.down_ewma + 5000.0
        {
            baseline.last_receive_time = self.start_time;
            recent.last_receive_time = self.start_time;
            info!(
                "Reflector {} OWD >5 s above baseline — triggering reselection",
                ping.reflector
            );
            return true;
        }

        // Update EWMAs
        baseline.down_ewma =
            baseline.down_ewma * self.slow_factor + (1.0 - self.slow_factor) * ping.down_time;
        baseline.up_ewma =
            baseline.up_ewma * self.slow_factor + (1.0 - self.slow_factor) * ping.up_time;

        recent.down_ewma =
            recent.down_ewma * self.fast_factor + (1.0 - self.fast_factor) * ping.down_time;
        recent.up_ewma =
            recent.up_ewma * self.fast_factor + (1.0 - self.fast_factor) * ping.up_time;

        // Baseline can never exceed recent (fast-track path improvements)
        if baseline.down_ewma > recent.down_ewma {
            baseline.down_ewma = recent.down_ewma;
        }
        if baseline.up_ewma > recent.up_ewma {
            baseline.up_ewma = recent.up_ewma;
        }

        info!(
            "Reflector {} dl_base={:.1} ul_base={:.1} dl_recent={:.1} ul_recent={:.1}",
            ping.reflector,
            baseline.down_ewma,
            baseline.up_ewma,
            recent.down_ewma,
            recent.up_ewma,
        );

        false
    }

    fn on_tcp_excess(&mut self, _sample: TcpExcessSample) -> bool {
        false
    }

    fn dl_deltas(&self, now: Instant, max_age_secs: f64) -> Vec<f64> {
        let mut v: Vec<f64> = self
            .owd_baseline
            .iter()
            .filter_map(|(ip, baseline)| {
                let recent = self.owd_recent.get(ip)?;
                let age = now
                    .duration_since(recent.last_receive_time)
                    .as_secs_f64();
                if age < max_age_secs {
                    Some(recent.down_ewma - baseline.down_ewma)
                } else {
                    None
                }
            })
            .collect();
        v.sort_by(|a, b| a.total_cmp(b));
        v
    }

    fn ul_deltas(&self, now: Instant, max_age_secs: f64) -> Vec<f64> {
        let mut v: Vec<f64> = self
            .owd_baseline
            .iter()
            .filter_map(|(ip, baseline)| {
                let recent = self.owd_recent.get(ip)?;
                let age = now
                    .duration_since(recent.last_receive_time)
                    .as_secs_f64();
                if age < max_age_secs {
                    Some(recent.up_ewma - baseline.up_ewma)
                } else {
                    None
                }
            })
            .collect();
        v.sort_by(|a, b| a.total_cmp(b));
        v
    }

    fn reflector_rtts(&self) -> Vec<(IpAddr, f64)> {
        self.owd_recent
            .iter()
            .map(|(ip, s)| (*ip, s.down_ewma + s.up_ewma))
            .collect()
    }
}
