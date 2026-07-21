// SPDX-FileCopyrightText: 2022-Present Charles Corrigan mailto:chas-iot@runegate.org (github @chas-iot)
// SPDX-FileCopyrightText: 2022-Present Daniel Lakeland mailto:dlakelan@street-artists.org (github @dlakelan)
// SPDX-FileCopyrightText: 2022-Present Mark Baker mailto:mark@vpost.net (github @Fail-Safe)
// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use crate::Config;
use crate::SHUTDOWN;
use crate::metrics::{Metric, MetricsSender};
use crate::pinger::PingReply;
use crate::util::ArcMutex;
use crate::util::MutexExt;
use flume::{Receiver, Sender};
use log::{debug, info};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::Ordering;
use std::time::Instant;

#[derive(Copy, Clone)]
pub struct ReflectorStats {
    pub down_ewma: f64,
    pub up_ewma: f64,
    pub last_receive_time_s: Instant,
}

pub struct Baseliner {
    pub config: Config,
    pub owd_baseline: ArcMutex<HashMap<IpAddr, ReflectorStats>>,
    pub owd_recent: ArcMutex<HashMap<IpAddr, ReflectorStats>>,
    pub reselect_trigger: Sender<bool>,
    pub start_time: Instant,
    pub stats_rx: Receiver<PingReply>,
    pub baseline_metrics: MetricsSender,
    pub event_metrics: MetricsSender,
}

fn ewma_factor(tick: f64, dur: f64) -> f64 {
    ((0.5_f64).ln() / (dur / tick)).exp()
}

fn update_reflector_stats(
    mut baseline: ReflectorStats,
    mut recent: ReflectorStats,
    down_time: f64,
    up_time: f64,
    receive_time: Instant,
    start_time: Instant,
    slow_factor: f64,
    fast_factor: f64,
) -> (ReflectorStats, ReflectorStats, bool) {
    if receive_time
        .duration_since(baseline.last_receive_time_s)
        .as_secs_f64()
        > 30.0
        || receive_time
            .duration_since(recent.last_receive_time_s)
            .as_secs_f64()
            > 30.0
    {
        baseline.down_ewma = down_time;
        baseline.up_ewma = up_time;
        recent.down_ewma = down_time;
        recent.up_ewma = up_time;
    }

    baseline.last_receive_time_s = receive_time;
    recent.last_receive_time_s = receive_time;

    if up_time > baseline.up_ewma + 5000.0 || down_time > baseline.down_ewma + 5000.0 {
        baseline.last_receive_time_s = start_time;
        recent.last_receive_time_s = start_time;
        return (baseline, recent, true);
    }

    baseline.down_ewma = baseline.down_ewma * slow_factor + (1.0 - slow_factor) * down_time;
    baseline.up_ewma = baseline.up_ewma * slow_factor + (1.0 - slow_factor) * up_time;

    recent.down_ewma = recent.down_ewma * fast_factor + (1.0 - fast_factor) * down_time;
    recent.up_ewma = recent.up_ewma * fast_factor + (1.0 - fast_factor) * up_time;

    if baseline.down_ewma > recent.down_ewma {
        baseline.down_ewma = recent.down_ewma;
    }

    if baseline.up_ewma > recent.up_ewma {
        baseline.up_ewma = recent.up_ewma;
    }

    (baseline, recent, false)
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
            if SHUTDOWN.load(Ordering::Relaxed) {
                info!("Baseliner shutting down");
                return Ok(());
            }
            let time_data = self.stats_rx.recv()?;

            let mut owd_baseline_map = self.owd_baseline.lock_anyhow()?;
            let mut owd_recent_map = self.owd_recent.lock_anyhow()?;

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

            let (updated_baseline, updated_recent, anomaly) = update_reflector_stats(
                *owd_baseline,
                *owd_recent,
                time_data.down_time,
                time_data.up_time,
                time_data.last_receive_time_s,
                self.start_time,
                slow_factor,
                fast_factor,
            );
            *owd_baseline = updated_baseline;
            *owd_recent = updated_recent;

            // if this reflection is more than 5 seconds higher than baseline... mark it no good and trigger a reselection
            if anomaly {
                info!(
                    "Reflector {} has OWD > 5 seconds more than baseline, triggering reselection",
                    time_data.reflector
                );
                self.event_metrics.send(Metric::Event {
                    name: "reselection",
                    reason: "anomaly",
                    reflector: Some(time_data.reflector),
                    tags: &[],
                });
                // The reselect channel is bounded to 1,
                // so we use try_send to avoid blocking if the channel is full
                let _ = self.reselect_trigger.try_send(true);
            }

            self.baseline_metrics.send(Metric::Baseline {
                reflector: time_data.reflector,
                baseline_up_ewma: owd_baseline.up_ewma,
                baseline_down_ewma: owd_baseline.down_ewma,
                recent_up_ewma: owd_recent.up_ewma,
                recent_down_ewma: owd_recent.down_ewma,
            });

            debug!(
                "Reflector {} up baseline = {} down baseline = {}",
                time_data.reflector, owd_baseline.up_ewma, owd_baseline.down_ewma
            );
            debug!(
                "Reflector {} up recent = {} down recent = {}",
                time_data.reflector, owd_recent.up_ewma, owd_recent.down_ewma
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn stats(down_ewma: f64, up_ewma: f64, last_receive_time_s: Instant) -> ReflectorStats {
        ReflectorStats {
            down_ewma,
            up_ewma,
            last_receive_time_s,
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn updates_ewmas() {
        let start = Instant::now();
        let receive_time = start + Duration::from_secs(1);
        let (baseline, recent, anomaly) = update_reflector_stats(
            stats(100.0, 100.0, start),
            stats(100.0, 100.0, start),
            200.0,
            300.0,
            receive_time,
            start,
            0.9,
            0.5,
        );

        assert!(!anomaly);
        assert_close(baseline.down_ewma, 110.0);
        assert_close(baseline.up_ewma, 120.0);
        assert_close(recent.down_ewma, 150.0);
        assert_close(recent.up_ewma, 200.0);
        assert_eq!(baseline.last_receive_time_s, receive_time);
        assert_eq!(recent.last_receive_time_s, receive_time);
    }

    #[test]
    fn keeps_baseline_at_or_below_recent() {
        let start = Instant::now();
        let (baseline, recent, anomaly) = update_reflector_stats(
            stats(200.0, 200.0, start),
            stats(100.0, 100.0, start),
            100.0,
            100.0,
            start + Duration::from_secs(1),
            start,
            0.9,
            0.5,
        );

        assert!(!anomaly);
        assert_close(baseline.down_ewma, 100.0);
        assert_close(baseline.up_ewma, 100.0);
        assert_close(recent.down_ewma, 100.0);
        assert_close(recent.up_ewma, 100.0);
    }

    #[test]
    fn resets_stats_after_a_long_gap() {
        let start = Instant::now();
        let receive_time = start + Duration::from_secs(31);
        let (baseline, recent, anomaly) = update_reflector_stats(
            stats(100.0, 200.0, start),
            stats(300.0, 400.0, start),
            500.0,
            600.0,
            receive_time,
            start,
            0.9,
            0.5,
        );

        assert!(!anomaly);
        assert_close(baseline.down_ewma, 500.0);
        assert_close(baseline.up_ewma, 600.0);
        assert_close(recent.down_ewma, 500.0);
        assert_close(recent.up_ewma, 600.0);
        assert_eq!(baseline.last_receive_time_s, receive_time);
        assert_eq!(recent.last_receive_time_s, receive_time);
    }

    #[test]
    fn marks_anomalies_for_reselection() {
        let start = Instant::now();
        let (baseline, recent, anomaly) = update_reflector_stats(
            stats(100.0, 200.0, start),
            stats(300.0, 400.0, start),
            5_101.0,
            200.0,
            start + Duration::from_secs(1),
            start,
            0.9,
            0.5,
        );

        assert!(anomaly);
        assert_close(baseline.down_ewma, 100.0);
        assert_close(baseline.up_ewma, 200.0);
        assert_close(recent.down_ewma, 300.0);
        assert_close(recent.up_ewma, 400.0);
        assert_eq!(baseline.last_receive_time_s, start);
        assert_eq!(recent.last_receive_time_s, start);
    }

    #[test]
    fn does_not_mark_exact_anomaly_threshold() {
        let start = Instant::now();
        let receive_time = start + Duration::from_secs(1);
        let (baseline, recent, anomaly) = update_reflector_stats(
            stats(100.0, 200.0, start),
            stats(100.0, 200.0, start),
            5_100.0,
            5_200.0,
            receive_time,
            start,
            0.9,
            0.5,
        );

        assert!(!anomaly);
        assert_eq!(baseline.last_receive_time_s, receive_time);
        assert_eq!(recent.last_receive_time_s, receive_time);
    }

    #[test]
    fn resets_both_tracks_when_either_track_is_stale() {
        let start = Instant::now();
        let recent_time = start + Duration::from_secs(20);
        let receive_time = start + Duration::from_secs(31);
        let (baseline, recent, anomaly) = update_reflector_stats(
            stats(100.0, 200.0, start),
            stats(300.0, 400.0, recent_time),
            500.0,
            600.0,
            receive_time,
            start,
            0.9,
            0.5,
        );

        assert!(!anomaly);
        assert_close(baseline.down_ewma, 500.0);
        assert_close(baseline.up_ewma, 600.0);
        assert_close(recent.down_ewma, 500.0);
        assert_close(recent.up_ewma, 600.0);
    }
}
