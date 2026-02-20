use bincode::error::DecodeError;
use bincode::BorrowDecode;
use neli::consts::nl::NlmF;
use neli::consts::rtnl::{Arphrd, Iff, Ifla, RtAddrFamily, Rtm, Tca};
use neli::consts::socket::NlFamily;
use neli::err::{RouterError, SerError};
use neli::nl::{NlPayload, Nlmsghdr};
use neli::router::synchronous::NlRouter;
use neli::rtnl::{Ifinfomsg, IfinfomsgBuilder, RtattrBuilder, Tcmsg, TcmsgBuilder};
use neli::types::{Buffer, RtBuffer};
use neli::utils::Groups;
use std::io;
use std::str::Utf8Error;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NetlinkError {
    #[error("Couldn't deserialize to struct")]
    Deserialization(#[from] Box<DecodeError>),

    #[error("Couldn't find intreface `{0}`")]
    InterfaceNotFound(String),

    #[error("Netlink interface error")]
    NlInterfaceError(#[from] RouterError<Rtm, Ifinfomsg>),

    #[error("Something went wrong while finding qdisc")]
    NlQdiscError(#[from] RouterError<Rtm, Tcmsg>),

    #[error("Couldn't find CAKE qdisc on interface `{0}`")]
    NoQdiscFound(String),

    #[error("Couldn't find interface statistics: `{0}`")]
    NoInterfaceStatsFound(String),

    #[error("Couldn't open Netlink socket")]
    OpenSocket(#[from] io::Error),

    #[error("Serialization error")]
    Serialization(#[from] SerError),

    #[error("Error happened while parsing UTF-8 string")]
    Utf8Error(#[from] Utf8Error),

    #[error("Invalid Rtm type (expected {expected:?}, found {found:?})")]
    WrongType { expected: Rtm, found: Rtm },
}

#[derive(Clone, Copy, Debug)]
pub struct Qdisc {
    ifindex: i32,
    parent: u32,
}

#[derive(BorrowDecode, Copy, Clone, Default, Debug)]
#[repr(C)]
pub struct RtnlLinkStats64 {
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_errors: u64,
    pub tx_errors: u64,
    pub rx_dropped: u64,
    pub tx_dropped: u64,
    pub multicast: u64,
    pub collisions: u64,
    pub rx_length_errors: u64,
    pub rx_over_errors: u64,
    pub rx_crc_errors: u64,
    pub rx_frame_errors: u64,
    pub rx_fifo_errors: u64,
    pub rx_missed_errors: u64,
    pub tx_aborted_errors: u64,
    pub tx_carrier_errors: u64,
    pub tx_fifo_errors: u64,
    pub tx_heartbeat_errors: u64,
    pub tx_window_errors: u64,
    pub rx_compressed: u64,
    pub tx_compressed: u64,
    pub rx_nohandler: u64,
}

pub enum TcaCake {
    BaseRate64 = 2,
}

pub struct Netlink {}

impl Netlink {
    fn nl_interface_get(
        socket: &NlRouter,
        ifname: &str,
    ) -> Result<neli::router::synchronous::NlRouterReceiverHandle<Rtm, Ifinfomsg>, NetlinkError>
    {
        let mut attrs = RtBuffer::new();

        const RTEXT_FILTER_VF: i32 = 1 << 0;

        let attr_ifname = RtattrBuilder::default()
            .rta_type(Ifla::Ifname)
            .rta_payload(ifname)
            .build()
            .unwrap();
        let attr_ext_mask = RtattrBuilder::default()
            .rta_type(Ifla::ExtMask)
            .rta_payload(RTEXT_FILTER_VF)
            .build()
            .unwrap();
        attrs.push(attr_ifname);
        attrs.push(attr_ext_mask);

        let if_msg = IfinfomsgBuilder::default()
            .ifi_family(RtAddrFamily::Unspecified)
            .ifi_type(Arphrd::None)
            .ifi_index(-1)
            .ifi_flags(Iff::empty())
            .ifi_change(Iff::empty())
            .rtattrs(attrs)
            .build()
            .unwrap();

        let recv = socket.send::<_, _, Rtm, Ifinfomsg>(
            Rtm::Getlink,
            NlmF::REQUEST | NlmF::ACK,
            NlPayload::Payload(if_msg),
        )?;

        Ok(recv)
    }

    pub fn find_interface(ifname: &str) -> Result<i32, NetlinkError> {
        let (socket, _) = NlRouter::connect(NlFamily::Route, None, Groups::empty()).unwrap();

        let recv = Self::nl_interface_get(&socket, ifname)?;

        for response in recv {
            let header: Nlmsghdr<Rtm, Ifinfomsg> = response?;

            if header.nl_type() != &Rtm::Newlink {
                return Err(NetlinkError::WrongType {
                    expected: Rtm::Newlink,
                    found: *header.nl_type(),
                });
            }

            if let NlPayload::Payload(p) = header.nl_payload() {
                return Ok(*p.ifi_index());
            }
        }

        // we shouldn't reach here
        Err(NetlinkError::InterfaceNotFound(ifname.to_string()))
    }

    pub fn get_interface_stats(ifname: &str) -> Result<RtnlLinkStats64, NetlinkError> {
        let (socket, _) = NlRouter::connect(NlFamily::Route, None, Groups::empty()).unwrap();

        let recv = Self::nl_interface_get(&socket, ifname)?;

        for response in recv {
            let header: Nlmsghdr<Rtm, Ifinfomsg> = response?;

            if header.nl_type() != &Rtm::Newlink {
                return Err(NetlinkError::WrongType {
                    expected: Rtm::Newlink,
                    found: *header.nl_type(),
                });
            }

            if let NlPayload::Payload(p) = header.nl_payload() {
                for attr in p.rtattrs().iter() {
                    if attr.rta_type() == &Ifla::Stats64 {
                        let buf = attr.rta_payload().as_ref();

                        let stats: RtnlLinkStats64 =
                            bincode::borrow_decode_from_slice(buf, bincode::config::standard())
                                .map_err(|e| NetlinkError::Deserialization(Box::new(e)))?
                                .0;

                        return Ok(stats);
                    }
                }
            }
        }

        Err(NetlinkError::NoInterfaceStatsFound(ifname.to_string()))
    }

    pub fn qdisc_from_ifindex(ifindex: i32) -> Result<Qdisc, NetlinkError> {
        let (socket, _) = NlRouter::connect(NlFamily::Route, None, Groups::empty()).unwrap();
        let tc_msg = TcmsgBuilder::default()
            .tcm_family(u8::from(RtAddrFamily::Unspecified))
            .tcm_ifindex(ifindex)
            .tcm_handle(0)
            .tcm_parent(0)
            .tcm_info(0)
            .rtattrs(RtBuffer::new())
            .build()
            .unwrap();

        let recv = socket.send::<_, _, Rtm, Tcmsg>(
            Rtm::Getqdisc,
            NlmF::REQUEST | NlmF::DUMP,
            NlPayload::Payload(tc_msg),
        )?;

        for response in recv {
            let header: Nlmsghdr<Rtm, Tcmsg> = response?;

            if let NlPayload::Payload(p) = header.nl_payload() {
                if header.nl_type() != &Rtm::Newqdisc {
                    return Err(NetlinkError::WrongType {
                        expected: Rtm::Newqdisc,
                        found: *header.nl_type(),
                    });
                }

                if *p.tcm_ifindex() == ifindex {
                    let mut _type = "";

                    for attr in p.rtattrs().iter() {
                        if attr.rta_type() == &Tca::Kind {
                            let buff = attr.rta_payload().as_ref();
                            _type = std::str::from_utf8(buff)?.trim_end_matches('\0');
                            // Null terminator is valid UTF-8, but breaks comparison, so we remove it
                        }
                    }

                    let type_to_look_for = "cake";

                    if _type.eq(type_to_look_for) {
                        let qdisc = Qdisc {
                            ifindex: p.tcm_ifindex as i32,
                            parent: p.tcm_parent,
                        };

                        return Ok(qdisc);
                    }
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
        let mut socket = NlSocketHandle::connect(NlFamily::Route, None, &[])?;
        let bandwidth = bandwidth_kbit * 1000 / 8;

        let mut attrs = RtBuffer::new();

        let attr_type = Rtattr::new(None, Tca::Kind, "cake")?;
        let mut attr_options = Rtattr::new(None, Tca::Options, Buffer::from(Vec::new()))?;
        attr_options.add_nested_attribute(&Rtattr::new(
            None,
            TcaCake::BaseRate64 as u16,
            bandwidth,
        )?)?;

        attrs.push(attr_type);
        attrs.push(attr_options);

        let tc_msg = Tcmsg::new(
            u8::from(RtAddrFamily::Unspecified),
            qdisc.ifindex,
            0,
            qdisc.parent,
            0,
            attrs,
        );

        let nlhdr = Nlmsghdr::new(
            None,
            Rtm::Newqdisc,
            NlmFFlags::new(&[NlmF::Request, NlmF::Ack]),
            None,
            None,
            NlPayload::Payload(tc_msg),
        );

        socket.send(nlhdr)?;
        Ok(())
    }
}
