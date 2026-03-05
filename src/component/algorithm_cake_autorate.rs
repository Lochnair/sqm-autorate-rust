use super::{RateAlgorithm, RateContext, RateResult};
use crate::config::CakeAutorateConfig;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Per-direction mutable state.
struct DirState {
    /// Per-reflector OWD baseline (ms). Asymmetric EWMA tracks improvements
    /// quickly (α=0.9) and increases slowly (α=0.001).
    /// Keyed by index — assumes a stable sorted reflector list from the context.
    baselines: Vec<f64>,
    /// Per-reflector delta EWMA (ms).
    delta_ewmas: Vec<f64>,
    /// Circular window of "is delayed" booleans for bufferbloat detection.
    delay_window: VecDeque<bool>,
    /// Current shaper rate (kbit/s).
    current_rate: f64,
    /// Last time we applied a bufferbloat decrease.
    last_bloat_response: Instant,
    /// Last time we applied a decay adjustment.
    last_decay: Instant,
}

impl DirState {
    fn new(base_rate: f64, window_size: usize) -> Self {
        let past = Instant::now() - Duration::from_secs(3600);
        Self {
            baselines: Vec::new(),
            delta_ewmas: Vec::new(),
            delay_window: VecDeque::with_capacity(window_size),
            current_rate: base_rate * 0.6,
            last_bloat_response: past,
            last_decay: past,
        }
    }

    /// Ensure per-reflector vectors are sized to `n`.
    fn ensure_reflectors(&mut self, n: usize, init_delta: f64) {
        if self.baselines.len() < n {
            self.baselines.resize(n, init_delta);
            self.delta_ewmas.resize(n, 0.0);
        }
    }

    fn is_bufferbloat(&self, threshold: usize) -> bool {
        self.delay_window
            .iter()
            .filter(|&&d| d)
            .count()
            >= threshold
    }
}

/// CAKE-autorate rate-control algorithm.
///
/// Maintains per-reflector OWD baselines with an **asymmetric EWMA**:
/// - Fast decrease (α=0.9): quickly tracks path improvements.
/// - Slow increase (α=0.001): ignores transient spikes.
///
/// A circular window of per-sample delay classifications is used for bufferbloat
/// detection. Rate adjustments are:
/// - **Bufferbloat**: 1–25% decrease scaled by severity, with refractory period.
/// - **High load + no delay**: 0–4% increase scaled by latency headroom.
/// - **Low/idle load**: ±1% decay toward configured base rate.
pub struct CakeAutorateAlgorithm {
    config: CakeAutorateConfig,
    base_dl: f64,
    base_ul: f64,
    min_dl: f64,
    min_ul: f64,
    dl: DirState,
    ul: DirState,
}

impl CakeAutorateAlgorithm {
    pub fn new(
        config: CakeAutorateConfig,
        base_dl: f64,
        base_ul: f64,
        min_dl: f64,
        min_ul: f64,
    ) -> Self {
        let w = config.detection_window;
        Self {
            dl: DirState::new(base_dl, w),
            ul: DirState::new(base_ul, w),
            config,
            base_dl,
            base_ul,
            min_dl,
            min_ul,
        }
    }

    /// Update baselines and delta EWMAs from sorted OWD deltas, then classify
    /// whether this sample is delayed.  Returns the mean OWD delta.
    fn update_dir(
        config: &CakeAutorateConfig,
        dir: &mut DirState,
        deltas: &[f64],
        delay_thr_ms: f64,
        utilisation: f64,
        base_rate: f64,
    ) -> f64 {
        if deltas.is_empty() {
            if dir.delay_window.len() >= config.detection_window {
                dir.delay_window.pop_front();
            }
            dir.delay_window.push_back(false);
            return 0.0;
        }

        dir.ensure_reflectors(deltas.len(), deltas[deltas.len() / 2]);

        // Compute wire-packet transmission compensation at current rate.
        // At very low rates a 1500-byte packet takes a non-trivial time.
        let compensation_ms = if dir.current_rate > 0.0 {
            (1000.0 * 1500.0 * 8.0) / (dir.current_rate * 1000.0)
        } else {
            0.0
        };
        let compensated_thr = delay_thr_ms + compensation_ms;

        let mut sum_delta = 0.0;
        let mut n_delayed = 0usize;

        for (i, &delta) in deltas.iter().enumerate() {
            // Asymmetric EWMA baseline update (only during low/idle load)
            if utilisation / base_rate.max(1.0) < config.high_load_thr {
                let alpha = if delta >= dir.baselines[i] {
                    config.alpha_baseline_increase
                } else {
                    config.alpha_baseline_decrease
                };
                dir.baselines[i] = alpha * delta + (1.0 - alpha) * dir.baselines[i];

                // Delta EWMA for reflector quality tracking
                let owd_delta = delta - dir.baselines[i];
                dir.delta_ewmas[i] = config.alpha_delta_ewma * owd_delta
                    + (1.0 - config.alpha_delta_ewma) * dir.delta_ewmas[i];
            }

            let owd_delta = delta - dir.baselines[i];
            sum_delta += owd_delta;
            if owd_delta > compensated_thr {
                n_delayed += 1;
            }
        }

        let avg_delta = sum_delta / deltas.len() as f64;
        let is_delayed = n_delayed > deltas.len() / 2;

        if dir.delay_window.len() >= config.detection_window {
            dir.delay_window.pop_front();
        }
        dir.delay_window.push_back(is_delayed);

        avg_delta
    }

    fn adjust_dir(
        config: &CakeAutorateConfig,
        dir: &mut DirState,
        avg_delta: f64,
        utilisation: f64,
        base_rate: f64,
        min_rate: f64,
        delay_thr_ms: f64,
        max_adjust_up_thr_ms: f64,
        max_adjust_down_thr_ms: f64,
    ) -> f64 {
        let now = Instant::now();
        let rate = dir.current_rate;

        if dir.is_bufferbloat(config.detection_threshold) {
            // Bufferbloat detected — aggressive decrease
            let refractory = Duration::from_secs_f64(config.bufferbloat_refractory_ms / 1000.0);
            if now.duration_since(dir.last_bloat_response) >= refractory {
                let span = (max_adjust_down_thr_ms - delay_thr_ms).max(1.0);
                let severity = ((avg_delta - delay_thr_ms) / span).clamp(0.0, 1.0);
                // Ranges from 1% decrease (no severity) to 25% (full severity)
                let factor = 0.99 - severity * (0.99 - 0.75);
                let new_rate = (rate * factor).max(min_rate).floor();
                dir.last_bloat_response = now;
                dir.current_rate = new_rate;
                return new_rate;
            }
        }

        let load = utilisation / rate.max(1.0);
        if load >= config.high_load_thr {
            // High load, no bufferbloat — conservative increase (0–4%)
            let span = (delay_thr_ms - max_adjust_up_thr_ms).max(1.0);
            let headroom = ((delay_thr_ms - avg_delta) / span).clamp(0.0, 1.0);
            let factor = 1.0 + headroom * 0.04;
            let new_rate = (rate * factor).min(base_rate).floor();
            dir.current_rate = new_rate;
            return new_rate;
        }

        // Low / idle load — decay ±1% toward base rate
        let refractory = Duration::from_secs_f64(config.decay_refractory_ms / 1000.0);
        if now.duration_since(dir.last_decay) >= refractory {
            let new_rate = if rate > base_rate {
                (rate * 0.99).max(base_rate)
            } else if rate < base_rate {
                (rate * 1.01).min(base_rate)
            } else {
                rate
            };
            let new_rate = new_rate.max(min_rate).floor();
            dir.last_decay = now;
            dir.current_rate = new_rate;
            return new_rate;
        }

        rate.max(min_rate).floor()
    }
}

impl RateAlgorithm for CakeAutorateAlgorithm {
    fn initial_rates(&self) -> (f64, f64) {
        (self.base_dl * 0.6, self.base_ul * 0.6)
    }

    fn min_change_interval(&self) -> f64 {
        self.config.min_change_interval
    }

    fn calculate(&mut self, ctx: &RateContext) -> RateResult {
        let dl_avg = Self::update_dir(
            &self.config,
            &mut self.dl,
            &ctx.dl_deltas,
            self.config.dl_delay_thr_ms,
            ctx.dl_utilisation,
            self.base_dl,
        );
        let ul_avg = Self::update_dir(
            &self.config,
            &mut self.ul,
            &ctx.ul_deltas,
            self.config.ul_delay_thr_ms,
            ctx.ul_utilisation,
            self.base_ul,
        );

        let dl_rate = Self::adjust_dir(
            &self.config,
            &mut self.dl,
            dl_avg,
            ctx.dl_utilisation,
            self.base_dl,
            self.min_dl,
            self.config.dl_delay_thr_ms,
            self.config.max_adjust_up_thr_ms,
            self.config.max_adjust_down_thr_ms,
        );
        let ul_rate = Self::adjust_dir(
            &self.config,
            &mut self.ul,
            ul_avg,
            ctx.ul_utilisation,
            self.base_ul,
            self.min_ul,
            self.config.ul_delay_thr_ms,
            self.config.max_adjust_up_thr_ms,
            self.config.max_adjust_down_thr_ms,
        );

        let trigger_reselect = ctx.dl_deltas.is_empty() && ctx.ul_deltas.is_empty();

        RateResult {
            dl_rate,
            ul_rate,
            trigger_reselect,
        }
    }
}
