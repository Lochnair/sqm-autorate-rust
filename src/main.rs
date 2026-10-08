// SPDX-FileCopyrightText: 2022-Present Charles Corrigan mailto:chas-iot@runegate.org (github @chas-iot)
// SPDX-FileCopyrightText: 2022-Present Daniel Lakeland mailto:dlakelan@street-artists.org (github @dlakelan)
// SPDX-FileCopyrightText: 2022-Present Mark Baker mailto:mark@vpost.net (github @Fail-Safe)
// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

extern crate core;

mod baseliner;
mod controller;
mod log;
mod measurement;
mod metrics;
mod platform;
mod ratecontroller;
mod reflector_selector;
mod settings;
mod time;
mod util;

use crate::baseliner::Baseliner;
use crate::controller::Controller;
use crate::measurement::icmp::IcmpEngine;
use crate::measurement::irtt::IrttEngine;
use crate::metrics::{Metric, Metrics, MetricsSender};
use crate::platform::{TrafficControlBackend, traffic_control_backend, warn_platform_limitations};
use crate::ratecontroller::Ratecontroller;
use crate::reflector_selector::ReflectorSelector;
use crate::settings::Settings;
use crate::util::RwLockExt;
use ::log::{info, warn};
use flume::RecvTimeoutError;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::thread::{JoinHandle, sleep};
use std::time::{Duration, Instant};

struct ReflectorSetup {
    peers: Arc<RwLock<Vec<IpAddr>>>,
    pool: Vec<IpAddr>,
    reselection_enabled: bool,
}
pub static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn signal_handler(_: libc::c_int) {
    SHUTDOWN.store(true, Ordering::Relaxed);
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

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
    })
}

fn spawn_task<F, E>(
    rt: &tokio::runtime::Runtime,
    error_tx: &flume::Sender<anyhow::Error>,
    task: F,
) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = Result<(), E>> + Send + 'static,
    E: Into<anyhow::Error> + Send + 'static,
{
    let error_tx = error_tx.clone();

    rt.spawn(async move {
        if let Err(error) = task.await {
            let _ = error_tx.send(error.into());
        }
    })
}

fn spawn_worker<F>(
    name: &str,
    error_tx: &flume::Sender<anyhow::Error>,
    worker: F,
) -> anyhow::Result<JoinHandle<()>>
where
    F: FnOnce() -> anyhow::Result<()> + Send + 'static,
{
    let error_tx = error_tx.clone();

    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            if let Err(error) = worker() {
                let _ = error_tx.send(error);
            }
        })
        .map_err(|e| anyhow::anyhow!(e))
}

fn wait_for_exit(error_rx: &flume::Receiver<anyhow::Error>) -> anyhow::Result<()> {
    loop {
        match error_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(error) => {
                return Err(anyhow::anyhow!("worker exited with error: {error}"));
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
    let rt = tokio::runtime::Runtime::new()?;

    if settings.advanced_settings.dry_run {
        info!("*** MONITORING MODE ACTIVE — qdisc rates will NOT be changed ***");
    }

    let start_time = Instant::now();

    let ReflectorSetup {
        peers: reflector_peers,
        pool: reflector_pool,
        reselection_enabled,
    } = setup_reflectors(settings)?;

    let (control_snapshot_tx, control_snapshot_rx) = flume::unbounded();
    let (error_tx, error_rx) = flume::unbounded::<anyhow::Error>();
    let (icmp_binding_tx, icmp_binding_rx) = flume::unbounded();
    let (irtt_binding_tx, irtt_binding_rx) = flume::unbounded();
    let (measurement_tx, measurement_rx) = flume::unbounded();
    let (reflector_tx, reflector_rx) = flume::unbounded();
    let (reselect_tx, reselect_rx) = flume::bounded(1);
    let (selection_snapshot_tx, selection_snapshot_rx) = if reselection_enabled {
        let (tx, rx) = flume::unbounded();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    let controller = Controller::new(reflector_rx, icmp_binding_tx);
    let icmp_engine = {
        let _guard = rt.enter();
        IcmpEngine::new(
            start_time,
            &settings.network,
            measurement_tx.clone(),
            icmp_binding_rx,
        )?
    };
    let (_irtt_task, irtt_engine) = {
        let _guard = rt.enter();
        IrttEngine::new(start_time, measurement_tx, irtt_binding_rx)?
    };

    spawn_task(&rt, &error_tx, icmp_engine.run());
    spawn_task(&rt, &error_tx, irtt_engine.run());

    let dropped = Arc::new(AtomicU32::new(0));

    let (metrics_tx, metrics_thread_handle) = if settings.observability.enabled {
        let (tx, rx) = flume::bounded(1000);
        let metrics = Metrics {
            settings: settings.clone(),
            metrics_rx: rx,
            metrics_dropped: Arc::clone(&dropped),
        };
        let handle = spawn_worker("metrics", &error_tx, move || metrics.run())?;
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

    let baseliner = Baseliner {
        settings: settings.clone(),
        reselect_trigger: reselect_tx.clone(),
        start_time,
        stats_rx: measurement_rx,
        control_tx: control_snapshot_tx,
        selection_tx: selection_snapshot_tx,
        baseline_metrics,
        event_metrics: event_metrics.clone(),
        ping_metrics,
    };

    let mut main_traffic_control = traffic_control_backend();
    let (down_shaper, up_shaper) = initialize_shaper(settings, &mut main_traffic_control)?;

    spawn_worker("baseliner", &error_tx, move || baseliner.run())?;
    spawn_task(
        &rt,
        &error_tx,
        controller.run(reflector_peers.read_anyhow()?.clone()),
    );

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
            reflector_tx,
        };
        spawn_worker("reselection", &error_tx, move || reflector_selector.run())?;
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

    spawn_worker("ratecontroller", &error_tx, move || ratecontroller.run())?;

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
