use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant, SystemTime},
};

use anyhow::Context;
use log::{debug, warn};
use tokio::sync::watch;

use crate::{
    diagnostics::{DiagnosticRecord, DiagnosticsSender, DirectionStatistics, SpeedHistory},
    law::{Law, LawContext},
    measurement::model::{MeasurementEvent, MeasurementObservation, MeasurementStream},
    metrics::Metric,
    runtime::{Direction, Shaper},
    settings::Settings,
};

const ANOMALY_THRESHOLD_MS: f64 = 5_000.0;
const BASELINE_HALF_LIFE_SECS: f64 = 135.0;
const RECENT_HALF_LIFE_SECS: f64 = 0.4;
const STALE_AFTER: Duration = Duration::from_secs(30);

struct RateState {
    current_kbps: f64,
    safe_rates: VecDeque<f64>,
}

impl RateState {
    fn new(base_speed: f64, size: u32, rng: &mut fastrand::Rng) -> Self {
        let mut rates = VecDeque::new();

        for _ in 0..size {
            rates.push_back((rng.f64() * 0.2 + 0.75) * base_speed);
        }

        Self {
            current_kbps: base_speed,
            safe_rates: rates,
        }
    }

    fn calculate_rate(
        &mut self,
        shaper: &Shaper,
        traffic_kbps: f64,
        deltas: &[f64],
        delay_threshold_ms: f64,
        high_load_level: f64,
        rng: &mut fastrand::Rng,
    ) -> f64 {
        let mut next_kbps = self.current_kbps;

        if !deltas.is_empty() {
            let delta = deltas[deltas.len() / 2];

            if delta > 0.0 {
                let load = traffic_kbps / self.current_kbps;

                if delta < delay_threshold_ms && load > high_load_level {
                    self.safe_rates.pop_front();
                    self.safe_rates
                        .push_back((self.current_kbps * load).floor());

                    let max_rate = self
                        .safe_rates
                        .iter()
                        .copied()
                        .max_by(f64::total_cmp)
                        .unwrap();
                    next_kbps = self.current_kbps
                        * (1.0 + 0.1 * (1.0_f64 - self.current_kbps / max_rate).max(0.0))
                        + (shaper.base_kbps * 0.03);
                }

                if delta > delay_threshold_ms {
                    let rnd_rate = self.safe_rates[rng.usize(..self.safe_rates.len())];
                    next_kbps = rnd_rate.min(0.9 * self.current_kbps * load);
                }
            }
        }

        next_kbps.max(shaper.min_kbps).floor()
    }
}

pub(crate) struct SqmaLaw {
    settings: Settings,

    reflectors: HashMap<MeasurementStream, ReflectorState>,

    download: Option<RateState>,
    upload: Option<RateState>,

    fast_factor: f64,
    slow_factor: f64,

    rng: fastrand::Rng,
    diagnostics: Option<DiagnosticsSender>,

    // TODO: Eventually the selector should do its own calculations,
    // and not depend on the law for measurements
    control_tx: watch::Sender<Option<ControlSnapshot>>,
}

#[async_trait::async_trait]
impl Law for SqmaLaw {
    async fn run(mut self: Box<Self>, mut ctx: LawContext) -> anyhow::Result<()> {
        self.initialize(&ctx)?;

        let interval = Duration::from_secs_f64(self.settings.advanced_settings.min_change_interval);

        let mut ticker = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(10),
            interval,
        );

        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                event = ctx.measurements.recv() => {
                    let event = event.context("Measurement channel closed")?;
                    self.observe(event, &ctx)?;
                }

                _ = ticker.tick() => {
                    self.evaluate(&mut ctx)?;

                    self.reflectors.retain(|_, state| {
                        Instant::now().saturating_duration_since(state.last_receive_at) <= STALE_AFTER
                    });
                }
            }
        }
    }
}

impl SqmaLaw {
    pub fn new(
        settings: Settings,
        control_tx: watch::Sender<Option<ControlSnapshot>>,
        diagnostics: Option<DiagnosticsSender>,
    ) -> Self {
        let tick = settings.advanced_settings.tick_interval;

        Self {
            settings,
            reflectors: HashMap::new(),
            download: None,
            upload: None,
            fast_factor: (0.5_f64.ln() / (RECENT_HALF_LIFE_SECS / tick)).exp(),
            slow_factor: (0.5_f64.ln() / (BASELINE_HALF_LIFE_SECS / tick)).exp(),
            rng: fastrand::Rng::new(),
            control_tx,
            diagnostics,
        }
    }

    fn initialize(&mut self, ctx: &super::LawContext) -> anyhow::Result<()> {
        let size = self.settings.advanced_settings.speed_hist_size;

        if let Some(shaper) = ctx.shaper(Direction::Download) {
            let initial = shaper.base_kbps * 0.6;

            ctx.set_rate(Direction::Download, initial)?;

            let mut state = RateState::new(shaper.base_kbps, size, &mut self.rng);
            state.current_kbps = initial;

            self.download = Some(state);
        }

        if let Some(shaper) = ctx.shaper(Direction::Upload) {
            let initial = shaper.base_kbps * 0.6;

            ctx.set_rate(Direction::Upload, initial)?;

            let mut state = RateState::new(shaper.base_kbps, size, &mut self.rng);
            state.current_kbps = initial;

            self.upload = Some(state);
        }

        Ok(())
    }

    fn deltas(&self, targets: &[MeasurementStream], now: Instant) -> (Vec<f64>, Vec<f64>) {
        let mut down = Vec::with_capacity(targets.len());
        let mut up = Vec::with_capacity(targets.len());

        let max_age = Duration::from_secs_f64(self.settings.advanced_settings.tick_interval * 2.0);

        for target in targets {
            let Some(state) = self.reflectors.get(target) else {
                continue;
            };

            if now.saturating_duration_since(state.last_receive_at) >= max_age {
                continue;
            }

            let down_delta = state.recent.down - state.baseline.down;
            let up_delta = state.recent.up - state.baseline.up;

            down.push(down_delta);
            up.push(up_delta);

            debug!(
                "Reflector: {} ({:?}) down_delay: {} up_delay: {}",
                target.peer, target.source, down_delta, up_delta
            );
        }

        down.sort_by(f64::total_cmp);
        up.sort_by(f64::total_cmp);

        (down, up)
    }

    fn evaluate(&mut self, ctx: &mut super::LawContext) -> anyhow::Result<()> {
        let now = Instant::now();

        let (down_deltas, up_deltas, target_count) = {
            let targets = ctx.targets.read().expect("active streams lock poisoned");
            let (down, up) = self.deltas(targets.as_slice(), now);
            (down, up, targets.len())
        };

        let num_reflectors = self.settings.reflectors.num_reflectors as usize;
        let minimum = num_reflectors.min(target_count);
        if target_count == 0 || down_deltas.len() < minimum || up_deltas.len() < minimum {
            warn!(
                "Not enough delta values (D: {}, U: {}, need {}), triggering reselection",
                down_deltas.len(),
                up_deltas.len(),
                minimum
            );
            let _ = ctx.reselect_trigger.try_send(true);
        }

        if down_deltas.is_empty() || up_deltas.is_empty() {
            warn!("No reflector data available, dropping to minimum rates");
            ctx.metrics.send(Metric::Event {
                name: "reflector_unavailable",
                reason: "",
                reflector: None,
                tags: &[],
            });

            for (direction, state) in [
                (Direction::Download, self.download.as_mut()),
                (Direction::Upload, self.upload.as_mut()),
            ] {
                let (Some(state), Some(shaper)) = (state, ctx.shaper(direction)) else {
                    continue;
                };

                ctx.set_rate(direction, shaper.min_kbps)?;
                state.current_kbps = shaper.min_kbps;
            }

            return Ok(());
        }

        let Some(traffic) = *ctx.traffic.borrow() else {
            // No traffic sample available. Retain current rates.
            ctx.metrics.send(Metric::Event {
                name: "traffic_sample_unavailable",
                reason: "",
                reflector: None,
                tags: &[],
            });
            return Ok(());
        };

        // Load uses the rate in effect during the traffic sample, before this evaluation.
        let (down_load, up_load) = if self.diagnostics.is_some() {
            (
                self.download
                    .as_ref()
                    .map(|s| traffic.rx_kbps / s.current_kbps),
                self.upload
                    .as_ref()
                    .map(|s| traffic.tx_kbps / s.current_kbps),
            )
        } else {
            (None, None)
        };

        for (direction, state, deltas, traffic_kbps, delay_ms) in [
            (
                Direction::Download,
                self.download.as_mut(),
                down_deltas.as_slice(),
                traffic.rx_kbps,
                self.settings.advanced_settings.download_delay_ms,
            ),
            (
                Direction::Upload,
                self.upload.as_mut(),
                up_deltas.as_slice(),
                traffic.tx_kbps,
                self.settings.advanced_settings.upload_delay_ms,
            ),
        ] {
            let (Some(state), Some(shaper)) = (state, ctx.shaper(direction)) else {
                continue;
            };

            let next = state.calculate_rate(
                shaper,
                traffic_kbps,
                deltas,
                delay_ms,
                self.settings.advanced_settings.high_load_level,
                &mut self.rng,
            );

            if next != state.current_kbps {
                ctx.set_rate(direction, next)?;
                state.current_kbps = next;
            }
        }

        if let Some(diagnostics) = &mut self.diagnostics {
            diagnostics.publish(now, |history_due| DiagnosticRecord {
                time: SystemTime::now(),
                download: self.download.as_ref().map(|s| DirectionStatistics {
                    load: down_load.unwrap(),
                    delta_ms: down_deltas[down_deltas.len() / 2],
                    rate_kbps: s.current_kbps,
                }),
                upload: self.upload.as_ref().map(|s| DirectionStatistics {
                    load: up_load.unwrap(),
                    delta_ms: up_deltas[up_deltas.len() / 2],
                    rate_kbps: s.current_kbps,
                }),
                history: history_due.then(|| SpeedHistory {
                    download: self
                        .download
                        .as_ref()
                        .map(|s| s.safe_rates.iter().copied().collect()),
                    upload: self
                        .upload
                        .as_ref()
                        .map(|s| s.safe_rates.iter().copied().collect()),
                }),
            });
        }

        Ok(())
    }

    fn observe(&mut self, event: MeasurementEvent, ctx: &super::LawContext) -> anyhow::Result<()> {
        let observation = match event {
            MeasurementEvent::Observation(reply) => reply,
            MeasurementEvent::Loss(_) => return Ok(()),
        };

        let sample = EwmaStats::from_reply(&observation);
        let (state, anomaly) = {
            let state = self
                .reflectors
                .entry(observation.stream)
                .or_insert_with(|| ReflectorState::new(sample, observation.observed_at));

            let anomaly = state.update(
                sample,
                observation.observed_at,
                self.slow_factor,
                self.fast_factor,
            );

            (*state, anomaly)
        };

        if anomaly {
            self.reflectors.remove(&observation.stream);
            ctx.metrics.send(Metric::Event {
                name: "reselection",
                reason: "anomaly",
                reflector: Some(observation.stream.peer),
                tags: &[],
            });

            let _ = ctx.reselect_trigger.try_send(true);
        } else {
            ctx.metrics.send(Metric::Baseline {
                reflector: observation.stream.peer,
                baseline_up_ewma: state.baseline.up,
                baseline_down_ewma: state.baseline.down,
                recent_up_ewma: state.recent.up,
                recent_down_ewma: state.recent.down,
            });
        }

        self.control_tx.send_replace(Some(ControlSnapshot {
            reflectors: self.reflectors.clone(),
        }));

        ctx.metrics.send(Metric::Ping {
            reflector: observation.stream.peer,
            measurement_type: observation.stream.source,
            rtt: observation.rtt_ms(),
            up_time: observation
                .one_way
                .map(|o| o.uplink.as_nanos() as f64 / 1000000.0)
                .unwrap_or(observation.rtt_ms() / 2.0),
            down_time: observation
                .one_way
                .map(|o| o.downlink.as_nanos() as f64 / 1000000.0)
                .unwrap_or(observation.rtt_ms() / 2.0),
        });

        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ControlSnapshot {
    pub reflectors: HashMap<MeasurementStream, ReflectorState>,
}

#[derive(Clone, Copy, Debug)]
pub struct EwmaStats {
    pub down: f64,
    pub up: f64,
}

impl EwmaStats {
    fn from_reply(reply: &MeasurementObservation) -> Self {
        Self {
            down: reply
                .one_way
                .map(|o| o.downlink.as_nanos() as f64 / 1000000.0)
                .unwrap_or(reply.rtt_ms() / 2.0),
            up: reply
                .one_way
                .map(|o| o.uplink.as_nanos() as f64 / 1000000.0)
                .unwrap_or(reply.rtt_ms() / 2.0),
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

    fn update(
        &mut self,
        sample: EwmaStats,
        received_at: Instant,
        slow_factor: f64,
        fast_factor: f64,
    ) -> bool {
        if received_at.saturating_duration_since(self.last_receive_at) > STALE_AFTER {
            self.reset(sample, received_at);
        }

        if sample.up > self.baseline.up + ANOMALY_THRESHOLD_MS
            || sample.down > self.baseline.down + ANOMALY_THRESHOLD_MS
        {
            return true;
        }

        self.baseline.update(sample, slow_factor);
        self.recent.update(sample, fast_factor);

        self.baseline.down = self.baseline.down.min(self.recent.down);
        self.baseline.up = self.baseline.up.min(self.recent.up);
        self.last_receive_at = received_at;

        false
    }
}
