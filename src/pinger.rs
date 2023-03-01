use crate::MeasurementType;
use etherparse::ReadError;
use log::{debug, error};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
use std::str::FromStr;
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
    InvalidPacket(#[from] ReadError),
    #[error("Invalid protocol")]
    InvalidProtocol(String),
    #[error("Invalid packet type")]
    InvalidType(String),
    #[error("No transport")]
    NoTransport,
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

fn open_socket(type_: MeasurementType) -> io::Result<Socket> {
    match type_ {
        MeasurementType::Icmp | MeasurementType::IcmpTimestamps => {
            Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::ICMPV4))
        }
        MeasurementType::Ntp => Socket::new(Domain::IPV4, Type::DGRAM, None),
        _ => {
            unimplemented!()
        }
    }
}

trait ReadFrom {
    fn read_from(&mut self) -> io::Result<(Vec<u8>, SockAddr)>;
}

impl ReadFrom for Socket {
    fn read_from(&mut self) -> io::Result<(Vec<u8>, SockAddr)> {
        let mut buffer = Vec::with_capacity(4096);
        let (received, addr) = self.recv_from(buffer.spare_capacity_mut())?;

        unsafe {
            buffer.set_len(received);
        }
        Ok((buffer, addr))
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
            let (buf, sender) = match socket.read_from() {
                Ok(val) => val,
                Err(_) => continue,
            };

            // etherparse doesn't like when the size in the header doesn't match the buffer
            // so resize the buffer when actual packet size is known
            //buf = buf[..size].as_mut();

            let addr: IpAddr = sender.as_socket().unwrap().ip();

            let reflectors = reflectors_lock.read().unwrap();
            if !reflectors.contains(&addr) {
                continue;
            }

            let reply = match self.parse_packet(id, addr, buf.as_slice()) {
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

    fn parse_packet(&self, id: u16, reflector: IpAddr, buf: &[u8]) -> Result<PingReply, PingError>;
}

pub trait PingSender {
    fn send(
        &mut self,
        id: u16,
        type_: MeasurementType,
        reflectors_lock: Arc<RwLock<Vec<IpAddr>>>,
    ) -> anyhow::Result<()> {
        let socket = &open_socket(type_)?;

        let mut seq: u16 = 0;
        let tick_duration_ms: u16 = 500;

        loop {
            let reflectors_unlocked = reflectors_lock.read().unwrap();
            let reflectors = reflectors_unlocked.clone();
            drop(reflectors_unlocked);
            let sleep_duration =
                Duration::from_millis((tick_duration_ms / reflectors.len() as u16) as u64);

            for reflector in reflectors.iter() {
                let addr: SockAddr = match reflector.is_ipv4() {
                    true => {
                        let ip4 = Ipv4Addr::from_str(&*reflector.to_string()).unwrap();
                        let sock4 = SocketAddrV4::new(ip4, 0);
                        sock4.into()
                    }
                    false => {
                        let ip6 = Ipv6Addr::from_str(&*reflector.to_string()).unwrap();
                        let sock6 = SocketAddrV6::new(ip6, 0, 0, 0);
                        sock6.into()
                    }
                };

                let buf_v = self.craft_packet(id, seq);
                let buf = buf_v.as_slice();

                socket.send_to(buf, &addr)?;
                thread::sleep(sleep_duration);
            }

            if seq == u16::MAX {
                seq = 0;
            } else {
                seq += 1;
            }
        }
    }

    fn craft_packet(&self, id: u16, seq: u16) -> Vec<u8>;
}
