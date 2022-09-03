extern crate core;

mod cake;
mod config;
mod netlink;
mod pinger;
mod pinger_icmp;
mod pinger_icmp_ts;

use std::net::IpAddr;
use std::str::FromStr;
use std::{process, thread, time};

use crate::config::Config;
use crate::netlink::{find_interface, find_qdisc, get_interface_stats, set_qdisc_rate};
use crate::pinger::Pinger;
use crate::pinger_icmp::PingerICMPEcho;
use crate::pinger_icmp_ts::PingerICMPTimestamps;

#[macro_export]
macro_rules! error {
    ($a:expr,$b:expr) => {
        match $a {
            //Some(e) => return e,
            Ok(e) => e,
            Err(e) => {
                println!("{}: {}", $b, $a);
            }
        }
    };
}

fn main() {
    println!("starting up");

    let config = Config::new();
    test(&config);

    let mut reflectors: Vec<IpAddr> = Vec::new();

    reflectors.push(IpAddr::from_str("1.0.0.1").unwrap());
    reflectors.push(IpAddr::from_str("1.1.1.1").unwrap());
    reflectors.push(IpAddr::from_str("9.9.9.9").unwrap());
    reflectors.push(IpAddr::from_str("9.9.9.10").unwrap());

    get_interface_stats(config.upload_interface.as_str()).unwrap();

    let mut pinger_receiver =
        PingerICMPTimestamps::new((process::id() & 0xFFFF) as u16, reflectors.clone());
    let mut pinger_sender =
        PingerICMPTimestamps::new((process::id() & 0xFFFF) as u16, reflectors.clone());

    let receiver_handle = thread::Builder::new()
        .name("receiver".to_string())
        .spawn(move || pinger_receiver.receive_loop())
        .unwrap();
    let sender_handle = thread::Builder::new()
        .name("sender".to_string())
        .spawn(move || pinger_sender.sender_loop())
        .unwrap();

    let threads = vec![receiver_handle, sender_handle];

    for thread in threads {
        thread.join().unwrap();
    }
}

fn test(config: &Config) {
    if true {
        return;
    }

    let start = time::Instant::now();
    let ifindex =
        find_interface(config.upload_interface.as_str()).expect("Couldn't find interface");
    let find_if_time = time::Instant::now().duration_since(start);
    println!("time get intf: {}", find_if_time.as_micros());

    let start = time::Instant::now();
    let qdisc = find_qdisc(ifindex).expect("Couldn't find qdisc");
    let find_qdisc_time = time::Instant::now().duration_since(start);

    println!("time get qdisc: {}", find_qdisc_time.as_micros());

    let start = time::Instant::now();
    set_qdisc_rate(qdisc, 1000 * 1000 / 8).unwrap();
    let set_qdisc_time = time::Instant::now().duration_since(start);
    println!("time set qdisc: {}", set_qdisc_time.as_micros());

    println!(
        "time total: {}",
        find_if_time.as_micros() + find_qdisc_time.as_micros() + set_qdisc_time.as_micros()
    );
}
