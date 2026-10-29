// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use std::io;
use std::sync::Mutex;

use log::info;
use netlink_bindings::{consts, rt_link, tc};
use netlink_socket2::NetlinkSocket;
use thiserror::Error;

use crate::platform::InterfaceStats;

const TC_H_ROOT: u32 = 0xffff_ffff;

#[derive(Debug, Error)]
pub(crate) enum NetlinkError {
    #[error("Couldn't find interface `{0}`")]
    InterfaceNotFound(String),

    #[error("Netlink error: {0}")]
    Netlink(#[from] io::Error),

    #[error("Netlink reply error: {0}")]
    Reply(#[from] netlink_socket2::ReplyError),

    #[error("Couldn't find root CAKE qdisc on interface `{0}`")]
    NoQdiscFound(String),

    #[error("Root CAKE qdisc on interface `{0}` has a zero handle; refusing to modify it")]
    ZeroQdiscHandle(String),

    #[error("Couldn't find interface statistics: `{0}`")]
    NoInterfaceStatsFound(String),

    #[error("Netlink socket lock poisoned")]
    SocketPoisoned,
}

#[derive(Default)]
pub(crate) struct Platform {
    stats_socket: Mutex<NetlinkSocket>,
    shaper_socket: Mutex<NetlinkSocket>,
}

impl Platform {
    pub(crate) fn read_stats(&self, interface: &str) -> Result<InterfaceStats, NetlinkError> {
        let mut socket = self
            .stats_socket
            .lock()
            .map_err(|_| NetlinkError::SocketPoisoned)?;
        let mut request = rt_link::Request::new().op_getlink_do(&Default::default());
        request
            .encode()
            .push_ifname_bytes(interface.as_bytes())
            .push_ext_mask(1 /* RTEXT_FILTER_VF */);

        let mut iter = socket.request(&request)?;
        while let Some(reply) = iter.recv() {
            let (_, attrs) = reply?;
            if let Ok(stats) = attrs.get_stats64() {
                return Ok(InterfaceStats {
                    rx_bytes: stats.rx_bytes,
                    tx_bytes: stats.tx_bytes,
                });
            }
        }

        Err(NetlinkError::NoInterfaceStatsFound(interface.to_string()))
    }

    pub(crate) fn validate_shaper(&self, interface: &str) -> Result<(), NetlinkError> {
        let mut socket = self
            .shaper_socket
            .lock()
            .map_err(|_| NetlinkError::SocketPoisoned)?;
        Self::root_qdisc(&mut socket, interface)?;
        Ok(())
    }

    pub(crate) fn set_rate(
        &self,
        interface: &str,
        bandwidth_kbit: u64,
        dry_run: bool,
    ) -> Result<(), NetlinkError> {
        // Hold the lock through lookup and acknowledgement so requests sharing this socket
        // cannot consume each other's replies.
        let mut socket = self
            .shaper_socket
            .lock()
            .map_err(|_| NetlinkError::SocketPoisoned)?;
        let qdisc = Self::root_qdisc(&mut socket, interface)?;

        if dry_run {
            info!(
                "dry-run: would set qdisc ifindex={} to {} kbit/s",
                qdisc.ifindex, bandwidth_kbit
            );
            return Ok(());
        }

        let bandwidth = bandwidth_kbit * 1000 / 8;
        let mut header = tc::Tcmsg::new();
        header.ifindex = qdisc.ifindex;
        header.parent = TC_H_ROOT;
        header.handle = qdisc.handle;
        // Modify only: CREATE/REPLACE could recreate a qdisc removed after lookup.
        let mut request = tc::Request::new().op_newqdisc_do(&header);
        request
            .encode()
            .push_kind(c"cake")
            .nested_options_cake()
            .push_base_rate64(bandwidth)
            .end_nested();

        let mut iter = socket.request(&request)?;
        iter.recv_ack()?;

        Ok(())
    }

    fn root_qdisc(socket: &mut NetlinkSocket, interface: &str) -> Result<tc::Tcmsg, NetlinkError> {
        let ifindex = Self::find_interface(socket, interface)?;
        let mut header = tc::Tcmsg::new();
        header.ifindex = ifindex;
        header.parent = TC_H_ROOT;
        // Qdisc GET uses the notification path; ECHO requests a unicast reply.
        let request = tc::Request::new()
            .set_flags(consts::NLM_F_ECHO as u16)
            .op_getqdisc_do(&header);

        let mut iter = socket.request(&request)?;
        let (header, attrs) = iter.recv_one()?;
        if attrs.get_kind().is_ok_and(|kind| kind == c"cake") {
            // A zero handle can let even a change-only request replace another kind.
            if header.handle == 0 {
                return Err(NetlinkError::ZeroQdiscHandle(interface.to_string()));
            }
            Ok(header)
        } else {
            Err(NetlinkError::NoQdiscFound(interface.to_string()))
        }
    }

    fn find_interface(socket: &mut NetlinkSocket, interface: &str) -> Result<i32, NetlinkError> {
        let mut request = rt_link::Request::new().op_getlink_do(&Default::default());
        request.encode().push_ifname_bytes(interface.as_bytes());

        let mut iter = socket.request(&request)?;
        let (header, _) = iter
            .recv_one()
            .map_err(|_| NetlinkError::InterfaceNotFound(interface.to_string()))?;
        Ok(header.ifi_index)
    }
}

pub(crate) fn warn_platform_limitations() {}
