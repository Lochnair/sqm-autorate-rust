use nix::sys::socket::{
    recvfrom, sendto, socket, AddressFamily, MsgFlags, SockFlag, SockProtocol, SockType,
    SockaddrIn, SockaddrIn6, SockaddrLike,
};
use std::error::Error;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
use std::str::FromStr;
use std::thread;
use std::time::Duration;

pub trait PingListener {
    fn new(id: u16, reflectors: Vec<IpAddr>) -> Self;

    fn get_id(&self) -> u16;
    fn get_reflectors(&self) -> Vec<IpAddr>;

    fn listen(&mut self) {
        let socket = socket(
            AddressFamily::Inet,
            SockType::Raw,
            SockFlag::empty(), /* value */
            SockProtocol::ICMP,
        )
        .expect("Couldn't open socket");

        loop {
            let mut v: Vec<u8> = vec![0; 128];
            let buf = v.as_mut_slice();

            let (size, sender) = match recvfrom::<SockaddrIn>(socket, buf) {
                Ok(val) => val,
                Err(_) => continue,
            };

            let (rtt, down_time, up_time) = match Self::parse_packet(buf, size) {
                Ok(val) => val,
                Err(e) => {
                    println!("Shit went to hell: {}", e.to_string());
                    continue;
                }
            };
        }
    }

    fn parse_packet(buf: &[u8], len: usize) -> Result<(i64, i64, i64), Box<dyn Error>>;
}

pub trait PingSender {
    fn new(id: u16, reflectors: Vec<IpAddr>) -> Self;

    fn get_id(&self) -> u16;
    fn get_reflectors(&self) -> Vec<IpAddr>;

    fn send(&mut self) {
        let socket = socket(
            AddressFamily::Inet,
            SockType::Raw,
            SockFlag::empty(), /* value */
            SockProtocol::ICMP,
        )
        .expect("Couldn't open socket");

        let mut seq: u16 = 0;
        let tick_duration_ms: u16 = 500;
        let sleep_duration = Duration::from_millis(tick_duration_ms as u64);

        loop {
            let reflectors = Self::get_reflectors(self);

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

                let buf_v = self.craft_packet(seq);
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

    fn craft_packet(&self, seq: u16) -> Vec<u8>;
}

pub trait Pinger {
    fn new(id: u16, reflectors: Vec<IpAddr>) -> Self;
    fn receive_loop(&mut self);
    fn sender_loop(&mut self);
    fn send_ping(&mut self, reflector: &IpAddr, id: u16, seq: u16) -> Result<(), Box<dyn Error>>;
}
