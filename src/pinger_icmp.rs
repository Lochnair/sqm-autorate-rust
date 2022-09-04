use bincode::{deserialize, serialize};
use std::error::Error;
use std::net::IpAddr;
use std::{thread, time};

use crate::pinger::{PingListener, PingSender};
use crate::Pinger;
use byteorder::*;
use neli::err::NlError;
use nix::sys::time::TimeValLike;
use nix::time::{clock_gettime, ClockId};
use pnet::packet::icmp::IcmpTypes::EchoReply;
use pnet::packet::ip::IpNextHeaderProtocols;
use pnet::packet::*;
use pnet_transport::TransportChannelType::Layer4;
use pnet_transport::TransportProtocol::Ipv4;
use pnet_transport::*;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Clone, Default, Debug)]
//#[repr(C)]
pub struct ICMPEcho {
    type_: u8,
    code: u8,
    checksum: u16,
    identifier: u16,
    sequence: u16,
    #[serde(with = "serde_bytes")]
    payload: Vec<u8>,
}

pub struct PingerICMPEcho {
    id: u16,
    reflectors: Vec<IpAddr>,
    rx: TransportReceiver,
    tx: TransportSender,
}

pub struct PingerICMPEchoListener {
    id: u16,
    reflectors: Vec<IpAddr>,
}

pub struct PingerICMPEchoSender {
    id: u16,
    reflectors: Vec<IpAddr>,
}

fn calculate_checksum(buffer: &mut [u8]) -> u16 {
    let mut sum = 0u32;
    for word in buffer.chunks(2) {
        let mut part = u16::from(word[0]) << 8;
        if word.len() > 1 {
            part += u16::from(word[1]);
        }
        sum = sum.wrapping_add(u32::from(part));
    }

    while (sum >> 16) > 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }

    !sum as u16
}

impl Pinger for PingerICMPEcho {
    fn new(id: u16, reflectors: Vec<IpAddr>) -> Self {
        let protocol = Layer4(Ipv4(IpNextHeaderProtocols::Icmp));

        // Create a new transport channel, dealing with layer 4 packets on a test protocol
        // It has a receive buffer of 4096 bytes.
        let (tx, rx) = match transport_channel(4096, protocol) {
            Ok((tx, rx)) => (tx, rx),
            Err(e) => panic!(
                "An error occurred when creating the transport channel: {}",
                e
            ),
        };

        PingerICMPEcho {
            id,
            reflectors,
            rx,
            tx,
        }
    }

    fn receive_loop(&mut self) {
        let mut iter = icmp_packet_iter(&mut self.rx);
        loop {
            let (packet, sender) = match iter.next() {
                Ok(res) => res,
                Err(_) => continue,
            };

            let curr_time: u64 = clock_gettime(ClockId::CLOCK_MONOTONIC)
                .unwrap()
                .num_milliseconds() as u64;

            if packet.get_icmp_type() != EchoReply {
                continue;
            }

            let icmp_response = icmp::echo_reply::EchoReplyPacket::new(packet.packet()).unwrap();

            if icmp_response.get_identifier() != self.id {
                continue;
            }

            let sent_time: u64 = icmp_response.payload().read_u64::<NativeEndian>().unwrap();
            let rtt = curr_time - sent_time;

            println!("Type: {:4}  | Reflector IP: {:>15}  | Seq: {:5}  | Current time: {:8}  |  Sent time: {:8}  | RTT: {:8}", "ICMP", sender.to_string(), icmp_response.get_sequence_number(), curr_time, sent_time, rtt);
        }
    }

    fn sender_loop(&mut self) {
        println!("My ID is: {}", self.id);

        let mut seq: u16 = 0;
        let tick_duration_ms: u16 = 500;
        let sleep_duration = time::Duration::from_millis(tick_duration_ms as u64);

        loop {
            let reflectors = self.reflectors.clone();

            for reflector in reflectors.iter() {
                self.send_ping(reflector, self.id, seq)
                    .expect("TODO: panic message");
            }

            thread::sleep(sleep_duration);

            seq += 1;

            if seq > u16::MAX {
                seq = 0;
            }
        }
    }

    fn send_ping(&mut self, reflector: &IpAddr, id: u16, seq: u16) -> Result<(), Box<dyn Error>> {
        let time = clock_gettime(ClockId::CLOCK_MONOTONIC).unwrap();

        let mut buf = [0u8; 8 + 56];

        let mut packet = icmp::echo_request::MutableEchoRequestPacket::new(&mut buf).unwrap();

        packet.populate(&icmp::echo_request::EchoRequest {
            icmp_type: icmp::IcmpTypes::EchoRequest,
            icmp_code: icmp::IcmpCode::new(0),
            checksum: 0,
            identifier: id,
            sequence_number: seq,
            payload: Vec::from(time.num_milliseconds().to_ne_bytes()),
        });

        packet.set_checksum(icmp::checksum(
            &icmp::IcmpPacket::new(&packet.packet()).unwrap(),
        ));

        self.tx.send_to(packet, *reflector).expect("Error sending");

        Ok(())
    }
}

impl PingListener for PingerICMPEchoListener {
    fn new(id: u16, reflectors: Vec<IpAddr>) -> Self {
        PingerICMPEchoListener { id, reflectors }
    }

    fn get_id(&self) -> u16 {
        self.id
    }

    fn get_reflectors(&self) -> Vec<IpAddr> {
        self.reflectors.clone()
    }

    fn parse_packet(buf: &[u8], len: usize) -> Result<(i64, i64, i64), Box<dyn Error>> {
        if len != 44 {
            println!("len: {}", len);
            return Err(Box::new(NlError::msg("Wrong length")));
        }

        let hdr: ICMPEcho = deserialize(buf).expect("Failed to parse ICMP packet");

        todo!()
    }
}

impl PingSender for PingerICMPEchoSender {
    fn new(id: u16, reflectors: Vec<IpAddr>) -> Self {
        PingerICMPEchoSender { id, reflectors }
    }

    fn get_id(&self) -> u16 {
        self.id
    }

    fn get_reflectors(&self) -> Vec<IpAddr> {
        self.reflectors.clone()
    }

    fn craft_packet(&self, seq: u16) -> Vec<u8> {
        let time = clock_gettime(ClockId::CLOCK_MONOTONIC).unwrap();
        let time_u64: u64 = time.num_milliseconds() as u64;

        let mut hdr = ICMPEcho {
            type_: 8,
            code: 0,
            checksum: 0,
            identifier: self.get_id(),
            sequence: seq,
            payload: time_u64.to_ne_bytes().to_vec(),
        };

        println!("id: {}", hdr.identifier);
        println!("seq: {}", hdr.sequence);

        let mut tmp_buf_v = serialize(&hdr).unwrap();
        let mut tmp_buf = tmp_buf_v.as_mut_slice();
        hdr.checksum = calculate_checksum(tmp_buf).to_be();

        serialize(&hdr).unwrap()
    }
}
