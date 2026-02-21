use std::io;
use std::str::Utf8Error;

use netlink_bindings::{rt_link, tc};
use netlink_socket2::NetlinkSocket;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NetlinkError {
    #[error("Couldn't find interface `{0}`")]
    InterfaceNotFound(String),

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

    #[error("Invalid reply type")]
    InvalidReply,
}

#[derive(Clone, Copy, Debug)]
pub struct Qdisc {
    pub ifindex: i32,
    pub parent: u32,
}

pub struct Netlink {}

impl Netlink {
    pub fn find_interface(ifname: &str) -> Result<i32, NetlinkError> {
        let mut socket = NetlinkSocket::new();

        let mut request = rt_link::Request::new()
            .op_getlink_do_request(&Default::default());
        request.encode().push_ifname_bytes(ifname.as_bytes());

        let mut iter = socket.request(&request)?;
        let (header, _) = iter.recv_one()?;
        Ok(header.ifi_index())
    }

    pub fn get_interface_stats(ifname: &str) -> Result<(u64, u64), NetlinkError> {
        let mut socket = NetlinkSocket::new();

        let mut request = rt_link::Request::new()
            .op_getlink_do_request(&Default::default());
        request.encode()
            .push_ifname_bytes(ifname.as_bytes())
            .push_ext_mask(1 /* RTEXT_FILTER_VF */);

        let mut iter = socket.request(&request)?;
        while let Some(reply) = iter.recv() {
            let (_, attrs) = reply?;
            for attr in attrs {
                if let Ok(rt_link::OpGetlinkDoReply::Stats64(stats)) = attr {
                    return Ok((stats.rx_bytes(), stats.tx_bytes()));
                }
            }
        }

        Err(NetlinkError::NoInterfaceStatsFound(ifname.to_string()))
    }

    pub fn qdisc_from_ifindex(ifindex: i32) -> Result<Qdisc, NetlinkError> {
        let mut socket = NetlinkSocket::new();
        let header = tc::PushTcmsg::new();
        let request = tc::Request::new().op_getqdisc_dump_request(&header);

        let mut iter = socket.request(&request)?;
        while let Some(reply) = iter.recv() {
            let (header, attrs) = reply?;

            if header.ifindex() == ifindex {
                let mut is_cake = false;
                for attr in attrs {
                    if let Ok(tc::OpGetqdiscDumpReply::Kind(kind)) = attr {
                        if kind.to_str().map_err(|_| NetlinkError::NlQdiscError("Invalid UTF-8".to_string()))? == "cake" {
                            is_cake = true;
                            break;
                        }
                    }
                }

                if is_cake {
                    return Ok(Qdisc {
                        ifindex,
                        parent: header.parent(),
                    });
                }
            }
        }

        Err(NetlinkError::NoQdiscFound(ifindex.to_string()))
    }

    pub fn qdisc_from_ifname(ifname: &str) -> Result<Qdisc, NetlinkError> {
        let ifindex = Netlink::find_interface(ifname)?;
        Netlink::qdisc_from_ifindex(ifindex)
    }

    pub fn set_qdisc_rate(qdisc: Qdisc, bandwidth_kbit: u64) -> Result<(), NetlinkError> {
        let mut socket = NetlinkSocket::new();
        let bandwidth = bandwidth_kbit * 1000 / 8;

        let mut header = tc::PushTcmsg::new();
        header.set_ifindex(qdisc.ifindex);
        header.set_parent(qdisc.parent);

        let mut request = tc::Request::new()
            .set_change()
            .op_newqdisc_do_request(&header);
        request.encode()
            .push_kind(c"cake")
            .nested_options_cake()
            .push_base_rate64(bandwidth)
            .end_nested();

        let mut iter = socket.request(&request)?;
        iter.recv_ack()?;

        Ok(())
    }
}
