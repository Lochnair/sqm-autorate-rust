// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use std::ffi::CString;
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::ptr;

use log::warn;

use super::noop::NoopTrafficControl;
use crate::platform::{InterfaceStats, InterfaceStatsProvider};

#[derive(Debug, thiserror::Error)]
pub(crate) enum FreeBsdInterfaceStatsError {
    #[error("interface name contains a NUL byte")]
    InterfaceNameContainsNul(#[source] std::ffi::NulError),

    #[error("interface `{interface}` was not found")]
    InterfaceNotFound {
        interface: String,
        #[source]
        source: io::Error,
    },

    #[error("failed to query statistics for interface `{interface}`")]
    Sysctl {
        interface: String,
        #[source]
        source: io::Error,
    },

    #[error(
        "unexpected interface-statistics response size for `{interface}`: \
         expected {expected} bytes, received {actual}"
    )]
    UnexpectedResponseSize {
        interface: String,
        expected: usize,
        actual: usize,
    },
}

#[derive(Debug, Default)]
pub(crate) struct FreeBsdInterfaceStats;

impl InterfaceStatsProvider for FreeBsdInterfaceStats {
    type Error = FreeBsdInterfaceStatsError;

    fn read_stats(&mut self, interface: &str) -> Result<InterfaceStats, Self::Error> {
        let interface_name = CString::new(interface)
            .map_err(FreeBsdInterfaceStatsError::InterfaceNameContainsNul)?;
        let interface_index = unsafe { libc::if_nametoindex(interface_name.as_ptr()) };
        if interface_index == 0 {
            return Err(FreeBsdInterfaceStatsError::InterfaceNotFound {
                interface: interface.to_string(),
                source: io::Error::last_os_error(),
            });
        }

        let mib = [
            libc::CTL_NET,
            libc::PF_LINK,
            libc::NETLINK_GENERIC,
            libc::IFMIB_IFDATA,
            interface_index as libc::c_int,
            libc::IFDATA_GENERAL,
        ];

        let mut ifmd = MaybeUninit::<libc::ifmibdata>::uninit();
        let mut returned_len = size_of::<libc::ifmibdata>();

        let result = unsafe {
            libc::sysctl(
                mib.as_ptr(),
                mib.len() as libc::c_uint,
                ifmd.as_mut_ptr().cast::<libc::c_void>(),
                &mut returned_len,
                ptr::null(),
                0,
            )
        };

        if result == -1 {
            return Err(FreeBsdInterfaceStatsError::Sysctl {
                interface: interface.to_string(),
                source: io::Error::last_os_error(),
            });
        }

        let expected_len = size_of::<libc::ifmibdata>();
        if returned_len != expected_len {
            return Err(FreeBsdInterfaceStatsError::UnexpectedResponseSize {
                interface: interface.to_string(),
                expected: expected_len,
                actual: returned_len,
            });
        }

        let ifmd = unsafe { ifmd.assume_init() };

        Ok(InterfaceStats {
            rx_bytes: ifmd.ifmd_data.ifi_ibytes,
            tx_bytes: ifmd.ifmd_data.ifi_obytes,
        })
    }
}

pub(crate) type PlatformInterfaceStats = FreeBsdInterfaceStats;
pub(crate) type PlatformTrafficControl = NoopTrafficControl;

pub(crate) fn interface_stats_provider() -> PlatformInterfaceStats {
    FreeBsdInterfaceStats
}

pub(crate) fn traffic_control_backend() -> PlatformTrafficControl {
    NoopTrafficControl
}

pub(crate) fn warn_platform_limitations() {
    warn!(
        "Traffic control is unavailable on FreeBSD; requested rate changes will be calculated but not applied."
    );
}
