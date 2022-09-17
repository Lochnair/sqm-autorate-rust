extern crate core;

mod baseliner;
mod cake;
mod config;
mod error;
mod netlink;
mod pinger;
mod pinger_icmp;
mod pinger_icmp_ts;
mod ratecontroller;
mod reflector_selector;
mod utils;

use crate::baseliner::{Baseliner, ReflectorStats};
use std::collections::HashMap;
use std::net::IpAddr;
use std::process::ExitCode;
use std::str::FromStr;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;
use std::{process, thread};

use crate::config::Config;
use crate::netlink::Netlink;
use crate::pinger::{PingListener, PingSender, SocketType};
use crate::pinger_icmp::{PingerICMPEchoListener, PingerICMPEchoSender};
use crate::pinger_icmp_ts::{PingerICMPTimestampListener, PingerICMPTimestampSender};
use crate::ratecontroller::{Ratecontroller, StatsDirection};
use crate::reflector_selector::ReflectorSelector;
use crate::utils::Utils;

fn main() -> ExitCode {
    println!("starting up");

    let config = Config::new();
    let mut reflectors = match config.load_reflectors() {
        Ok(refl) => refl,
        Err(e) => {
            println!("Couldn't load reflectors: {}", e.to_string());
            return ExitCode::FAILURE;
        }
    };

    // The identifier field in ICMP is only 4 bytes
    // so take the last 4 bytes of the PID as the ID
    let id = (process::id() & 0xFFFF) as u16;

    // Create data structures shared by different threads
    let mut owd_baseline = Arc::new(Mutex::new(HashMap::<IpAddr, ReflectorStats>::new()));
    let mut owd_recent = Arc::new(Mutex::new(HashMap::<IpAddr, ReflectorStats>::new()));
    let mut reflector_peers_lock = Arc::new(Mutex::new(Vec::<IpAddr>::new()));
    let mut reflector_pool = Vec::<IpAddr>::new();
    let reflector_pool_size = reflectors.len();

    let default_reflectors = [
        IpAddr::from_str("9.9.9.9").unwrap(),
        IpAddr::from_str("8.238.120.14").unwrap(),
        IpAddr::from_str("74.82.42.42").unwrap(),
        IpAddr::from_str("194.242.2.2").unwrap(),
        IpAddr::from_str("208.67.222.222").unwrap(),
        IpAddr::from_str("94.140.14.14").unwrap(),
    ];

    match reflector_pool_size > 5 {
        true => {
            let mut peers = reflector_peers_lock.lock().unwrap();
            peers.append(default_reflectors.to_vec().as_mut());
            reflector_pool.append(reflectors.as_mut());
        }
        false => {
            let mut peers = reflector_peers_lock.lock().unwrap();
            peers.append(default_reflectors.to_vec().as_mut());
        }
    }

    let (baseliner_stats_sender, baseliner_stats_receiver) = channel();
    let (reselect_sender, reselect_receiver) = channel();

    //let mut pinger_receiver = PingerICMPTimestampListener::new(id);
    //let mut pinger_sender = PingerICMPTimestampSender::new(id);
    let mut pinger_receiver = PingerICMPEchoListener::new(id);
    let mut pinger_sender = PingerICMPEchoSender::new(id);

    let mut baseliner = Baseliner {
        config: config.clone(),
        owd_baseline: owd_baseline.clone(),
        owd_recent: owd_recent.clone(),
        reselect_trigger: reselect_sender.clone(),
        stats_receiver: baseliner_stats_receiver,
    };

    let dl_intf = config.clone().download_interface;
    let ul_intf = config.clone().upload_interface;
    let down_ifindex = Netlink::find_interface(dl_intf.as_str()).unwrap();
    let up_ifindex = Netlink::find_interface(ul_intf.as_str()).unwrap();
    let down_qdisc = Netlink::find_qdisc(down_ifindex).unwrap();
    let up_qdisc = Netlink::find_qdisc(up_ifindex).unwrap();

    /* Set initial TC values to minimum
     * so there should be no initial bufferbloat to
     * fool the baseliner
     */
    Netlink::set_qdisc_rate(down_qdisc, 5000).expect("Couldn't set ingress bandwidth");
    Netlink::set_qdisc_rate(up_qdisc, 1000).expect("Couldn't set egress bandwidth");
    sleep(Duration::new(0, 5e8 as u32));

    let reflector_peers_lock_clone = reflector_peers_lock.clone();
    let receiver_handle = thread::Builder::new()
        .name("receiver".to_string())
        .spawn(move || {
            pinger_receiver.listen(
                SocketType::ICMP,
                reflector_peers_lock_clone,
                baseliner_stats_sender,
            )
        })
        .expect("Couldn't spawn ping receiver thread");
    let baseliner_handle = thread::Builder::new()
        .name("baseliner".to_string())
        .spawn(move || baseliner.run())
        .expect("Couldn't spawn baseliner thread");
    let reflector_peers_lock_clone = reflector_peers_lock.clone();
    let sender_handle = thread::Builder::new()
        .name("sender".to_string())
        .spawn(move || pinger_sender.send(SocketType::ICMP, reflector_peers_lock_clone))
        .expect("Couldn't spawn ping sender thread");

    let mut threads = vec![receiver_handle, sender_handle, baseliner_handle];

    if reflector_pool_size > 5 {
        let reflector_selector = ReflectorSelector {
            config: config.clone(),
            owd_baseline: owd_baseline.clone(),
            owd_recent: owd_recent.clone(),
            reflector_peers_lock: reflector_peers_lock.clone(),
            reflector_pool,
            trigger_channel: reselect_receiver,
        };
        let reselection_handle = thread::Builder::new()
            .name("reselection".to_string())
            .spawn(move || reflector_selector.run())
            .expect("Couldn't spawn reflector selector thread");
        threads.push(reselection_handle);
    }

    // Sleep 10 seconds before we start adjusting speeds
    sleep(Duration::new(10, 0));

    let ratecontroller = Ratecontroller {
        config: config.clone(),
        owd_baseline: owd_baseline.clone(),
        owd_recent: owd_recent.clone(),
        reflectors_lock: reflector_peers_lock.clone(),
        reselect_trigger: reselect_sender.clone(),
    };

    let ratecontroller_handle = thread::Builder::new()
        .name("ratecontroller".to_string())
        .spawn(move || ratecontroller.run(StatsDirection::TX, StatsDirection::RX))
        .expect("Couldn't spawn ratecontroller thread");

    threads.push(ratecontroller_handle);

    for thread in threads {
        println!("thread: {}", thread.thread().name().unwrap());
        thread.join().unwrap();
    }

    ExitCode::SUCCESS
}
