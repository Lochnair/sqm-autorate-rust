extern crate core;

mod baseliner;
mod cake;
mod config;
mod endian;
mod log;
mod netlink;
mod pinger;
mod pinger_icmp;
mod pinger_icmp_ts;
mod ratecontroller;
mod reflector_selector;
mod utils;

use crate::baseliner::{Baseliner, ReflectorStats};
use ::log::debug;
use std::collections::HashMap;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex, RwLock};
use std::thread::sleep;
use std::time::Duration;
use std::{process, thread};

use crate::config::{Config, MeasurementType};
use crate::netlink::Netlink;
use crate::pinger::{PingListener, PingSender};
use crate::pinger_icmp::{PingerICMPEchoListener, PingerICMPEchoSender};
use crate::pinger_icmp_ts::{PingerICMPTimestampListener, PingerICMPTimestampSender};
use crate::ratecontroller::{Ratecontroller, StatsDirection};
use crate::reflector_selector::ReflectorSelector;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> anyhow::Result<()> {
    println!("Starting sqm-autorate version {}", VERSION);

    let config = Config::new()?;
    log::init(config.log_level)?;
    let mut reflectors = config.load_reflectors()?;

    // The identifier field in ICMP is only 2 bytes
    // so take the last 2 bytes of the PID as the ID
    let id = (process::id() & 0xFFFF) as u16;

    // Create data structures shared by different threads
    let owd_baseline = Arc::new(Mutex::new(HashMap::<IpAddr, ReflectorStats>::new()));
    let owd_recent = Arc::new(Mutex::new(HashMap::<IpAddr, ReflectorStats>::new()));
    let reflector_peers_lock = Arc::new(RwLock::new(Vec::<IpAddr>::new()));
    let mut reflector_pool = Vec::<IpAddr>::new();
    let reflector_pool_size = reflectors.len();

    let default_reflectors = [
        IpAddr::from_str("9.9.9.9")?,
        IpAddr::from_str("8.238.120.14")?,
        IpAddr::from_str("74.82.42.42")?,
        IpAddr::from_str("194.242.2.2")?,
        IpAddr::from_str("208.67.222.222")?,
        IpAddr::from_str("94.140.14.14")?,
    ];

    match reflector_pool_size > 5 {
        true => {
            let mut peers = reflector_peers_lock.write().unwrap();
            peers.append(default_reflectors.to_vec().as_mut());
            reflector_pool.append(reflectors.as_mut());
        }
        false => {
            let mut peers = reflector_peers_lock.write().unwrap();
            peers.append(default_reflectors.to_vec().as_mut());
        }
    }

    let (baseliner_stats_sender, baseliner_stats_receiver) = channel();
    let (reselect_sender, reselect_receiver) = channel();

    let (mut pinger_receiver, mut pinger_sender) = match config.measurement_type {
        MeasurementType::ICMP => (
            Box::new(PingerICMPEchoListener {}) as Box<dyn PingListener + Send>,
            Box::new(PingerICMPEchoSender {}) as Box<dyn PingSender + Send>,
        ),
        MeasurementType::ICMPTimestamps => (
            Box::new(PingerICMPTimestampListener {}) as Box<dyn PingListener + Send>,
            Box::new(PingerICMPTimestampSender {}) as Box<dyn PingSender + Send>,
        ),
        MeasurementType::NTP | MeasurementType::TCPTimestamps => {
            todo!()
        }
    };

    let baseliner = Baseliner {
        config: config.clone(),
        owd_baseline: owd_baseline.clone(),
        owd_recent: owd_recent.clone(),
        reselect_trigger: reselect_sender.clone(),
        stats_receiver: baseliner_stats_receiver,
    };

    let down_qdisc = Netlink::qdisc_from_ifname(config.download_interface.as_str())?;
    let up_qdisc = Netlink::qdisc_from_ifname(config.upload_interface.as_str())?;

    /* Set initial TC values to minimum
     * so there should be no initial bufferbloat to
     * fool the baseliner
     */
    Netlink::set_qdisc_rate(down_qdisc, config.download_min_kbits as u64)?;
    Netlink::set_qdisc_rate(up_qdisc, config.upload_min_kbits as u64)?;
    sleep(Duration::new(0, 5e8 as u32));

    let reflector_peers_lock_clone = reflector_peers_lock.clone();
    let receiver_handle = thread::Builder::new().name("receiver".to_string()).spawn(
        move || -> anyhow::Result<()> {
            pinger_receiver.listen(
                id,
                config.measurement_type,
                reflector_peers_lock_clone,
                baseliner_stats_sender,
            )
        },
    )?;
    let baseliner_handle = thread::Builder::new()
        .name("baseliner".to_string())
        .spawn(move || -> anyhow::Result<()> { baseliner.run() })?;
    let reflector_peers_lock_clone = reflector_peers_lock.clone();
    let sender_handle = thread::Builder::new().name("sender".to_string()).spawn(
        move || -> anyhow::Result<()> {
            pinger_sender.send(id, config.measurement_type, reflector_peers_lock_clone)
        },
    )?;

    let mut threads = vec![receiver_handle, sender_handle, baseliner_handle];

    if reflector_pool_size > 5 {
        let reflector_selector = ReflectorSelector {
            config: config.clone(),
            owd_recent: owd_recent.clone(),
            reflector_peers_lock: reflector_peers_lock.clone(),
            reflector_pool,
            trigger_channel: reselect_receiver,
        };
        let reselection_handle = thread::Builder::new()
            .name("reselection".to_string())
            .spawn(move || reflector_selector.run())?;
        threads.push(reselection_handle);
    }

    // Sleep 10 seconds before we start adjusting speeds
    sleep(Duration::new(10, 0));

    let dl_direction;
    let ul_direction;

    if config.download_interface.starts_with("ifb") || config.download_interface.starts_with("veth")
    {
        dl_direction = StatsDirection::TX;
    } else if config.download_interface == "br-lan" {
        dl_direction = StatsDirection::RX;
    } else {
        dl_direction = StatsDirection::RX;
    }

    if config.upload_interface.starts_with("ifb") || config.upload_interface.starts_with("veth") {
        ul_direction = StatsDirection::RX;
    } else {
        ul_direction = StatsDirection::TX;
    }

    let mut ratecontroller = Ratecontroller::new(
        config.clone(),
        owd_baseline.clone(),
        owd_recent.clone(),
        reflector_peers_lock.clone(),
        reselect_sender.clone(),
    )?;

    debug!(
        "Download direction: {}:{:?}",
        config.download_interface, dl_direction
    );

    debug!(
        "Upload direction: {}:{:?}",
        config.upload_interface, ul_direction
    );

    let ratecontroller_handle = thread::Builder::new()
        .name("ratecontroller".to_string())
        .spawn(move || ratecontroller.run(dl_direction, ul_direction))?;

    threads.push(ratecontroller_handle);

    for thread in threads {
        thread.join().expect("Error happened in thread")?;
    }

    Ok(())
}
