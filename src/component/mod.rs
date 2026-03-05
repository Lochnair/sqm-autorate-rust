pub mod algorithm_cake_autorate;
pub mod algorithm_sqm_ewma;
pub mod algorithm_tievolu;
pub mod baseliner_ewma;

#[cfg(feature = "lua")]
pub mod algorithm_lua;

#[cfg(feature = "ebpf")]
pub mod tcp_monitor;

use crate::pinger::PingReply;
use std::net::IpAddr;
use std::time::Instant;

// ── Delay measurement input types ─────────────────────────────────────────────

pub struct TcpExcessSample {
    /// Excess download RTT above per-connection minimum (ms)
    pub dl_excess_ms: f64,
    /// Excess upload RTT above per-connection minimum (ms)
    pub ul_excess_ms: f64,
    pub connection_count: u32,
}

/// Measurements that can be fed into a Baseliner.
pub enum DelayMeasurement {
    Ping(PingReply),
    TcpExcess(TcpExcessSample),
}

// ── Baseliner trait ───────────────────────────────────────────────────────────

/// Maintains per-reflector OWD baselines and exposes delay state to the rate
/// control loop and the reflector selector.
///
/// Implementations are wrapped in `Arc<RwLock<dyn Baseliner>>`:
/// - The baseliner thread takes a **write** lock to call `on_ping` / `on_tcp_excess`.
/// - The rate-controller and reflector-selector take a **read** lock to query state.
pub trait Baseliner: Send + Sync {
    /// Update state from a new ICMP ping reply.
    /// Returns `true` if reflector reselection should be triggered.
    fn on_ping(&mut self, ping: PingReply) -> bool;

    /// Update state from a TCP excess sample. No-op by default.
    fn on_tcp_excess(&mut self, _sample: TcpExcessSample) -> bool {
        false
    }

    /// Sorted ascending OWD deltas (recent EWMA − baseline EWMA) for the
    /// download direction. Only reflectors with data newer than `max_age_secs`
    /// are included.
    fn dl_deltas(&self, now: Instant, max_age_secs: f64) -> Vec<f64>;

    /// Sorted ascending OWD deltas for the upload direction.
    fn ul_deltas(&self, now: Instant, max_age_secs: f64) -> Vec<f64>;

    /// (reflector_ip, rtt_sum) pairs used by the reflector selector to rank
    /// candidates. rtt_sum = down_ewma + up_ewma (in whatever units the
    /// baseliner uses — both directions must use the same unit).
    fn reflector_rtts(&self) -> Vec<(IpAddr, f64)>;
}

// ── Rate algorithm types ──────────────────────────────────────────────────────

/// Context passed to a `RateAlgorithm` each control-loop tick.
pub struct RateContext {
    /// Sorted ascending OWD deltas for download (ms or raw EWMA units).
    pub dl_deltas: Vec<f64>,
    /// Sorted ascending OWD deltas for upload.
    pub ul_deltas: Vec<f64>,
    pub current_dl_rate: f64,
    pub current_ul_rate: f64,
    /// Download utilisation in kbit/s (8 * bytes_delta / elapsed_secs / 1000).
    pub dl_utilisation: f64,
    /// Upload utilisation in kbit/s.
    pub ul_utilisation: f64,
    pub elapsed_secs: f64,
    pub base_dl_rate: f64,
    pub base_ul_rate: f64,
    pub min_dl_rate: f64,
    pub min_ul_rate: f64,
}

/// Decision returned by `RateAlgorithm::calculate`.
pub struct RateResult {
    pub dl_rate: f64,
    pub ul_rate: f64,
    /// Algorithm requests a reflector reselection (e.g. too few active peers).
    pub trigger_reselect: bool,
}

/// A pluggable rate-control algorithm.
pub trait RateAlgorithm: Send {
    /// Starting (dl, ul) rates in kbit/s used when the control loop first starts.
    fn initial_rates(&self) -> (f64, f64);

    /// Minimum sleep between control-loop ticks (seconds).
    fn min_change_interval(&self) -> f64;

    /// Compute new (dl, ul) rates given the current context.
    fn calculate(&mut self, ctx: &RateContext) -> RateResult;
}
