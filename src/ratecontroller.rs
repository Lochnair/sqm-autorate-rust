// SPDX-FileCopyrightText: 2022-Present Charles Corrigan mailto:chas-iot@runegate.org (github @chas-iot)
// SPDX-FileCopyrightText: 2022-Present Daniel Lakeland mailto:dlakelan@street-artists.org (github @dlakelan)
// SPDX-FileCopyrightText: 2022-Present Mark Baker mailto:mark@vpost.net (github @Fail-Safe)
// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use crate::SHUTDOWN;
use crate::metrics::{Metric, MetricsSender};
use crate::platform::{
    InterfaceStatsProvider, PlatformInterfaceStats, PlatformTrafficControl, TrafficControlBackend,
    interface_stats_provider, traffic_control_backend,
};
use crate::time::Time;
use crate::util::{ArcMutex, ArcRwLock, MutexExt, RwLockExt};
use crate::{Config, ReflectorStats};
use flume::Sender;
use log::{debug, info, warn};
use rustix::time::ClockId;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::net::IpAddr;
use std::sync::atomic::Ordering;
use std::thread::sleep;
use std::time::{Duration, Instant};

#[derive(Copy, Clone, Debug, PartialEq)]
enum Direction {
    Down,
    Up,
}

#[derive(Copy, Clone, Debug)]
pub enum StatsDirection {
    RX,
    TX,
}

fn generate_initial_speeds(base_speed: f64, size: u32) -> Vec<f64> {
    let mut rates = Vec::new();

    for _ in 0..size {
        rates.push((fastrand::f64() * 0.2 + 0.75) * base_speed);
    }

    rates
}

fn get_interface_stats<S: InterfaceStatsProvider>(
    stats_provider: &mut S,
    download_interface: &str,
    upload_interface: &str,
    down_direction: StatsDirection,
    up_direction: StatsDirection,
) -> Result<(i128, i128), S::Error> {
    let down = stats_provider.read_stats(download_interface)?;
    let up = stats_provider.read_stats(upload_interface)?;

    let rx_bytes = match down_direction {
        StatsDirection::RX => down.rx_bytes,
        StatsDirection::TX => down.tx_bytes,
    };

    let tx_bytes = match up_direction {
        StatsDirection::RX => up.rx_bytes,
        StatsDirection::TX => up.tx_bytes,
    };

    Ok((rx_bytes.into(), tx_bytes.into()))
}

#[derive(Clone, Debug)]
struct State<H> {
    current_bytes: i128,
    current_rate: f64,
    delta_stat: f64,
    deltas: Vec<f64>,
    shaper: H,
    load: f64,
    next_rate: f64,
    nrate: usize,
    previous_bytes: i128,
    prev_t: Instant,
    safe_rates: Vec<f64>,
    utilisation: f64,
}

impl<H> State<H> {
    fn new(shaper: H, previous_bytes: i128, safe_rates: Vec<f64>) -> Self {
        State {
            current_bytes: 0,
            current_rate: 0.0,
            delta_stat: 0.0,
            deltas: Vec::new(),
            load: 0.0,
            next_rate: 0.0,
            nrate: 0,
            shaper,
            previous_bytes,
            prev_t: Instant::now(),
            safe_rates,
            utilisation: 0.0,
        }
    }
}

pub struct Ratecontroller<S: InterfaceStatsProvider, T: TrafficControlBackend> {
    config: Config,
    down_direction: StatsDirection,
    owd_baseline: ArcMutex<HashMap<IpAddr, ReflectorStats>>,
    owd_recent: ArcMutex<HashMap<IpAddr, ReflectorStats>>,
    reflectors_lock: ArcRwLock<Vec<IpAddr>>,
    reselect_trigger: Sender<bool>,
    state_dl: State<T::Handle>,
    state_ul: State<T::Handle>,
    stats_provider: S,
    traffic_control: T,
    up_direction: StatsDirection,
    metrics: MetricsSender,
}

impl<S: InterfaceStatsProvider, T: TrafficControlBackend> Ratecontroller<S, T> {
    fn new_with_backends(
        config: Config,
        owd_baseline: ArcMutex<HashMap<IpAddr, ReflectorStats>>,
        owd_recent: ArcMutex<HashMap<IpAddr, ReflectorStats>>,
        reflectors_lock: ArcRwLock<Vec<IpAddr>>,
        reselect_trigger: Sender<bool>,
        down_direction: StatsDirection,
        up_direction: StatsDirection,
        metrics: MetricsSender,
        mut stats_provider: S,
        mut traffic_control: T,
    ) -> anyhow::Result<Self> {
        let dl_shaper = traffic_control.find_shaper(config.download_interface.as_str())?;
        let dl_safe_rates =
            generate_initial_speeds(config.download_base_kbits, config.speed_hist_size);
        let ul_shaper = traffic_control.find_shaper(config.upload_interface.as_str())?;
        let ul_safe_rates =
            generate_initial_speeds(config.upload_base_kbits, config.speed_hist_size);

        let (cur_rx, cur_tx) = get_interface_stats(
            &mut stats_provider,
            &config.download_interface,
            &config.upload_interface,
            down_direction,
            up_direction,
        )?;

        Ok(Self {
            config,
            down_direction,
            owd_baseline,
            owd_recent,
            reflectors_lock,
            reselect_trigger,
            state_dl: State::new(dl_shaper, cur_rx, dl_safe_rates),
            state_ul: State::new(ul_shaper, cur_tx, ul_safe_rates),
            stats_provider,
            traffic_control,
            up_direction,
            metrics,
        })
    }

    fn request_initial_rates(&mut self) -> anyhow::Result<()> {
        // Set rates to 60% of base rate to make sure we start with sane baselines.
        self.state_dl.current_rate = self.config.download_base_kbits * 0.6;
        self.state_ul.current_rate = self.config.upload_base_kbits * 0.6;

        self.traffic_control.set_rate(
            &self.state_dl.shaper,
            self.state_dl.current_rate.round() as u64,
            self.config.dry_run,
        )?;
        self.traffic_control.set_rate(
            &self.state_ul.shaper,
            self.state_ul.current_rate.round() as u64,
            self.config.dry_run,
        )?;

        Ok(())
    }

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

        let now_t = Instant::now();
        let dur = now_t.duration_since(state.prev_t);

        if !state.deltas.is_empty() {
            state.next_rate = state.current_rate;

            state.delta_stat = if state.deltas.len() >= 3 {
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
                    / dur.as_secs_f64();
                state.load = state.utilisation / state.current_rate;

                if state.delta_stat < delay_ms && state.load > self.config.high_load_level {
                    state.safe_rates[state.nrate] = (state.current_rate * state.load).floor();
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
                    match state
                        .safe_rates
                        .get(fastrand::usize(..state.safe_rates.len()))
                    {
                        Some(rnd_rate) => {
                            state.next_rate = rnd_rate.min(0.9 * state.current_rate * state.load);
                        }
                        None => {
                            state.next_rate = 0.9 * state.current_rate * state.load;
                        }
                    }
                }
            }
        }

        state.next_rate = state.next_rate.max(min_rate).floor();
        state.previous_bytes = state.current_bytes;
        state.prev_t = now_t;

        Ok(())
    }

    fn update_deltas(&mut self) -> anyhow::Result<()> {
        let state_dl = &mut self.state_dl;
        let state_ul = &mut self.state_ul;

        state_dl.deltas.clear();
        state_ul.deltas.clear();

        let now_t = Instant::now();
        let owd_baseline = self.owd_baseline.lock_anyhow()?;
        let owd_recent = self.owd_recent.lock_anyhow()?;
        let reflectors = self.reflectors_lock.read_anyhow()?;

        for reflector in reflectors.iter() {
            // only consider this data if it's less than 2 * tick_duration seconds old
            if owd_baseline.contains_key(reflector)
                && owd_recent.contains_key(reflector)
                && now_t
                    .duration_since(owd_recent[reflector].last_receive_time_s)
                    .as_secs_f64()
                    < self.config.tick_interval * 2.0
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
        state_dl.deltas.sort_by(|a, b| a.total_cmp(b));
        state_ul.deltas.sort_by(|a, b| a.total_cmp(b));

        if state_dl.deltas.len() < 5 || state_ul.deltas.len() < 5 {
            // trigger reselection
            warn!(
                "Not enough delta values (D: {}, U: {}, need 5), triggering reselection",
                state_dl.deltas.len(),
                state_ul.deltas.len()
            );
            let _ = self.reselect_trigger.send(true);
        }

        Ok(())
    }
}

impl Ratecontroller<PlatformInterfaceStats, PlatformTrafficControl> {
    pub fn new(
        config: Config,
        owd_baseline: ArcMutex<HashMap<IpAddr, ReflectorStats>>,
        owd_recent: ArcMutex<HashMap<IpAddr, ReflectorStats>>,
        reflectors_lock: ArcRwLock<Vec<IpAddr>>,
        reselect_trigger: Sender<bool>,
        down_direction: StatsDirection,
        up_direction: StatsDirection,
        metrics: MetricsSender,
    ) -> anyhow::Result<Self> {
        Self::new_with_backends(
            config,
            owd_baseline,
            owd_recent,
            reflectors_lock,
            reselect_trigger,
            down_direction,
            up_direction,
            metrics,
            interface_stats_provider(),
            traffic_control_backend(),
        )
    }
}

impl<S: InterfaceStatsProvider, T: TrafficControlBackend> Ratecontroller<S, T> {
    pub fn run(&mut self) -> anyhow::Result<()> {
        let sleep_time = Duration::from_secs_f64(self.config.min_change_interval);

        let mut lastchg_t = Instant::now();
        let mut lastdump_t = Instant::now();

        self.request_initial_rates()?;

        let mut speed_hist_fd: Option<File> = None;
        let mut speed_hist_fd_inner: File;
        let mut stats_fd: Option<File> = None;
        let mut stats_fd_inner: File;

        if !self.config.suppress_statistics {
            speed_hist_fd_inner = File::options()
                .create(true)
                .write(true)
                .truncate(true)
                .open(self.config.speed_hist_file.as_str())?;

            speed_hist_fd_inner.write_all("time,counter,upspeed,downspeed\n".as_bytes())?;
            speed_hist_fd_inner.flush()?;

            speed_hist_fd = Some(speed_hist_fd_inner);

            stats_fd_inner = File::options()
                .create(true)
                .write(true)
                .truncate(true)
                .open(self.config.stats_file.as_str())?;

            stats_fd_inner.write_all(
                "times,timens,rxload,txload,deltadelaydown,deltadelayup,dlrate,uprate\n".as_bytes(),
            )?;
            stats_fd_inner.flush()?;

            stats_fd = Some(stats_fd_inner);
        }

        loop {
            if SHUTDOWN.load(Ordering::Relaxed) {
                info!("Rate controller shutting down");
                return Ok(());
            }
            sleep(sleep_time);
            let now_t = Instant::now();

            if now_t.duration_since(lastchg_t).as_secs_f64() > self.config.min_change_interval {
                // if it's been long enough, and the stats indicate needing to change speeds
                // change speeds here

                (self.state_dl.current_bytes, self.state_ul.current_bytes) = get_interface_stats(
                    &mut self.stats_provider,
                    &self.config.download_interface,
                    &self.config.upload_interface,
                    self.down_direction,
                    self.up_direction,
                )?;
                self.update_deltas()?;

                if self.state_dl.deltas.is_empty() || self.state_ul.deltas.is_empty() {
                    warn!("No reflector data available, dropping to minimum rates");
                    self.metrics.send(Metric::Event {
                        name: "reflector_unavailable",
                        reason: "",
                        reflector: None,
                        tags: &[],
                    });
                    self.state_dl.next_rate = self.config.download_min_kbits;
                    self.state_ul.next_rate = self.config.upload_min_kbits;

                    self.traffic_control.set_rate(
                        &self.state_dl.shaper,
                        self.state_dl.next_rate as u64,
                        self.config.dry_run,
                    )?;
                    self.traffic_control.set_rate(
                        &self.state_ul.shaper,
                        self.state_ul.next_rate as u64,
                        self.config.dry_run,
                    )?;

                    self.state_dl.current_rate = self.state_dl.next_rate;
                    self.state_ul.current_rate = self.state_ul.next_rate;
                    continue;
                }

                self.calculate_rate(Direction::Down)?;
                self.calculate_rate(Direction::Up)?;

                if self.state_dl.next_rate != self.state_dl.current_rate
                    || self.state_ul.next_rate != self.state_ul.current_rate
                {
                    info!(
                        "Requesting rates (D/U): {} / {} kbit/s",
                        self.state_dl.next_rate as u64, self.state_ul.next_rate as u64
                    );
                }

                if self.state_dl.next_rate != self.state_dl.current_rate {
                    self.traffic_control.set_rate(
                        &self.state_dl.shaper,
                        self.state_dl.next_rate as u64,
                        self.config.dry_run,
                    )?;
                }

                if self.state_ul.next_rate != self.state_ul.current_rate {
                    self.traffic_control.set_rate(
                        &self.state_ul.shaper,
                        self.state_ul.next_rate as u64,
                        self.config.dry_run,
                    )?;
                }

                self.state_dl.current_rate = self.state_dl.next_rate;
                self.state_ul.current_rate = self.state_ul.next_rate;

                let stats_time = Time::new(ClockId::Realtime);
                debug!(
                    "{},{},{},{},{},{},{},{}",
                    stats_time.secs(),
                    stats_time.nsecs(),
                    self.state_dl.load,
                    self.state_ul.load,
                    self.state_dl.delta_stat,
                    self.state_ul.delta_stat,
                    self.state_dl.current_rate as u64,
                    self.state_ul.current_rate as u64
                );

                self.metrics.send(Metric::Rate {
                    dl_rate: self.state_dl.current_rate,
                    ul_rate: self.state_ul.current_rate,
                    rx_load: self.state_dl.load,
                    tx_load: self.state_ul.load,
                    delta_delay_down: self.state_dl.delta_stat,
                    delta_delay_up: self.state_ul.delta_stat,
                });

                let stats_write_error = stats_fd.as_mut().and_then(|fd| {
                    fd.write_all(
                        format!(
                            "{},{},{},{},{},{},{},{}\n",
                            stats_time.secs(),
                            stats_time.nsecs(),
                            self.state_dl.load,
                            self.state_ul.load,
                            self.state_dl.delta_stat,
                            self.state_ul.delta_stat,
                            self.state_dl.current_rate as u64,
                            self.state_ul.current_rate as u64
                        )
                        .as_bytes(),
                    )
                    .err()
                });
                if let Some(e) = stats_write_error {
                    warn!("Failed to write statistics: {}", e);
                }

                lastchg_t = now_t;
            }

            if let Some(ref mut fd) = speed_hist_fd
                && now_t.duration_since(lastdump_t).as_secs_f64() > 300.0
            {
                for i in 0..self.config.speed_hist_size as usize {
                    let hist_time = Time::new(ClockId::Realtime);
                    if let Err(e) = fd.write_all(
                        format!(
                            "{},{},{},{}\n",
                            hist_time.as_secs_f64(),
                            i,
                            self.state_ul.safe_rates[i],
                            self.state_dl.safe_rates[i]
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
