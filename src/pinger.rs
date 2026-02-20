use crate::MeasurementType;
use icmp_socket::socket::IcmpSocket;
use icmp_socket::{IcmpSocket4, Icmpv4Packet};
use log::{debug};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::mpsc::Sender;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use std::{io, thread};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum PingError {
    #[error("Couldn't parse number")]
    InvalidNumber(#[from] io::Error),
    #[error("Error parsing packet")]
    InvalidPacket(String),
    #[error("Invalid packet type")]
    InvalidType(String),
    #[error("Wrong ICMP identifier (expected {expected:?}, found {found:?})")]
    WrongID { expected: u16, found: u16 },
}

pub struct PingReply {
    pub reflector: IpAddr,
    pub seq: u16,
    pub rtt: i64,
    pub current_time: i64,
    pub down_time: f64,
    pub up_time: f64,
    pub originate_timestamp: i64,
    pub receive_timestamp: i64,
    pub transmit_timestamp: i64,
    pub last_receive_time_s: Instant,
}

fn open_socket(type_: MeasurementType) -> io::Result<IcmpSocket4> {
    match type_ {
        MeasurementType::Icmp | MeasurementType::IcmpTimestamps => {
            IcmpSocket4::new()
        },
        _ => {
            unimplemented!()
        }
    }
}

pub trait PingListener {
    fn listen(
        &mut self,
        id: u16,
        type_: MeasurementType,
        reflectors_lock: Arc<RwLock<Vec<IpAddr>>>,
        stats_sender: Sender<PingReply>,
    ) -> anyhow::Result<()> {
        let socket = &mut open_socket(type_)?;

        loop {
            let (pkt, sender) = match socket.rcv_from() {
                Ok(val) => val,
                Err(_) => continue,
            };

            let addr: IpAddr = sender.as_socket().unwrap().ip();

            let reflectors = reflectors_lock.read().unwrap();
            if !reflectors.contains(&addr) {
                continue;
            }

            let reply = match self.parse_packet(id, addr, pkt) {
                Ok(val) => val,
                Err(_) => {
                    // parse_packet will throw an error if it's an unknown protocol etc.
                    // so just quietly move on
                    continue;
                }
            };

            debug!("Type: {:4}  | Reflector IP: {:>15}  | Seq: {:5}  | Current time: {:8}  |  Originate: {:8}  |  Received time: {:8}  |  Transmit time : {:8}  |  RTT: {:8}  | UL time: {:5}  | DL time: {:5}", "ICMP", addr.to_string(), reply.seq, reply.current_time, reply.originate_timestamp, reply.receive_timestamp, reply.transmit_timestamp, reply.rtt, reply.up_time, reply.down_time);
            stats_sender.send(reply).unwrap();
        }
    }

    fn parse_packet(&self, id: u16, reflector: IpAddr, pkt: Icmpv4Packet) -> Result<PingReply, PingError>;
}

pub trait PingSender {
    fn send(
        &mut self,
        id: u16,
        type_: MeasurementType,
        reflectors_lock: Arc<RwLock<Vec<IpAddr>>>,
    ) -> anyhow::Result<()> {
        let mut socket = open_socket(type_)?;

        let mut seq: u16 = 0;
        let tick_duration_ms: u16 = 500;

        loop {
            let reflectors_unlocked = reflectors_lock.read().unwrap();
            let reflectors = reflectors_unlocked.clone();
            drop(reflectors_unlocked);
            let sleep_duration =
                Duration::from_millis((tick_duration_ms / reflectors.len() as u16) as u64);

            for reflector in reflectors.iter() {
                let addr: Ipv4Addr = match reflector {
                    IpAddr::V4(ipv4) => *ipv4,
                    IpAddr::V6(_) => unimplemented!(),
                };

                socket.send_to(addr, self.craft_packet(id, seq))?;
                thread::sleep(sleep_duration);
            }

            if seq == u16::MAX {
                seq = 0;
            } else {
                seq += 1;
            }
        }
    }

    fn craft_packet(&self, id: u16, seq: u16) -> icmp_socket::packet::Icmpv4Packet;
}
