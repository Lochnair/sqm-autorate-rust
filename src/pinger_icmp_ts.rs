use crate::pinger::{PingError, PingListener, PingReply, PingSender};
use crate::time::Time;
use icmp_socket::packet::WithTimestampRequest;
use icmp_socket::Icmpv4Message;
use icmp_socket::Icmpv4Packet;
use rustix::thread::ClockId;
use std::net::IpAddr;
use std::time::Instant;

pub struct PingerICMPTimestampListener {}

pub struct PingerICMPTimestampSender {}

impl PingListener for PingerICMPTimestampListener {
    // Result: RTT, down time, up time
    fn parse_packet(&self, id: u16, reflector: IpAddr, pkt: Icmpv4Packet) -> Result<PingReply, PingError> {
        match pkt.typ {
            // 14 = Timestamp reply
            14 => {
                if let Icmpv4Message::TimestampReply {
                    identifier,
                    sequence,
                    originate,
                    receive,
                    transmit,
                } = pkt.message
                {
                    if identifier != id {
                        return Err(PingError::WrongID {
                            expected: id,
                            found: identifier,
                        });
                    }

                    let time_now = Time::new(ClockId::Realtime);
                    let time_since_midnight = time_now.get_time_since_midnight();

                    let rtt: i64 = time_since_midnight - originate as i64;
                    let dl_time: i64 = time_since_midnight - transmit as i64;
                    let ul_time: i64 = receive as i64 - originate as i64;

                    Ok(PingReply {
                        reflector,
                        seq: sequence,
                        rtt,
                        current_time: time_since_midnight,
                        down_time: dl_time as f64,
                        up_time: ul_time as f64,
                        originate_timestamp: originate as i64,
                        receive_timestamp: receive as i64,
                        transmit_timestamp: transmit as i64,
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

impl PingSender for PingerICMPTimestampSender {
    fn craft_packet(&self, id: u16, seq: u16) -> Icmpv4Packet {
        let time_since_midnight = Time::new(ClockId::Realtime).get_time_since_midnight();
        Icmpv4Packet::with_timestamp_request(id, seq, time_since_midnight as u32, 0, 0).unwrap()
    }
}
