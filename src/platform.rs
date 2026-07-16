// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

#[cfg(any(not(target_os = "linux"), test))]
use log::debug;
#[cfg(any(not(target_os = "linux"), test))]
use std::collections::HashMap;
#[cfg(any(not(target_os = "linux"), test))]
use std::convert::Infallible;
use std::error::Error;
#[cfg(any(not(any(target_os = "linux", target_os = "macos")), test))]
use thiserror::Error;

#[cfg(target_os = "linux")]
use crate::netlink::Netlink;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::MacOsInterfaceStats;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterfaceStats {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

pub trait InterfaceStatsProvider: Send {
    type Error: Error + Send + Sync + 'static;

    fn read_stats(&mut self, interface: &str) -> Result<InterfaceStats, Self::Error>;
}

pub trait TrafficControlBackend: Send {
    type Error: Error + Send + Sync + 'static;
    type Handle: Send + 'static;

    fn find_shaper(&mut self, interface: &str) -> Result<Self::Handle, Self::Error>;

    fn set_rate(
        &mut self,
        shaper: &Self::Handle,
        bandwidth_kbit: u64,
        dry_run: bool,
    ) -> Result<(), Self::Error>;

    fn is_observe_only(&self) -> bool {
        false
    }
}

#[cfg(any(not(any(target_os = "linux", target_os = "macos")), test))]
#[derive(Debug, Default)]
pub struct UnsupportedInterfaceStats;

#[cfg(any(not(any(target_os = "linux", target_os = "macos")), test))]
#[derive(Debug, Error, PartialEq, Eq)]
#[error("interface statistics are unsupported on target OS `{target_os}`")]
pub struct UnsupportedInterfaceStatsError {
    target_os: &'static str,
}

#[cfg(any(not(any(target_os = "linux", target_os = "macos")), test))]
impl InterfaceStatsProvider for UnsupportedInterfaceStats {
    type Error = UnsupportedInterfaceStatsError;

    fn read_stats(&mut self, _: &str) -> Result<InterfaceStats, Self::Error> {
        Err(UnsupportedInterfaceStatsError {
            target_os: std::env::consts::OS,
        })
    }
}

#[cfg(any(not(target_os = "linux"), test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObserveOnlyShaper {
    interface: String,
}

#[cfg(any(not(target_os = "linux"), test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObserveOnlyRequest {
    bandwidth_kbit: u64,
    dry_run: bool,
}

#[cfg(any(not(target_os = "linux"), test))]
#[derive(Debug, Default)]
pub struct ObserveOnlyTrafficControl {
    last_requests: HashMap<String, ObserveOnlyRequest>,
}

#[cfg(any(not(target_os = "linux"), test))]
impl TrafficControlBackend for ObserveOnlyTrafficControl {
    type Error = Infallible;
    type Handle = ObserveOnlyShaper;

    fn find_shaper(&mut self, interface: &str) -> Result<Self::Handle, Self::Error> {
        Ok(ObserveOnlyShaper {
            interface: interface.to_string(),
        })
    }

    fn set_rate(
        &mut self,
        shaper: &Self::Handle,
        bandwidth_kbit: u64,
        dry_run: bool,
    ) -> Result<(), Self::Error> {
        debug!(
            "Observe-only traffic control requested rate {} kbit/s for interface {} (dry_run={})",
            bandwidth_kbit, shaper.interface, dry_run
        );
        self.last_requests.insert(
            shaper.interface.clone(),
            ObserveOnlyRequest {
                bandwidth_kbit,
                dry_run,
            },
        );
        Ok(())
    }

    fn is_observe_only(&self) -> bool {
        true
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
pub struct UnsupportedTrafficControl;

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub struct UnsupportedShaper;

#[cfg(test)]
#[derive(Debug, Error, PartialEq, Eq)]
#[error("traffic control is unsupported on target OS `{target_os}`")]
pub struct UnsupportedTrafficControlError {
    target_os: &'static str,
}

#[cfg(test)]
impl TrafficControlBackend for UnsupportedTrafficControl {
    type Error = UnsupportedTrafficControlError;
    type Handle = UnsupportedShaper;

    fn find_shaper(&mut self, _: &str) -> Result<Self::Handle, Self::Error> {
        Err(UnsupportedTrafficControlError {
            target_os: std::env::consts::OS,
        })
    }

    fn set_rate(&mut self, _: &UnsupportedShaper, _: u64, _: bool) -> Result<(), Self::Error> {
        Err(UnsupportedTrafficControlError {
            target_os: std::env::consts::OS,
        })
    }
}

#[cfg(target_os = "linux")]
pub type PlatformInterfaceStats = Netlink;
#[cfg(target_os = "macos")]
pub type PlatformInterfaceStats = MacOsInterfaceStats;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub type PlatformInterfaceStats = UnsupportedInterfaceStats;

#[cfg(target_os = "linux")]
pub type PlatformTrafficControl = Netlink;
#[cfg(not(target_os = "linux"))]
pub type PlatformTrafficControl = ObserveOnlyTrafficControl;

pub fn interface_stats_provider() -> PlatformInterfaceStats {
    PlatformInterfaceStats::default()
}

pub fn traffic_control_backend() -> PlatformTrafficControl {
    PlatformTrafficControl::default()
}
