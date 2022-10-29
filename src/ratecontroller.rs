use crate::netlink::{Netlink, NetlinkError, Qdisc};
use crate::time::Time;
use crate::{Config, ReflectorStats};
use log::{debug, error, info, warn};
use rand::seq::SliceRandom;
use rand::thread_rng;
use rand::RngCore;
use rustix::thread::ClockId;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::net::IpAddr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};
use std::thread::sleep;
use std::time::Duration;
use thiserror::Error;

#[derive(Copy, Clone, Debug, PartialEq)]
enum Direction {
    Down,
    Up,
}

#[derive(Debug, Error)]
pub enum RatecontrolError {
    #[error("Netlink error")]
    Netlink(#[from] NetlinkError),
}

#[derive(Copy, Clone, Debug)]
pub enum StatsDirection {
    RX,
    TX,
}

fn generate_initial_speeds(base_speed: f64, size: u32) -> Vec<f64> {
    let mut rates = Vec::new();

    for _ in 0..size {
        rates.push((thread_rng().next_u64() as f64 * 0.2 + 0.75) * base_speed);
    }

    rates
}

fn get_interface_stats(
    config: &Config,
    down_direction: StatsDirection,
    up_direction: StatsDirection,
) -> Result<(i128, i128), RatecontrolError> {
    let down_stats = Netlink::get_interface_stats(config.download_interface.as_str())?;
    let up_stats = Netlink::get_interface_stats(config.upload_interface.as_str())?;
    let (down_rx, down_tx) = (down_stats.rx_bytes, down_stats.tx_bytes);
    let (up_rx, up_tx) = (up_stats.rx_bytes, up_stats.tx_bytes);

    let rx_bytes = match down_direction {
        StatsDirection::RX => down_rx,
        StatsDirection::TX => down_tx,
    };

    let tx_bytes = match up_direction {
        StatsDirection::RX => up_rx,
        StatsDirection::TX => up_tx,
    };

    Ok((rx_bytes.into(), tx_bytes.into()))
}

#[derive(Clone, Debug)]
struct State {
    current_bytes: i128,
    current_rate: f64,
    delta_stat: f64,
    deltas: Vec<f64>,
    qdisc: Qdisc,
    load: f64,
    next_rate: f64,
    now_t: f64,
    nrate: usize,
    previous_bytes: i128,
    previous_bytes_t: f64,
    safe_rates: Vec<f64>,
    utilisation: f64,
}

pub struct Ratecontroller {
    config: Config,
    owd_baseline: Arc<Mutex<HashMap<IpAddr, ReflectorStats>>>,
    owd_recent: Arc<Mutex<HashMap<IpAddr, ReflectorStats>>>,
    reflectors_lock: Arc<RwLock<Vec<IpAddr>>>,
    reselect_trigger: Sender<bool>,

    state_dl: State,
    state_ul: State,
}

impl Ratecontroller {
    fn calculate_rate(&mut self, direction: Direction) -> anyhow::Result<()> {
        let (base_rate, delay_ms, min_rate, state) = if direction == Direction::Down {
            (
                self.config.download_base_kbits,
                self.config.download_delay_ms,
                self.config.download_min_kbits,
                &mut self.state_dl,
            )
        } else {
            (
                self.config.upload_base_kbits,
                self.config.upload_delay_ms,
                self.config.upload_min_kbits,
                &mut self.state_ul,
            )
        };

        if !state.deltas.is_empty() {
            state.next_rate = state.current_rate;

            if state.deltas.len() < 3 {
                state.next_rate = min_rate;
            } else {
                state.delta_stat = if state.deltas[2] > 0.0 {
                    state.deltas[2]
                } else {
                    state.deltas[0]
                };

                if state.delta_stat > 0.0 {
                    /*
                     * TODO - find where the (8 / 1000) comes from and
                     *    i. convert to a pre-computed factor
                     *    ii. ideally, see if it can be defined in terms of constants, eg ticks per second and number of active reflectors
                     */
                    state.utilisation = (8.0 / 1000.0)
                        * (state.current_bytes as f64 - state.previous_bytes as f64)
                        / (state.now_t - state.previous_bytes_t);
                    state.load = state.utilisation / state.current_rate;

                    if state.delta_stat > 0.0
                        && state.delta_stat < delay_ms
                        && state.load > self.config.high_load_level
                    {
                        state.safe_rates[state.nrate] = (state.current_rate * state.load).round();
                        let max_rate = state
                            .safe_rates
                            .iter()
                            .max_by(|a, b| a.total_cmp(b))
                            .unwrap();
                        state.next_rate = state.current_rate
                            * (1.0 + 0.1 * (1.0_f64 - state.current_rate / max_rate).max(0.0))
                            + (base_rate * 0.03);
                        state.nrate += 1;
                        state.nrate %= self.config.speed_hist_size as usize;
                    }

                    if state.delta_stat > delay_ms {
                        let mut rng = thread_rng();
                        match state.safe_rates.choose(&mut rng) {
                            Some(rnd_rate) => {
                                state.next_rate =
                                    rnd_rate.min(0.9 * state.current_rate * state.load);
                            }
                            None => {
                                state.next_rate = 0.9 * state.current_rate * state.load;
                            }
                        }
                    }
                }
            }
        }

        state.previous_bytes_t = state.now_t;
        state.previous_bytes = state.current_bytes;

        state.next_rate = state.next_rate.max(min_rate).round();

        Ok(())
    }

    fn update_deltas(&mut self, now_abstime: f64) {
        let state_dl = &mut self.state_dl;
        let state_ul = &mut self.state_ul;

        state_dl.deltas.clear();
        state_ul.deltas.clear();

        let owd_baseline = self.owd_baseline.lock().unwrap();
        let owd_recent = self.owd_recent.lock().unwrap();
        let reflectors = self.reflectors_lock.read().unwrap();

        for reflector in reflectors.iter() {
            // only consider this data if it's less than 2 * tick_duration seconds old
            if owd_baseline.contains_key(reflector)
                && owd_recent.contains_key(reflector)
                && owd_recent[reflector].last_receive_time_s
                    > now_abstime - 2.0 * self.config.tick_interval
            {
                state_dl
                    .deltas
                    .push(owd_recent[reflector].down_ewma - owd_baseline[reflector].down_ewma);
                state_ul
                    .deltas
                    .push(owd_recent[reflector].up_ewma - owd_baseline[reflector].up_ewma);

                debug!(
                    "Reflector: {} down_delay: {} up_delay: {}",
                    reflector,
                    state_dl.deltas.last().unwrap(),
                    state_ul.deltas.last().unwrap()
                );
            }
        }

        // sort owd's lowest to highest
        state_dl.deltas.sort_by(|a, b| a.partial_cmp(b).unwrap());
        state_ul.deltas.sort_by(|a, b| a.partial_cmp(b).unwrap());

        if state_dl.deltas.len() < 5 || state_ul.deltas.len() < 5 {
            // trigger reselection
            warn!("Not enough delta values, triggering reselection");
            let _ = self.reselect_trigger.send(true);
        }
    }

    pub fn new(
        config: Config,
        owd_baseline: Arc<Mutex<HashMap<IpAddr, ReflectorStats>>>,
        owd_recent: Arc<Mutex<HashMap<IpAddr, ReflectorStats>>>,
        reflectors_lock: Arc<RwLock<Vec<IpAddr>>>,
        reselect_trigger: Sender<bool>,
    ) -> anyhow::Result<Self> {
        let dl_qdisc = Netlink::qdisc_from_ifname(config.download_interface.as_str())?;
        let dl_safe_rates =
            generate_initial_speeds(config.download_base_kbits, config.speed_hist_size);
        let ul_qdisc = Netlink::qdisc_from_ifname(config.upload_interface.as_str())?;
        let ul_safe_rates =
            generate_initial_speeds(config.upload_base_kbits, config.speed_hist_size);

        Ok(Self {
            config,
            owd_baseline,
            owd_recent,
            reflectors_lock,
            reselect_trigger,
            state_dl: State {
                current_bytes: 0,
                current_rate: 0.0,
                delta_stat: 0.0,
                deltas: Vec::new(),
                load: 0.0,
                next_rate: 0.0,
                now_t: 0.0,
                nrate: 0,
                qdisc: dl_qdisc,
                previous_bytes: 0,
                previous_bytes_t: 0.0,
                safe_rates: dl_safe_rates,
                utilisation: 0.0,
            },
            state_ul: State {
                current_bytes: 0,
                current_rate: 0.0,
                delta_stat: 0.0,
                deltas: Vec::new(),
                load: 0.0,
                next_rate: 0.0,
                now_t: 0.0,
                nrate: 0,
                qdisc: ul_qdisc,
                previous_bytes: 0,
                previous_bytes_t: 0.0,
                safe_rates: ul_safe_rates,
                utilisation: 0.0,
            },
        })
    }

    pub fn run(
        &mut self,
        down_direction: StatsDirection,
        up_direction: StatsDirection,
    ) -> anyhow::Result<()> {
        let sleep_time = Duration::from_secs_f64(self.config.min_change_interval);

        let time = Time::new(ClockId::Monotonic);
        let start_s = time.get_seconds() as f64;
        let (mut lastchg_s, mut lastchg_ns) =
            (time.get_seconds() as f64, time.get_nanoseconds() as f64);
        let mut lastchg_t = lastchg_s - start_s + lastchg_ns / 1e9;
        let mut lastdump_t = lastchg_t - 310.0;

        // set qdisc rates to 60% of base rate to make sure we start with sane baselines
        self.state_dl.current_rate = self.config.download_base_kbits * 0.6;
        self.state_ul.current_rate = self.config.upload_base_kbits * 0.6;

        Netlink::set_qdisc_rate(
            self.state_dl.qdisc,
            self.state_dl.current_rate.round() as u64,
        )?;
        Netlink::set_qdisc_rate(
            self.state_ul.qdisc,
            self.state_ul.current_rate.round() as u64,
        )?;

        let mut speed_hist_fd: Option<File> = None;
        let mut speed_hist_fd_inner: File;
        let mut stats_fd: Option<File> = None;
        let mut stats_fd_inner: File;

        if !self.config.suppress_statistics {
            speed_hist_fd_inner = File::options()
                .create(true)
                .write(true)
                .open(self.config.speed_hist_file.as_str())?;

            speed_hist_fd_inner.write_all("time,counter,upspeed,downspeed\n".as_bytes())?;
            speed_hist_fd_inner.flush()?;

            speed_hist_fd = Some(speed_hist_fd_inner);

            stats_fd_inner = File::options()
                .create(true)
                .write(true)
                .open(self.config.stats_file.as_str())?;

            stats_fd_inner.write_all(
                "times,timens,rxload,txload,deltadelaydown,deltadelayup,dlrate,uprate\n".as_bytes(),
            )?;
            stats_fd_inner.flush()?;

            stats_fd = Some(stats_fd_inner);
        }

        loop {
            sleep(sleep_time);

            let now = Time::new(ClockId::Monotonic);
            let (mut now_s, now_ns) = (now.get_seconds() as f64, now.get_nanoseconds() as f64);
            let now_abstime = now_s + now_ns / 1e9;
            now_s -= start_s;
            let now_t = now_s + now_ns / 1e9;

            if now_t - lastchg_t > self.config.min_change_interval {
                // if it's been long enough, and the stats indicate needing to change speeds
                // change speeds here

                (self.state_dl.current_bytes, self.state_ul.current_bytes) =
                    get_interface_stats(&self.config, down_direction, up_direction)?;
                if self.state_dl.current_bytes == -1 || self.state_ul.current_bytes == -1 {
                    warn!(
                    "One or both Netlink stats could not be read. Skipping rate control algorithm");
                    continue;
                }

                self.state_dl.now_t = now_t;
                self.state_ul.now_t = now_t;
                self.update_deltas(now_abstime);
                self.calculate_rate(Direction::Down)?;
                self.calculate_rate(Direction::Up)?;

                if self.state_dl.next_rate != self.state_dl.current_rate
                    || self.state_ul.next_rate != self.state_ul.current_rate
                {
                    info!(
                        "self.state_ul.next_rate {} self.state_dl.next_rate {}",
                        self.state_ul.next_rate, self.state_dl.next_rate
                    );
                }

                if self.state_dl.next_rate != self.state_dl.current_rate {
                    Netlink::set_qdisc_rate(self.state_dl.qdisc, self.state_dl.next_rate as u64)?;
                }

                if self.state_ul.next_rate != self.state_ul.current_rate {
                    Netlink::set_qdisc_rate(self.state_ul.qdisc, self.state_ul.next_rate as u64)?;
                }

                self.state_dl.current_rate = self.state_dl.next_rate;
                self.state_ul.current_rate = self.state_ul.next_rate;

                let lastchg = Clock::new(ClockId::Monotonic);
                (lastchg_s, lastchg_ns) = (
                    lastchg.get_seconds() as f64,
                    lastchg.get_nanoseconds() as f64,
                );

                debug!(
                    "{},{},{},{},{},{},{},{}",
                    lastchg_s,
                    lastchg_ns,
                    self.state_dl.load,
                    self.state_ul.load,
                    self.state_dl.delta_stat,
                    self.state_ul.delta_stat,
                    self.state_dl.current_rate,
                    self.state_ul.current_rate
                );

                if let Some(ref mut fd) = stats_fd {
                    if let Err(e) = fd.write(
                        format!(
                            "{},{},{},{},{},{},{},{}\n",
                            lastchg_s,
                            lastchg_ns,
                            self.state_dl.load,
                            self.state_ul.load,
                            self.state_dl.delta_stat,
                            self.state_ul.delta_stat,
                            self.state_dl.current_rate,
                            self.state_ul.current_rate
                        )
                        .as_bytes(),
                    ) {
                        warn!("Failed to write statistics: {}", e);
                    }
                }

                lastchg_s -= start_s;
                lastchg_t = lastchg_s + lastchg_ns / 1e9;
            }

            if let Some(ref mut fd) = speed_hist_fd {
                if now_t - lastdump_t > 300.0 {
                    for i in 0..self.config.speed_hist_size as usize {
                        if let Err(e) = fd.write_all(
                            format!(
                                "{},{},{},{}\n",
                                now_t, i, self.state_ul.safe_rates[i], self.state_dl.safe_rates[i]
                            )
                            .as_bytes(),
                        ) {
                            warn!("Failed to write speed history file: {}", e);
                        }
                    }

                    lastdump_t = now_t;
                }
            }
        }
    }
}
