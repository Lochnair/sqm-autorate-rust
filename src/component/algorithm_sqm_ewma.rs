use super::{RateAlgorithm, RateContext, RateResult};
use crate::config::SqmEwmaConfig;

fn generate_initial_speeds(base_speed: f64, size: u32) -> Vec<f64> {
    (0..size)
        .map(|_| (fastrand::f64() * 0.2 + 0.75) * base_speed)
        .collect()
}

/// Per-direction mutable state used by the EWMA algorithm.
struct DirectionState {
    safe_rates: Vec<f64>,
    nrate: usize,
}

impl DirectionState {
    fn new(base_rate: f64, size: u32) -> Self {
        Self {
            safe_rates: generate_initial_speeds(base_rate, size),
            nrate: 0,
        }
    }
}

/// The original sqm-autorate EWMA rate-control algorithm.
///
/// ## Rate decision logic (per direction)
/// - **No delay & high load** (`delta < delay_ms` AND `load > high_load_level`):
///   Increase rate — store current safe rate, then step up toward the historical
///   maximum plus a small fraction of the base rate.
/// - **High delay** (`delta > delay_ms`):
///   Decrease rate — pick a random previously-safe rate, capped at 90% of the
///   current throughput.
/// - Otherwise: hold current rate.
///
/// All rates are clamped to `[min_rate, ∞)`.
pub struct SqmEwmaAlgorithm {
    config: SqmEwmaConfig,
    base_dl: f64,
    base_ul: f64,
    min_dl: f64,
    min_ul: f64,
    dl: DirectionState,
    ul: DirectionState,
}

impl SqmEwmaAlgorithm {
    pub fn new(config: SqmEwmaConfig, base_dl: f64, base_ul: f64, min_dl: f64, min_ul: f64) -> Self {
        let size = config.speed_hist_size;
        Self {
            dl: DirectionState::new(base_dl, size),
            ul: DirectionState::new(base_ul, size),
            config,
            base_dl,
            base_ul,
            min_dl,
            min_ul,
        }
    }
}

/// Core rate-step calculation for a single direction.
///
/// Returns the new rate in kbit/s (already clamped to `min_rate`).
fn calc_direction(
    deltas: &[f64],
    current_rate: f64,
    utilisation: f64,
    delay_ms: f64,
    high_load_level: f64,
    base_rate: f64,
    min_rate: f64,
    state: &mut DirectionState,
) -> f64 {
    if deltas.is_empty() {
        return min_rate;
    }

    let delta_stat = if deltas.len() >= 3 { deltas[2] } else { deltas[0] };
    let mut next_rate = current_rate;

    if delta_stat > 0.0 {
        let load = if current_rate > 0.0 {
            utilisation / current_rate
        } else {
            0.0
        };

        if delta_stat < delay_ms && load > high_load_level {
            // No significant delay and high utilisation → increase
            state.safe_rates[state.nrate] = (current_rate * load).floor();
            let max_safe = state
                .safe_rates
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max);
            next_rate = current_rate
                * (1.0 + 0.1 * (1.0 - current_rate / max_safe).max(0.0))
                + base_rate * 0.03;
            state.nrate = (state.nrate + 1) % state.safe_rates.len();
        }

        if delta_stat > delay_ms {
            // Delay detected → decrease toward a random previously-safe rate
            let rnd_rate = state.safe_rates[fastrand::usize(..state.safe_rates.len())];
            next_rate = rnd_rate.min(0.9 * current_rate * load);
        }
    }

    next_rate.max(min_rate).floor()
}

impl RateAlgorithm for SqmEwmaAlgorithm {
    fn initial_rates(&self) -> (f64, f64) {
        (self.base_dl * 0.6, self.base_ul * 0.6)
    }

    fn min_change_interval(&self) -> f64 {
        self.config.min_change_interval
    }

    fn calculate(&mut self, ctx: &RateContext) -> RateResult {
        let trigger_reselect = ctx.dl_deltas.len() < 5 || ctx.ul_deltas.len() < 5;

        let dl_rate = calc_direction(
            &ctx.dl_deltas,
            ctx.current_dl_rate,
            ctx.dl_utilisation,
            self.config.download_delay_ms,
            self.config.high_load_level,
            self.base_dl,
            self.min_dl,
            &mut self.dl,
        );

        let ul_rate = calc_direction(
            &ctx.ul_deltas,
            ctx.current_ul_rate,
            ctx.ul_utilisation,
            self.config.upload_delay_ms,
            self.config.high_load_level,
            self.base_ul,
            self.min_ul,
            &mut self.ul,
        );

        RateResult {
            dl_rate,
            ul_rate,
            trigger_reselect,
        }
    }
}
