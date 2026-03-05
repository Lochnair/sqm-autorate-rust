extern crate core;

mod baseliner;
mod component;
mod config;
mod log;
mod netlink;
mod pinger;
mod pinger_icmp;
mod pinger_icmp_ts;
mod ratecontroller;
mod reflector_selector;
mod time;
mod util;

use ::log::info;
use component::baseliner_ewma::SqmEwmaBaseliner;
use component::algorithm_sqm_ewma::SqmEwmaAlgorithm;
use component::algorithm_cake_autorate::CakeAutorateAlgorithm;
use component::algorithm_tievolu::TievolAlgorithm;
use component::{Baseliner, RateAlgorithm};
use config::{AlgorithmType, AppConfig};
use netlink::Netlink;
use pinger::{PingListener, PingSender};
use pinger_icmp::{PingerICMPEchoListener, PingerICMPEchoSender};
use pinger_icmp_ts::{PingerICMPTimestampListener, PingerICMPTimestampSender};
use ratecontroller::StatsDirection;
use reflector_selector::ReflectorSelector;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::mpsc::channel;
use std::sync::{Arc, RwLock};
use std::thread::sleep;
use std::time::Duration;
use std::{env, process, thread};

const VERSION: &str = env!("CARGO_PKG_VERSION");

// Default reflectors used when no pool file is large enough
const DEFAULT_REFLECTORS: &[&str] = &[
    "9.9.9.9",
    "8.238.120.14",
    "74.82.42.42",
    "194.242.2.2",
    "208.67.222.222",
    "94.140.14.14",
];

fn usage() -> ! {
    eprintln!("Usage: sqm-autorate-rust <config.toml>");
    #[cfg(feature = "uci")]
    eprintln!("       sqm-autorate-rust --uci");
    process::exit(1);
}

fn load_app_config() -> anyhow::Result<AppConfig> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("--uci") => {
            #[cfg(feature = "uci")]
            return config::load_config_uci();
            #[cfg(not(feature = "uci"))]
            {
                eprintln!("--uci requires the 'uci' feature flag");
                process::exit(1);
            }
        }
        Some(path) => config::load_config(path),
        None => {
            // Try a few conventional locations before giving up
            for candidate in &[
                "/etc/sqm-autorate/sqm-autorate.toml",
                "/etc/sqm-autorate.toml",
                "sqm-autorate.toml",
            ] {
                if std::path::Path::new(candidate).exists() {
                    return config::load_config(candidate);
                }
            }
            eprintln!("No config file found. Pass a path as the first argument.");
            usage();
        }
    }
}

fn build_algorithm(cfg: &AppConfig) -> anyhow::Result<Box<dyn RateAlgorithm>> {
    let net = &cfg.network;
    let min_dl = net.download_min_kbits();
    let min_ul = net.upload_min_kbits();

    match cfg.algorithm.algorithm_type {
        AlgorithmType::SqmEwma => Ok(Box::new(SqmEwmaAlgorithm::new(
            cfg.algorithm.sqm_ewma.clone(),
            net.download_base_kbits,
            net.upload_base_kbits,
            min_dl,
            min_ul,
        ))),

        AlgorithmType::CakeAutorate => Ok(Box::new(CakeAutorateAlgorithm::new(
            cfg.algorithm.cake_autorate.clone(),
            net.download_base_kbits,
            net.upload_base_kbits,
            min_dl,
            min_ul,
        ))),

        AlgorithmType::Tievolu => Ok(Box::new(TievolAlgorithm::new(
            cfg.algorithm.tievolu.clone(),
            net.download_base_kbits,
            net.upload_base_kbits,
            min_dl,
            min_ul,
        ))),

        AlgorithmType::Lua => {
            #[cfg(feature = "lua")]
            {
                let lua_cfg = cfg.algorithm.lua.as_ref().unwrap();
                Ok(Box::new(
                    component::algorithm_lua::LuaAlgorithm::new(
                        lua_cfg,
                        net.download_base_kbits,
                        net.upload_base_kbits,
                    )?,
                ))
            }
            #[cfg(not(feature = "lua"))]
            anyhow::bail!(
                "Algorithm type 'lua' requires the 'lua' feature — rebuild with --features lua"
            )
        }
    }
}

fn main() -> anyhow::Result<()> {
    println!("Starting sqm-autorate version {VERSION}");

    let cfg = load_app_config()?;
    log::init(cfg.output.log_level)?;

    info!("sqm-autorate {VERSION} starting");
    info!("Algorithm: {:?}", cfg.algorithm.algorithm_type);

    let id = (process::id() & 0xFFFF) as u16;

    // ── Build reflector lists ─────────────────────────────────────────────────
    let all_reflectors = config::load_reflectors(&cfg.ping_source.reflector_list)?;
    let pool_size = all_reflectors.len();
    let num_reflectors = cfg.ping_source.num_reflectors as usize;

    let default_peers: Vec<IpAddr> = DEFAULT_REFLECTORS
        .iter()
        .map(|s| IpAddr::from_str(s).unwrap())
        .collect();

    let reflector_peers_lock = Arc::new(RwLock::new(default_peers));
    let reflector_pool: Vec<IpAddr>;

    let use_dynamic_selector = pool_size > num_reflectors;
    if use_dynamic_selector {
        reflector_pool = all_reflectors;
    } else {
        reflector_pool = Vec::new();
    }

    // ── Build shared baseliner ────────────────────────────────────────────────
    let baseliner: Arc<RwLock<dyn Baseliner>> = Arc::new(RwLock::new(
        SqmEwmaBaseliner::new(cfg.ping_source.tick_interval),
    ));

    // ── Channels ──────────────────────────────────────────────────────────────
    let (ping_tx, ping_rx) = channel();
    let (reselect_tx, reselect_rx) = channel::<bool>();

    // ── Pinger implementation ─────────────────────────────────────────────────
    let (mut pinger_receiver, mut pinger_sender): (
        Box<dyn PingListener + Send>,
        Box<dyn PingSender + Send>,
    ) = match cfg.ping_source.measurement_type {
        config::MeasurementType::Icmp => (
            Box::new(PingerICMPEchoListener {}),
            Box::new(PingerICMPEchoSender {}),
        ),
        config::MeasurementType::IcmpTimestamps => (
            Box::new(PingerICMPTimestampListener {}),
            Box::new(PingerICMPTimestampSender {}),
        ),
        config::MeasurementType::Ntp | config::MeasurementType::TcpTimestamps => {
            todo!("NTP and TCP timestamp pingers are not yet implemented")
        }
    };

    // ── Initialise qdisc to minimum rates ────────────────────────────────────
    let down_qdisc = Netlink::qdisc_from_ifname(cfg.network.download_interface.as_str())?;
    let up_qdisc = Netlink::qdisc_from_ifname(cfg.network.upload_interface.as_str())?;
    let min_dl = cfg.network.download_min_kbits();
    let min_ul = cfg.network.upload_min_kbits();

    info!("Setting shaper to minimum rates: DL={min_dl} UL={min_ul} kbit/s");
    Netlink::set_qdisc_rate(down_qdisc, min_dl as u64)?;
    Netlink::set_qdisc_rate(up_qdisc, min_ul as u64)?;

    // Brief settle to let the shaper drain any queued bloat
    info!("Settling for 2 s…");
    sleep(Duration::new(2, 0));

    // ── Algorithm ─────────────────────────────────────────────────────────────
    let mut algorithm = build_algorithm(&cfg)?;

    // ── Error bus ─────────────────────────────────────────────────────────────
    let (error_tx, error_rx) = channel::<anyhow::Error>();

    // Thread: pinger receiver
    {
        let err_tx = error_tx.clone();
        let peers = reflector_peers_lock.clone();
        let mtype = cfg.ping_source.measurement_type;
        thread::Builder::new()
            .name("receiver".into())
            .spawn(move || {
                if let Err(e) = pinger_receiver.listen(id, mtype, peers, ping_tx) {
                    let _ = err_tx.send(e);
                }
            })?;
    }

    // Thread: baseliner
    {
        let err_tx = error_tx.clone();
        let b = baseliner.clone();
        let rs = reselect_tx.clone();
        thread::Builder::new()
            .name("baseliner".into())
            .spawn(move || {
                if let Err(e) = baseliner::run(b, ping_rx, rs) {
                    let _ = err_tx.send(e);
                }
            })?;
    }

    // Thread: pinger sender
    {
        let err_tx = error_tx.clone();
        let peers = reflector_peers_lock.clone();
        let tick = cfg.ping_source.tick_interval;
        let mtype = cfg.ping_source.measurement_type;
        thread::Builder::new()
            .name("sender".into())
            .spawn(move || {
                if let Err(e) = pinger_sender.send(id, mtype, peers, tick) {
                    let _ = err_tx.send(e);
                }
            })?;
    }

    // Thread: reflector selector (only when pool is larger than active count)
    if use_dynamic_selector {
        let selector = ReflectorSelector {
            config: cfg.ping_source.clone(),
            baseliner: baseliner.clone(),
            reflector_peers_lock: reflector_peers_lock.clone(),
            reflector_pool,
            trigger_channel: reselect_rx,
        };
        let err_tx = error_tx.clone();
        thread::Builder::new()
            .name("reselection".into())
            .spawn(move || {
                if let Err(e) = selector.run() {
                    let _ = err_tx.send(e);
                }
            })?;
    }

    // Wait 10 s before we start touching rates (let baselines stabilise)
    info!("Waiting 10 s before enabling rate control…");
    sleep(Duration::new(10, 0));

    // ── Determine interface stats direction ───────────────────────────────────
    let dl_dir = if cfg.network.download_interface.starts_with("ifb")
        || cfg.network.download_interface.starts_with("veth")
    {
        StatsDirection::TX
    } else {
        StatsDirection::RX
    };
    let ul_dir = if cfg.network.upload_interface.starts_with("ifb")
        || cfg.network.upload_interface.starts_with("veth")
    {
        StatsDirection::RX
    } else {
        StatsDirection::TX
    };

    // Thread: rate controller
    {
        let err_tx = error_tx.clone();
        let b = baseliner.clone();
        let net = cfg.network.clone();
        let out = cfg.output.clone();
        let rs = reselect_tx.clone();
        thread::Builder::new()
            .name("ratecontroller".into())
            .spawn(move || {
                if let Err(e) = ratecontroller::run(
                    algorithm.as_mut(),
                    b,
                    &net,
                    rs,
                    down_qdisc,
                    up_qdisc,
                    dl_dir,
                    ul_dir,
                    &out,
                ) {
                    let _ = err_tx.send(e);
                }
            })?;
    }

    // Drop original sender so recv() unblocks when all threads exit cleanly
    drop(error_tx);

    match error_rx.recv() {
        Ok(e) => Err(anyhow::anyhow!("thread exited with error: {e}")),
        Err(_) => Ok(()), // all senders dropped = all threads exited cleanly
    }
}
