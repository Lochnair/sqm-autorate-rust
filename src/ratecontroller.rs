use crate::netlink::Netlink;
use crate::{Config, ReflectorStats, Utils};
use rand::seq::SliceRandom;
use rand::thread_rng;
use rand::RngCore;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

#[derive(Copy, Clone)]
pub enum StatsDirection {
    RX,
    TX,
}

fn f64_max(a: f64, b: f64) -> f64 {
    *[a, b].iter().max_by(|a, b| a.total_cmp(b)).unwrap()
}

fn f64_min(a: f64, b: f64) -> f64 {
    *[a, b].iter().min_by(|a, b| a.total_cmp(b)).unwrap()
}

fn get_interface_stats(
    config: Config,
    down_direction: StatsDirection,
    up_direction: StatsDirection,
) -> (i128, i128) {
    let (down_rx, down_tx) = match Netlink::get_interface_stats(config.download_interface.as_str())
    {
        Ok(stats) => (stats.rx_bytes as i128, stats.tx_bytes as i128),
        Err(e) => {
            println!("Error while retrieving stats: {}", e.to_string());
            (-1, -1)
        }
    };
    let (up_rx, up_tx) = match Netlink::get_interface_stats(config.upload_interface.as_str()) {
        Ok(stats) => (stats.rx_bytes as i128, stats.tx_bytes as i128),
        Err(e) => {
            println!("Error while retrieving stats: {}", e.to_string());
            (-1, -1)
        }
    };

    let rx_bytes = match down_direction {
        RX => down_rx,
        TX => down_tx,
    };

    let tx_bytes = match up_direction {
        RX => up_rx,
        TX => up_tx,
    };

    (rx_bytes, tx_bytes)
}

pub struct Ratecontroller {
    pub(crate) config: Config,
    pub(crate) owd_baseline: Arc<Mutex<HashMap<IpAddr, ReflectorStats>>>,
    pub(crate) owd_recent: Arc<Mutex<HashMap<IpAddr, ReflectorStats>>>,
    pub(crate) reflectors_lock: Arc<Mutex<Vec<IpAddr>>>,
    pub(crate) reselect_trigger: Sender<bool>,
}

impl Ratecontroller {
    pub fn run(&self, down_direction: StatsDirection, up_direction: StatsDirection) {
        let sleep_time_s = self.config.min_change_interval.floor() as u64;
        let sleep_time_ns = ((self.config.min_change_interval % 1.0) * 1e9) as u32;
        let sleep_time = Duration::new(sleep_time_s, sleep_time_ns);

        let (start_s, start_ns) = Utils::get_current_time();
        let (mut lastchg_s, mut lastchg_ns) = Utils::get_current_time();
        let mut lastchg_t = lastchg_s - start_s + lastchg_ns / 1e9;
        let mut lastdump_t = lastchg_t - 310.0;

        let mut rng = thread_rng();

        let base_dl_rate = 45000.0;
        let base_ul_rate = 15000.0;
        let min_dl_rate = 5000.0;
        let min_ul_rate = 1000.0;
        let histsize = 100;

        let dl_max_delta_owd = 15.0;
        let ul_max_delta_owd = 15.0;

        let high_load_level = 0.8;

        let down_ifindex =
            Netlink::find_interface(self.config.download_interface.as_str()).unwrap();
        let up_ifindex = Netlink::find_interface(self.config.upload_interface.as_str()).unwrap();
        let down_qdisc = Netlink::find_qdisc(down_ifindex).unwrap();
        let up_qdisc = Netlink::find_qdisc(up_ifindex).unwrap();

        // set qdisc rates to 60% of base rate to make sure we start with sane baselines
        let mut cur_dl_rate: f64 = base_dl_rate * 0.6;
        let mut cur_ul_rate: f64 = base_ul_rate * 0.6;
        Netlink::set_qdisc_rate(down_qdisc, cur_dl_rate.floor() as u64).unwrap();
        Netlink::set_qdisc_rate(up_qdisc, cur_ul_rate.floor() as u64).unwrap();

        let (mut prev_rx_bytes, mut prev_tx_bytes) =
            get_interface_stats(self.config.clone(), down_direction, up_direction);

        if prev_rx_bytes == -1 || prev_tx_bytes == -1 {
            panic!("Couldn't retrieve stats from interface.");
        }

        let mut t_prev_bytes = lastchg_t;

        let mut safe_dl_rates: Vec<f64> = Vec::new();
        let mut safe_ul_rates: Vec<f64> = Vec::new();
        for i in 0..histsize {
            safe_dl_rates.push((rand::thread_rng().next_u32() as f64 * 0.2 + 0.75) * base_dl_rate);
            safe_ul_rates.push((rand::thread_rng().next_u32() as f64 * 0.2 + 0.75) * base_dl_rate);
        }

        let mut nrate_up = 0;
        let mut nrate_down = 0;

        loop {
            let (mut now_s, now_ns) = Utils::get_current_time();
            let now_abstime = now_s + now_ns / 1e9;
            now_s = now_s - start_s;
            let now_t = now_s + now_ns / 1e9;

            if now_t - lastchg_t > self.config.min_change_interval {
                let owd_baseline = self.owd_baseline.lock().unwrap();
                let owd_recent = self.owd_recent.lock().unwrap();
                let reflectors = self.reflectors_lock.lock().unwrap();

                let mut down_delta_stat: f64;
                let mut up_delta_stat: f64;

                if reflectors.len() > 0 {
                    let mut down_deltas = Vec::new();
                    let mut up_deltas = Vec::new();
                    let (
                        mut next_dl_rate,
                        mut next_ul_rate,
                        mut rx_load,
                        mut tx_load,
                        mut up_utilisation,
                        mut down_utilisation,
                    ): (f64, f64, f64, f64, f64, f64);

                    rx_load = -1.0;
                    tx_load = -1.0;
                    down_delta_stat = 0.0;
                    up_delta_stat = 0.0;

                    for reflector in reflectors.iter() {
                        if owd_baseline.contains_key(reflector)
                            && owd_recent.contains_key(reflector)
                            && owd_recent[reflector].last_receive_time_s
                                > now_abstime - 2.0 * self.config.tick_duration
                        {
                            down_deltas.push(
                                owd_recent[reflector].down_ewma - owd_baseline[reflector].down_ewma,
                            );
                            up_deltas.push(
                                owd_recent[reflector].up_ewma - owd_baseline[reflector].up_ewma,
                            );
                        }
                    }

                    if down_deltas.len() < 5 || up_deltas.len() < 5 {
                        // trigger reselection
                        let _ = self.reselect_trigger.send(true);
                    }

                    next_dl_rate = cur_dl_rate;
                    next_ul_rate = cur_ul_rate;

                    let (cur_rx_bytes, cur_tx_bytes) =
                        get_interface_stats(self.config.clone(), down_direction, up_direction);
                    if cur_rx_bytes == -1 || cur_tx_bytes == -1 {
                        println!("One or both Netlink stats could not be read. Skipping rate control algorithm");
                    } else if down_deltas.len() < 3 || up_deltas.len() < 3 {
                        next_dl_rate = min_dl_rate;
                        next_ul_rate = min_ul_rate;
                    } else {
                        down_deltas.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        up_deltas.sort_by(|a, b| a.partial_cmp(b).unwrap());

                        down_delta_stat = Utils::a_else_b(down_deltas[2], down_deltas[0]);
                        up_delta_stat = Utils::a_else_b(up_deltas[2], up_deltas[0]);

                        if down_delta_stat > 0.0 && up_delta_stat > 0.0 {
                            /*
                             * TODO - find where the (8 / 1000) comes from and
                             *    i. convert to a pre-computed factor
                             *    ii. ideally, see if it can be defined in terms of constants, eg ticks per second and number of active reflectors
                             */
                            down_utilisation = (8.0 / 1000.0)
                                * (cur_rx_bytes as f64 - prev_rx_bytes as f64)
                                / (now_t - t_prev_bytes);
                            rx_load = down_utilisation / cur_dl_rate;
                            up_utilisation = (8.0 / 1000.0)
                                * (cur_tx_bytes as f64 - prev_tx_bytes as f64)
                                / (now_t - t_prev_bytes);
                            tx_load = up_utilisation / cur_ul_rate;
                            next_ul_rate = cur_ul_rate;
                            next_dl_rate = cur_dl_rate;

                            if down_delta_stat > 0.0
                                && down_delta_stat < dl_max_delta_owd
                                && rx_load > high_load_level
                            {
                                safe_dl_rates[nrate_down] = (cur_dl_rate * rx_load).floor();
                                let max_dl =
                                    safe_dl_rates.iter().max_by(|a, b| a.total_cmp(b)).unwrap();
                                next_dl_rate = cur_dl_rate
                                    * (1.0 + 0.1 * f64_max(0.0, 1.0 - cur_dl_rate / max_dl))
                                    + (base_dl_rate * 0.03);
                                nrate_down = nrate_down + 1;
                                nrate_down = nrate_down % histsize;
                            }

                            if up_delta_stat > 0.0
                                && up_delta_stat < dl_max_delta_owd
                                && tx_load > high_load_level
                            {
                                safe_ul_rates[nrate_up] = (cur_ul_rate * tx_load).floor();
                                let max_ul =
                                    safe_ul_rates.iter().max_by(|a, b| a.total_cmp(b)).unwrap();
                                next_ul_rate = cur_ul_rate
                                    * (1.0 + 0.1 * f64_max(0.0, 1.0 - cur_ul_rate / max_ul))
                                    + (base_dl_rate * 0.03);
                                nrate_up = nrate_up + 1;
                                nrate_up = nrate_up % histsize;
                            }

                            if down_delta_stat > dl_max_delta_owd {
                                if safe_dl_rates.len() > 0 {
                                    next_dl_rate = f64_min(
                                        0.9 * cur_dl_rate * rx_load,
                                        *safe_dl_rates.choose(&mut rng).unwrap(),
                                    )
                                } else {
                                    next_dl_rate = 0.9 * cur_dl_rate * rx_load;
                                }
                            }

                            if up_delta_stat > ul_max_delta_owd {
                                if safe_ul_rates.len() > 0 {
                                    next_ul_rate = f64_min(
                                        0.9 * cur_ul_rate * tx_load,
                                        *safe_ul_rates.choose(&mut rng).unwrap(),
                                    )
                                } else {
                                    next_ul_rate = 0.9 * cur_ul_rate * tx_load;
                                }
                            }
                        }
                    }

                    t_prev_bytes = now_t;
                    prev_rx_bytes = cur_rx_bytes;
                    prev_tx_bytes = cur_tx_bytes;

                    next_dl_rate = f64_max(min_dl_rate, next_dl_rate).floor();
                    next_ul_rate = f64_max(min_ul_rate, next_ul_rate).floor();

                    if next_dl_rate != cur_dl_rate || next_ul_rate != cur_ul_rate {
                        println!(
                            "next_ul_rate {} next_dl_rate {}",
                            next_ul_rate, next_dl_rate
                        );
                    }

                    if next_dl_rate != cur_dl_rate {
                        Netlink::set_qdisc_rate(down_qdisc, next_dl_rate as u64).unwrap();
                    }

                    if next_ul_rate != cur_ul_rate {
                        Netlink::set_qdisc_rate(up_qdisc, next_ul_rate as u64).unwrap();
                    }

                    cur_dl_rate = next_dl_rate;
                    cur_ul_rate = next_ul_rate;

                    (lastchg_s, lastchg_ns) = Utils::get_current_time();

                    if rx_load > 0.0
                        && tx_load > 0.0
                        && down_delta_stat > 0.0
                        && up_delta_stat > 0.0
                    {
                        println!(
                            "{},{},{},{},{},{},{},{}",
                            lastchg_s,
                            lastchg_ns,
                            rx_load,
                            tx_load,
                            down_delta_stat,
                            up_delta_stat,
                            cur_dl_rate,
                            cur_ul_rate
                        );
                    }

                    lastchg_s = lastchg_s - start_s;
                    lastchg_t = lastchg_s + lastchg_ns / 1e9;
                }
            }

            sleep(sleep_time);
        }
    }
}
