use anyhow::Result;
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::net::IpAddr;
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Config file not found at '{0}'")]
    NotFound(String),
    #[error("Failed to parse config: {0}")]
    ParseError(String),
    #[error("Reflector list not found at '{0}'")]
    ReflectorListNotFound(String),
    #[error("Algorithm type '{0}' requires feature flag '{0}' — rebuild with --features {0}")]
    FeatureNotEnabled(String),
    #[error("Algorithm type 'lua' requires [algorithm.lua] section with 'script' key")]
    LuaConfigMissing,
}

// ── MeasurementType ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum MeasurementType {
    Icmp,
    IcmpTimestamps,
    Ntp,
    TcpTimestamps,
}

impl Default for MeasurementType {
    fn default() -> Self {
        Self::IcmpTimestamps
    }
}

// ── AlgorithmType ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AlgorithmType {
    #[default]
    SqmEwma,
    Lua,
    CakeAutorate,
    Tievolu,
}

// ── NetworkConfig ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct NetworkConfig {
    pub download_interface: String,
    pub upload_interface: String,
    pub download_base_kbits: f64,
    #[serde(default = "NetworkConfig::default_min_percent")]
    pub download_min_percent: f64,
    pub upload_base_kbits: f64,
    #[serde(default = "NetworkConfig::default_min_percent")]
    pub upload_min_percent: f64,
}

impl NetworkConfig {
    fn default_min_percent() -> f64 {
        20.0
    }

    pub fn download_min_kbits(&self) -> f64 {
        (self.download_base_kbits * self.download_min_percent.clamp(1.0, 80.0) / 100.0).floor()
    }

    pub fn upload_min_kbits(&self) -> f64 {
        (self.upload_base_kbits * self.upload_min_percent.clamp(1.0, 80.0) / 100.0).floor()
    }
}

// ── PingSourceConfig ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct PingSourceConfig {
    #[serde(rename = "type", default)]
    pub measurement_type: MeasurementType,
    #[serde(default = "PingSourceConfig::default_reflector_list")]
    pub reflector_list: String,
    #[serde(default = "PingSourceConfig::default_num_reflectors")]
    pub num_reflectors: u8,
    #[serde(default = "PingSourceConfig::default_tick_interval")]
    pub tick_interval: f64,
    #[serde(default = "PingSourceConfig::default_peer_reselection_time")]
    pub peer_reselection_time: u64,
}

impl PingSourceConfig {
    fn default_reflector_list() -> String {
        "/etc/sqm-autorate/reflectors-icmp.csv".into()
    }
    fn default_num_reflectors() -> u8 {
        5
    }
    fn default_tick_interval() -> f64 {
        0.5
    }
    fn default_peer_reselection_time() -> u64 {
        15
    }
}

impl Default for PingSourceConfig {
    fn default() -> Self {
        Self {
            measurement_type: MeasurementType::default(),
            reflector_list: Self::default_reflector_list(),
            num_reflectors: Self::default_num_reflectors(),
            tick_interval: Self::default_tick_interval(),
            peer_reselection_time: Self::default_peer_reselection_time(),
        }
    }
}

// ── OutputConfig ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct OutputConfig {
    #[serde(
        default = "OutputConfig::default_log_level",
        deserialize_with = "OutputConfig::deser_log_level"
    )]
    pub log_level: log::Level,
    #[serde(default = "OutputConfig::default_speed_hist_file")]
    pub speed_hist_file: String,
    #[serde(default = "OutputConfig::default_stats_file")]
    pub stats_file: String,
    #[serde(default)]
    pub suppress_statistics: bool,
}

impl OutputConfig {
    fn default_log_level() -> log::Level {
        log::Level::Error
    }
    fn default_speed_hist_file() -> String {
        "/tmp/sqm-speedhist.csv".into()
    }
    fn default_stats_file() -> String {
        "/tmp/sqm-autorate.csv".into()
    }

    fn deser_log_level<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<log::Level, D::Error> {
        let s = String::deserialize(d)?;
        s.parse::<log::Level>().map_err(serde::de::Error::custom)
    }
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            log_level: log::Level::Error,
            speed_hist_file: Self::default_speed_hist_file(),
            stats_file: Self::default_stats_file(),
            suppress_statistics: false,
        }
    }
}

// ── Algorithm configs ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct SqmEwmaConfig {
    #[serde(default = "SqmEwmaConfig::default_delay_ms")]
    pub download_delay_ms: f64,
    #[serde(default = "SqmEwmaConfig::default_delay_ms")]
    pub upload_delay_ms: f64,
    #[serde(default = "SqmEwmaConfig::default_high_load_level")]
    pub high_load_level: f64,
    #[serde(default = "SqmEwmaConfig::default_min_change_interval")]
    pub min_change_interval: f64,
    #[serde(default = "SqmEwmaConfig::default_speed_hist_size")]
    pub speed_hist_size: u32,
}

impl SqmEwmaConfig {
    fn default_delay_ms() -> f64 {
        15.0
    }
    fn default_high_load_level() -> f64 {
        0.8
    }
    fn default_min_change_interval() -> f64 {
        0.5
    }
    fn default_speed_hist_size() -> u32 {
        100
    }
}

impl Default for SqmEwmaConfig {
    fn default() -> Self {
        Self {
            download_delay_ms: Self::default_delay_ms(),
            upload_delay_ms: Self::default_delay_ms(),
            high_load_level: Self::default_high_load_level(),
            min_change_interval: Self::default_min_change_interval(),
            speed_hist_size: Self::default_speed_hist_size(),
        }
    }
}

#[cfg(feature = "lua")]
#[derive(Debug, Clone, Deserialize)]
pub struct LuaConfig {
    pub script: String,
    #[serde(flatten)]
    pub extra: toml::Table,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CakeAutorateConfig {
    /// Download OWD delta threshold to classify as delayed (ms)
    #[serde(default = "CakeAutorateConfig::default_dl_delay_thr_ms")]
    pub dl_delay_thr_ms: f64,
    /// Upload OWD delta threshold to classify as delayed (ms)
    #[serde(default = "CakeAutorateConfig::default_ul_delay_thr_ms")]
    pub ul_delay_thr_ms: f64,
    /// Max avg delta before rate increase is suppressed (ms)
    #[serde(default = "CakeAutorateConfig::default_max_adjust_up_thr_ms")]
    pub max_adjust_up_thr_ms: f64,
    /// Avg delta at which maximum decrease is applied (ms)
    #[serde(default = "CakeAutorateConfig::default_max_adjust_down_thr_ms")]
    pub max_adjust_down_thr_ms: f64,
    /// Circular bufferbloat detection window size
    #[serde(default = "CakeAutorateConfig::default_detection_window")]
    pub detection_window: usize,
    /// Number of delayed samples in window to declare bufferbloat
    #[serde(default = "CakeAutorateConfig::default_detection_threshold")]
    pub detection_threshold: usize,
    /// High load fraction (0.0-1.0)
    #[serde(default = "CakeAutorateConfig::default_high_load_thr")]
    pub high_load_thr: f64,
    /// kbps below which connection is "idle"
    #[serde(default = "CakeAutorateConfig::default_connection_active_thr_kbps")]
    pub connection_active_thr_kbps: f64,
    /// Refractory period between bufferbloat responses (ms)
    #[serde(default = "CakeAutorateConfig::default_bufferbloat_refractory_ms")]
    pub bufferbloat_refractory_ms: f64,
    /// Refractory period between decay steps (ms)
    #[serde(default = "CakeAutorateConfig::default_decay_refractory_ms")]
    pub decay_refractory_ms: f64,
    /// EWMA alpha for baseline increase (slow)
    #[serde(default = "CakeAutorateConfig::default_alpha_baseline_increase")]
    pub alpha_baseline_increase: f64,
    /// EWMA alpha for baseline decrease (fast)
    #[serde(default = "CakeAutorateConfig::default_alpha_baseline_decrease")]
    pub alpha_baseline_decrease: f64,
    /// EWMA alpha for per-reflector delta smoothing
    #[serde(default = "CakeAutorateConfig::default_alpha_delta_ewma")]
    pub alpha_delta_ewma: f64,
    /// Minimum change interval (same as rate controller loop interval)
    #[serde(default = "CakeAutorateConfig::default_min_change_interval")]
    pub min_change_interval: f64,
}

impl CakeAutorateConfig {
    fn default_dl_delay_thr_ms() -> f64 { 30.0 }
    fn default_ul_delay_thr_ms() -> f64 { 30.0 }
    fn default_max_adjust_up_thr_ms() -> f64 { 10.0 }
    fn default_max_adjust_down_thr_ms() -> f64 { 60.0 }
    fn default_detection_window() -> usize { 6 }
    fn default_detection_threshold() -> usize { 3 }
    fn default_high_load_thr() -> f64 { 0.75 }
    fn default_connection_active_thr_kbps() -> f64 { 2000.0 }
    fn default_bufferbloat_refractory_ms() -> f64 { 300.0 }
    fn default_decay_refractory_ms() -> f64 { 1000.0 }
    fn default_alpha_baseline_increase() -> f64 { 0.001 }
    fn default_alpha_baseline_decrease() -> f64 { 0.9 }
    fn default_alpha_delta_ewma() -> f64 { 0.095 }
    fn default_min_change_interval() -> f64 { 0.5 }
}

impl Default for CakeAutorateConfig {
    fn default() -> Self {
        Self {
            dl_delay_thr_ms: Self::default_dl_delay_thr_ms(),
            ul_delay_thr_ms: Self::default_ul_delay_thr_ms(),
            max_adjust_up_thr_ms: Self::default_max_adjust_up_thr_ms(),
            max_adjust_down_thr_ms: Self::default_max_adjust_down_thr_ms(),
            detection_window: Self::default_detection_window(),
            detection_threshold: Self::default_detection_threshold(),
            high_load_thr: Self::default_high_load_thr(),
            connection_active_thr_kbps: Self::default_connection_active_thr_kbps(),
            bufferbloat_refractory_ms: Self::default_bufferbloat_refractory_ms(),
            decay_refractory_ms: Self::default_decay_refractory_ms(),
            alpha_baseline_increase: Self::default_alpha_baseline_increase(),
            alpha_baseline_decrease: Self::default_alpha_baseline_decrease(),
            alpha_delta_ewma: Self::default_alpha_delta_ewma(),
            min_change_interval: Self::default_min_change_interval(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TievolConfig {
    /// Download latency threshold under load (ms) — required
    pub dl_max_loaded_latency_ms: Option<f64>,
    /// Upload latency threshold under load (ms) — required
    pub ul_max_loaded_latency_ms: Option<f64>,
    /// Download target resting rate (kbits/s). Defaults to download_base_kbits.
    pub dl_bw_standard_kbits: Option<f64>,
    /// Upload target resting rate (kbits/s). Defaults to upload_base_kbits.
    pub ul_bw_standard_kbits: Option<f64>,
    /// Minimum increase step size (%)
    #[serde(default = "TievolConfig::default_increase_min_pc")]
    pub increase_min_pc: f64,
    /// Maximum increase step size (%)
    #[serde(default = "TievolConfig::default_increase_max_pc")]
    pub increase_max_pc: f64,
    /// Latency-to-increase scaling factor
    #[serde(default = "TievolConfig::default_increase_factor")]
    pub increase_factor: f64,
    /// Minimum load fraction to trigger increase
    #[serde(default = "TievolConfig::default_increase_load_thr")]
    pub increase_load_thr: f64,
    /// Seconds to wait after a decrease before increasing
    #[serde(default = "TievolConfig::default_increase_delay_after_decrease_s")]
    pub increase_delay_after_decrease_s: f64,
    /// Seconds to wait between increases
    #[serde(default = "TievolConfig::default_increase_delay_after_increase_s")]
    pub increase_delay_after_increase_s: f64,
    /// Minimum decrease step size (%)
    #[serde(default = "TievolConfig::default_decrease_min_pc")]
    pub decrease_min_pc: f64,
    /// Conservative margin below bad-bandwidth level (%)
    #[serde(default = "TievolConfig::default_decrease_overshoot_pc")]
    pub decrease_overshoot_pc: f64,
    /// Seconds to wait between decreases
    #[serde(default = "TievolConfig::default_decrease_delay_s")]
    pub decrease_delay_s: f64,
    /// Step size back toward standard bandwidth (%)
    #[serde(default = "TievolConfig::default_relax_pc")]
    pub relax_pc: f64,
    /// Max load fraction to allow relax step
    #[serde(default = "TievolConfig::default_relax_load_thr")]
    pub relax_load_thr: f64,
    /// Seconds to wait between relax steps
    #[serde(default = "TievolConfig::default_relax_delay_s")]
    pub relax_delay_s: f64,
    /// Number of ICMP samples used per decision
    #[serde(default = "TievolConfig::default_max_recent_results")]
    pub max_recent_results: usize,
    /// Fraction of bad pings to classify latency as bad
    #[serde(default = "TievolConfig::default_bad_ping_pc")]
    pub bad_ping_pc: f64,
    /// Control loop interval (s)
    #[serde(default = "TievolConfig::default_min_change_interval")]
    pub min_change_interval: f64,
}

impl TievolConfig {
    fn default_increase_min_pc() -> f64 { 1.0 }
    fn default_increase_max_pc() -> f64 { 25.0 }
    fn default_increase_factor() -> f64 { 1.0 }
    fn default_increase_load_thr() -> f64 { 0.70 }
    fn default_increase_delay_after_decrease_s() -> f64 { 600.0 }
    fn default_increase_delay_after_increase_s() -> f64 { 0.0 }
    fn default_decrease_min_pc() -> f64 { 10.0 }
    fn default_decrease_overshoot_pc() -> f64 { 5.0 }
    fn default_decrease_delay_s() -> f64 { 2.0 }
    fn default_relax_pc() -> f64 { 5.0 }
    fn default_relax_load_thr() -> f64 { 0.50 }
    fn default_relax_delay_s() -> f64 { 60.0 }
    fn default_max_recent_results() -> usize { 20 }
    fn default_bad_ping_pc() -> f64 { 25.0 }
    fn default_min_change_interval() -> f64 { 0.5 }
}

impl Default for TievolConfig {
    fn default() -> Self {
        Self {
            dl_max_loaded_latency_ms: None,
            ul_max_loaded_latency_ms: None,
            dl_bw_standard_kbits: None,
            ul_bw_standard_kbits: None,
            increase_min_pc: Self::default_increase_min_pc(),
            increase_max_pc: Self::default_increase_max_pc(),
            increase_factor: Self::default_increase_factor(),
            increase_load_thr: Self::default_increase_load_thr(),
            increase_delay_after_decrease_s: Self::default_increase_delay_after_decrease_s(),
            increase_delay_after_increase_s: Self::default_increase_delay_after_increase_s(),
            decrease_min_pc: Self::default_decrease_min_pc(),
            decrease_overshoot_pc: Self::default_decrease_overshoot_pc(),
            decrease_delay_s: Self::default_decrease_delay_s(),
            relax_pc: Self::default_relax_pc(),
            relax_load_thr: Self::default_relax_load_thr(),
            relax_delay_s: Self::default_relax_delay_s(),
            max_recent_results: Self::default_max_recent_results(),
            bad_ping_pc: Self::default_bad_ping_pc(),
            min_change_interval: Self::default_min_change_interval(),
        }
    }
}

// ── AlgorithmSection ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AlgorithmSection {
    #[serde(rename = "type", default)]
    pub algorithm_type: AlgorithmType,
    #[serde(default)]
    pub sqm_ewma: SqmEwmaConfig,
    #[cfg(feature = "lua")]
    pub lua: Option<LuaConfig>,
    #[serde(default)]
    pub cake_autorate: CakeAutorateConfig,
    #[serde(default)]
    pub tievolu: TievolConfig,
}

// ── TcpMonitorConfig ──────────────────────────────────────────────────────────

#[cfg(feature = "ebpf")]
#[derive(Debug, Clone, Deserialize)]
pub struct TcpMonitorConfig {
    pub enabled: bool,
    pub wan_interface: String,
    #[serde(default)]
    pub dynamic_reflectors: bool,
    #[serde(default = "TcpMonitorConfig::default_max_dynamic_reflectors")]
    pub max_dynamic_reflectors: usize,
}

#[cfg(feature = "ebpf")]
impl TcpMonitorConfig {
    fn default_max_dynamic_reflectors() -> usize { 5 }
}

// ── AppConfig ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub network: NetworkConfig,
    #[serde(default)]
    pub ping_source: PingSourceConfig,
    #[serde(default)]
    pub algorithm: AlgorithmSection,
    #[serde(default)]
    pub output: OutputConfig,
    #[cfg(feature = "ebpf")]
    pub tcp_monitor: Option<TcpMonitorConfig>,
}

impl AppConfig {
    pub fn validate(&self) -> Result<()> {
        match self.algorithm.algorithm_type {
            AlgorithmType::Lua => {
                #[cfg(not(feature = "lua"))]
                return Err(ConfigError::FeatureNotEnabled("lua".into()).into());

                #[cfg(feature = "lua")]
                if self.algorithm.lua.is_none() {
                    return Err(ConfigError::LuaConfigMissing.into());
                }
            }
            _ => {}
        }
        Ok(())
    }
}

// ── Loaders ───────────────────────────────────────────────────────────────────

pub fn load_config(path: &str) -> Result<AppConfig> {
    let content = std::fs::read_to_string(path)
        .map_err(|_| ConfigError::NotFound(path.to_string()))?;
    let config: AppConfig = toml::from_str(&content)
        .map_err(|e| ConfigError::ParseError(e.to_string()))?;
    config.validate()?;
    Ok(config)
}

#[cfg(feature = "uci")]
pub fn load_config_uci() -> Result<AppConfig> {
    use log::warn;
    use rust_uci::Uci;

    let mut uci = Uci::new().map_err(|e| anyhow::anyhow!("UCI open error: {e}"))?;

    macro_rules! uci_get {
        ($uci:expr, $key:expr) => {
            $uci.get($key).ok()
        };
    }

    macro_rules! uci_parse {
        ($uci:expr, $key:expr, $default:expr) => {
            $uci.get($key)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or($default)
        };
    }

    let download_interface = uci_get!(uci, "sqm-autorate.@network[0].download_interface")
        .ok_or_else(|| anyhow::anyhow!("UCI: missing download_interface"))?;
    let upload_interface = uci_get!(uci, "sqm-autorate.@network[0].upload_interface")
        .ok_or_else(|| anyhow::anyhow!("UCI: missing upload_interface"))?;
    let download_base_kbits: f64 = uci_get!(uci, "sqm-autorate.@network[0].download_base_kbits")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("UCI: missing download_base_kbits"))?;
    let upload_base_kbits: f64 = uci_get!(uci, "sqm-autorate.@network[0].upload_base_kbits")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("UCI: missing upload_base_kbits"))?;

    let network = NetworkConfig {
        download_interface,
        upload_interface,
        download_base_kbits,
        download_min_percent: uci_parse!(uci, "sqm-autorate.@network[0].download_min_percent", 20.0),
        upload_base_kbits,
        upload_min_percent: uci_parse!(uci, "sqm-autorate.@network[0].upload_min_percent", 20.0),
    };

    let ping_source = PingSourceConfig {
        measurement_type: uci_get!(uci, "sqm-autorate.@advanced_settings[0].measurement_type")
            .and_then(|v| v.parse().ok())
            .unwrap_or_default(),
        reflector_list: uci_get!(uci, "sqm-autorate.@advanced_settings[0].reflector_list_file")
            .unwrap_or_else(PingSourceConfig::default_reflector_list),
        num_reflectors: uci_parse!(uci, "sqm-autorate.@advanced_settings[0].num_reflectors", 5u8),
        tick_interval: uci_parse!(uci, "sqm-autorate.@advanced_settings[0].tick_interval", 0.5f64),
        peer_reselection_time: uci_parse!(
            uci,
            "sqm-autorate.@advanced_settings[0].peer_reselection_time",
            15u64
        ),
    };

    let log_level = uci_get!(uci, "sqm-autorate.@output[0].log_level")
        .and_then(|v| v.parse().ok())
        .unwrap_or(log::Level::Error);

    let output = OutputConfig {
        log_level,
        speed_hist_file: uci_get!(uci, "sqm-autorate.@output[0].speed_hist_file")
            .unwrap_or_else(OutputConfig::default_speed_hist_file),
        stats_file: uci_get!(uci, "sqm-autorate.@output[0].stats_file")
            .unwrap_or_else(OutputConfig::default_stats_file),
        suppress_statistics: uci_parse!(
            uci,
            "sqm-autorate.@output[0].suppress_statistics",
            false
        ),
    };

    let sqm_ewma = SqmEwmaConfig {
        download_delay_ms: uci_parse!(
            uci,
            "sqm-autorate.@advanced_settings[0].download_delay_ms",
            15.0f64
        ),
        upload_delay_ms: uci_parse!(
            uci,
            "sqm-autorate.@advanced_settings[0].upload_delay_ms",
            15.0f64
        ),
        high_load_level: uci_parse!(
            uci,
            "sqm-autorate.@advanced_settings[0].high_load_level",
            0.8f64
        ),
        min_change_interval: uci_parse!(
            uci,
            "sqm-autorate.@advanced_settings[0].min_change_interval",
            0.5f64
        ),
        speed_hist_size: uci_parse!(
            uci,
            "sqm-autorate.@advanced_settings[0].speed_hist_size",
            100u32
        ),
    };

    let algorithm = AlgorithmSection {
        algorithm_type: AlgorithmType::SqmEwma,
        sqm_ewma,
        #[cfg(feature = "lua")]
        lua: None,
        cake_autorate: CakeAutorateConfig::default(),
        tievolu: TievolConfig::default(),
    };

    let config = AppConfig {
        network,
        ping_source,
        algorithm,
        output,
        #[cfg(feature = "ebpf")]
        tcp_monitor: None,
    };

    config.validate()?;
    Ok(config)
}

impl FromStr for MeasurementType {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "icmp" => Ok(Self::Icmp),
            "icmp-timestamps" | "icmp_timestamps" => Ok(Self::IcmpTimestamps),
            "ntp" => Ok(Self::Ntp),
            "tcp-timestamps" | "tcp_timestamps" => Ok(Self::TcpTimestamps),
            _ => Err(format!("unknown measurement type: {s}")),
        }
    }
}

// ── Reflector list loader ─────────────────────────────────────────────────────

pub fn load_reflectors(path: &str) -> Result<Vec<IpAddr>> {
    let f = std::fs::File::open(path)
        .map_err(|_| ConfigError::ReflectorListNotFound(path.to_string()))?;
    let reader = BufReader::new(f);
    let mut reflectors = Vec::with_capacity(50);
    let mut first = true;

    for line in reader.lines() {
        if first {
            first = false;
            continue;
        }
        let line = line?;
        let ip_str = line.split(',').next().unwrap_or("").trim();
        if let Ok(ip) = IpAddr::from_str(ip_str) {
            reflectors.push(ip);
        }
    }

    Ok(reflectors)
}
