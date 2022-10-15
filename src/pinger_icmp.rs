use std::net::IpAddr;

use crate::clock::Clock;
use crate::pinger::{PingError, PingListener, PingReply, PingSender};
use byteorder::*;
use etherparse::TransportSlice::{Icmpv4, Icmpv6};
use etherparse::{IcmpEchoHeader, Icmpv4Header, Icmpv4Type, SlicedPacket};
use rustix::thread::ClockId;

pub struct PingerICMPEchoListener {}

pub struct PingerICMPEchoSender {}

impl PingListener for PingerICMPEchoListener {
    // Result: RTT, down time, up time
    fn parse_packet(&self, id: u16, reflector: IpAddr, buf: &[u8]) -> Result<PingReply, PingError> {
        match SlicedPacket::from_ip(buf) {
            Err(err) => Err(PingError::InvalidPacket(err)),
            Ok(value) => match value.transport {
                Some(Icmpv4(icmp)) => match icmp.icmp_type() {
                    Icmpv4Type::EchoReply(echo) => {
                        if echo.id != id {
                            return Err(PingError::WrongID {
                                expected: id,
                                found: echo.id,
                            });
                        }

                        let time_sent = icmp
                            .payload()
                            .read_u64::<NativeEndian>()
                            .expect("Couldn't parse payload to time")
                            as i64;

                        let clock = Clock::new(ClockId::Monotonic);
                        let time_ms = clock.to_milliseconds() as i64;

                        let rtt: i64 = time_ms - time_sent;
                        Ok(PingReply {
                            reflector,
                            seq: echo.seq,
                            rtt,
                            current_time: time_ms,
                            down_time: (rtt / 2) as f64,
                            up_time: (rtt / 2) as f64,
                            originate_timestamp: 0,
                            receive_timestamp: 0,
                            transmit_timestamp: 0,
                            last_receive_time_s: clock.get_seconds() as f64
                                + (clock.get_nanoseconds() as f64 / 1e9),
                        })
                    }
                    type_ => Err(PingError::InvalidType(format!("{:?}", type_))),
                },
                Some(Icmpv6(slice)) => Err(PingError::InvalidProtocol(format!("{:?}", slice))),
                Some(type_) => Err(PingError::InvalidProtocol(format!("{:?}", type_))),
                None => Err(PingError::NoTransport),
            },
        }
    }
}

impl PingSender for PingerICMPEchoSender {
    fn craft_packet(&self, id: u16, seq: u16) -> Vec<u8> {
        let clock = Clock::new(ClockId::Monotonic);
        let time_ms = clock.to_milliseconds();
        let payload = time_ms.to_ne_bytes();

        // Construct a header with checksum based on the payload
        let hdr = Icmpv4Header::with_checksum(
            Icmpv4Type::EchoRequest(IcmpEchoHeader { id, seq }),
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
