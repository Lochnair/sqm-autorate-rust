// SPDX-FileCopyrightText: 2022-Present Charles Corrigan mailto:chas-iot@runegate.org (github @chas-iot)
// SPDX-FileCopyrightText: 2022-Present Daniel Lakeland mailto:dlakelan@street-artists.org (github @dlakelan)
// SPDX-FileCopyrightText: 2022-Present Mark Baker mailto:mark@vpost.net (github @Fail-Safe)
// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use crate::SHUTDOWN;
use crate::baseliner::ControlSnapshot;
use crate::metrics::{Metric, MetricsSender};
use crate::platform::{
    InterfaceStatsProvider, PlatformInterfaceStats, PlatformTrafficControl, TrafficControlBackend,
    interface_stats_provider, traffic_control_backend,
};
use crate::settings::Settings;
use crate::time::Time;
use crate::util::{ArcRwLock, RwLockExt};
use flume::{Receiver, Sender};
use log::{debug, info, warn};
use rustix::time::ClockId;
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

fn generate_initial_speeds(rng: &mut fastrand::Rng, base_speed: f64, size: u32) -> Vec<f64> {
    let mut rates = Vec::new();

    for _ in 0..size {
        rates.push((rng.f64() * 0.2 + 0.75) * base_speed);
    }

    rates
}

fn get_interface_stats<S: InterfaceStatsProvider>(
    stats_provider: &mut S,
    upload_interface: &str,
) -> Result<(i128, i128), S::Error> {
    let stats = stats_provider.read_stats(upload_interface)?;

    Ok((stats.rx_bytes.into(), stats.tx_bytes.into()))
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

fn drain_latest_snapshot(
    snapshot_rx: &Receiver<ControlSnapshot>,
    latest: &mut Option<ControlSnapshot>,
) {
    while let Ok(snapshot) = snapshot_rx.try_recv() {
        if latest
            .as_ref()
            .is_none_or(|current| snapshot.generated_at >= current.generated_at)
        {
            *latest = Some(snapshot);
        }
    }
}

fn deltas_from_snapshot(
    snapshot: Option<&ControlSnapshot>,
    reflectors: &[IpAddr],
    now: Instant,
    tick_interval: f64,
) -> (Vec<f64>, Vec<f64>) {
    let mut down = Vec::new();
    let mut up = Vec::new();

    let Some(snapshot) = snapshot else {
        return (down, up);
    };

    for reflector in reflectors {
        let Some(state) = snapshot.reflectors.get(reflector) else {
            continue;
        };

        if now.duration_since(state.last_receive_at).as_secs_f64() < tick_interval * 2.0 {
            let down_delta = state.recent.down - state.baseline.down;
            let up_delta = state.recent.up - state.baseline.up;
            down.push(down_delta);
            up.push(up_delta);

            debug!(
                "Reflector: {} down_delay: {} up_delay: {}",
                reflector, down_delta, up_delta
            );
        }
    }

    down.sort_by(|a, b| a.total_cmp(b));
    up.sort_by(|a, b| a.total_cmp(b));

    (down, up)
}

pub struct Ratecontroller<S: InterfaceStatsProvider, T: TrafficControlBackend> {
    settings: Settings,
    control_snapshot: Option<ControlSnapshot>,
    control_snapshot_rx: Receiver<ControlSnapshot>,
    reflectors_lock: ArcRwLock<Vec<IpAddr>>,
    reselect_trigger: Sender<bool>,
    state_dl: State<T::Handle>,
    state_ul: State<T::Handle>,
    rng: fastrand::Rng,
    stats_provider: S,
    traffic_control: T,
    metrics: MetricsSender,
}

impl<S: InterfaceStatsProvider, T: TrafficControlBackend> Ratecontroller<S, T> {
    fn new_with_backends(
        settings: Settings,
        control_snapshot_rx: Receiver<ControlSnapshot>,
        reflectors_lock: ArcRwLock<Vec<IpAddr>>,
        reselect_trigger: Sender<bool>,
        metrics: MetricsSender,
        mut stats_provider: S,
        mut traffic_control: T,
        mut rng: fastrand::Rng,
    ) -> anyhow::Result<Self> {
        let dl_shaper =
            traffic_control.find_shaper(settings.network.download_interface.as_str())?;
        let dl_safe_rates = generate_initial_speeds(
            &mut rng,
            settings.network.download_base_kbits,
            settings.advanced_settings.speed_hist_size,
        );
        let ul_shaper = traffic_control.find_shaper(settings.network.upload_interface.as_str())?;
        let ul_safe_rates = generate_initial_speeds(
            &mut rng,
            settings.network.upload_base_kbits,
            settings.advanced_settings.speed_hist_size,
        );

        let (cur_rx, cur_tx) =
            get_interface_stats(&mut stats_provider, &settings.network.upload_interface)?;

        Ok(Self {
            settings,
            control_snapshot: None,
            control_snapshot_rx,
            reflectors_lock,
            reselect_trigger,
            state_dl: State::new(dl_shaper, cur_rx, dl_safe_rates),
            state_ul: State::new(ul_shaper, cur_tx, ul_safe_rates),
            rng,
            stats_provider,
            traffic_control,
            metrics,
        })
    }

    fn request_initial_rates(&mut self) -> anyhow::Result<()> {
        // Set rates to 60% of base rate to make sure we start with sane baselines.
        self.state_dl.current_rate = self.settings.network.download_base_kbits * 0.6;
        self.state_ul.current_rate = self.settings.network.upload_base_kbits * 0.6;

        self.traffic_control.set_rate(
            &self.state_dl.shaper,
            self.state_dl.current_rate.round() as u64,
            self.settings.advanced_settings.dry_run,
        )?;
        self.traffic_control.set_rate(
            &self.state_ul.shaper,
            self.state_ul.current_rate.round() as u64,
            self.settings.advanced_settings.dry_run,
        )?;

        Ok(())
    }

    fn calculate_rate(&mut self, direction: Direction) -> anyhow::Result<()> {
        let (base_rate, delay_ms, min_rate, state) = if direction == Direction::Down {
            (
                self.settings.network.download_base_kbits,
                self.settings.advanced_settings.download_delay_ms,
                self.settings.network.download_min_kbits(),
                &mut self.state_dl,
            )
        } else {
            (
                self.settings.network.upload_base_kbits,
                self.settings.advanced_settings.upload_delay_ms,
                self.settings.network.upload_min_kbits(),
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

                if state.delta_stat < delay_ms
                    && state.load > self.settings.advanced_settings.high_load_level
                {
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
                    state.nrate %= self.settings.advanced_settings.speed_hist_size as usize;
                }

                if state.delta_stat > delay_ms {
                    match state
                        .safe_rates
                        .get(self.rng.usize(..state.safe_rates.len()))
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

    pub fn run(&mut self) -> anyhow::Result<()> {
        let sleep_time =
            Duration::from_secs_f64(self.settings.advanced_settings.min_change_interval);

        let mut lastchg_t = Instant::now();
        let mut lastdump_t = Instant::now();

        self.request_initial_rates()?;

        let mut speed_hist_fd: Option<File> = None;
        let mut speed_hist_fd_inner: File;
        let mut stats_fd: Option<File> = None;
        let mut stats_fd_inner: File;

        if !self.settings.output.suppress_statistics {
            speed_hist_fd_inner = File::options()
                .create(true)
                .write(true)
                .truncate(true)
                .open(self.settings.output.speed_hist_file.as_str())?;

            speed_hist_fd_inner.write_all("time,counter,upspeed,downspeed\n".as_bytes())?;
            speed_hist_fd_inner.flush()?;

            speed_hist_fd = Some(speed_hist_fd_inner);

            stats_fd_inner = File::options()
                .create(true)
                .write(true)
                .truncate(true)
                .open(self.settings.output.stats_file.as_str())?;

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

            if now_t.duration_since(lastchg_t).as_secs_f64()
                > self.settings.advanced_settings.min_change_interval
            {
                // if it's been long enough, and the stats indicate needing to change speeds
                // change speeds here

                (self.state_dl.current_bytes, self.state_ul.current_bytes) = get_interface_stats(
                    &mut self.stats_provider,
                    &self.settings.network.upload_interface,
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
                    self.state_dl.next_rate = self.settings.network.download_min_kbits();
                    self.state_ul.next_rate = self.settings.network.upload_min_kbits();

                    self.traffic_control.set_rate(
                        &self.state_dl.shaper,
                        self.state_dl.next_rate as u64,
                        self.settings.advanced_settings.dry_run,
                    )?;
                    self.traffic_control.set_rate(
                        &self.state_ul.shaper,
                        self.state_ul.next_rate as u64,
                        self.settings.advanced_settings.dry_run,
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
                        self.settings.advanced_settings.dry_run,
                    )?;
                }

                if self.state_ul.next_rate != self.state_ul.current_rate {
                    self.traffic_control.set_rate(
                        &self.state_ul.shaper,
                        self.state_ul.next_rate as u64,
                        self.settings.advanced_settings.dry_run,
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
                for i in 0..self.settings.advanced_settings.speed_hist_size as usize {
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

    fn update_deltas(&mut self) -> anyhow::Result<()> {
        drain_latest_snapshot(&self.control_snapshot_rx, &mut self.control_snapshot);
        let now_t = Instant::now();
        let reflectors = self.reflectors_lock.read_anyhow()?;
        let (down_deltas, up_deltas) = deltas_from_snapshot(
            self.control_snapshot.as_ref(),
            &reflectors,
            now_t,
            self.settings.advanced_settings.tick_interval,
        );
        self.state_dl.deltas = down_deltas;
        self.state_ul.deltas = up_deltas;

        if self.state_dl.deltas.len() < 5 || self.state_ul.deltas.len() < 5 {
            // trigger reselection
            warn!(
                "Not enough delta values (D: {}, U: {}, need 5), triggering reselection",
                self.state_dl.deltas.len(),
                self.state_ul.deltas.len()
            );
            let _ = self.reselect_trigger.try_send(true);
        }

        Ok(())
    }
}

impl Ratecontroller<PlatformInterfaceStats, PlatformTrafficControl> {
    pub fn new(
        settings: Settings,
        control_snapshot_rx: Receiver<ControlSnapshot>,
        reflectors_lock: ArcRwLock<Vec<IpAddr>>,
        reselect_trigger: Sender<bool>,
        metrics: MetricsSender,
    ) -> anyhow::Result<Self> {
        Self::new_with_backends(
            settings,
            control_snapshot_rx,
            reflectors_lock,
            reselect_trigger,
            metrics,
            interface_stats_provider(),
            traffic_control_backend(),
            fastrand::Rng::new(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseliner::{EwmaStats, ReflectorState};
    use std::collections::HashMap;

    #[test]
    fn fixed_seed_reproduces_initial_safe_rates() {
        let mut first = fastrand::Rng::with_seed(42);
        let mut second = fastrand::Rng::with_seed(42);

        let first_rates = generate_initial_speeds(&mut first, 100_000.0, 100);
        let second_rates = generate_initial_speeds(&mut second, 100_000.0, 100);

        assert_eq!(first_rates, second_rates);
    }

    fn reflector_state(
        baseline_down: f64,
        baseline_up: f64,
        recent_down: f64,
        recent_up: f64,
        last_receive_at: Instant,
    ) -> ReflectorState {
        ReflectorState {
            baseline: EwmaStats {
                down: baseline_down,
                up: baseline_up,
            },
            recent: EwmaStats {
                down: recent_down,
                up: recent_up,
            },
            last_receive_at,
        }
    }

    fn snapshot(generated_at: Instant, marker: f64) -> ControlSnapshot {
        let reflector = "192.0.2.1".parse().unwrap();
        ControlSnapshot {
            generated_at,
            reflectors: HashMap::from([(
                reflector,
                reflector_state(marker, marker, marker, marker, generated_at),
            )]),
        }
    }

    #[test]
    fn drains_queued_snapshots_and_keeps_the_newest() {
        let start = Instant::now();
        let newest_at = start + Duration::from_secs(2);
        let (tx, rx) = flume::unbounded();
        tx.send(snapshot(start + Duration::from_secs(1), 1.0))
            .unwrap();
        tx.send(snapshot(newest_at, 2.0)).unwrap();
        let mut latest = None;

        drain_latest_snapshot(&rx, &mut latest);

        let latest = latest.unwrap();
        assert!(rx.is_empty());
        assert_eq!(latest.generated_at, newest_at);
        assert_eq!(
            latest.reflectors[&"192.0.2.1".parse().unwrap()].recent.down,
            2.0
        );
    }

    #[test]
    fn calculates_sorted_deltas_from_active_fresh_snapshot_state() {
        let now = Instant::now();
        let first: IpAddr = "192.0.2.1".parse().unwrap();
        let second: IpAddr = "192.0.2.2".parse().unwrap();
        let stale: IpAddr = "192.0.2.3".parse().unwrap();
        let inactive: IpAddr = "192.0.2.4".parse().unwrap();
        let snapshot = ControlSnapshot {
            generated_at: now,
            reflectors: HashMap::from([
                (first, reflector_state(10.0, 20.0, 14.0, 28.0, now)),
                (second, reflector_state(10.0, 20.0, 11.0, 22.0, now)),
                (
                    stale,
                    reflector_state(10.0, 20.0, 110.0, 220.0, now - Duration::from_secs(3)),
                ),
                (inactive, reflector_state(10.0, 20.0, 60.0, 80.0, now)),
            ]),
        };

        let (down, up) = deltas_from_snapshot(Some(&snapshot), &[first, second, stale], now, 1.0);

        assert_eq!(down, vec![1.0, 4.0]);
        assert_eq!(up, vec![2.0, 8.0]);
    }
}
