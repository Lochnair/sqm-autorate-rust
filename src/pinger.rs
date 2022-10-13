use crate::MeasurementType;
use etherparse::ReadError;
use log::{debug, error};
use nix::errno::Errno;
use nix::sys::socket::{
    recvfrom, sendto, socket, AddressFamily, MsgFlags, SockFlag, SockProtocol, SockType,
    SockaddrIn, SockaddrIn6, SockaddrLike,
};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
use std::os::unix::io::RawFd;
use std::str::FromStr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, RwLock};
use std::time::Duration;
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
    #[error("Socket error")]
    Socket(#[from] Errno),
    #[error("Wrong ICMP identifier (expected {expected:?}, found {found:?})")]
    WrongID { expected: u16, found: u16 },
}

#[allow(dead_code)]
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
    pub last_receive_time_s: f64,
}

fn open_socket(type_: MeasurementType) -> nix::Result<RawFd> {
    match type_ {
        MeasurementType::ICMP | MeasurementType::ICMPTimestamps => {
            socket(
                AddressFamily::Inet,
                SockType::Raw,
                SockFlag::empty(), /* value */
                SockProtocol::ICMP,
            )
        }
        MeasurementType::NTP => {
            socket(
                AddressFamily::Inet,
                SockType::Datagram,
                SockFlag::empty(), /* value */
                SockProtocol::Udp,
            )
        }
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
        let socket = open_socket(type_)?;

        loop {
            let mut v: Vec<u8> = vec![0; 40];
            let mut buf = v.as_mut_slice();

            let (size, sender) = match recvfrom::<SockaddrIn>(socket, buf) {
                Ok(val) => val,
                Err(_) => continue,
            };

            // etherparse doesn't like when the size in the header doesn't match the buffer
            // so resize the buffer when actual packet size is known
            buf = buf[..size].as_mut();

            let addr_bytes = sender.expect("Should be an address here").ip();
            let addr = IpAddr::from(addr_bytes.to_be_bytes());

            let reflectors = reflectors_lock.read().unwrap();
            if !reflectors.contains(&addr) {
                continue;
            }

            let reply = match self.parse_packet(id, addr, buf) {
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
        let socket = open_socket(type_)?;

        let mut seq: u16 = 0;
        let tick_duration_ms: u16 = 500;
        let sleep_duration = Duration::from_millis(tick_duration_ms as u64);

        loop {
            let reflectors_unlocked = reflectors_lock.read().unwrap();
            let reflectors = reflectors_unlocked.clone();
            drop(reflectors_unlocked);

            for reflector in reflectors.iter() {
                let addr: Box<dyn SockaddrLike>;

                match reflector.is_ipv4() {
                    true => {
                        let ip4 = Ipv4Addr::from_str(&*reflector.to_string()).unwrap();
                        let sock4 = SocketAddrV4::new(ip4, 0);
                        addr = Box::new(SockaddrIn::from(sock4));
                    }
                    false => {
                        let ip6 = Ipv6Addr::from_str(&*reflector.to_string()).unwrap();
                        let sock6 = SocketAddrV6::new(ip6, 0, 0, 0);
                        addr = Box::new(SockaddrIn6::from(sock6));
                    }
                }

                let buf_v = self.craft_packet(id, seq);
                let buf = buf_v.as_slice();

                sendto(socket, buf, addr.as_ref(), MsgFlags::empty()).expect("Couldn't send ping");
            }

            thread::sleep(sleep_duration);

            seq += 1;

            if seq >= u16::MAX {
                seq = 0;
            }
        }
    }

    fn craft_packet(&self, id: u16, seq: u16) -> Vec<u8>;
}
