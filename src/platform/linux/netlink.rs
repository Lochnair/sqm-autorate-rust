// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use std::io;
use std::str::Utf8Error;

use log::{info, warn};
use netlink_bindings::{rt_link, tc};
use netlink_socket2::NetlinkSocket;
use std::thread::sleep;
use std::time::{Duration, Instant};
use thiserror::Error;

use crate::SHUTDOWN;
use crate::platform::{InterfaceStats, InterfaceStatsProvider, TrafficControlBackend};
use std::sync::atomic::Ordering;

#[derive(Debug, Error)]
pub(crate) enum NetlinkError {
    #[error("Netlink error: {0}")]
    Netlink(#[from] io::Error),

    #[error("Netlink reply error: {0}")]
    Reply(#[from] netlink_socket2::ReplyError),

    #[error("Something went wrong while finding qdisc")]
    NlQdiscError(String),

    #[error("Couldn't find CAKE qdisc on interface `{0}`")]
    NoQdiscFound(String),

    #[error("Couldn't find interface statistics: `{0}`")]
    NoInterfaceStatsFound(String),

    #[error("Error happened while parsing UTF-8 string")]
    Utf8Error(#[from] Utf8Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Qdisc {
    ifname: String,
    ifindex: i32,
    parent: u32,
}

impl NetlinkError {
    fn is_interface_missing(&self) -> bool {
        match self {
            Self::Netlink(error) => {
                matches!(error.raw_os_error(), Some(libc::ENODEV | libc::ENOENT))
            }
            Self::Reply(error) => matches!(
                error.as_io_error().raw_os_error(),
                Some(libc::ENODEV | libc::ENOENT)
            ),
            _ => false,
        }
    }

    fn is_qdisc_loss(&self) -> bool {
        self.is_interface_missing() || matches!(self, Self::NoQdiscFound(_))
    }
}

#[derive(Debug, Default)]
pub(crate) struct Netlink {}

impl Netlink {
    fn find_interface(ifname: &str) -> Result<i32, NetlinkError> {
        let mut socket = NetlinkSocket::new();

        let mut request = rt_link::Request::new().op_getlink_do(&Default::default());
        request.encode().push_ifname_bytes(ifname.as_bytes());

        let mut iter = socket.request(&request)?;
        let (header, _) = iter.recv_one()?;
        Ok(header.ifi_index)
    }

    fn get_interface_stats(ifname: &str) -> Result<(u64, u64), NetlinkError> {
        let mut socket = NetlinkSocket::new();

        let mut request = rt_link::Request::new().op_getlink_do(&Default::default());
        request
            .encode()
            .push_ifname_bytes(ifname.as_bytes())
            .push_ext_mask(1 /* RTEXT_FILTER_VF */);

        let mut iter = socket.request(&request)?;
        while let Some(reply) = iter.recv() {
            let (_, attrs) = reply?;
            if let Ok(stats) = attrs.get_stats64() {
                return Ok((stats.rx_bytes, stats.tx_bytes));
            }
        }

        Err(NetlinkError::NoInterfaceStatsFound(ifname.to_string()))
    }

    fn qdisc_from_ifindex(ifindex: i32, ifname: &str) -> Result<Qdisc, NetlinkError> {
        let mut socket = NetlinkSocket::new();
        let header = tc::Tcmsg::new();
        let request = tc::Request::new().op_getqdisc_dump(&header);

        let mut iter = socket.request(&request)?;
        while let Some(reply) = iter.recv() {
            let (header, attrs) = reply?;

            if header.ifindex == ifindex
                && let Ok(kind) = attrs.get_kind()
                && kind
                    .to_str()
                    .map_err(|_| NetlinkError::NlQdiscError("Invalid UTF-8".to_string()))?
                    == "cake"
            {
                return Ok(Qdisc {
                    ifname: ifname.to_owned(),
                    ifindex,
                    parent: header.parent,
                });
            }
        }

        Err(NetlinkError::NoQdiscFound(ifname.to_string()))
    }

    fn qdisc_from_ifname(ifname: &str) -> Result<Qdisc, NetlinkError> {
        let ifindex = Netlink::find_interface(ifname)?;
        Netlink::qdisc_from_ifindex(ifindex, ifname)
    }

    fn set_qdisc_rate(
        qdisc: &Qdisc,
        bandwidth_kbit: u64,
        dry_run: bool,
    ) -> Result<(), NetlinkError> {
        if dry_run {
            info!(
                "dry-run: would set qdisc ifindex={} to {} kbit/s",
                qdisc.ifindex, bandwidth_kbit
            );
            return Ok(());
        }

        let mut socket = NetlinkSocket::new();
        let bandwidth = bandwidth_kbit * 1000 / 8;

        let mut header = tc::Tcmsg::new();
        header.ifindex = qdisc.ifindex;
        header.parent = qdisc.parent;
        let mut request = tc::Request::new().set_change().op_newqdisc_do(&header);
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
}

impl InterfaceStatsProvider for Netlink {
    type Error = NetlinkError;

    fn read_stats(&mut self, interface: &str) -> Result<InterfaceStats, Self::Error> {
        let (rx_bytes, tx_bytes) = Self::get_interface_stats(interface)?;
        Ok(InterfaceStats { rx_bytes, tx_bytes })
    }

    fn is_interface_missing(error: &Self::Error) -> bool {
        error.is_interface_missing()
    }
}

impl TrafficControlBackend for Netlink {
    type Error = NetlinkError;
    type Handle = Qdisc;

    fn find_shaper(&mut self, interface: &str) -> Result<Self::Handle, Self::Error> {
        Self::qdisc_from_ifname(interface)
    }

    fn set_rate(
        &mut self,
        shaper: &Self::Handle,
        bandwidth_kbit: u64,
        dry_run: bool,
    ) -> Result<(), Self::Error> {
        let mut last_warning = None;
        loop {
            let result = Self::qdisc_from_ifname(&shaper.ifname)
                .and_then(|current| Self::set_qdisc_rate(&current, bandwidth_kbit, dry_run));
            match result {
                Ok(()) => return Ok(()),
                Err(error) if error.is_qdisc_loss() && !SHUTDOWN.load(Ordering::Relaxed) => {
                    if last_warning
                        .is_none_or(|at: Instant| at.elapsed() >= Duration::from_secs(30))
                    {
                        warn!(
                            "CAKE on {} is unavailable: {error}; waiting for SQM",
                            shaper.ifname
                        );
                        last_warning = Some(Instant::now());
                    }
                    sleep(Duration::from_millis(500));
                }
                Err(error) => return Err(error),
            }
        }
    }
}
