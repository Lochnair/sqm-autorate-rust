// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InterfaceStats {
    pub(crate) rx_bytes: u64,
    pub(crate) tx_bytes: u64,
}

#[cfg(any(target_os = "freebsd", target_os = "macos"))]
mod noop;

#[cfg(target_os = "freebsd")]
mod freebsd;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(any(target_os = "freebsd", target_os = "linux", target_os = "macos")))]
compile_error!("RAVEL supports only Linux, FreeBSD, and macOS");

#[cfg(target_os = "freebsd")]
pub(crate) use freebsd::{Platform, warn_platform_limitations};

#[cfg(target_os = "linux")]
pub(crate) use linux::{Platform, warn_platform_limitations};

#[cfg(target_os = "macos")]
pub(crate) use macos::{Platform, warn_platform_limitations};
