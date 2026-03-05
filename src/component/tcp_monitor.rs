//! eBPF-backed TCP RTT monitor (requires the `ebpf` feature).
//!
//! This module drives [`net_vitals::NetVitals`], converts raw `RttSample`
//! measurements into [`TcpExcessSample`] events, and feeds them to the shared
//! [`Baseliner`] so that TCP-induced latency can complement ICMP OWD data.
//!
//! Per-flow excess is computed as `measured_rtt - per_flow_minimum_rtt`.
//! The per-flow minimum decays very slowly upward over time so that a
//! recovered connection doesn't permanently inflate baselines.

use super::{Baseliner, TcpExcessSample};
use anyhow::Result;
use net_vitals::{FlowEvent, NetVitals, RttSample};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, RwLock};

/// How quickly the per-flow minimum RTT decays upward when measurements are
/// higher than the stored minimum (very slow — one part per million per sample).
const MIN_RELAX_ALPHA: f64 = 0.000_001;

/// Excess below this threshold (in ms) is considered noise and ignored.
const EXCESS_FLOOR_MS: f64 = 1.0;

// ── Per-flow minimum tracker ──────────────────────────────────────────────────

struct FlowMin {
    min_rtt_ns: f64,
}

impl FlowMin {
    fn new(first_rtt_ns: u64) -> Self {
        Self { min_rtt_ns: first_rtt_ns as f64 }
    }

    /// Update the tracked minimum and return excess RTT in milliseconds.
    fn update(&mut self, rtt_ns: u64) -> f64 {
        let rtt = rtt_ns as f64;
        if rtt < self.min_rtt_ns {
            // Snap minimum down immediately.
            self.min_rtt_ns = rtt;
        } else {
            // Allow minimum to drift upward very slowly (gradual forgetting).
            self.min_rtt_ns += (rtt - self.min_rtt_ns) * MIN_RELAX_ALPHA;
        }
        let excess_ns = (rtt - self.min_rtt_ns).max(0.0);
        excess_ns / 1_000_000.0 // ns → ms
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Load the eBPF program on `iface`, then block-poll both ring buffers until
/// `running` is cleared.
///
/// - RTT excess samples are forwarded to `baseliner`.
/// - If `discovery_tx` is `Some`, new TCP flows (outgoing SYN events) whose
///   remote host shows ECN capability are sent as candidate reflector IPs
///   (Phase D dynamic discovery).
///
/// Returns `Err` if the eBPF program fails to load (e.g. insufficient
/// privileges, missing BTF).
pub fn run(
    iface: &str,
    baseliner: Arc<RwLock<dyn Baseliner>>,
    running: Arc<AtomicBool>,
    discovery_tx: Option<SyncSender<IpAddr>>,
) -> Result<()> {
    let mut nv = NetVitals::load(iface)?;
    let mut flows: HashMap<(u32, u32, u16, u16), FlowMin> = HashMap::new();

    nv.run(
        running.as_ref(),
        // on_flow: outgoing SYN observed — use dst_ip as a dynamic reflector candidate
        // when the remote host shows ECN capability (ECE or CWR set in SYN).
        |event: FlowEvent| {
            let Some(ref tx) = discovery_tx else { return };
            if event.ece == 0 && event.cwr == 0 {
                return; // Not ECN-capable; skip.
            }
            // Egress SYN: src=local, dst=remote. We want the remote IP.
            let remote = IpAddr::V4(Ipv4Addr::from(u32::from_be(event.dst_ip)));
            let _ = tx.try_send(remote); // Non-blocking; silently drop if buffer full.
        },
        // on_rtt: RTT sample from the TCP timestamp option echo.
        |sample: RttSample| {
            let key = (sample.src_ip, sample.dst_ip, sample.src_port, sample.dst_port);

            // Update per-flow minimum and compute excess.
            let excess_ms = {
                let flow = flows
                    .entry(key)
                    .or_insert_with(|| FlowMin::new(sample.rtt_ns));
                flow.update(sample.rtt_ns)
            };

            // Skip sub-noise excess to avoid injecting noise into the baseliner.
            if excess_ms < EXCESS_FLOOR_MS {
                return;
            }

            let connection_count = flows.len() as u32;

            // We receive round-trip samples; without separate egress/ingress
            // RTT breakdowns, split excess symmetrically between dl and ul.
            let tcp_sample = TcpExcessSample {
                dl_excess_ms: excess_ms / 2.0,
                ul_excess_ms: excess_ms / 2.0,
                connection_count,
            };

            if let Ok(mut b) = baseliner.write() {
                b.on_tcp_excess(tcp_sample);
            }
        },
    );

    Ok(())
}
