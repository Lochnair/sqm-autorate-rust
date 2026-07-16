// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use std::ffi::{CString, NulError};
use std::io;
use std::mem::size_of;
use std::ptr;

use thiserror::Error;

use super::{InterfaceStats, InterfaceStatsProvider};

const ROUTE_MESSAGE_PREFIX_LEN: usize = size_of::<libc::c_ushort>() + 2;
const SYSCTL_FETCH_ATTEMPTS: usize = 3;

#[derive(Debug, Error)]
pub enum MacOsInterfaceStatsError {
    #[error("interface name contains a NUL byte")]
    InterfaceNameContainsNul(#[source] NulError),

    #[error("interface `{interface}` was not found")]
    InterfaceNotFound {
        interface: String,
        #[source]
        source: io::Error,
    },

    #[error("sysctl failed while {operation}")]
    Sysctl {
        operation: &'static str,
        #[source]
        source: io::Error,
    },

    #[error("malformed route-message response: {0}")]
    MalformedResponse(String),

    #[error("no RTM_IFINFO2 statistics returned for interface index {interface_index}")]
    NoInterfaceStats { interface_index: libc::c_uint },
}

#[derive(Debug, Default)]
pub struct MacOsInterfaceStats;

impl InterfaceStatsProvider for MacOsInterfaceStats {
    type Error = MacOsInterfaceStatsError;

    fn read_stats(&mut self, interface: &str) -> Result<InterfaceStats, Self::Error> {
        let interface_name =
            CString::new(interface).map_err(MacOsInterfaceStatsError::InterfaceNameContainsNul)?;
        // SAFETY: `interface_name` is a live, NUL-terminated C string for the duration of the call.
        let interface_index = unsafe { libc::if_nametoindex(interface_name.as_ptr()) };
        if interface_index == 0 {
            return Err(MacOsInterfaceStatsError::InterfaceNotFound {
                interface: interface.to_string(),
                source: io::Error::last_os_error(),
            });
        }

        let response = route_interface_list(interface_index)?;
        parse_route_messages(&response, interface_index)
    }
}

fn route_interface_list(
    interface_index: libc::c_uint,
) -> Result<Vec<u8>, MacOsInterfaceStatsError> {
    let mut mib = [
        libc::CTL_NET,
        libc::PF_ROUTE,
        0,
        0,
        libc::NET_RT_IFLIST2,
        interface_index as libc::c_int,
    ];

    for attempt in 0..SYSCTL_FETCH_ATTEMPTS {
        let mut required_len = 0;
        // SAFETY: `mib` has the declared six elements, `required_len` is writable, and the
        // null data pointers request only the response size without reading or writing data.
        let size_result = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                ptr::null_mut(),
                &mut required_len,
                ptr::null_mut(),
                0,
            )
        };
        if size_result != 0 {
            return Err(MacOsInterfaceStatsError::Sysctl {
                operation: "querying the interface-list buffer size",
                source: io::Error::last_os_error(),
            });
        }

        let mut response = vec![0_u8; required_len];
        let mut returned_len = required_len;
        // SAFETY: `response` owns `required_len` initialized bytes, `returned_len` describes
        // that allocation, and all other pointers remain valid for the duration of the call.
        let fetch_result = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                response.as_mut_ptr().cast(),
                &mut returned_len,
                ptr::null_mut(),
                0,
            )
        };
        if fetch_result == 0 {
            if returned_len > response.len() {
                return Err(MacOsInterfaceStatsError::MalformedResponse(format!(
                    "sysctl returned length {returned_len} for a {}-byte buffer",
                    response.len()
                )));
            }
            response.truncate(returned_len);
            return Ok(response);
        }

        let source = io::Error::last_os_error();
        let buffer_too_small = matches!(source.raw_os_error(), Some(libc::ENOMEM | libc::ENOBUFS));
        if !buffer_too_small || attempt + 1 == SYSCTL_FETCH_ATTEMPTS {
            return Err(MacOsInterfaceStatsError::Sysctl {
                operation: "fetching the interface list",
                source,
            });
        }
    }

    unreachable!("the bounded sysctl retry loop always returns")
}

fn parse_route_messages(
    response: &[u8],
    requested_interface_index: libc::c_uint,
) -> Result<InterfaceStats, MacOsInterfaceStatsError> {
    let mut offset = 0;

    while offset < response.len() {
        let remaining = response.len() - offset;
        if remaining < ROUTE_MESSAGE_PREFIX_LEN {
            return Err(MacOsInterfaceStatsError::MalformedResponse(format!(
                "truncated route-message prefix at offset {offset}: {remaining} bytes remain"
            )));
        }

        let message_len = usize::from(u16::from_ne_bytes([response[offset], response[offset + 1]]));
        if message_len < ROUTE_MESSAGE_PREFIX_LEN {
            return Err(MacOsInterfaceStatsError::MalformedResponse(format!(
                "route message at offset {offset} has implausible length {message_len}"
            )));
        }

        let message_end = offset.checked_add(message_len).ok_or_else(|| {
            MacOsInterfaceStatsError::MalformedResponse(format!(
                "route message length overflows at offset {offset}"
            ))
        })?;
        if message_end > response.len() {
            return Err(MacOsInterfaceStatsError::MalformedResponse(format!(
                "route message at offset {offset} declares length {message_len}, exceeding the {}-byte response",
                response.len()
            )));
        }

        let message_type = response[offset + 3];
        if message_type == libc::RTM_IFINFO2 as u8 {
            if message_len < size_of::<libc::if_msghdr2>() {
                return Err(MacOsInterfaceStatsError::MalformedResponse(format!(
                    "RTM_IFINFO2 message at offset {offset} has length {message_len}, smaller than {}",
                    size_of::<libc::if_msghdr2>()
                )));
            }

            // SAFETY: the bounds checks above prove that a complete `if_msghdr2` lies within
            // `response`; route messages are not guaranteed to have Rust alignment, so the value
            // is copied with `read_unaligned` instead of creating a reference into the byte slice.
            let header = unsafe {
                ptr::read_unaligned(response.as_ptr().add(offset).cast::<libc::if_msghdr2>())
            };
            if libc::c_uint::from(header.ifm_index) == requested_interface_index {
                return Ok(InterfaceStats {
                    rx_bytes: header.ifm_data.ifi_ibytes,
                    tx_bytes: header.ifm_data.ifi_obytes,
                });
            }
        }

        offset = message_end;
    }

    Err(MacOsInterfaceStatsError::NoInterfaceStats {
        interface_index: requested_interface_index,
    })
}
