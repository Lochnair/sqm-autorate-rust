use log::error;
use nix::sys::socket::{
    recvfrom, sendto, socket, AddressFamily, MsgFlags, SockFlag, SockProtocol, SockType,
    SockaddrIn, SockaddrIn6, SockaddrLike,
};
use std::error::Error;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
use std::os::unix::io::RawFd;
use std::str::FromStr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

pub enum SocketType {
    ICMP,
    UDP,
}

#[allow(dead_code)]
pub struct PingReply {
    pub(crate) reflector: IpAddr,
    pub(crate) seq: u16,
    pub(crate) rtt: i64,
    pub(crate) current_time: i64,
    pub(crate) down_time: f64,
    pub(crate) up_time: f64,
    pub(crate) originate_timestamp: i64,
    pub(crate) receive_timestamp: i64,
    pub(crate) transmit_timestamp: i64,
    pub(crate) last_receive_time_s: f64,
}

fn open_socket(type_: SocketType) -> nix::Result<RawFd> {
    match type_ {
        SocketType::ICMP => {
            socket(
                AddressFamily::Inet,
                SockType::Raw,
                SockFlag::empty(), /* value */
                SockProtocol::ICMP,
            )
        }
        SocketType::UDP => {
            socket(
                AddressFamily::Inet,
                SockType::Datagram,
                SockFlag::empty(), /* value */
                SockProtocol::Udp,
            )
        }
    }
}

pub trait PingListener {
    fn listen(
        &mut self,
        id: u16,
        type_: SocketType,
        reflectors_lock: Arc<RwLock<Vec<IpAddr>>>,
        stats_sender: Sender<PingReply>,
    ) -> Result<(), Box<dyn Error>> {
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

            let reply = match self.parse_packet(id, addr, buf, size) {
                Ok(val) => val,
                Err(e) => {
                    error!(
                        "Something went wrong while parsing packet: {}",
                        e.to_string()
                    );
                    continue;
                }
            };

            //debug!("Type: {:4}  | Reflector IP: {:>15}  | Seq: {:5}  | Current time: {:8}  |  Originate: {:8}  |  Received time: {:8}  |  Transmit time : {:8}  |  RTT: {:8}  | UL time: {:5}  | DL time: {:5}", "ICMP", addr.to_string(), reply.seq, reply.current_time, reply.originate_timestamp, reply.receive_timestamp, reply.transmit_timestamp, reply.rtt, reply.up_time, reply.down_time);
            stats_sender.send(reply).unwrap();
        }
    }

    fn parse_packet(
        &self,
        id: u16,
        reflector: IpAddr,
        buf: &[u8],
        len: usize,
    ) -> Result<PingReply, Box<dyn Error>>;
}

pub trait PingSender {
    fn send(
        &mut self,
        id: u16,
        type_: SocketType,
        reflectors_lock: Arc<RwLock<Vec<IpAddr>>>,
    ) -> Result<(), Box<dyn Error>> {
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
