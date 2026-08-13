// SPDX-FileCopyrightText: 2022-Present Charles Corrigan mailto:chas-iot@runegate.org (github @chas-iot)
// SPDX-FileCopyrightText: 2022-Present Daniel Lakeland mailto:dlakelan@street-artists.org (github @dlakelan)
// SPDX-FileCopyrightText: 2022-Present Mark Baker mailto:mark@vpost.net (github @Fail-Safe)
// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use crate::SHUTDOWN;
use crate::metrics::{Metric, MetricsSender};
use crate::pinger::PingReply;
use crate::settings::Settings;
use crate::trace::{Recorder, v1};
use flume::{Receiver, Sender};
use log::{debug, info};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

const STALE_AFTER: Duration = Duration::from_secs(30);
const ANOMALY_THRESHOLD_MS: f64 = 5_000.0;
const BASELINE_HALF_LIFE_SECS: f64 = 135.0;
const RECENT_HALF_LIFE_SECS: f64 = 0.4;

#[derive(Clone, Copy, Debug)]
pub struct EwmaStats {
    pub down: f64,
    pub up: f64,
}

impl EwmaStats {
    fn from_reply(reply: &PingReply) -> Self {
        Self {
            down: reply.down_time,
            up: reply.up_time,
        }
    }

    fn update(&mut self, sample: Self, factor: f64) {
        self.down = self.down * factor + sample.down * (1.0 - factor);
        self.up = self.up * factor + sample.up * (1.0 - factor);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ReflectorState {
    pub baseline: EwmaStats,
    pub recent: EwmaStats,
    pub last_receive_at: Instant,
}

impl ReflectorState {
    fn new(sample: EwmaStats, received_at: Instant) -> Self {
        Self {
            baseline: sample,
            recent: sample,
            last_receive_at: received_at,
        }
    }

    fn reset(&mut self, sample: EwmaStats, received_at: Instant) {
        *self = Self::new(sample, received_at);
    }
}

#[derive(Clone, Debug)]
pub struct ControlSnapshot {
    pub generated_at: Instant,
    pub reflectors: HashMap<IpAddr, ReflectorState>,
}

pub struct Baseliner {
    pub settings: Settings,
    pub reselect_trigger: Sender<bool>,
    pub start_time: Instant,
    pub stats_rx: Receiver<PingReply>,
    pub control_tx: Sender<ControlSnapshot>,
    pub selection_tx: Option<Sender<ControlSnapshot>>,
    pub baseline_metrics: MetricsSender,
    pub event_metrics: MetricsSender,
    pub trace: Recorder,
}

fn ewma_factor(tick: f64, dur: f64) -> f64 {
    ((0.5_f64).ln() / (dur / tick)).exp()
}

fn update_reflector_state(
    state: &mut ReflectorState,
    sample: EwmaStats,
    received_at: Instant,
    start_time: Instant,
    slow_factor: f64,
    fast_factor: f64,
) -> bool {
    if received_at.saturating_duration_since(state.last_receive_at) > STALE_AFTER {
        state.reset(sample, received_at);
    }

    if sample.up > state.baseline.up + ANOMALY_THRESHOLD_MS
        || sample.down > state.baseline.down + ANOMALY_THRESHOLD_MS
    {
        state.last_receive_at = start_time;
        return true;
    }

    state.baseline.update(sample, slow_factor);
    state.recent.update(sample, fast_factor);

    state.baseline.down = state.baseline.down.min(state.recent.down);
    state.baseline.up = state.baseline.up.min(state.recent.up);
    state.last_receive_at = received_at;

    false
}

fn process_reply(
    reflectors: &mut HashMap<IpAddr, ReflectorState>,
    reply: &PingReply,
    start_time: Instant,
    slow_factor: f64,
    fast_factor: f64,
) -> (ReflectorState, bool) {
    let sample = EwmaStats::from_reply(reply);
    let state = reflectors
        .entry(reply.reflector)
        .or_insert_with(|| ReflectorState::new(sample, reply.last_receive_time_s));
    let anomaly = update_reflector_state(
        state,
        sample,
        reply.last_receive_time_s,
        start_time,
        slow_factor,
        fast_factor,
    );

    (*state, anomaly)
}

fn control_snapshot(
    reflectors: &HashMap<IpAddr, ReflectorState>,
    generated_at: Instant,
) -> ControlSnapshot {
    ControlSnapshot {
        generated_at,
        reflectors: reflectors.clone(),
    }
}

impl Baseliner {
    pub fn run(&self) -> anyhow::Result<()> {
        let mut reflectors = HashMap::<IpAddr, ReflectorState>::new();

        let slow_factor = ewma_factor(
            self.settings.advanced_settings.tick_interval,
            BASELINE_HALF_LIFE_SECS,
        );
        let fast_factor = ewma_factor(
            self.settings.advanced_settings.tick_interval,
            RECENT_HALF_LIFE_SECS,
        );

        loop {
            if SHUTDOWN.load(Ordering::Relaxed) {
                info!("Baseliner shutting down");
                return Ok(());
            }

            let reply = self.stats_rx.recv()?;
            let reflector = reply.reflector;
            let (state, anomaly) = process_reply(
                &mut reflectors,
                &reply,
                self.start_time,
                slow_factor,
                fast_factor,
            );

            if anomaly {
                info!(
                    "Reflector {} has OWD > 5 seconds more than baseline, triggering reselection",
                    reflector
                );
                self.event_metrics.send(Metric::Event {
                    name: "reselection",
                    reason: "anomaly",
                    reflector: Some(reflector),
                    tags: &[],
                });
                let _ = self.reselect_trigger.try_send(true);
            }

            self.baseline_metrics.send(Metric::Baseline {
                reflector,
                baseline_up_ewma: state.baseline.up,
                baseline_down_ewma: state.baseline.down,
                recent_up_ewma: state.recent.up,
                recent_down_ewma: state.recent.down,
            });

            debug!(
                "Reflector {} up baseline = {} down baseline = {}",
                reflector, state.baseline.up, state.baseline.down
            );
            debug!(
                "Reflector {} up recent = {} down recent = {}",
                reflector, state.recent.up, state.recent.down
            );

            let snapshot = control_snapshot(&reflectors, Instant::now());
            self.trace.linearize(|trace| {
                if trace.is_enabled() {
                    trace.record_at(
                        reply.last_receive_time_s,
                        v1::Event::PingReply {
                            reflector: reply.reflector,
                            down_time_ms: reply.down_time,
                            up_time_ms: reply.up_time,
                        },
                    );
                }

                if let Some(selection_tx) = &self.selection_tx {
                    let _ = selection_tx.send(snapshot.clone());
                }
                self.control_tx.send(snapshot)
            })?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::MeasurementType;
    use std::time::Duration;

    fn state(
        baseline_down: f64,
        baseline_up: f64,
        recent_down: f64,
        recent_up: f64,
        last_receive_at: Instant,
    ) -> ReflectorState {
        ReflectorState {
            baseline: EwmaStats {
                down: baseline_down,
                up: baseline_up,
            },
            recent: EwmaStats {
                down: recent_down,
                up: recent_up,
            },
            last_receive_at,
        }
    }

    fn sample(down: f64, up: f64) -> EwmaStats {
        EwmaStats { down, up }
    }

    fn reply(reflector: IpAddr, down_time: f64, up_time: f64, received_at: Instant) -> PingReply {
        PingReply {
            reflector,
            measurement_type: MeasurementType::Icmp,
            seq: 0,
            rtt: down_time + up_time,
            current_time: 0,
            down_time,
            up_time,
            originate_timestamp: 0,
            receive_timestamp: 0,
            transmit_timestamp: 0,
            last_receive_time_s: received_at,
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
        let mut state = state(100.0, 100.0, 100.0, 100.0, start);
        let anomaly = update_reflector_state(
            &mut state,
            sample(200.0, 300.0),
            receive_time,
            start,
            0.9,
            0.5,
        );

        assert!(!anomaly);
        assert_close(state.baseline.down, 110.0);
        assert_close(state.baseline.up, 120.0);
        assert_close(state.recent.down, 150.0);
        assert_close(state.recent.up, 200.0);
        assert_eq!(state.last_receive_at, receive_time);
    }

    #[test]
    fn keeps_baseline_at_or_below_recent() {
        let start = Instant::now();
        let mut state = state(200.0, 200.0, 100.0, 100.0, start);
        let anomaly = update_reflector_state(
            &mut state,
            sample(100.0, 100.0),
            start + Duration::from_secs(1),
            start,
            0.9,
            0.5,
        );

        assert!(!anomaly);
        assert_close(state.baseline.down, 100.0);
        assert_close(state.baseline.up, 100.0);
        assert_close(state.recent.down, 100.0);
        assert_close(state.recent.up, 100.0);
    }

    #[test]
    fn resets_stats_after_a_long_gap() {
        let start = Instant::now();
        let receive_time = start + Duration::from_secs(31);
        let mut state = state(100.0, 200.0, 300.0, 400.0, start);
        let anomaly = update_reflector_state(
            &mut state,
            sample(500.0, 600.0),
            receive_time,
            start,
            0.9,
            0.5,
        );

        assert!(!anomaly);
        assert_close(state.baseline.down, 500.0);
        assert_close(state.baseline.up, 600.0);
        assert_close(state.recent.down, 500.0);
        assert_close(state.recent.up, 600.0);
        assert_eq!(state.last_receive_at, receive_time);
    }

    #[test]
    fn does_not_reset_at_exactly_thirty_seconds() {
        let start = Instant::now();
        let receive_time = start + STALE_AFTER;
        let mut state = state(100.0, 200.0, 100.0, 200.0, start);
        let anomaly = update_reflector_state(
            &mut state,
            sample(200.0, 300.0),
            receive_time,
            start,
            0.9,
            0.5,
        );

        assert!(!anomaly);
        assert_close(state.baseline.down, 110.0);
        assert_close(state.baseline.up, 210.0);
        assert_close(state.recent.down, 150.0);
        assert_close(state.recent.up, 250.0);
    }

    #[test]
    fn marks_anomalies_for_reselection() {
        let start = Instant::now();
        let mut state = state(100.0, 200.0, 300.0, 400.0, start);
        let anomaly = update_reflector_state(
            &mut state,
            sample(5_101.0, 200.0),
            start + Duration::from_secs(1),
            start,
            0.9,
            0.5,
        );

        assert!(anomaly);
        assert_close(state.baseline.down, 100.0);
        assert_close(state.baseline.up, 200.0);
        assert_close(state.recent.down, 300.0);
        assert_close(state.recent.up, 400.0);
        assert_eq!(state.last_receive_at, start);
    }

    #[test]
    fn does_not_mark_exact_anomaly_threshold() {
        let start = Instant::now();
        let receive_time = start + Duration::from_secs(1);
        let mut state = state(100.0, 200.0, 100.0, 200.0, start);
        let anomaly = update_reflector_state(
            &mut state,
            sample(5_100.0, 5_200.0),
            receive_time,
            start,
            0.9,
            0.5,
        );

        assert!(!anomaly);
        assert_eq!(state.last_receive_at, receive_time);
    }

    #[test]
    fn processed_reply_updates_combined_reflector_state() {
        let start = Instant::now();
        let received_at = start + Duration::from_secs(1);
        let reflector = "192.0.2.1".parse().unwrap();
        let mut reflectors = HashMap::new();
        reflectors.insert(reflector, state(100.0, 100.0, 100.0, 100.0, start));

        let (updated, anomaly) = process_reply(
            &mut reflectors,
            &reply(reflector, 200.0, 300.0, received_at),
            start,
            0.9,
            0.5,
        );

        assert!(!anomaly);
        assert_close(updated.baseline.down, 110.0);
        assert_close(updated.baseline.up, 120.0);
        assert_close(updated.recent.down, 150.0);
        assert_close(updated.recent.up, 200.0);
        assert_eq!(updated.last_receive_at, received_at);
        assert_close(reflectors[&reflector].recent.up, 200.0);
    }

    #[test]
    fn snapshot_contains_updated_combined_state() {
        let start = Instant::now();
        let received_at = start + Duration::from_secs(1);
        let generated_at = received_at + Duration::from_millis(1);
        let reflector = "192.0.2.1".parse().unwrap();
        let mut reflectors = HashMap::new();

        let (updated, anomaly) = process_reply(
            &mut reflectors,
            &reply(reflector, 20.0, 30.0, received_at),
            start,
            0.9,
            0.5,
        );
        let snapshot = control_snapshot(&reflectors, generated_at);

        assert!(!anomaly);
        assert_eq!(snapshot.generated_at, generated_at);
        assert_close(
            snapshot.reflectors[&reflector].baseline.down,
            updated.baseline.down,
        );
        assert_close(snapshot.reflectors[&reflector].recent.up, updated.recent.up);
        assert_eq!(snapshot.reflectors[&reflector].last_receive_at, received_at);
    }
}
