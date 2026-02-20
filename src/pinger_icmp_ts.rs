use crate::endian::ToNativeEndian;
use crate::pinger::{PingError, PingListener, PingReply, PingSender};
use crate::time::Time;
use etherparse::icmpv4::TimestampMessage;
use etherparse::TransportSlice::{Icmpv4, Icmpv6};
use etherparse::{Icmpv4Header, Icmpv4Type, SlicedPacket};
use rustix::thread::ClockId;
use std::net::IpAddr;
use std::time::Instant;

pub struct PingerICMPTimestampListener {}

pub struct PingerICMPTimestampSender {}

impl PingListener for PingerICMPTimestampListener {
    // Result: RTT, down time, up time
    fn parse_packet(&self, id: u16, reflector: IpAddr, buf: &[u8]) -> Result<PingReply, PingError> {
        match SlicedPacket::from_ip(buf) {
            Err(err) => Err(PingError::InvalidPacket(err)),
            Ok(value) => match value.transport {
                Some(Icmpv4(icmp)) => match icmp.icmp_type() {
                    Icmpv4Type::TimestampReply(reply) => {
                        if reply.id != id {
                            return Err(PingError::WrongID {
                                expected: id,
                                found: reply.id,
                            });
                        }

                        let time_now = Time::new(ClockId::Realtime);
                        let time_since_midnight = time_now.get_time_since_midnight();

                        let originate_timestamp = reply.originate_timestamp;
                        let receive_timestamp = reply.receive_timestamp;
                        let transmit_timestamp = reply.transmit_timestamp;

                        let rtt: i64 = time_since_midnight - originate_timestamp as i64;
                        let dl_time: i64 = time_since_midnight - transmit_timestamp as i64;
                        let ul_time: i64 = receive_timestamp as i64 - originate_timestamp as i64;

                        Ok(PingReply {
                            reflector,
                            seq: reply.seq,
                            rtt,
                            current_time: time_since_midnight,
                            down_time: dl_time as f64,
                            up_time: ul_time as f64,
                            originate_timestamp: originate_timestamp as i64,
                            receive_timestamp: receive_timestamp as i64,
                            transmit_timestamp: transmit_timestamp as i64,
                            last_receive_time_s: Instant::now(),
                        })
                    }
                    type_ => Err(PingError::InvalidType(format!("{:?}", type_))),
                },
                Some(Icmpv6(slice)) => Err(PingError::InvalidType(format!("{:?}", slice))),
                Some(type_) => Err(PingError::InvalidType(format!("{:?}", type_))),
                None => Err(PingError::NoTransport),
            },
        }
    }
}

impl PingSender for PingerICMPTimestampSender {
    fn craft_packet(&self, id: u16, seq: u16) -> Vec<u8> {
        let time_since_midnight = Time::new(ClockId::Realtime).get_time_since_midnight();

        let payload: [u8; 0] = [];

        // Construct a header with checksum based on the payload
        let hdr = Icmpv4Header::with_checksum(
            Icmpv4Type::TimestampRequest(TimestampMessage {
                id,
                seq,
                originate_timestamp: time_since_midnight as u32,
                receive_timestamp: 0,
                transmit_timestamp: 0,
            }),
            &payload,
        );

        // Create a buffer to hold the result of header + payload
        let mut result = Vec::<u8>::with_capacity(hdr.header_len());

        // Write the header to the buffer
        hdr.write(&mut result).expect("Error writing packet");

        result
    }
}
