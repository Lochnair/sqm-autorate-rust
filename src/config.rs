use crate::error::{ConfigParseError, InvalidMeasurementTypeError, MissingConfigError};
#[cfg(feature = "uci")]
use log::warn;
use log::Level;
#[cfg(feature = "uci")]
use rust_uci::Uci;
use std::error::Error;
use std::fs::File;
use std::io::BufRead;
use std::net::IpAddr;
use std::path::Path;
use std::str::FromStr;
use std::{env, io};

fn read_lines<P>(filename: P) -> io::Result<io::Lines<io::BufReader<File>>>
where
    P: AsRef<Path>,
{
    let file = File::open(filename)?;
    Ok(io::BufReader::new(file).lines())
}

#[derive(Clone, Copy, Debug)]
pub enum MeasurementType {
    ICMP = 1,
    ICMPTimestamps,
    NTP,
    TCPTimestamps,
}

impl FromStr for MeasurementType {
    type Err = InvalidMeasurementTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        return match s.to_lowercase().as_str() {
            "icmp" => Ok(MeasurementType::ICMP),
            "icmp-timestamps" => Ok(MeasurementType::ICMPTimestamps),
            "ntp" => Ok(MeasurementType::NTP),
            "tcp-timestamps" => Ok(MeasurementType::TCPTimestamps),
            &_ => Err(InvalidMeasurementTypeError {
                type_: s.to_string(),
            }),
        };
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Config {
    // Network section
    pub(crate) download_interface: String,
    pub(crate) upload_interface: String,
    pub(crate) download_base_kbits: f64,
    pub(crate) download_min_kbits: f64,
    pub(crate) upload_base_kbits: f64,
    pub(crate) upload_min_kbits: f64,

    // Output section
    pub(crate) log_level: Level,
    pub(crate) speed_hist_file: String,
    pub(crate) stats_file: String,
    pub(crate) suppress_statistics: bool,

    // Advanced section
    pub(crate) download_delay_ms: f64,
    pub(crate) high_load_level: f64,
    pub(crate) min_change_interval: f64,
    pub(crate) measurement_type: MeasurementType,
    pub(crate) num_reflectors: u8,
    pub(crate) reflector_list_file: String,
    pub(crate) speed_hist_size: u32,
    pub(crate) tick_interval: f64,
    pub(crate) upload_delay_ms: f64,
}

impl Config {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            // Network section
            download_base_kbits: Self::get::<f64>(
                "SQMA_DOWNLOAD_BASE_KBITS",
                "sqm-autorate.@network[0].download_base_kbits",
                None,
            )?,
            download_interface: Self::get::<String>(
                "SQMA_DOWNLOAD_INTERFACE",
                "sqm-autorate.@network[0].download_interface",
                None,
            )?,
            download_min_kbits: Self::get::<f64>(
                "SQMA_DOWNLOAD_MIN_KBITS",
                "sqm-autorate.@network[0].download_min_kbits",
                None,
            )?,
            upload_base_kbits: Self::get::<f64>(
                "SQMA_UPLOAD_BASE_KBITS",
                "sqm-autorate.@network[0].upload_base_kbits",
                None,
            )?,
            upload_interface: Self::get::<String>(
                "SQMA_UPLOAD_INTERFACE",
                "sqm-autorate.@network[0].upload_interface",
                None,
            )?,
            upload_min_kbits: Self::get::<f64>(
                "SQMA_UPLOAD_MIN_KBITS",
                "sqm-autorate.@network[0].upload_min_kbits",
                None,
            )?,
            // Output section
            log_level: Self::get::<Level>(
                "SQMA_LOG_LEVEL",
                "sqm-autorate.@output[0].log_level",
                Some(Level::Error),
            )?,
            speed_hist_file: Self::get::<String>(
                "SQMA_SPEED_HIST_FILE",
                "sqm-autorate.@output[0].speed_hist_file",
                Some("/tmp/sqm-speedhist.csv".parse()?),
            )?,
            stats_file: Self::get::<String>(
                "SQMA_STATS_FILE",
                "sqm-autorate.@output[0].stats_file",
                Some("/tmp/sqm-autorate.csv".parse()?),
            )?,
            suppress_statistics: Self::get::<bool>(
                "SQMA_SUPPRESS_STATISTICS",
                "sqm-autorate.@output[0].suppress_statistics",
                Some(false),
            )?,
            // Advanced section
            download_delay_ms: Self::get::<f64>(
                "SQMA_DOWNLOAD_DELAY_MS",
                "sqm-autorate.@advanced_settings[0].download_delay_ms",
                Some(15.0),
            )?,
            high_load_level: Self::get::<f64>(
                "SQMA_HIGH_LOAD_LEVEL",
                "sqm-autorate.@advanced_settings[0].high_load_level",
                Some(0.8),
            )?,
            measurement_type: Self::get::<MeasurementType>(
                "SQMA_MEASUREMENT_TYPE",
                "sqm-autorate.@advanced_settings[0].measurement_type",
                Some(MeasurementType::ICMPTimestamps),
            )?,
            min_change_interval: Self::get::<f64>(
                "SQMA_MIN_CHANGE_INTERVAL",
                "sqm-autorate.@advanced_settings[0].min_change_interval",
                Some(0.5),
            )?,
            num_reflectors: Self::get::<u8>(
                "SQMA_NUM_REFLECTORS",
                "sqm-autorate.@advanced_settings[0].num_reflectors",
                Some(5),
            )?,
            reflector_list_file: Self::get::<String>(
                "SQMA_REFLECTOR_LIST_FILE",
                "sqm-autorate.@advanced_settings[0].reflector_list_file",
                Some("/etc/sqm-autorate/reflectors-icmp.csv".parse()?),
            )?,
            speed_hist_size: Self::get::<u32>(
                "SQMA_SPEED_HIST_SIZE",
                "sqm-autorate.@advanced_settings[0].speed_hist_size",
                Some(100),
            )?,
            tick_interval: Self::get::<f64>(
                "SQMA_TICK_INTERVAL",
                "sqm-autorate.@advanced_settings[0].tick_interval",
                Some(0.5),
            )?,
            upload_delay_ms: Self::get::<f64>(
                "SQMA_UPLOAD_DELAY_MS",
                "sqm-autorate.@advanced_settings[0].upload_delay_ms",
                Some(15.0),
            )?,
        })
    }

    fn get<T: std::str::FromStr>(
        env_key: &str,
        uci_key: &str,
        default: Option<T>,
    ) -> Result<T, Box<dyn Error>> {
        return match Self::get_value(env_key, uci_key) {
            Some(val) => match val.parse::<T>() {
                Ok(parsed_val) => Ok(parsed_val),
                // Ran into an compilation error while trying to return the
                // error as-is, so using my own error type to indicate something went wrong while parsing
                Err(_) => Err(Box::new(ConfigParseError {
                    config_key: env_key.to_string(),
                })),
            },
            None => {
                return match default {
                    Some(val) => Ok(val),
                    None => Err(Box::new(MissingConfigError {
                        config_key: env_key.to_string(),
                    })),
                }
            }
        };
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

    pub fn load_reflectors(&self) -> Result<Vec<IpAddr>, Box<dyn Error>> {
        let lines = read_lines(self.reflector_list_file.clone())?;

        let mut reflectors: Vec<IpAddr> = Vec::with_capacity(50);

        let mut first = true;

        for line in lines {
            if first {
                first = false;
                continue;
            }

            let line = line?;
            let columns: Vec<&str> = line.split(",").collect();
            reflectors.push(IpAddr::from_str(columns[0])?);
        }

        Ok(reflectors)
    }
}
