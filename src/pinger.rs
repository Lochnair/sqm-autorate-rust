use byteorder::{BigEndian, ReadBytesExt};
use nix::sys::socket::{
    recvfrom, sendto, AddressFamily, MsgFlags, SockFlag, SockProtocol, SockType, SockaddrIn,
    SockaddrIn6, SockaddrLike,
};
use nix::sys::time::TimeValLike;
use nix::time::{clock_gettime, ClockId};
use std::error::Error;
use std::mem::size_of;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
use std::os::unix::io::RawFd;
use std::str::FromStr;
use std::{thread, time};

struct ICMP {
    icmp_type: u8,
    code: u8,
    checksum: u16,
    identifier: u16,
    sequence: u16,
    payload: u64,
}

impl ICMP {
    fn to_bytes(&self) -> Box<[u8; 16]> {
        let checksum = self.checksum.to_be_bytes();
        let identifier = self.identifier.to_be_bytes();
        let sequence = self.sequence.to_be_bytes();
        let payload = self.payload.to_be_bytes();

        let buf = [
            self.icmp_type,
            self.code,
            checksum[0],
            checksum[1],
            identifier[0],
            identifier[1],
            sequence[0],
            sequence[1],
            payload[0],
            payload[1],
            payload[2],
            payload[3],
            payload[4],
            payload[5],
            payload[6],
            payload[7],
        ];

        return Box::new(buf);
    }
}

impl From<&[u8]> for ICMP {
    fn from(buf: &[u8]) -> Self {
        ICMP {
            icmp_type: buf[0],
            code: buf[1],
            checksum: (&buf[2..3]).read_u16::<BigEndian>().unwrap(),
            identifier: (&buf[4..5]).read_u16::<BigEndian>().unwrap(),
            sequence: (&buf[6..7]).read_u16::<BigEndian>().unwrap(),
            payload: (&buf[8..16]).read_u64::<BigEndian>().unwrap(),
        }
    }
}

impl From<&mut [u8]> for ICMP {
    fn from(buf: &mut [u8]) -> Self {
        ICMP {
            icmp_type: buf[0],
            code: buf[1],
            checksum: (&buf[2..3]).read_u16::<BigEndian>().unwrap(),
            identifier: (&buf[4..5]).read_u16::<BigEndian>().unwrap(),
            sequence: (&buf[6..7]).read_u16::<BigEndian>().unwrap(),
            payload: (&buf[8..16]).read_u64::<BigEndian>().unwrap(),
        }
    }
}

pub trait Pinger {
    fn new(reflectors: Vec<IpAddr>) -> Self;
    fn receive_loop(&self);
    fn receive_ping(&self) -> Result<(), Box<dyn Error>>;
    fn sender_loop(&self);
    fn send_ping(&self, reflector: &IpAddr, seq: u16) -> Result<(), Box<dyn Error>>;
}

#[derive(Clone)]
pub struct PingerICMPEcho {
    reflectors: Vec<IpAddr>,
    socket: RawFd,
}

impl PingerICMPEcho {
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
}

impl Pinger for PingerICMPEcho {
    fn new(reflectors: Vec<IpAddr>) -> Self {
        let socket = nix::sys::socket::socket(
            AddressFamily::Inet,
            SockType::Raw,
            SockFlag::empty(),
            SockProtocol::ICMP,
        )
        .expect("Couldn't open a socket");

        PingerICMPEcho { reflectors, socket }
    }

    fn receive_loop(&self) {
        loop {
            let mut v: Vec<u8> = vec![0; size_of::<ICMP>()];
            let buf = v.as_mut_slice();

            println!("buf len: {}", buf.len());

            let res = recvfrom::<SockaddrIn>(self.socket, buf).unwrap();
            let remote = res.1.unwrap();

            let ip = IpAddr::from(remote.ip().to_ne_bytes());

            if self.reflectors.contains(&ip) {
                println!("ip: {}", ip.to_string());
                println!("usize: {}", res.0);
                println!("{:?}", buf);

                let hdr: ICMP = buf.into();
                println!("Type: {}", hdr.icmp_type);
            }
        }
    }

    fn receive_ping(&self) -> Result<(), Box<dyn Error>> {
        // Process ping RTT
        /*let ping_rtt = SystemTime::now()
        .duration_since(ping_start_time)
        .unwrap_or(Duration::from_secs(0));*/
        todo!()
    }

    fn sender_loop(&self) {
        let mut seq: u16 = 0;
        let tick_duration_ms: u16 = 500;
        let sleep_duration = time::Duration::from_millis(tick_duration_ms as u64);

        loop {
            for reflector in self.reflectors.iter() {
                self.send_ping(reflector, seq).expect("TODO: panic message");
            }

            thread::sleep(sleep_duration);

            seq += 1;

            if seq > std::u16::MAX {
                seq = 0;
            }
        }
    }

    fn send_ping(&self, reflector: &IpAddr, seq: u16) -> Result<(), Box<dyn Error>> {
        let time = clock_gettime(ClockId::CLOCK_MONOTONIC).unwrap();

        let mut hdr = ICMP {
            icmp_type: 8,
            code: 0,
            checksum: 0,
            identifier: 0xBABE,
            sequence: seq,
            payload: time.num_milliseconds() as u64,
        };

        let mut bytes = hdr.to_bytes();
        let buf: &mut [u8] = bytes.as_mut_slice();

        hdr.checksum = PingerICMPEcho::calculate_checksum(buf);
        let bytes = hdr.to_bytes();
        let buf = bytes.as_slice();

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

        sendto(self.socket, buf, addr.as_ref(), MsgFlags::empty()).expect("TODO: F message");

        Ok(())
    }
}
