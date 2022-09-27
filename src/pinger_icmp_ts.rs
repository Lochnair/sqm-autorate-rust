use std::error::Error;
use std::net::IpAddr;

use crate::error::PingParseError;
use crate::pinger::{PingListener, PingReply, PingSender};
use crate::Utils;
use etherparse::icmpv4::TimestampMessage;
use etherparse::TransportSlice::Icmpv4;
use etherparse::{Icmpv4Header, Icmpv4Type, SlicedPacket};
use log::warn;
use nix::time::{clock_gettime, ClockId};

pub struct PingerICMPTimestampListener {}

pub struct PingerICMPTimestampSender {}

impl PingListener for PingerICMPTimestampListener {
    // Result: RTT, down time, up time
    fn parse_packet(
        &self,
        id: u16,
        reflector: IpAddr,
        buf: &[u8],
        len: usize,
    ) -> Result<PingReply, Box<dyn Error>> {
        match SlicedPacket::from_ip(buf) {
            Err(value) => warn!("Error parsing packet: {:?}", value),
            Ok(value) => match value.transport {
                Some(Icmpv4(icmp)) => match icmp.icmp_type() {
                    Icmpv4Type::TimestampReply(reply) => {
                        if reply.id != id {
                            return Err(Box::new(PingParseError {
                                msg: "Wrong ID".to_string(),
                            }));
                        }

                        let time_now = clock_gettime(ClockId::CLOCK_REALTIME).unwrap();
                        let time_since_midnight: i64 = (time_now.tv_sec() as i64 % 86400 * 1000)
                            + (time_now.tv_nsec() as i64 / 1000000);

                        let originate_timestamp = Utils::to_ne(reply.originate_timestamp);
                        let receive_timestamp = Utils::to_ne(reply.receive_timestamp);
                        let transmit_timestamp = Utils::to_ne(reply.transmit_timestamp);

                        let rtt: i64 = time_since_midnight - originate_timestamp as i64;
                        let dl_time: i64 = time_since_midnight - transmit_timestamp as i64;
                        let ul_time: i64 = receive_timestamp as i64 - originate_timestamp as i64;

                        return Ok(PingReply {
                            reflector,
                            seq: reply.seq,
                            rtt,
                            current_time: time_since_midnight,
                            down_time: dl_time as f64,
                            up_time: ul_time as f64,
                            originate_timestamp: originate_timestamp as i64,
                            receive_timestamp: receive_timestamp as i64,
                            transmit_timestamp: transmit_timestamp as i64,
                            last_receive_time_s: time_now.tv_sec() as f64
                                + (time_now.tv_nsec() as f64 / 1e9),
                        });
                    }
                    _ => {}
                },
                Some(_) => {}
                None => {}
            },
        }

        Err(Box::new(PingParseError {
            msg: "Reached end of parsing function".to_string(),
        }))
    }
}

impl PingSender for PingerICMPTimestampSender {
    fn craft_packet(&self, id: u16, seq: u16) -> Vec<u8> {
        let time = clock_gettime(ClockId::CLOCK_REALTIME).unwrap();
        let time_since_midnight: u32 =
            ((time.tv_sec() % 86400 * 1000) + (time.tv_nsec() / 1000000)) as u32;

        let payload: [u8; 0] = [];

        // Construct a header with checksum based on the payload
        let hdr = Icmpv4Header::with_checksum(
            Icmpv4Type::TimestampRequest(TimestampMessage {
                id,
                seq,
                originate_timestamp: time_since_midnight,
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
