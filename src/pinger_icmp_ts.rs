use crate::config::MeasurementType;
use crate::pinger::{PingError, PingListener, PingReply, PingSender};
use crate::time::Time;
use icmp_socket2::Icmpv4Message;
use icmp_socket2::Icmpv4Packet;
use icmp_socket2::packet::WithTimestampRequest;
use rustix::thread::ClockId;
use std::net::IpAddr;
use std::time::Instant;

pub struct PingerICMPTimestampListener {}

pub struct PingerICMPTimestampSender {}

impl PingListener for PingerICMPTimestampListener {
    // Result: RTT, down time, up time
    fn parse_packet(
        &self,
        id: u16,
        reflector: IpAddr,
        measurement_type: MeasurementType,
        pkt: Icmpv4Packet,
    ) -> Result<PingReply, PingError> {
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

                    let rtt = (time_since_midnight - originate as i64) as f64;
                    let dl_time = (time_since_midnight - transmit as i64) as f64;
                    let ul_time = (receive as i64 - originate as i64) as f64;

                    Ok(PingReply {
                        reflector,
                        measurement_type,
                        seq: sequence,
                        rtt,
                        current_time: time_since_midnight,
                        down_time: dl_time,
                        up_time: ul_time,
                        originate_timestamp: originate as i64,
                        receive_timestamp: receive as i64,
                        transmit_timestamp: transmit as i64,
                        last_receive_time_s: Instant::now(),
                    })
                } else {
                    Err(PingError::InvalidPacket(format!(
                        "Packet had type {:?}, but did not match the structure",
                        pkt.typ
                    )))
                }
            }
            type_ => Err(PingError::InvalidType(format!("{:?}", type_))),
        }
    }
}

impl PingSender for PingerICMPTimestampSender {
    fn craft_packet(&self, id: u16, seq: u16) -> (Icmpv4Packet, i64) {
        let time_since_midnight = Time::new(ClockId::Realtime).get_time_since_midnight();
        (
            Icmpv4Packet::with_timestamp_request(id, seq, time_since_midnight as u32, 0, 0)
                .unwrap(),
            time_since_midnight as i64,
        )
    }
}
