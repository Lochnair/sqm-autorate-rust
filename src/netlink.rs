use crate::cake::TcaCake;

use neli::consts::nl::{NlmF, NlmFFlags};
use neli::consts::rtnl::{Arphrd, IffFlags, Ifla, RtAddrFamily, Rtm, Tca};
use neli::consts::socket::NlFamily;
use neli::err::NlError;
use neli::nl::{NlPayload, Nlmsghdr};
use neli::rtnl::{Ifinfomsg, Rtattr, Tcmsg};
use neli::socket::NlSocketHandle;
use neli::types::{Buffer, RtBuffer};
use serde::Deserialize;
use std::error::Error;
use std::fmt::Display;

use bincode::deserialize;
use std::fmt;

#[derive(Default, Debug)]
struct NoQdiscFoundError;

impl Error for NoQdiscFoundError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        // The compiler transparently casts `&sqlx::Error` into a `&dyn Error`
        None
    }
}

// Implement std::fmt::Display for AppError
impl Display for NoQdiscFoundError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Couldn't find a CAKE qdisc for interface") // user-facing output
    }
}

#[derive(Deserialize, Copy, Clone, Default, Debug)]
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

#[allow(dead_code)]
pub struct Qdisc {
    ifindex: i32,
    handle: u32,
    family: u8,
    refcnt: u32,
    parent: u32,
}

fn nl_interface_get(socket: &mut NlSocketHandle, ifname: &str) -> Result<(), Box<dyn Error>> {
    let mut attrs = RtBuffer::new();

    const RTEXT_FILTER_VF: i32 = 1 << 0;

    let attr_ifname = Rtattr::new(None, Ifla::Ifname, ifname).unwrap();
    let attr_ext_mask = Rtattr::new(None, Ifla::ExtMask, RTEXT_FILTER_VF).unwrap();
    attrs.push(attr_ifname);
    attrs.push(attr_ext_mask);

    let if_msg = Ifinfomsg::new(
        RtAddrFamily::Unspecified,
        Arphrd::None,
        -1,
        IffFlags::empty(),
        IffFlags::empty(),
        attrs,
    );

    let nlhdr = Nlmsghdr::new(
        None,
        Rtm::Getlink,
        NlmFFlags::new(&[NlmF::Request, NlmF::Ack]),
        None,
        None,
        NlPayload::Payload(if_msg),
    );

    socket.send(nlhdr)?;

    Ok(())
}

pub fn find_interface(ifname: &str) -> Result<i32, Box<dyn Error>> {
    let mut socket = NlSocketHandle::connect(NlFamily::Route, None, &[]).unwrap();

    nl_interface_get(&mut socket, ifname).expect("Wrong when sending intf");

    for response in socket.iter(false) {
        let header: Nlmsghdr<Rtm, Ifinfomsg> = response?;

        if header.nl_type != Rtm::Newlink {
            return Err(Box::new(NlError::msg("Netlink error retrieving link")));
        }

        if let NlPayload::Payload(p) = header.nl_payload {
            return Ok(p.ifi_index);
        }
    }

    // we shouldn't reach here
    Ok(-1)
}

pub fn get_interface_stats(ifname: &str) -> Result<RtnlLinkStats64, Box<dyn Error>> {
    let mut socket = NlSocketHandle::connect(NlFamily::Route, None, &[]).unwrap();

    nl_interface_get(&mut socket, ifname).expect("Wrong when sending intf");

    for response in socket.iter(false) {
        let header: Nlmsghdr<Rtm, Ifinfomsg> = response?;

        if header.nl_type != Rtm::Newlink {
            return Err(Box::new(NlError::msg("Netlink error retrieving link")));
        }

        if let NlPayload::Payload(p) = header.nl_payload {
            for attr in p.rtattrs.iter() {
                if attr.rta_type == Ifla::Stats64 {
                    let buf = attr.rta_payload.as_ref();

                    let stats: RtnlLinkStats64 = deserialize(buf).unwrap();

                    return Ok(stats);
                }
            }
        }
    }

    Err(Box::new(NlError::msg("Error getting stats")))
}

pub fn find_qdisc(ifindex: i32) -> Result<Qdisc, Box<dyn Error>> {
    let mut socket = NlSocketHandle::connect(NlFamily::Route, None, &[]).unwrap();
    let tc_msg = Tcmsg::new(
        u8::from(RtAddrFamily::Unspecified),
        0,
        0,
        0,
        0,
        RtBuffer::new(),
    );

    let nlhdr = Nlmsghdr::new(
        None,
        Rtm::Getqdisc,
        NlmFFlags::new(&[NlmF::Request, NlmF::Dump]),
        None,
        None,
        NlPayload::Payload(tc_msg),
    );

    socket.send(nlhdr)?;

    for response in socket.iter(false) {
        let header: Nlmsghdr<Rtm, Tcmsg> = response?;

        if let NlPayload::Payload(p) = header.nl_payload {
            if header.nl_type != Rtm::Newqdisc {
                return Err(Box::new(NlError::msg("Netlink error retrieving qdisc")));
            }

            if p.tcm_ifindex == ifindex {
                let mut _type = "";

                for attr in p.rtattrs.iter() {
                    if attr.rta_type == Tca::Kind {
                        let buff = attr.rta_payload.as_ref();
                        _type = std::str::from_utf8(buff)
                            .expect("Found invalid UTF-8 string")
                            .trim_end_matches('\0'); // Null terminator is valid UTF-8, but breaks comparison, so we remove it
                    }
                }

                let type_to_look_for = "cake";

                if _type.eq(type_to_look_for) {
                    let qdisc = Qdisc {
                        ifindex: p.tcm_ifindex as i32,
                        handle: p.tcm_handle,
                        family: p.tcm_family,
                        refcnt: p.tcm_info,
                        parent: p.tcm_parent,
                    };

                    return Ok(qdisc);
                }
            }
        }
    }

    return Err(Box::new(NoQdiscFoundError));
}

pub fn set_qdisc_rate(qdisc: Qdisc, bandwidth: u64) -> Result<(), Box<dyn Error>> {
    let mut socket = NlSocketHandle::connect(NlFamily::Route, None, &[]).unwrap();

    let mut attrs = RtBuffer::new();

    let attr_type = Rtattr::new(None, Tca::Kind, "cake").unwrap();
    let mut attr_options = Rtattr::new(None, Tca::Options, Buffer::from(Vec::new())).unwrap();
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
