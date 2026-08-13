// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use serde::de::{Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::IpAddr;

#[cfg(feature = "trace")]
use crate::settings::Settings;
#[cfg(any(feature = "trace", test))]
use std::time::Duration;
#[cfg(any(feature = "trace", test))]
use std::time::Instant;
#[cfg(feature = "trace")]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "trace")]
pub(crate) const FORMAT_VERSION: u16 = 1;
#[cfg(feature = "trace")]
pub(crate) const FORMAT_NAME: &str = "legacy-control-trace-v1";

#[cfg(feature = "trace")]
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct Timestamp {
    pub(crate) mono_ns: u64,
    pub(crate) unix_us: i64,
}

#[cfg(feature = "trace")]
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct Record {
    pub(crate) seq: u64,
    pub(crate) timestamp: Timestamp,
    #[serde(flatten)]
    pub(crate) event: Event,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Event {
    #[cfg(feature = "trace")]
    Header {
        format: String,
        version: u16,
        program_version: String,
        control: ControlConfig,
    },
    PingReply {
        reflector: IpAddr,
        down_time_ms: f64,
        up_time_ms: f64,
    },
    ControllerInitialized {
        #[serde(deserialize_with = "deserialize_i128")]
        previous_rx_bytes: i128,
        #[serde(deserialize_with = "deserialize_i128")]
        previous_tx_bytes: i128,
        download_prev_mono_ns: u64,
        upload_prev_mono_ns: u64,
        download_history_index: u64,
        upload_history_index: u64,
        download_safe_rates_kbit: Vec<f64>,
        upload_safe_rates_kbit: Vec<f64>,
    },
    ControlEvaluation {
        evaluation_id: u64,
        loop_mono_ns: u64,
        #[serde(deserialize_with = "deserialize_i128")]
        rx_bytes: i128,
        #[serde(deserialize_with = "deserialize_i128")]
        tx_bytes: i128,
        active_reflectors: Vec<IpAddr>,
    },
    RateCalculation {
        evaluation_id: u64,
        direction: Direction,
    },
    RandomSafeRateChoice {
        evaluation_id: u64,
        direction: Direction,
        index: u64,
        rate_kbit: f64,
    },
    RequestedRates {
        evaluation_id: Option<u64>,
        reason: RateRequestReason,
        download_kbit: u64,
        upload_kbit: u64,
        download_requested: bool,
        upload_requested: bool,
    },
    End {
        clean: bool,
    },
}

fn deserialize_i128<'de, D>(deserializer: D) -> Result<i128, D::Error>
where
    D: Deserializer<'de>,
{
    struct I128Visitor;

    impl Visitor<'_> for I128Visitor {
        type Value = i128;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a signed or unsigned integer")
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
            Ok(i128::from(value))
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
            Ok(i128::from(value))
        }
    }

    deserializer.deserialize_any(I128Visitor)
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Direction {
    Download,
    Upload,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RateRequestReason {
    Startup,
    Control,
    NoReflectorData,
}

#[cfg(feature = "trace")]
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct ControlConfig {
    pub(crate) tick_interval_secs: f64,
    pub(crate) min_change_interval_secs: f64,
    pub(crate) download_base_kbit: f64,
    pub(crate) upload_base_kbit: f64,
    pub(crate) download_min_kbit: f64,
    pub(crate) upload_min_kbit: f64,
    pub(crate) download_delay_ms: f64,
    pub(crate) upload_delay_ms: f64,
    pub(crate) high_load_level: f64,
    pub(crate) speed_history_size: u32,
}

#[cfg(feature = "trace")]
impl ControlConfig {
    pub(crate) fn from_settings(settings: &Settings) -> Self {
        Self {
            tick_interval_secs: settings.advanced_settings.tick_interval,
            min_change_interval_secs: settings.advanced_settings.min_change_interval,
            download_base_kbit: settings.network.download_base_kbits,
            upload_base_kbit: settings.network.upload_base_kbits,
            download_min_kbit: settings.network.download_min_kbits(),
            upload_min_kbit: settings.network.upload_min_kbits(),
            download_delay_ms: settings.advanced_settings.download_delay_ms,
            upload_delay_ms: settings.advanced_settings.upload_delay_ms,
            high_load_level: settings.advanced_settings.high_load_level,
            speed_history_size: settings.advanced_settings.speed_hist_size,
        }
    }
}

#[cfg(feature = "trace")]
pub(crate) fn timestamp(origin: Instant, monotonic: Instant, realtime: SystemTime) -> Timestamp {
    Timestamp {
        mono_ns: monotonic_ns(origin, monotonic),
        unix_us: unix_us(realtime),
    }
}

#[cfg(any(feature = "trace", test))]
pub(crate) fn monotonic_ns(origin: Instant, monotonic: Instant) -> u64 {
    duration_ns(monotonic.saturating_duration_since(origin))
}

#[cfg(any(feature = "trace", test))]
fn duration_ns(duration: Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}

#[cfg(feature = "trace")]
fn unix_us(realtime: SystemTime) -> i64 {
    match realtime.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration
            .as_micros()
            .min(i64::MAX as u128)
            .try_into()
            .unwrap_or(i64::MAX),
        Err(error) => {
            let micros = error.duration().as_micros().min(i64::MAX as u128) as i64;
            -micros
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    #[cfg(feature = "trace")]
    fn control_config() -> ControlConfig {
        ControlConfig {
            tick_interval_secs: 0.5,
            min_change_interval_secs: 0.5,
            download_base_kbit: 100_000.0,
            upload_base_kbit: 20_000.0,
            download_min_kbit: 20_000.0,
            upload_min_kbit: 4_000.0,
            download_delay_ms: 15.0,
            upload_delay_ms: 15.0,
            high_load_level: 0.8,
            speed_history_size: 100,
        }
    }

    #[cfg(feature = "trace")]
    pub(crate) fn header_event() -> Event {
        Event::Header {
            format: FORMAT_NAME.to_owned(),
            version: FORMAT_VERSION,
            program_version: "test".to_owned(),
            control: control_config(),
        }
    }

    #[cfg(feature = "trace")]
    #[test]
    fn serializes_versioned_header() {
        let origin = Instant::now();
        let record = Record {
            seq: 0,
            timestamp: timestamp(origin, origin, UNIX_EPOCH + Duration::from_secs(1)),
            event: header_event(),
        };

        let json = serde_json::to_value(&record).unwrap();

        assert_eq!(json["seq"], 0);
        assert_eq!(json["type"], "header");
        assert_eq!(json["format"], FORMAT_NAME);
        assert_eq!(json["version"], FORMAT_VERSION);
        assert_eq!(json["timestamp"]["mono_ns"], 0);
        assert_eq!(
            crate::trace::reader::parse_record_v1(&serde_json::to_string(&record).unwrap())
                .unwrap(),
            record
        );
    }

    #[test]
    fn converts_existing_instants_relative_to_origin() {
        let origin = Instant::now();

        assert_eq!(
            monotonic_ns(origin, origin + Duration::from_nanos(123_456)),
            123_456
        );
        assert_eq!(monotonic_ns(origin, origin - Duration::from_nanos(1)), 0);
    }
}
