// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use anyhow::{Context, bail};
use config::{Config, ConfigError, Environment};
use log::Level;
use serde::{Deserialize, Deserializer, de};
use std::fmt::{self, Display};
use std::net::IpAddr;
use std::str::FromStr;
use uci_derive::{UciConfig, UciSection};

pub(crate) mod uci_schema;
mod validation;

#[cfg(all(feature = "uci", unix))]
use crate::platform::unix::uci::UciSource;

struct FlexiBool(bool);

impl FromStr for FlexiBool {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "1" | "true" | "yes" | "y" | "on" | "enabled" => Ok(FlexiBool(true)),
            "0" | "false" | "no" | "n" | "off" | "disabled" => Ok(FlexiBool(false)),
            _ => Err(ConfigError::Message(format!(
                "invalid boolean string: {}",
                s
            ))),
        }
    }
}

fn deserialize_flex_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    struct FlexiBoolVisitor;

    impl<'de> de::Visitor<'de> for FlexiBoolVisitor {
        type Value = bool;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a boolean or a UCI-compatible boolean string")
        }

        fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(value)
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            value
                .parse::<FlexiBool>()
                .map(|value| value.0)
                .map_err(E::custom)
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.visit_str(&value)
        }
    }

    deserializer.deserialize_any(FlexiBoolVisitor)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MeasurementType {
    Icmp = 1,
    IcmpTimestamps,
}

impl Display for MeasurementType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MeasurementType::Icmp => write!(f, "icmp"),
            MeasurementType::IcmpTimestamps => write!(f, "icmp-timestamps"),
        }
    }
}

impl FromStr for MeasurementType {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "icmp" => Ok(MeasurementType::Icmp),
            "icmp-timestamps" | "icmp_timestamps" => Ok(MeasurementType::IcmpTimestamps),
            _ => Err(ConfigError::Message(format!(
                "invalid measurement type: {s}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ObservabilityProtocol {
    Udp,
    Tcp,
}

impl FromStr for ObservabilityProtocol {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "udp" => Ok(ObservabilityProtocol::Udp),
            "tcp" => Ok(ObservabilityProtocol::Tcp),
            _ => Err(ConfigError::Message(format!(
                "invalid observability protocol: {s}",
            ))),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReflectorRecord {
    reflector_ip: IpAddr,
    ip_version: u8,
    #[allow(dead_code)]
    description: String,
}

#[derive(Clone, Debug, Deserialize, UciConfig)]
#[uci(package = "sqm-autorate-rust")]
pub(crate) struct Settings {
    #[uci(section)]
    pub(crate) network: NetworkSettings,

    #[serde(default)]
    #[uci(section)]
    pub(crate) output: OutputSettings,

    #[serde(default)]
    #[uci(section)]
    pub(crate) observability: ObservabilitySettings,

    #[serde(default)]
    #[uci(section)]
    pub(crate) advanced_settings: AdvancedSettings,
}

impl Settings {
    pub(crate) fn load() -> Result<Self, ConfigError> {
        let builder = Config::builder();

        #[cfg(all(feature = "uci", unix))]
        let builder = builder.add_source(UciSource::from_schema::<Self>().required(false));

        #[cfg(feature = "toml")]
        let builder = builder.add_source(
            config::File::with_name("/etc/sqm-autorate-rust/config.toml").required(false),
        );

        let builder = builder.add_source(Environment::with_prefix("SQMA").separator("__"));
        let built_config = builder.build()?;

        built_config.try_deserialize()
    }

    pub fn load_reflectors(&self) -> anyhow::Result<Vec<IpAddr>> {
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .trim(csv::Trim::All)
            .from_path(&self.advanced_settings.reflector_list_file)
            .with_context(|| {
                format!(
                    "failed to open reflector list: {}",
                    self.advanced_settings.reflector_list_file
                )
            })?;

        let mut reflectors = Vec::with_capacity(50);

        for record in reader.deserialize::<ReflectorRecord>() {
            let record = record.with_context(|| {
                format!(
                    "failed to parse reflector list: {}",
                    self.advanced_settings.reflector_list_file
                )
            })?;

            match (record.ip_version, record.reflector_ip) {
                (4, IpAddr::V4(_)) | (6, IpAddr::V6(_)) => {}
                (version, address) => {
                    bail!("reflector {address} has inconsistent IP version {version}");
                }
            }

            reflectors.push(record.reflector_ip);
        }

        Ok(reflectors)
    }
}

#[derive(Clone, Debug, Deserialize, UciSection)]
pub(crate) struct NetworkSettings {
    pub(crate) download_interface: String,
    pub(crate) upload_interface: String,
    pub(crate) download_base_kbits: f64,
    #[serde(default = "default_min_percent")]
    pub(crate) download_min_percent: f64,
    pub(crate) upload_base_kbits: f64,
    #[serde(default = "default_min_percent")]
    pub(crate) upload_min_percent: f64,
}

impl NetworkSettings {
    pub(crate) fn download_min_kbits(&self) -> f64 {
        (self.download_base_kbits * self.download_min_percent / 100.0).floor()
    }

    pub(crate) fn upload_min_kbits(&self) -> f64 {
        (self.upload_base_kbits * self.upload_min_percent / 100.0).floor()
    }
}

#[derive(Clone, Debug, Deserialize, UciSection)]
#[serde(default)]
pub(crate) struct OutputSettings {
    #[serde(deserialize_with = "deserialize_from_str")]
    pub(crate) log_level: Level,
    pub(crate) speed_hist_file: String,
    pub(crate) stats_file: String,
    #[serde(deserialize_with = "deserialize_flex_bool")]
    pub(crate) suppress_statistics: bool,
}

impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            log_level: Level::Error,
            speed_hist_file: "/tmp/sqm-speedhist.csv".to_owned(),
            stats_file: "/tmp/sqm-autorate-rust.csv".to_owned(),
            suppress_statistics: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, UciSection)]
#[serde(default)]
pub(crate) struct ObservabilitySettings {
    #[serde(deserialize_with = "deserialize_flex_bool")]
    pub(crate) enabled: bool,
    #[serde(deserialize_with = "deserialize_from_str")]
    pub(crate) protocol: ObservabilityProtocol,
    pub(crate) host: Option<String>,
    pub(crate) port: u16,
    pub(crate) batch_size: usize,
    pub(crate) batch_timeout_ms: u64,
    #[serde(deserialize_with = "deserialize_flex_bool")]
    pub(crate) export_ping_metrics: bool,
    #[serde(deserialize_with = "deserialize_flex_bool")]
    pub(crate) export_rate_metrics: bool,
    #[serde(deserialize_with = "deserialize_flex_bool")]
    pub(crate) export_baseline_metrics: bool,
    #[serde(deserialize_with = "deserialize_flex_bool")]
    pub(crate) export_events: bool,
    pub(crate) host_tag: String,
}

impl Default for ObservabilitySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            protocol: ObservabilityProtocol::Udp,
            host: None,
            port: 8089,
            batch_size: 25,
            batch_timeout_ms: 100,
            export_ping_metrics: false,
            export_rate_metrics: true,
            export_baseline_metrics: false,
            export_events: true,
            host_tag: default_host_tag(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, UciSection)]
#[serde(default)]
pub(crate) struct AdvancedSettings {
    pub(crate) download_delay_ms: f64,
    pub(crate) high_load_level: f64,
    pub(crate) min_change_interval: f64,
    #[serde(deserialize_with = "deserialize_from_str")]
    pub(crate) measurement_type: MeasurementType,
    pub(crate) num_reflectors: u8,
    pub(crate) peer_reselection_time: u64,
    pub(crate) reflector_list_file: String,
    pub(crate) speed_hist_size: u32,
    pub(crate) tick_interval: f64,
    pub(crate) upload_delay_ms: f64,
    #[serde(deserialize_with = "deserialize_flex_bool")]
    pub(crate) dry_run: bool,
}

impl Default for AdvancedSettings {
    fn default() -> Self {
        Self {
            download_delay_ms: 15.0,
            high_load_level: 0.8,
            min_change_interval: 0.5,
            measurement_type: MeasurementType::IcmpTimestamps,
            num_reflectors: 5,
            peer_reselection_time: 15,
            reflector_list_file: "/etc/sqm-autorate/reflectors-icmp.csv".to_owned(),
            speed_hist_size: 100,
            tick_interval: 0.5,
            upload_delay_ms: 15.0,
            dry_run: false,
        }
    }
}

fn default_min_percent() -> f64 {
    20.0
}

fn default_host_tag() -> String {
    rustix::system::uname()
        .nodename()
        .to_string_lossy()
        .into_owned()
}

fn deserialize_from_str<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
    T::Err: Display,
{
    let value = String::deserialize(deserializer)?;
    value.parse().map_err(de::Error::custom)
}
