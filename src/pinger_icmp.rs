use std::error::Error;
use std::net::IpAddr;

use crate::pinger::{PingListener, PingReply, PingSender};
use byteorder::*;
use etherparse::TransportSlice::{Icmpv4, Icmpv6};
use etherparse::{IcmpEchoHeader, Icmpv4Header, Icmpv4Type, SlicedPacket};
use nix::sys::time::TimeValLike;
use nix::time::{clock_gettime, ClockId};

pub struct PingerICMPEchoListener {
    id: u16,
}

pub struct PingerICMPEchoSender {
    id: u16,
}

impl PingListener for PingerICMPEchoListener {
    fn new(id: u16) -> Self {
        PingerICMPEchoListener { id }
    }

    fn get_id(&self) -> u16 {
        self.id
    }

    // Result: RTT, down time, up time
    fn parse_packet(
        reflector: IpAddr,
        buf: &[u8],
        len: usize,
    ) -> Result<PingReply, Box<dyn Error>> {
        match SlicedPacket::from_ip(buf) {
            Err(value) => println!("Err {:?}", value),
            Ok(value) => match value.transport {
                Some(Icmpv4(icmp)) => match icmp.icmp_type() {
                    Icmpv4Type::EchoReply(echo) => {
                        let time_sent = icmp
                            .payload()
                            .read_u64::<NativeEndian>()
                            .expect("Couldn't parse payload to time")
                            as i64;

                        let time_now = clock_gettime(ClockId::CLOCK_MONOTONIC).unwrap();

                        let time_ms = time_now.num_milliseconds();

                        let rtt = time_ms - time_sent;
                        return Ok(PingReply {
                            reflector,
                            seq: echo.seq,
                            rtt,
                            current_time: time_ms,
                            down_time: (rtt / 2) as f64,
                            up_time: (rtt / 2) as f64,
                            originate_timestamp: 0,
                            receive_timestamp: 0,
                            transmit_timestamp: 0,
                            last_receive_time_s: time_now.tv_sec() as f64
                                + (time_now.tv_nsec() as f64 / 1e9),
                        });
                    }
                    _ => {}
                },
                Some(Icmpv6(icmp)) => {}
                Some(_) => {}
                None => {}
            },
        }

        Ok(PingReply {
            reflector: IpAddr::from([0, 0, 0, 0]),
            seq: 0,
            rtt: 0,
            current_time: 0,
            down_time: 0.0,
            up_time: 0.0,
            originate_timestamp: 0,
            receive_timestamp: 0,
            transmit_timestamp: 0,
            last_receive_time_s: 0.0,
        })
    }
}

impl PingSender for PingerICMPEchoSender {
    fn new(id: u16) -> Self {
        PingerICMPEchoSender { id }
    }

    fn get_id(&self) -> u16 {
        self.id
    }

    fn craft_packet(&self, seq: u16) -> Vec<u8> {
        let time = clock_gettime(ClockId::CLOCK_MONOTONIC).unwrap();
        let time_u64: u64 = time.num_milliseconds() as u64;
        let payload = time_u64.to_ne_bytes();

        // Construct a header with checksum based on the payload
        let hdr = Icmpv4Header::with_checksum(
            Icmpv4Type::EchoRequest(IcmpEchoHeader {
                id: self.get_id(),
                seq,
            }),
            &payload,
        );

        // Create a buffer to hold the result of header + payload
        let mut result = Vec::<u8>::with_capacity(hdr.header_len() + payload.len());

        // Write the header to the buffer
        hdr.write(&mut result).expect("Error writing packet");

        // Write the payload to the buffer
        result.append(&mut payload.to_vec());

        result
    }
}
