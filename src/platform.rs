// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use std::error::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InterfaceStats {
    pub(crate) rx_bytes: u64,
    pub(crate) tx_bytes: u64,
}

pub(crate) trait InterfaceStatsProvider: Send {
    type Error: Error + Send + Sync + 'static;

    fn read_stats(&mut self, interface: &str) -> Result<InterfaceStats, Self::Error>;
}

pub(crate) trait TrafficControlBackend: Send {
    type Error: Error + Send + Sync + 'static;
    type Handle: Send + 'static;

    fn find_shaper(&mut self, interface: &str) -> Result<Self::Handle, Self::Error>;

    fn set_rate(
        &mut self,
        shaper: &Self::Handle,
        bandwidth_kbit: u64,
        dry_run: bool,
    ) -> Result<(), Self::Error>;
}

#[cfg(not(target_os = "linux"))]
mod noop;

#[cfg(target_os = "freebsd")]
mod freebsd;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(unix)]
pub mod unix;

#[cfg(not(any(target_os = "freebsd", target_os = "linux", target_os = "macos")))]
mod unsupported;

#[cfg(target_os = "freebsd")]
use freebsd as imp;

#[cfg(target_os = "linux")]
use linux as imp;

#[cfg(target_os = "macos")]
use macos as imp;

#[cfg(not(any(target_os = "freebsd", target_os = "linux", target_os = "macos")))]
use unsupported as imp;

pub(crate) use imp::{
    PlatformInterfaceStats, PlatformTrafficControl, interface_stats_provider,
    traffic_control_backend, warn_platform_limitations,
};
