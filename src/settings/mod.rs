// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use config::{Config, ConfigError, Environment};
use log::Level;
use serde::{Deserialize, Deserializer, de};
use std::fmt::{self, Display};
use std::str::FromStr;
use uci_derive::{UciConfig, UciSection};

#[cfg(all(feature = "uci", unix))]
use crate::settings::uci::UciSource;

#[cfg(all(feature = "uci", unix))]
pub mod uci;

#[cfg(all(feature = "uci", unix))]
pub(crate) mod uci_schema;
mod validation;

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
            formatter.write_str("a boolean, integer 0 or 1, or a UCI-compatible boolean string")
        }

        fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(value)
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            match value {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(E::custom("boolean integer must be 0 or 1")),
            }
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            match value {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(E::custom("boolean integer must be 0 or 1")),
            }
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

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum ReflectorProtocol {
    IcmpEcho = 1,
    IcmpTimestamps,
    Irtt,
}

impl Display for ReflectorProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReflectorProtocol::IcmpEcho => write!(f, "icmp-echo"),
            ReflectorProtocol::IcmpTimestamps => write!(f, "icmp-timestamps"),
            ReflectorProtocol::Irtt => write!(f, "irtt"),
        }
    }
}

impl FromStr for ReflectorProtocol {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "icmp-echo" => Ok(ReflectorProtocol::IcmpEcho),
            "icmp-timestamps" => Ok(ReflectorProtocol::IcmpTimestamps),
            "irtt" => Ok(ReflectorProtocol::Irtt),
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

#[derive(Debug, Clone, Deserialize, UciSection)]
pub(crate) struct ReflectorDefinition {
    pub(crate) host: String,
    pub(crate) country: Option<String>,
    pub(crate) protocols: Vec<ReflectorProtocol>,
    pub(crate) irtt_port: Option<u16>,
    #[allow(dead_code)]
    pub(crate) irtt_hmac: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReflectorRow {
    host: String,
    country: Option<String>,
    protocols: String,
    irtt_port: Option<u16>,
    irtt_hmac: Option<String>,
}

#[derive(Clone, Debug, Deserialize, UciConfig)]
#[uci(package = "sqm-autorate-rust")]
pub(crate) struct Settings {
    #[uci(section)]
    pub(crate) network: NetworkSettings,

    #[serde(default)]
    #[uci(section)]
    pub(crate) reflectors: ReflectorSettings,

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
        let builder = builder.add_source(
            UciSource::from_schema::<Self>()
                .with_repeated_schema(&[
                    uci_schema::UciRepeatedSectionMapping {
                        uci_section: "reflector",
                        config_path: "reflectors.entries",
                        options: <ReflectorDefinition as uci_schema::UciSectionSchema>::UCI_OPTIONS,
                    },
                    uci_schema::UciRepeatedSectionMapping {
                        uci_section: "bootstrap_reflector",
                        config_path: "reflectors.bootstrap",
                        options: <ReflectorDefinition as uci_schema::UciSectionSchema>::UCI_OPTIONS,
                    },
                ])
                .required(false),
        );

        #[cfg(feature = "toml")]
        let builder = builder.add_source(
            config::File::with_name("/etc/sqm-autorate-rust/config.toml").required(false),
        );

        let builder = builder.add_source(
            Environment::with_prefix("SQMA")
                .separator("__")
                .try_parsing(true)
                .list_separator(",")
                .with_list_parse_key("reflectors.files")
                .with_list_parse_key("reflectors.countries"),
        );
        let mut settings: Self = builder.build()?.try_deserialize()?;

        settings.reflectors.load_files()?;

        Ok(settings)
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

    #[allow(dead_code)]
    pub(crate) measurement_bind_device: Option<String>,
    #[allow(dead_code)]
    pub(crate) measurement_mark: Option<u32>,
    #[allow(dead_code)]
    pub(crate) measurement_fib: Option<u32>,
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
pub(crate) struct ReflectorSettings {
    #[serde(deserialize_with = "deserialize_reflector_files")]
    pub(crate) files: Vec<String>,
    pub(crate) countries: Vec<String>,

    #[uci(skip)]
    pub(crate) entries: Vec<ReflectorDefinition>,

    #[uci(skip)]
    pub(crate) bootstrap: Vec<ReflectorDefinition>,

    pub(crate) num_reflectors: u8,
    pub(crate) peer_reselection_time: u64,
}

impl ReflectorSettings {
    fn load_files(&mut self) -> Result<(), ConfigError> {
        for path in &self.files {
            let mut reader = csv::Reader::from_path(path)
                .map_err(|error| ConfigError::Message(error.to_string()))?;
            for row in reader.deserialize::<ReflectorRow>() {
                let row = row.map_err(|error| ConfigError::Message(error.to_string()))?;
                let protocols = row
                    .protocols
                    .split(['|', ';'])
                    .map(str::trim)
                    .filter(|protocol| !protocol.is_empty())
                    .map(ReflectorProtocol::from_str)
                    .collect::<Result<Vec<_>, _>>()?;
                if protocols.is_empty() {
                    return Err(ConfigError::Message(format!(
                        "reflector row in {path} has no protocols"
                    )));
                }
                self.entries.push(ReflectorDefinition {
                    host: row.host,
                    country: row.country,
                    protocols,
                    irtt_port: row.irtt_port,
                    irtt_hmac: row.irtt_hmac,
                });
            }
        }

        Ok(())
    }
}

impl Default for ReflectorSettings {
    fn default() -> Self {
        let icmp = |host: &str| ReflectorDefinition {
            host: host.to_owned(),
            country: None,
            protocols: vec![
                ReflectorProtocol::IcmpEcho,
                ReflectorProtocol::IcmpTimestamps,
            ],
            irtt_port: None,
            irtt_hmac: None,
        };

        let irtt = |host: &str, country: Option<&str>| ReflectorDefinition {
            host: host.to_owned(),
            country: country.map(str::to_owned),
            protocols: vec![ReflectorProtocol::Irtt],
            irtt_port: Some(2112),
            irtt_hmac: None,
        };

        Self {
            files: vec!["/etc/sqm-autorate-rust/reflectors.csv".into()],
            countries: Vec::new(),
            entries: Vec::new(),
            bootstrap: vec![
                icmp("9.9.9.9"),
                icmp("8.238.120.14"),
                icmp("74.82.42.42"),
                icmp("194.242.2.2"),
                icmp("208.67.222.222"),
                icmp("94.140.14.14"),
                irtt("global.irtt.lochnair.net", None),
                irtt("uk-cov1.irtt.lochnair.net", Some("GB")),
                irtt("uk-cov2.irtt.lochnair.net", Some("GB")),
                irtt("de-fra.irtt.lochnair.net", Some("DE")),
                irtt("nl-ams1.irtt.lochnair.net", Some("NL")),
            ],
            num_reflectors: 5,
            peer_reselection_time: 15,
        }
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
    pub(crate) measurement_type: ReflectorProtocol,
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
            measurement_type: ReflectorProtocol::IcmpTimestamps,
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

// UCI drops empty string options; `option files 'none'` disables file loading.
fn deserialize_reflector_files<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Files {
        List(Vec<String>),
        String(String),
    }

    match Files::deserialize(deserializer)? {
        Files::List(files) => Ok(files),
        Files::String(value) if value.trim().is_empty() || value.trim() == "none" => Ok(Vec::new()),
        Files::String(_) => Err(de::Error::custom(
            "reflector files must be a list, an empty string, or none",
        )),
    }
}
