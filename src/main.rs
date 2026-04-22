extern crate core;

mod baseliner;
mod config;
mod log;
mod metrics;
mod netlink;
mod pinger;
mod pinger_icmp;
mod pinger_icmp_ts;
mod ratecontroller;
mod reflector_selector;
mod time;
mod util;

use crate::baseliner::{Baseliner, ReflectorStats};
use crate::metrics::{Metric, Metrics, MetricsSender};
use ::log::{debug, info};
use flume::RecvTimeoutError;
use std::collections::HashMap;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::sleep;
use std::time::Duration;
use std::time::Instant;
use std::{process, thread};

use crate::config::{Config, MeasurementType};
use crate::netlink::Netlink;
use crate::pinger::{InFlightProbeCache, PingListener, PingSender};
use crate::pinger_icmp::{PingerICMPEchoListener, PingerICMPEchoSender};
use crate::pinger_icmp_ts::{PingerICMPTimestampListener, PingerICMPTimestampSender};
use crate::ratecontroller::{Ratecontroller, StatsDirection};
use crate::reflector_selector::ReflectorSelector;

pub static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn signal_handler(_: libc::c_int) {
    println!("Signal received, shutting down sqm-autorate-rust");
    SHUTDOWN.store(true, Ordering::Relaxed);
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> anyhow::Result<()> {
    println!("Starting sqm-autorate-rust version {}", VERSION);

    unsafe {
        libc::signal(
            libc::SIGINT,
            signal_handler as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            signal_handler as *const () as libc::sighandler_t,
        );
    }

    let config = Config::new()?;
    log::init(config.log_level)?;
    if config.dry_run {
        info!("*** MONITORING MODE ACTIVE — qdisc rates will NOT be changed ***");
    }
    let mut reflectors = config.load_reflectors()?;
    let start_t = Instant::now();

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

    match reflector_pool_size > config.num_reflectors as usize {
        true => {
            let mut peers = reflector_peers_lock.write().unwrap();
            peers.extend_from_slice(&default_reflectors);
            reflector_pool.append(reflectors.as_mut());
        }
        false => {
            let mut peers = reflector_peers_lock.write().unwrap();
            peers.extend_from_slice(&default_reflectors);
        }
    }

    let (baseliner_stats_tx, baseliner_stats_rx) = flume::unbounded();
    let (error_tx, error_rx) = flume::unbounded::<anyhow::Error>();
    let (reselect_tx, reselect_rx) = flume::unbounded();

    let dropped = Arc::new(AtomicU32::new(0));

    let (metrics_tx, metrics_thread_handle) = if config.observability_enabled {
        let (tx, rx) = flume::bounded(1000);
        let metrics = Metrics {
            config: config.clone(),
            metrics_rx: rx,
            metrics_dropped: Arc::clone(&dropped),
        };
        let err_tx = error_tx.clone();
        let handle = thread::Builder::new()
            .name("metrics".to_string())
            .spawn(move || {
                if let Err(e) = metrics.run() {
                    let _ = err_tx.send(e);
                }
            })?;
        (Some(tx), Some(handle))
    } else {
        (None, None)
    };

    let make_sender = |enabled: bool| -> MetricsSender {
        metrics_tx
            .as_ref()
            .filter(|_| enabled)
            .map(|tx| MetricsSender::new(tx.clone(), Arc::clone(&dropped)))
            .unwrap_or_else(MetricsSender::disabled)
    };

    let ping_metrics = make_sender(config.observability_export_ping_metrics);
    let baseline_metrics = make_sender(config.observability_export_baseline_metrics);
    let event_metrics = make_sender(config.observability_export_events);
    let rate_metrics = make_sender(config.observability_export_rate_metrics);

    event_metrics.send(Metric::Event {
        name: "starting",
        reason: "",
        reflector: None,
        tags: if config.dry_run {
            &[("dry_run", "true")]
        } else {
            &[]
        },
    });

    let (mut ping_listener, mut ping_sender) = match config.measurement_type {
        MeasurementType::Icmp => (
            Box::new(PingerICMPEchoListener {}) as Box<dyn PingListener + Send>,
            Box::new(PingerICMPEchoSender {}) as Box<dyn PingSender + Send>,
        ),
        MeasurementType::IcmpTimestamps => (
            Box::new(PingerICMPTimestampListener {}) as Box<dyn PingListener + Send>,
            Box::new(PingerICMPTimestampSender {}) as Box<dyn PingSender + Send>,
        ),
        MeasurementType::Ntp | MeasurementType::TcpTimestamps => {
            todo!()
        }
    };

    let baseliner = Baseliner {
        config: config.clone(),
        owd_baseline: owd_baseline.clone(),
        owd_recent: owd_recent.clone(),
        reselect_trigger: reselect_tx.clone(),
        start_time: start_t,
        stats_rx: baseliner_stats_rx,
        baseline_metrics,
        event_metrics: event_metrics.clone(),
    };

    let down_qdisc = Netlink::qdisc_from_ifname(config.download_interface.as_str())?;
    let up_qdisc = Netlink::qdisc_from_ifname(config.upload_interface.as_str())?;

    /* Set initial TC values to minimum
     * so there should be no initial bufferbloat to
     * fool the baseliner
     */
    info!(
        "Setting shaper rates to minimum (D/L): {} / {}",
        config.download_min_kbits, config.upload_min_kbits
    );
    Netlink::set_qdisc_rate(down_qdisc, config.download_min_kbits as u64, config.dry_run)?;
    Netlink::set_qdisc_rate(up_qdisc, config.upload_min_kbits as u64, config.dry_run)?;

    // Sleep for a few seconds to give the shaper a chance
    // to control the queue if load is heavy
    let settle_sleep_time = Duration::new(2, 0);
    info!(
        "Sleeping for {} to give the shaper a chance to get in control if there's bloat",
        settle_sleep_time.as_secs_f64()
    );
    sleep(settle_sleep_time);

    let err_tx = error_tx.clone();
    let reflector_peers_lock_clone = reflector_peers_lock.clone();
    thread::Builder::new()
        .name("receiver".to_string())
        .spawn(move || {
            if let Err(e) = ping_listener.listen(
                id,
                config.measurement_type,
                reflector_peers_lock_clone,
                baseliner_stats_tx,
                ping_metrics,
            ) {
                let _ = err_tx.send(e);
            }
        })?;

    let err_tx = error_tx.clone();
    thread::Builder::new()
        .name("baseliner".to_string())
        .spawn(move || {
            if let Err(e) = baseliner.run() {
                let _ = err_tx.send(e);
            }
        })?;

    let err_tx = error_tx.clone();
    let reflector_peers_lock_clone = reflector_peers_lock.clone();
    thread::Builder::new()
        .name("sender".to_string())
        .spawn(move || {
            if let Err(e) = ping_sender.send(
                id,
                config.measurement_type,
                reflector_peers_lock_clone,
                config.tick_interval,
            ) {
                let _ = err_tx.send(e);
            }
        })?;

    let main_event_metrics = event_metrics.clone();

    if reflector_pool_size > config.num_reflectors as usize {
        let reflector_selector = ReflectorSelector {
            config: config.clone(),
            owd_recent: owd_recent.clone(),
            reflector_peers_lock: reflector_peers_lock.clone(),
            reflector_pool,
            trigger_channel: reselect_rx,
            metrics: event_metrics,
        };
        let err_tx = error_tx.clone();
        thread::Builder::new()
            .name("reselection".to_string())
            .spawn(move || {
                if let Err(e) = reflector_selector.run() {
                    let _ = err_tx.send(e);
                }
            })?;
    }

    // Sleep 10 seconds before we start adjusting speeds
    sleep(Duration::new(10, 0));

    let dl_direction = if config.download_interface.starts_with("ifb")
        || config.download_interface.starts_with("veth")
    {
        StatsDirection::TX
    } else {
        StatsDirection::RX
    };
    let ul_direction = if config.upload_interface.starts_with("ifb")
        || config.upload_interface.starts_with("veth")
    {
        StatsDirection::RX
    } else {
        StatsDirection::TX
    };

    let mut ratecontroller = Ratecontroller::new(
        config.clone(),
        owd_baseline,
        owd_recent,
        reflector_peers_lock,
        reselect_tx,
        dl_direction,
        ul_direction,
        rate_metrics,
    )?;

    debug!(
        "Download direction: {}:{:?}",
        config.download_interface, dl_direction
    );

    debug!(
        "Upload direction: {}:{:?}",
        config.upload_interface, ul_direction
    );

    let err_tx = error_tx.clone();
    thread::Builder::new()
        .name("ratecontroller".to_string())
        .spawn(move || {
            if let Err(e) = ratecontroller.run() {
                let _ = err_tx.send(e);
            }
        })?;

    // Drop original sender so the channel disconnects if all threads exit cleanly
    drop(error_tx);

    let result = loop {
        match error_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(e) => break Err(anyhow::anyhow!("thread exited with error: {e}")),
            Err(RecvTimeoutError::Disconnected) => break Ok(()),
            Err(RecvTimeoutError::Timeout) => {
                if SHUTDOWN.load(Ordering::Relaxed) {
                    info!("Received shutdown signal");
                    break Ok(());
                }
            }
        }
    };

    let stopping_reason = if result.is_err() { "error" } else { "signal" };
    main_event_metrics.send(Metric::Event {
        name: "stopping",
        reason: stopping_reason,
        reflector: None,
        tags: &[],
    });

    // Drop all MetricsSender instances and the raw tx so the metrics channel
    // disconnects once all threads also drop their copies, allowing the
    // metrics thread to drain and exit cleanly.
    drop(main_event_metrics);
    drop(metrics_tx);
    if let Some(handle) = metrics_thread_handle {
        let _ = handle.join();
    }

    info!(
        "Restoring base shaper rates (D/L): {} / {}",
        config.download_base_kbits, config.upload_base_kbits
    );
    let _ = Netlink::set_qdisc_rate(
        down_qdisc,
        config.download_base_kbits as u64,
        config.dry_run,
    );
    let _ = Netlink::set_qdisc_rate(up_qdisc, config.upload_base_kbits as u64, config.dry_run);

    result
}
