use bincode::{deserialize, serialize};
use std::error::Error;
use std::net::IpAddr;
use std::{thread, time};

use crate::Pinger;
use nix::time::{clock_gettime, ClockId};
use pnet::packet::icmp::IcmpTypes::{Timestamp, TimestampReply};
use pnet::packet::ip::IpNextHeaderProtocols;
use pnet::packet::*;
use pnet_transport::TransportChannelType::Layer4;
use pnet_transport::TransportProtocol::Ipv4;
use pnet_transport::*;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Copy, Clone, Default, Debug)]
pub struct ICMPTimestamp {
    type_: u8,
    code: u8,
    checksum: u16,
    identifier: u16,
    sequence: u16,
    originate_time: u32,
    receive_time: u32,
    transmit_time: u32,
}

pub struct PingerICMPTimestamps {
    id: u16,
    reflectors: Vec<IpAddr>,
    rx: TransportReceiver,
    tx: TransportSender,
}

impl Pinger for PingerICMPTimestamps {
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

        PingerICMPTimestamps {
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

            let time = clock_gettime(ClockId::CLOCK_REALTIME).unwrap();
            let time_since_midnight: i64 =
                (time.tv_sec() % 86400 * 1000) + (time.tv_nsec() / 1000000);

            if packet.get_icmp_type() != TimestampReply {
                continue;
            }

            let hdr: ICMPTimestamp = deserialize(packet.packet()).unwrap();

            if hdr.identifier != self.id {
                println!("wrong id: {} != {}", hdr.identifier, self.id);
                continue;
            }

            let rtt: i64 = time_since_midnight - hdr.originate_time.to_be() as i64;
            let dl_time: i64 = time_since_midnight - hdr.transmit_time.to_be() as i64;
            let ul_time: i64 = hdr.receive_time.to_be() as i64 - hdr.originate_time.to_be() as i64;

            println!("Type: {:4}  | Reflector IP: {:>15}  | Seq: {:5}  | Current time: {:8}  |  Originate: {:8}  |  Received time: {:8}  |  Transmit time : {:8}  |  RTT: {:8}  | UL time: {:5}  | DL time: {:5}", "ICMP", sender.to_string(), hdr.sequence, time_since_midnight, hdr.originate_time.to_be(), hdr.receive_time.to_be(), hdr.transmit_time.to_be(), rtt, ul_time, dl_time);
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
        let mut buf = [0u8; 8 + 56];

        let mut packet = pnet::packet::icmp::MutableIcmpPacket::new(&mut buf).unwrap();

        let time = clock_gettime(ClockId::CLOCK_REALTIME).unwrap();
        let time_since_midnight: u32 =
            ((time.tv_sec() % 86400 * 1000) + (time.tv_nsec() / 1000000)) as u32;

        let icmp = ICMPTimestamp {
            type_: 0,
            code: 0,
            checksum: 0,
            identifier: id,
            sequence: seq,
            originate_time: time_since_midnight.to_be(),
            receive_time: 0,
            transmit_time: 0,
        };

        let buf_v: Vec<u8> = serialize(&icmp).unwrap();
        let payload = &buf_v[4..];

        packet.populate(&icmp::Icmp {
            icmp_type: Timestamp,
            icmp_code: icmp::IcmpCode::new(0),
            checksum: 0,
            payload: Vec::from(payload),
        });

        packet.set_checksum(icmp::checksum(
            &icmp::IcmpPacket::new(&packet.packet()).unwrap(),
        ));

        self.tx.send_to(packet, *reflector).expect("Error sending");

        Ok(())
    }
}
