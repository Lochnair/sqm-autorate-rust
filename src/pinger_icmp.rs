use std::net::IpAddr;
use std::time::Instant;

use crate::pinger::{PingError, PingListener, PingReply, PingSender};
use crate::time::Time;
use icmp_socket::Icmpv4Message;
use icmp_socket::Icmpv4Packet;
use icmp_socket::packet::WithEchoRequest;
use rustix::thread::ClockId;

pub struct PingerICMPEchoListener {}

pub struct PingerICMPEchoSender {}

impl PingListener for PingerICMPEchoListener {
    // Result: RTT, down time, up time
    fn parse_packet(&self, id: u16, reflector: IpAddr, pkt: Icmpv4Packet) -> Result<PingReply, PingError> {
        match pkt.typ {
            0 => {
                if let Icmpv4Message::EchoReply {
                    identifier,
                    sequence,
                    payload,
                } = pkt.message
                {
                    if identifier != id {
                        return Err(PingError::WrongID {
                            expected: id,
                            found: identifier,
                        });
                    }

                    let time_sent = match payload.as_slice().try_into() {
                        Ok(bytes) => u64::from_be_bytes(bytes) as i64,
                        Err(_) => {
                            return Err(PingError::InvalidPacket(format!("Expected 8 bytes payload, but found {}", payload.len())))
                        }
                    };

                    let clock = Time::new(ClockId::Monotonic);
                    let time_ms = clock.to_milliseconds() as i64;

                    let rtt: i64 = time_ms - time_sent;
                    Ok(PingReply {
                        reflector,
                        seq: sequence,
                        rtt,
                        current_time: time_ms,
                        down_time: (rtt / 2) as f64,
                        up_time: (rtt / 2) as f64,
                        originate_timestamp: time_sent,
                        receive_timestamp: 0,
                        transmit_timestamp: 0,
                        last_receive_time_s: Instant::now(),
                    })
                } else {
                    Err(PingError::InvalidPacket(format!("Packet had type {:?}, but did not match the structure", pkt.typ)))
                }

                
            },
            type_ => Err(PingError::InvalidType(format!("{:?}", type_))),
        }
    }
}

impl PingSender for PingerICMPEchoSender {
    fn craft_packet(&self, id: u16, seq: u16) -> Icmpv4Packet {
        let clock = Time::new(ClockId::Monotonic);
        let time_ms = clock.to_milliseconds();
        let payload = time_ms.to_be_bytes().to_vec();

        Icmpv4Packet::with_echo_request(id, seq, payload).unwrap()
    }
}
