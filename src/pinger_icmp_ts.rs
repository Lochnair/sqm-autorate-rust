// SPDX-FileCopyrightText: 2022-Present Charles Corrigan mailto:chas-iot@runegate.org (github @chas-iot)
// SPDX-FileCopyrightText: 2022-Present Daniel Lakeland mailto:dlakelan@street-artists.org (github @dlakelan)
// SPDX-FileCopyrightText: 2022-Present Mark Baker mailto:mark@vpost.net (github @Fail-Safe)
// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use crate::pinger::{PingError, PingListener, PingReply, PingSender};
use crate::settings::MeasurementType;
use crate::time::Time;
use icmp_socket2::Icmpv4Message;
use icmp_socket2::Icmpv4Packet;
use icmp_socket2::packet::WithTimestampRequest;
use rustix::time::ClockId;
use std::net::IpAddr;
use std::time::Instant;

pub struct PingerICMPTimestampListener {}

pub struct PingerICMPTimestampSender {}

const MS_PER_DAY: i64 = 86_400_000;

fn timestamp_delta(later: i64, earlier: i64) -> i64 {
    let delta = later - earlier;
    if delta < -MS_PER_DAY / 2 {
        delta + MS_PER_DAY
    } else if delta > MS_PER_DAY / 2 {
        delta - MS_PER_DAY
    } else {
        delta
    }
}

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

                    if receive & 0x8000_0000 != 0 || transmit & 0x8000_0000 != 0 {
                        return Err(PingError::InvalidPacket(
                            "non-standard RFC 792 timestamp".into(),
                        ));
                    }

                    let time_since_midnight =
                        Time::new(ClockId::Realtime).get_time_since_midnight();

                    let rtt = timestamp_delta(time_since_midnight, originate as i64) as f64;
                    let dl_time = timestamp_delta(time_since_midnight, transmit as i64) as f64;
                    let ul_time = timestamp_delta(receive as i64, originate as i64) as f64;

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

#[cfg(test)]
mod tests {
    use super::*;
    use icmp_socket2::packet::WithTimestampReply;

    #[test]
    fn timestamps_wrap_at_midnight_in_both_directions() {
        assert_eq!(timestamp_delta(20, 86_399_900), 120);
        assert_eq!(timestamp_delta(86_399_900, 20), -120);
    }

    #[test]
    fn non_standard_rfc_792_timestamps_are_rejected() {
        let packet = Icmpv4Packet::with_timestamp_reply(7, 1, 10, 0x8000_0001, 20).unwrap();
        let result = PingerICMPTimestampListener {}.parse_packet(
            7,
            "192.0.2.1".parse().unwrap(),
            MeasurementType::IcmpTimestamps,
            packet,
        );
        assert!(matches!(result, Err(PingError::InvalidPacket(_))));
    }
}
