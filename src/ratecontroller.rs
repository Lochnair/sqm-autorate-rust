use crate::component::{Baseliner, RateAlgorithm, RateContext};
use crate::config::{NetworkConfig, OutputConfig};
use crate::netlink::{Netlink, NetlinkError, Qdisc};
use crate::time::Time;
use log::{debug, info, warn};
use rustix::thread::ClockId;
use std::fs::File;
use std::io::Write;
use std::sync::mpsc::Sender;
use std::sync::{Arc, RwLock};
use std::thread::sleep;
use std::time::{Duration, Instant};
use thiserror::Error;

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

fn read_iface_bytes(
    network: &NetworkConfig,
    dl_dir: StatsDirection,
    ul_dir: StatsDirection,
) -> Result<(i128, i128), RatecontrolError> {
    let (dl_rx, dl_tx) = Netlink::get_interface_stats(network.download_interface.as_str())?;
    let (ul_rx, ul_tx) = Netlink::get_interface_stats(network.upload_interface.as_str())?;

    let dl_bytes = match dl_dir {
        StatsDirection::RX => dl_rx,
        StatsDirection::TX => dl_tx,
    };
    let ul_bytes = match ul_dir {
        StatsDirection::RX => ul_rx,
        StatsDirection::TX => ul_tx,
    };

    Ok((dl_bytes.into(), ul_bytes.into()))
}

/// Algorithm-agnostic rate-control loop.
///
/// Each tick:
/// 1. Read interface byte counters.
/// 2. Query sorted OWD deltas from the shared Baseliner (read-locked).
/// 3. Build a `RateContext` and call `algorithm.calculate()`.
/// 4. Apply the resulting rates via Netlink / TC.
/// 5. Write statistics to CSV if enabled.
pub fn run(
    algorithm: &mut dyn RateAlgorithm,
    baseliner: Arc<RwLock<dyn Baseliner>>,
    network: &NetworkConfig,
    reselect_tx: Sender<bool>,
    dl_qdisc: Qdisc,
    ul_qdisc: Qdisc,
    dl_dir: StatsDirection,
    ul_dir: StatsDirection,
    output: &OutputConfig,
) -> anyhow::Result<()> {
    let sleep_time = Duration::from_secs_f64(algorithm.min_change_interval());
    let tick_interval = algorithm.min_change_interval();

    let (initial_dl, initial_ul) = algorithm.initial_rates();
    let mut current_dl = initial_dl;
    let mut current_ul = initial_ul;

    Netlink::set_qdisc_rate(dl_qdisc, current_dl.round() as u64)?;
    Netlink::set_qdisc_rate(ul_qdisc, current_ul.round() as u64)?;

    let (mut prev_dl_bytes, mut prev_ul_bytes) =
        read_iface_bytes(network, dl_dir, ul_dir).unwrap_or((0, 0));
    let mut prev_t = Instant::now();

    // ── Statistics file setup ─────────────────────────────────────────────────
    let mut stats_fd: Option<File> = None;
    let mut speed_hist_fd: Option<File> = None;
    let mut speed_hist_counter: u64 = 0;

    if !output.suppress_statistics {
        let mut sf = File::options()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&output.stats_file)?;
        sf.write_all(b"times,timens,rxload,txload,deltadelaydown,deltadelayup,dlrate,uprate\n")?;
        sf.flush()?;
        stats_fd = Some(sf);

        let mut hf = File::options()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&output.speed_hist_file)?;
        hf.write_all(b"time,counter,upspeed,downspeed\n")?;
        hf.flush()?;
        speed_hist_fd = Some(hf);
    }

    let mut lastdump_t = Instant::now();

    loop {
        sleep(sleep_time);
        let now_t = Instant::now();

        // ── Read byte counters ────────────────────────────────────────────────
        let (cur_dl_bytes, cur_ul_bytes) = match read_iface_bytes(network, dl_dir, ul_dir) {
            Ok(v) => v,
            Err(e) => {
                warn!("Netlink stats read error: {e} — skipping tick");
                continue;
            }
        };

        if cur_dl_bytes == -1 || cur_ul_bytes == -1 {
            warn!("Netlink stats could not be read — skipping tick");
            continue;
        }

        let elapsed = now_t.duration_since(prev_t).as_secs_f64().max(f64::EPSILON);
        let dl_util = (8.0 / 1000.0) * (cur_dl_bytes - prev_dl_bytes) as f64 / elapsed;
        let ul_util = (8.0 / 1000.0) * (cur_ul_bytes - prev_ul_bytes) as f64 / elapsed;

        // ── Query delay state (read lock — no contention with pinger writes) ─
        let (dl_deltas, ul_deltas) = {
            let b = baseliner.read().expect("baseliner RwLock poisoned");
            let max_age = tick_interval * 2.0;
            (b.dl_deltas(now_t, max_age), b.ul_deltas(now_t, max_age))
        };

        // ── No data: drop to minimum rates ───────────────────────────────────
        if dl_deltas.is_empty() || ul_deltas.is_empty() {
            warn!("No reflector data — dropping to minimum rates");
            let min_dl = network.download_min_kbits();
            let min_ul = network.upload_min_kbits();
            if current_dl != min_dl {
                Netlink::set_qdisc_rate(dl_qdisc, min_dl as u64)?;
                current_dl = min_dl;
            }
            if current_ul != min_ul {
                Netlink::set_qdisc_rate(ul_qdisc, min_ul as u64)?;
                current_ul = min_ul;
            }
            let _ = reselect_tx.send(true);
            prev_dl_bytes = cur_dl_bytes;
            prev_ul_bytes = cur_ul_bytes;
            prev_t = now_t;
            continue;
        }

        // ── Representative delta stats for logging ────────────────────────────
        let dl_delta_stat = if dl_deltas.len() >= 3 {
            dl_deltas[2]
        } else {
            dl_deltas[0]
        };
        let ul_delta_stat = if ul_deltas.len() >= 3 {
            ul_deltas[2]
        } else {
            ul_deltas[0]
        };
        let dl_load = dl_util / current_dl.max(1.0);
        let ul_load = ul_util / current_ul.max(1.0);

        // ── Call algorithm ────────────────────────────────────────────────────
        let ctx = RateContext {
            dl_deltas,
            ul_deltas,
            current_dl_rate: current_dl,
            current_ul_rate: current_ul,
            dl_utilisation: dl_util,
            ul_utilisation: ul_util,
            elapsed_secs: elapsed,
            base_dl_rate: network.download_base_kbits,
            base_ul_rate: network.upload_base_kbits,
            min_dl_rate: network.download_min_kbits(),
            min_ul_rate: network.upload_min_kbits(),
        };

        let result = algorithm.calculate(&ctx);

        if result.trigger_reselect {
            warn!("Algorithm requested reflector reselection");
            let _ = reselect_tx.send(true);
        }

        // ── Apply rates ───────────────────────────────────────────────────────
        if result.dl_rate != current_dl || result.ul_rate != current_ul {
            info!(
                "Rate change: dl {} → {} kbit/s  ul {} → {} kbit/s",
                current_dl as u64, result.dl_rate as u64,
                current_ul as u64, result.ul_rate as u64,
            );
        }
        if result.dl_rate != current_dl {
            Netlink::set_qdisc_rate(dl_qdisc, result.dl_rate as u64)?;
        }
        if result.ul_rate != current_ul {
            Netlink::set_qdisc_rate(ul_qdisc, result.ul_rate as u64)?;
        }
        current_dl = result.dl_rate;
        current_ul = result.ul_rate;

        prev_dl_bytes = cur_dl_bytes;
        prev_ul_bytes = cur_ul_bytes;
        prev_t = now_t;

        // ── Statistics output ─────────────────────────────────────────────────
        let stats_time = Time::new(ClockId::Realtime);
        debug!(
            "{},{},{:.4},{:.4},{:.2},{:.2},{},{}",
            stats_time.secs(),
            stats_time.nsecs(),
            dl_load,
            ul_load,
            dl_delta_stat,
            ul_delta_stat,
            current_dl as u64,
            current_ul as u64,
        );

        if let Some(ref mut fd) = stats_fd {
            if let Err(e) = fd.write_all(
                format!(
                    "{},{},{:.4},{:.4},{:.2},{:.2},{},{}\n",
                    stats_time.secs(),
                    stats_time.nsecs(),
                    dl_load,
                    ul_load,
                    dl_delta_stat,
                    ul_delta_stat,
                    current_dl as u64,
                    current_ul as u64,
                )
                .as_bytes(),
            ) {
                warn!("Failed to write statistics: {e}");
            }
        }

        // Speed history dump every 5 minutes
        if let Some(ref mut fd) = speed_hist_fd {
            if now_t.duration_since(lastdump_t).as_secs_f64() > 300.0 {
                let hist_time = Time::new(ClockId::Realtime);
                if let Err(e) = fd.write_all(
                    format!(
                        "{},{},{},{}\n",
                        hist_time.as_secs_f64(),
                        speed_hist_counter,
                        current_ul as u64,
                        current_dl as u64,
                    )
                    .as_bytes(),
                ) {
                    warn!("Failed to write speed history: {e}");
                }
                speed_hist_counter += 1;
                lastdump_t = now_t;
            }
        }
    }
}
