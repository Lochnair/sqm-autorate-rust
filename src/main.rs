extern crate core;

mod cake;
mod config;
mod netlink;
mod pinger;

use std::net::IpAddr;
use std::str::FromStr;
use std::{thread, time};

use crate::config::Config;
use crate::netlink::{find_interface, find_qdisc, set_qdisc_rate};
use crate::pinger::{Pinger, PingerICMPEcho};

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
    test(config);

    let mut reflectors: Vec<IpAddr> = Vec::new();

    reflectors.push(IpAddr::from_str("1.0.0.1").unwrap());
    reflectors.push(IpAddr::from_str("1.1.1.1").unwrap());
    reflectors.push(IpAddr::from_str("9.9.9.9").unwrap());
    reflectors.push(IpAddr::from_str("9.9.9.10").unwrap());

    let pinger = PingerICMPEcho::new(reflectors);
    let pinger_receiver = pinger.clone();
    let pinger_sender = pinger.clone();

    let receiver_handle = thread::spawn(move || pinger_receiver.receive_loop());
    let sender_handle = thread::spawn(move || pinger_sender.sender_loop());

    receiver_handle.join().unwrap();
    sender_handle.join().unwrap();
}

fn test(config: Config) {
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
