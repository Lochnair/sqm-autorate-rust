// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use std::collections::HashMap;
use std::convert::Infallible;
use std::time::{Duration, Instant};

use log::warn;

use crate::platform::noop::NoopTrafficControl;
use crate::platform::{InterfaceStats, InterfaceStatsProvider};

// Synthetic rates are fixed per interface and intentionally conservative. Counter increments are
// derived from elapsed monotonic time, so changing the polling frequency does not change the
// apparent traffic rate.
const SYNTHETIC_RX_KBIT_RANGE: std::ops::RangeInclusive<u64> = 500..=25_000;
const SYNTHETIC_TX_KBIT_RANGE: std::ops::RangeInclusive<u64> = 250..=10_000;
const INITIAL_COUNTER_BYTES: u64 = 1;

#[derive(Debug)]
struct SyntheticInterfaceState {
    rx_bytes: u64,
    tx_bytes: u64,
    rx_kbit: u64,
    tx_kbit: u64,
    last_update: Instant,
}

impl SyntheticInterfaceState {
    fn new(now: Instant) -> Self {
        Self {
            rx_bytes: INITIAL_COUNTER_BYTES,
            tx_bytes: INITIAL_COUNTER_BYTES,
            rx_kbit: fastrand::u64(SYNTHETIC_RX_KBIT_RANGE),
            tx_kbit: fastrand::u64(SYNTHETIC_TX_KBIT_RANGE),
            last_update: now,
        }
    }

    fn update(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last_update);
        self.rx_bytes = self
            .rx_bytes
            .saturating_add(bytes_for_elapsed(elapsed, self.rx_kbit));
        self.tx_bytes = self
            .tx_bytes
            .saturating_add(bytes_for_elapsed(elapsed, self.tx_kbit));
        self.last_update = now;
    }
}

fn bytes_for_elapsed(elapsed: Duration, bitrate_kbit: u64) -> u64 {
    let bytes = elapsed
        .as_nanos()
        .saturating_mul(u128::from(bitrate_kbit))
        .saturating_mul(1_000)
        / 8_000_000_000;
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

#[derive(Debug, Default)]
pub(crate) struct SyntheticInterfaceStats {
    interfaces: HashMap<String, SyntheticInterfaceState>,
}

impl InterfaceStatsProvider for SyntheticInterfaceStats {
    type Error = Infallible;

    fn read_stats(&mut self, interface: &str) -> Result<InterfaceStats, Self::Error> {
        let now = Instant::now();
        let state = self
            .interfaces
            .entry(interface.to_string())
            .or_insert_with(|| SyntheticInterfaceState::new(now));
        state.update(now);

        Ok(InterfaceStats {
            rx_bytes: state.rx_bytes,
            tx_bytes: state.tx_bytes,
        })
    }
}

pub(crate) type PlatformInterfaceStats = SyntheticInterfaceStats;
pub(crate) type PlatformTrafficControl = NoopTrafficControl;

pub(crate) fn interface_stats_provider() -> PlatformInterfaceStats {
    SyntheticInterfaceStats::default()
}

pub(crate) fn traffic_control_backend() -> PlatformTrafficControl {
    NoopTrafficControl
}

pub(crate) fn warn_platform_limitations() {
    warn!(
        "This operating system has no native interface-statistics or traffic-control backend; synthetic interface load will be used and requested rate changes will not be applied."
    );
}
