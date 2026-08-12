// SPDX-FileCopyrightText: 2022-Present Charles Corrigan mailto:chas-iot@runegate.org (github @chas-iot)
// SPDX-FileCopyrightText: 2022-Present Daniel Lakeland mailto:dlakelan@street-artists.org (github @dlakelan)
// SPDX-FileCopyrightText: 2022-Present Mark Baker mailto:mark@vpost.net (github @Fail-Safe)
// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

extern crate core;

mod baseliner;
mod log;
mod metrics;
mod pinger;
mod pinger_icmp;
mod pinger_icmp_ts;
mod platform;
mod ratecontroller;
mod reflector_selector;
mod settings;
mod time;
mod util;

use crate::baseliner::Baseliner;
use crate::metrics::{Metric, Metrics, MetricsSender};
use crate::pinger::{InFlightProbeCache, PingListener, PingSender};
use crate::pinger_icmp::{PingerICMPEchoListener, PingerICMPEchoSender};
use crate::pinger_icmp_ts::{PingerICMPTimestampListener, PingerICMPTimestampSender};
use crate::platform::{TrafficControlBackend, traffic_control_backend, warn_platform_limitations};
use crate::ratecontroller::Ratecontroller;
use crate::reflector_selector::ReflectorSelector;
use crate::settings::MeasurementType;
use crate::settings::Settings;
use ::log::{debug, info, warn};
use flume::RecvTimeoutError;
use std::collections::HashMap;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::sleep;
use std::time::{Duration, Instant};
use std::{process, thread};

struct ReflectorSetup {
    peers: Arc<RwLock<Vec<IpAddr>>>,
    pool: Vec<IpAddr>,
    reselection_enabled: bool,
    active_count: usize,
}

type PingListenerBox = Box<dyn PingListener + Send>;
type PingSenderBox = Box<dyn PingSender + Send>;

pub static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn signal_handler(_: libc::c_int) {
    SHUTDOWN.store(true, Ordering::Relaxed);
}

const VERSION: &str = env!("CARGO_PKG_VERSION");
const RESELECTION_CANDIDATE_BURST: usize = 20;
const INFLIGHT_CAPACITY_DUPLICATE_FACTOR: usize = 2;
const INFLIGHT_CAPACITY_MIN: usize = 256;

fn compute_inflight_probe_capacity(
    settings: &Settings,
    active_reflector_count: usize,
    reselection_enabled: bool,
) -> usize {
    let tick_interval_ms = (settings.advanced_settings.tick_interval * 1000.0).max(1.0);
    let expected_path_delay_ms = (settings.advanced_settings.download_delay_ms
        + settings.advanced_settings.upload_delay_ms)
        .max(10.0);
    // Keep enough room for severe queueing events while still adapting to configured delay budgets.
    let max_rtt_ms = (expected_path_delay_ms * 20.0).clamp(1000.0, 10000.0);

    let burst_reflector_count = if reselection_enabled {
        active_reflector_count + RESELECTION_CANDIDATE_BURST
    } else {
        active_reflector_count
    };
    let probes_per_reflector = (max_rtt_ms / tick_interval_ms).ceil() as usize;

    (burst_reflector_count
        .saturating_mul(probes_per_reflector)
        .saturating_mul(INFLIGHT_CAPACITY_DUPLICATE_FACTOR))
    .max(INFLIGHT_CAPACITY_MIN)
}

fn create_pinger(measurement_type: MeasurementType) -> (PingListenerBox, PingSenderBox) {
    match measurement_type {
        MeasurementType::Icmp => (
            Box::new(PingerICMPEchoListener {}),
            Box::new(PingerICMPEchoSender {}),
        ),
        MeasurementType::IcmpTimestamps => (
            Box::new(PingerICMPTimestampListener {}),
            Box::new(PingerICMPTimestampSender {}),
        ),
    }
}

fn initialize_shaper<T: TrafficControlBackend>(
    settings: &Settings,
    traffic_control: &mut T,
) -> anyhow::Result<(T::Handle, T::Handle)> {
    initialize_shaper_with_settle_time(settings, traffic_control, Duration::from_secs(2))
}

fn initialize_shaper_with_settle_time<T: TrafficControlBackend>(
    settings: &Settings,
    traffic_control: &mut T,
    settle_time: Duration,
) -> anyhow::Result<(T::Handle, T::Handle)> {
    let down = traffic_control.find_shaper(&settings.network.download_interface)?;
    let up = traffic_control.find_shaper(&settings.network.upload_interface)?;

    info!(
        "Requesting minimum shaper rates (D/U): {} / {}",
        settings.network.download_min_kbits(),
        settings.network.upload_min_kbits(),
    );

    traffic_control.set_rate(
        &down,
        settings.network.download_min_kbits() as u64,
        settings.advanced_settings.dry_run,
    )?;
    traffic_control.set_rate(
        &up,
        settings.network.upload_min_kbits() as u64,
        settings.advanced_settings.dry_run,
    )?;

    info!(
        "Sleeping for {} seconds to give the shaper a chance to control existing bloat",
        settle_time.as_secs_f64(),
    );

    sleep(settle_time);

    Ok((down, up))
}

fn install_signal_handlers() {
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
}

fn probe_identifier() -> u16 {
    // The ICMP identifier is only two bytes.
    (process::id() & 0xffff) as u16
}

fn restore_shaper<T: TrafficControlBackend>(
    settings: &Settings,
    traffic_control: &mut T,
    down: &T::Handle,
    up: &T::Handle,
) {
    info!(
        "Requesting restoration to base shaper rates (D/U): {} / {}",
        settings.network.download_base_kbits, settings.network.upload_base_kbits,
    );

    if let Err(error) = traffic_control.set_rate(
        down,
        settings.network.download_base_kbits as u64,
        settings.advanced_settings.dry_run,
    ) {
        warn!("Failed to restore download shaper rate: {error}");
    }

    if let Err(error) = traffic_control.set_rate(
        up,
        settings.network.upload_base_kbits as u64,
        settings.advanced_settings.dry_run,
    ) {
        warn!("Failed to restore upload shaper rate: {error}");
    }
}

fn setup_reflectors(settings: &Settings) -> anyhow::Result<ReflectorSetup> {
    let configured = settings.load_reflectors()?;
    let configured_count = configured.len();

    let default_reflectors = [
        IpAddr::from_str("9.9.9.9")?,
        IpAddr::from_str("8.238.120.14")?,
        IpAddr::from_str("74.82.42.42")?,
        IpAddr::from_str("194.242.2.2")?,
        IpAddr::from_str("208.67.222.222")?,
        IpAddr::from_str("94.140.14.14")?,
    ];

    let reselection_enabled = configured_count > settings.advanced_settings.num_reflectors as usize;
    let pool = if reselection_enabled {
        configured
    } else {
        Vec::new()
    };

    Ok(ReflectorSetup {
        peers: Arc::new(RwLock::new(default_reflectors.to_vec())),
        pool,
        reselection_enabled,
        active_count: default_reflectors
            .len()
            .max(settings.advanced_settings.num_reflectors as usize),
    })
}

fn wait_for_exit(error_rx: &flume::Receiver<anyhow::Error>) -> anyhow::Result<()> {
    loop {
        match error_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(error) => {
                return Err(anyhow::anyhow!("thread exited with error: {error}"));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Ok(());
            }
            Err(RecvTimeoutError::Timeout) => {
                if SHUTDOWN.load(Ordering::Relaxed) {
                    info!("Received shutdown signal");
                    return Ok(());
                }
            }
        }
    }
}

fn run(settings: &Settings) -> anyhow::Result<()> {
    if settings.advanced_settings.dry_run {
        info!("*** MONITORING MODE ACTIVE — qdisc rates will NOT be changed ***");
    }

    let start_time = Instant::now();
    let probe_id = probe_identifier();

    let ReflectorSetup {
        peers: reflector_peers,
        pool: reflector_pool,
        reselection_enabled,
        active_count: active_reflector_count,
    } = setup_reflectors(settings)?;

    let (baseliner_stats_tx, baseliner_stats_rx) = flume::unbounded();
    let (control_snapshot_tx, control_snapshot_rx) = flume::unbounded();
    let (selection_snapshot_tx, selection_snapshot_rx) = if reselection_enabled {
        let (tx, rx) = flume::unbounded();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let (error_tx, error_rx) = flume::unbounded::<anyhow::Error>();
    let (reselect_tx, reselect_rx) = flume::bounded(1);

    let dropped = Arc::new(AtomicU32::new(0));

    let (metrics_tx, metrics_thread_handle) = if settings.observability.enabled {
        let (tx, rx) = flume::bounded(1000);
        let metrics = Metrics {
            settings: settings.clone(),
            metrics_rx: rx,
            metrics_dropped: Arc::clone(&dropped),
        };
        let err_tx = error_tx.clone();
        let handle = thread::Builder::new()
            .name("metrics".to_string())
            .spawn(move || {
                if let Err(error) = metrics.run() {
                    let _ = err_tx.send(error);
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

    let ping_metrics = make_sender(settings.observability.export_ping_metrics);
    let baseline_metrics = make_sender(settings.observability.export_baseline_metrics);
    let event_metrics = make_sender(settings.observability.export_events);
    let rate_metrics = make_sender(settings.observability.export_rate_metrics);

    event_metrics.send(Metric::Event {
        name: "starting",
        reason: "",
        reflector: None,
        tags: if settings.advanced_settings.dry_run {
            &[("dry_run", "true")]
        } else {
            &[]
        },
    });

    let (mut ping_listener, mut ping_sender) =
        create_pinger(settings.advanced_settings.measurement_type);

    let inflight_probe_capacity =
        compute_inflight_probe_capacity(settings, active_reflector_count, reselection_enabled);
    info!(
        "In-flight probe cache capacity: {} (active_reflectors={}, reselection_enabled={}, tick_interval_s={})",
        inflight_probe_capacity,
        active_reflector_count,
        reselection_enabled,
        settings.advanced_settings.tick_interval
    );

    let inflight: InFlightProbeCache =
        Arc::new(Mutex::new(HashMap::with_capacity(inflight_probe_capacity)));

    let baseliner = Baseliner {
        settings: settings.clone(),
        reselect_trigger: reselect_tx.clone(),
        start_time,
        stats_rx: baseliner_stats_rx,
        control_tx: control_snapshot_tx,
        selection_tx: selection_snapshot_tx,
        baseline_metrics,
        event_metrics: event_metrics.clone(),
    };

    let mut main_traffic_control = traffic_control_backend();
    let (down_shaper, up_shaper) = initialize_shaper(settings, &mut main_traffic_control)?;

    let err_tx = error_tx.clone();
    let listener_peers = Arc::clone(&reflector_peers);
    let listener_inflight = Arc::clone(&inflight);
    let measurement_type = settings.advanced_settings.measurement_type;
    thread::Builder::new()
        .name("receiver".to_string())
        .spawn(move || {
            if let Err(error) = ping_listener.listen(
                probe_id,
                measurement_type,
                listener_peers,
                listener_inflight,
                baseliner_stats_tx,
                ping_metrics,
            ) {
                let _ = err_tx.send(error);
            }
        })?;

    let err_tx = error_tx.clone();
    thread::Builder::new()
        .name("baseliner".to_string())
        .spawn(move || {
            if let Err(error) = baseliner.run() {
                let _ = err_tx.send(error);
            }
        })?;

    let err_tx = error_tx.clone();
    let sender_peers = Arc::clone(&reflector_peers);
    let sender_inflight = Arc::clone(&inflight);
    let measurement_type = settings.advanced_settings.measurement_type;
    let tick_interval = settings.advanced_settings.tick_interval;
    thread::Builder::new()
        .name("sender".to_string())
        .spawn(move || {
            if let Err(error) = ping_sender.send(
                probe_id,
                measurement_type,
                sender_peers,
                sender_inflight,
                tick_interval,
            ) {
                let _ = err_tx.send(error);
            }
        })?;

    let main_event_metrics = event_metrics.clone();

    if reselection_enabled {
        let reflector_selector = ReflectorSelector {
            settings: settings.clone(),
            snapshot_rx: selection_snapshot_rx
                .expect("reselection snapshot receiver must exist when reselection is enabled"),
            reflector_peers_lock: Arc::clone(&reflector_peers),
            reflector_pool,
            trigger_channel: reselect_rx,
            metrics: event_metrics,
        };
        let err_tx = error_tx.clone();
        thread::Builder::new()
            .name("reselection".to_string())
            .spawn(move || {
                if let Err(error) = reflector_selector.run() {
                    let _ = err_tx.send(error);
                }
            })?;
    }

    // Give the baseliner time to collect initial samples before adjusting rates.
    sleep(Duration::from_secs(10));

    let mut ratecontroller = Ratecontroller::new(
        settings.clone(),
        control_snapshot_rx,
        reflector_peers,
        reselect_tx,
        rate_metrics,
    )?;

    let err_tx = error_tx.clone();
    thread::Builder::new()
        .name("ratecontroller".to_string())
        .spawn(move || {
            if let Err(error) = ratecontroller.run() {
                let _ = err_tx.send(error);
            }
        })?;

    // Drop the original sender so the channel disconnects if all workers exit cleanly.
    drop(error_tx);

    let result = wait_for_exit(&error_rx);

    // Make the error path request shutdown too, rather than leaving the other workers running.
    SHUTDOWN.store(true, Ordering::Relaxed);

    let stopping_reason = if result.is_err() { "error" } else { "signal" };
    main_event_metrics.send(Metric::Event {
        name: "stopping",
        reason: stopping_reason,
        reflector: None,
        tags: &[],
    });

    // Drop all MetricsSender instances and the raw tx so the metrics channel
    // disconnects once all worker threads also drop their copies.
    drop(main_event_metrics);
    drop(metrics_tx);

    if let Some(handle) = metrics_thread_handle {
        let _ = handle.join();
    }

    restore_shaper(
        settings,
        &mut main_traffic_control,
        &down_shaper,
        &up_shaper,
    );

    result
}

fn main() -> anyhow::Result<()> {
    println!("Starting sqm-autorate-rust version {}", VERSION);

    install_signal_handlers();

    let settings = Settings::load()?;
    settings.validate()?;

    log::init(settings.output.log_level)?;
    warn_platform_limitations();

    run(&settings)
}
