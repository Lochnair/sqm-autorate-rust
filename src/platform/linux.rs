// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

mod netlink;

pub(crate) type PlatformInterfaceStats = netlink::Netlink;
pub(crate) type PlatformTrafficControl = netlink::Netlink;

pub(crate) fn interface_stats_provider() -> PlatformInterfaceStats {
    netlink::Netlink::default()
}

pub(crate) fn traffic_control_backend() -> PlatformTrafficControl {
    netlink::Netlink::default()
}

pub(crate) fn warn_platform_limitations() {}
