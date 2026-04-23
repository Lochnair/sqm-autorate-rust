// SPDX-FileCopyrightText: 2022-Present Charles Corrigan mailto:chas-iot@runegate.org (github @chas-iot)
// SPDX-FileCopyrightText: 2022-Present Daniel Lakeland mailto:dlakelan@street-artists.org (github @dlakelan)
// SPDX-FileCopyrightText: 2022-Present Mark Baker mailto:mark@vpost.net (github @Fail-Safe)
// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use anyhow::Result;
use log::Level;
#[cfg(feature = "uci")]
use log::warn;
#[cfg(feature = "uci")]
use rust_uci::Uci;
use std::fmt;
use std::fmt::Display;
use std::fs::File;
use std::io::BufRead;
use std::net::IpAddr;
use std::path::Path;
use std::str::FromStr;
use std::{env, io};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Invalid measurement type")]
    InvalidMeasurementType(String),
    #[error("Couldn't parse value for key: `{0}`: invalid value")]
    ParseError(String),
    #[error("No config value found for key: `{0}`")]
    MissingValue(String),
    #[error("Reflector list not found at: {0}")]
    ReflectorListNotFound(String),
}

fn read_lines<P>(filename: P) -> io::Result<io::Lines<io::BufReader<File>>>
where
    P: AsRef<Path>,
{
    let file = File::open(filename)?;
    Ok(io::BufReader::new(file).lines())
}

struct FlexiBool(bool);

impl FromStr for FlexiBool {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "1" | "true" | "yes" | "y" | "on" | "enabled" => Ok(FlexiBool(true)),
            "0" | "false" | "no" | "n" | "off" | "disabled" => Ok(FlexiBool(false)),
            _ => Err(ConfigError::ParseError(s.to_string())),
        }
    }
}

impl From<FlexiBool> for bool {
    fn from(f: FlexiBool) -> bool {
        f.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MeasurementType {
    Icmp = 1,
    IcmpTimestamps,
    Ntp,
    TcpTimestamps,
}

impl Display for MeasurementType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MeasurementType::Icmp => write!(f, "icmp"),
            MeasurementType::IcmpTimestamps => write!(f, "icmp-timestamps"),
            MeasurementType::Ntp => write!(f, "ntp"),
            MeasurementType::TcpTimestamps => write!(f, "tcp-timestamps"),
        }
    }
}

impl FromStr for MeasurementType {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        return match s.to_lowercase().as_str() {
            "icmp" => Ok(MeasurementType::Icmp),
            "icmp-timestamps" => Ok(MeasurementType::IcmpTimestamps),
            "ntp" => Ok(MeasurementType::Ntp),
            "tcp-timestamps" => Ok(MeasurementType::TcpTimestamps),
            &_ => Err(ConfigError::InvalidMeasurementType(s.to_string())),
        };
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
            _ => Err(ConfigError::InvalidMeasurementType(s.to_string())),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    // Network section
    pub download_interface: String,
    pub upload_interface: String,
    pub download_base_kbits: f64,
    pub download_min_percent: f64,
    pub download_min_kbits: f64,
    pub upload_base_kbits: f64,
    pub upload_min_percent: f64,
    pub upload_min_kbits: f64,

    // Output section
    pub log_level: Level,
    pub speed_hist_file: String,
    pub stats_file: String,
    pub suppress_statistics: bool,

    // Observability section
    pub observability_enabled: bool,
    pub observability_protocol: ObservabilityProtocol,
    pub observability_host: Option<String>,
    pub observability_port: u16,
    pub observability_batch_size: usize,
    pub observability_batch_timeout_ms: u64,
    pub observability_export_ping_metrics: bool,
    pub observability_export_rate_metrics: bool,
    pub observability_export_baseline_metrics: bool,
    pub observability_export_events: bool,
    pub observability_host_tag: String,

    // Advanced section
    pub download_delay_ms: f64,
    pub high_load_level: f64,
    pub min_change_interval: f64,
    pub measurement_type: MeasurementType,
    pub num_reflectors: u8,
    pub peer_reselection_time: u64,
    pub reflector_list_file: String,
    pub speed_hist_size: u32,
    pub tick_interval: f64,
    pub upload_delay_ms: f64,

    // Monitoring / dry-run
    pub dry_run: bool,
}

impl Config {
    pub fn new() -> Result<Self> {
        let config = Self {
            // Network section
            download_base_kbits: Self::get::<f64>(
                "SQMA_DOWNLOAD_BASE_KBITS",
                "sqm-autorate-rust.@network[0].download_base_kbits",
                None,
            )?,
            download_interface: Self::get::<String>(
                "SQMA_DOWNLOAD_INTERFACE",
                "sqm-autorate-rust.@network[0].download_interface",
                None,
            )?,
            download_min_percent: 0.0, // placeholder, computed below
            download_min_kbits: 0.0,   // placeholder, computed below
            upload_base_kbits: Self::get::<f64>(
                "SQMA_UPLOAD_BASE_KBITS",
                "sqm-autorate-rust.@network[0].upload_base_kbits",
                None,
            )?,
            upload_interface: Self::get::<String>(
                "SQMA_UPLOAD_INTERFACE",
                "sqm-autorate-rust.@network[0].upload_interface",
                None,
            )?,
            upload_min_percent: 0.0, // placeholder, computed below
            upload_min_kbits: 0.0,   // placeholder, computed below
            // Output section
            log_level: Self::get::<Level>(
                "SQMA_LOG_LEVEL",
                "sqm-autorate-rust.@output[0].log_level",
                Some(Level::Error),
            )?,
            speed_hist_file: Self::get::<String>(
                "SQMA_SPEED_HIST_FILE",
                "sqm-autorate-rust.@output[0].speed_hist_file",
                Some("/tmp/sqm-speedhist.csv".parse()?),
            )?,
            stats_file: Self::get::<String>(
                "SQMA_STATS_FILE",
                "sqm-autorate-rust.@output[0].stats_file",
                Some("/tmp/sqm-autorate-rust.csv".parse()?),
            )?,
            suppress_statistics: Self::get_bool(
                "SQMA_SUPPRESS_STATISTICS",
                "sqm-autorate-rust.@output[0].suppress_statistics",
                Some(false),
            )?,

            // Observability section
            observability_enabled: Self::get_bool(
                "SQMA_OBSERVABILITY_ENABLED",
                "sqm-autorate-rust.@observability[0].enabled",
                Some(false),
            )?,
            observability_protocol: Self::get::<ObservabilityProtocol>(
                "SQMA_OBSERVABILITY_PROTOCOL",
                "sqm-autorate-rust.@observability[0].protocol",
                Some(ObservabilityProtocol::Udp),
            )?,
            observability_host: Self::get_optional::<String>(
                "SQMA_OBSERVABILITY_HOST",
                "sqm-autorate-rust.@observability[0].host",
            ),
            observability_port: Self::get::<u16>(
                "SQMA_OBSERVABILITY_PORT",
                "sqm-autorate-rust.@observability[0].port",
                Some(8089),
            )?,
            observability_batch_size: Self::get::<usize>(
                "SQMA_OBSERVABILITY_BATCH_SIZE",
                "sqm-autorate-rust.@observability[0].batch_size",
                Some(25),
            )?,
            observability_batch_timeout_ms: Self::get::<u64>(
                "SQMA_OBSERVABILITY_BATCH_TIMEOUT_MS",
                "sqm-autorate-rust.@observability[0].batch_timeout_ms",
                Some(100),
            )?,
            observability_export_ping_metrics: Self::get_bool(
                "SQMA_OBSERVABILITY_EXPORT_PING_METRICS",
                "sqm-autorate-rust.@observability[0].export_ping_metrics",
                Some(false),
            )?,
            observability_export_rate_metrics: Self::get_bool(
                "SQMA_OBSERVABILITY_EXPORT_RATE_METRICS",
                "sqm-autorate-rust.@observability[0].export_rate_metrics",
                Some(true),
            )?,
            observability_export_baseline_metrics: Self::get_bool(
                "SQMA_OBSERVABILITY_EXPORT_BASELINE_METRICS",
                "sqm-autorate-rust.@observability[0].export_baseline_metrics",
                Some(false),
            )?,
            observability_export_events: Self::get_bool(
                "SQMA_OBSERVABILITY_EXPORT_EVENTS",
                "sqm-autorate-rust.@observability[0].export_events",
                Some(true),
            )?,
            observability_host_tag: Self::get::<String>(
                "SQMA_OBSERVABILITY_HOST_TAG",
                "sqm-autorate-rust.@observability[0].host_tag",
                Some(Self::get_host_tag()),
            )?,

            // Advanced section
            download_delay_ms: Self::get::<f64>(
                "SQMA_DOWNLOAD_DELAY_MS",
                "sqm-autorate-rust.@advanced_settings[0].download_delay_ms",
                Some(15.0),
            )?,
            high_load_level: Self::get::<f64>(
                "SQMA_HIGH_LOAD_LEVEL",
                "sqm-autorate-rust.@advanced_settings[0].high_load_level",
                Some(0.8),
            )?,
            measurement_type: Self::get::<MeasurementType>(
                "SQMA_MEASUREMENT_TYPE",
                "sqm-autorate-rust.@advanced_settings[0].measurement_type",
                Some(MeasurementType::IcmpTimestamps),
            )?,
            min_change_interval: Self::get::<f64>(
                "SQMA_MIN_CHANGE_INTERVAL",
                "sqm-autorate-rust.@advanced_settings[0].min_change_interval",
                Some(0.5),
            )?,
            num_reflectors: Self::get::<u8>(
                "SQMA_NUM_REFLECTORS",
                "sqm-autorate-rust.@advanced_settings[0].num_reflectors",
                Some(5),
            )?,
            peer_reselection_time: Self::get::<u64>(
                "SQMA_PEER_RESELECTION_TIME",
                "sqm-autorate-rust.@advanced_settings[0].peer_reselection_time",
                Some(15),
            )?,
            reflector_list_file: Self::get::<String>(
                "SQMA_REFLECTOR_LIST_FILE",
                "sqm-autorate-rust.@advanced_settings[0].reflector_list_file",
                Some("/etc/sqm-autorate/reflectors-icmp.csv".parse()?),
            )?,
            speed_hist_size: Self::get::<u32>(
                "SQMA_SPEED_HIST_SIZE",
                "sqm-autorate-rust.@advanced_settings[0].speed_hist_size",
                Some(100),
            )?,
            tick_interval: Self::get::<f64>(
                "SQMA_TICK_INTERVAL",
                "sqm-autorate-rust.@advanced_settings[0].tick_interval",
                Some(0.5),
            )?,
            upload_delay_ms: Self::get::<f64>(
                "SQMA_UPLOAD_DELAY_MS",
                "sqm-autorate-rust.@advanced_settings[0].upload_delay_ms",
                Some(15.0),
            )?,
            dry_run: Self::get_bool(
                "SQMA_DRY_RUN",
                "sqm-autorate-rust.@advanced_settings[0].dry_run",
                Some(false),
            )?,
        };

        let mut config = config;

        config.download_min_percent = Self::get::<f64>(
            "SQMA_DOWNLOAD_MIN_PERCENT",
            "sqm-autorate-rust.@network[0].download_min_percent",
            Some(20.0),
        )?
        .clamp(1.0, 80.0);

        config.upload_min_percent = Self::get::<f64>(
            "SQMA_UPLOAD_MIN_PERCENT",
            "sqm-autorate-rust.@network[0].upload_min_percent",
            Some(20.0),
        )?
        .clamp(1.0, 80.0);

        config.download_min_kbits =
            (config.download_base_kbits * config.download_min_percent / 100.0).floor();
        config.upload_min_kbits =
            (config.upload_base_kbits * config.upload_min_percent / 100.0).floor();

        Ok(config)
    }

    fn get<T: FromStr>(env_key: &str, uci_key: &str, default: Option<T>) -> Result<T, ConfigError> {
        match Self::get_value(env_key, uci_key) {
            Some(val) => match val.parse::<T>() {
                Ok(parsed_val) => Ok(parsed_val),
                // Ran into an compilation error while trying to return the
                // error as-is, so using my own error type to indicate something went wrong while parsing
                Err(_) => Err(ConfigError::ParseError(env_key.to_string())),
            },
            None => match default {
                Some(val) => Ok(val),
                None => Err(ConfigError::MissingValue(env_key.to_string())),
            },
        }
    }

    fn get_bool(env_key: &str, uci_key: &str, default: Option<bool>) -> Result<bool, ConfigError> {
        Self::get::<FlexiBool>(env_key, uci_key, default.map(FlexiBool)).map(|f| f.0)
    }

    fn get_host_tag() -> String {
        // rustix can give us the hostname without shelling out
        rustix::system::uname()
            .nodename()
            .to_string_lossy()
            .into_owned()
    }

    fn get_optional<T: FromStr>(env_key: &str, uci_key: &str) -> Option<T> {
        Self::get_value(env_key, uci_key).and_then(|val| val.parse::<T>().ok())
    }

    fn get_value(env_key: &str, uci_key: &str) -> Option<String> {
        if let Ok(val) = env::var(env_key) {
            return Some(val);
        }

        if let Some(val) = Self::get_from_uci(uci_key) {
            return Some(val);
        }

        None
    }

    #[cfg(feature = "uci")]
    fn get_from_uci(key: &str) -> Option<String> {
        let mut uci = match Uci::new() {
            Ok(val) => val,
            Err(e) => {
                warn!("Error opening UCI instance: {}", e);
                return None;
            }
        };

        return match uci.get(key) {
            Ok(val) => Some(val),
            Err(e) => {
                warn!("Problem getting config from UCI: {}", e);
                None
            }
        };
    }

    #[cfg(not(feature = "uci"))]
    fn get_from_uci(_: &str) -> Option<String> {
        None
    }

    pub fn load_reflectors(&self) -> Result<Vec<IpAddr>> {
        let lines = read_lines(self.reflector_list_file.clone()).map_err(|e| {
            ConfigError::ReflectorListNotFound(
                self.reflector_list_file.clone() + ": " + e.to_string().as_str(),
            )
        })?;

        let mut reflectors: Vec<IpAddr> = Vec::with_capacity(50);

        let mut first = true;

        for line in lines {
            if first {
                first = false;
                continue;
            }

            let line = line?;
            let columns: Vec<&str> = line.split(',').collect();
            reflectors.push(IpAddr::from_str(columns[0])?);
        }

        Ok(reflectors)
    }
}
