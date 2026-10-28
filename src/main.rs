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
#[cfg(feature = "irtt")]
use crate::measurement::irtt::IrttEngine;
use crate::measurement::model::{MeasurementSource, MeasurementStream};
use crate::metrics::{Metric, Metrics, MetricsSender};
use crate::platform::{TrafficControlBackend, traffic_control_backend, warn_platform_limitations};
use crate::ratecontroller::Ratecontroller;
use crate::reflector_selector::ReflectorSelector;
use crate::settings::{ReflectorDefinition, ReflectorProtocol, Settings};
use crate::util::RwLockExt;
use ::log::{info, warn};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::thread::{JoinHandle, sleep};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::watch;

struct ReflectorSetup {
    peers: Arc<RwLock<Vec<SocketAddr>>>,
    active_streams: Arc<RwLock<Vec<MeasurementStream>>>,
    endpoint_streams: Arc<RwLock<HashMap<SocketAddr, MeasurementStream>>>,
    pool: Vec<SocketAddr>,
    reflectors: HashMap<SocketAddr, ReflectorDefinition>,
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

fn resolve_reflectors(
    definitions: &[ReflectorDefinition],
    protocol: ReflectorProtocol,
    countries: &[String],
    filter_country: bool,
) -> anyhow::Result<HashMap<SocketAddr, ReflectorDefinition>> {
    let mut resolved = HashMap::new();
    for definition in definitions {
        if !definition.protocols.contains(&protocol) {
            continue;
        }
        if protocol != ReflectorProtocol::Irtt
            && definition
                .host
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_ipv6())
        {
            continue;
        }
        if filter_country
            && !countries.is_empty()
            && definition.country.as_ref().is_some_and(|country| {
                !countries
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(country))
            })
        {
            continue;
        }
        let port = if protocol == ReflectorProtocol::Irtt {
            definition.irtt_port.unwrap_or(2112)
        } else {
            0
        };
        let endpoints = match (definition.host.as_str(), port).to_socket_addrs() {
            Ok(endpoints) => endpoints,
            Err(error) => {
                warn!("Failed to resolve reflector {}: {error}", definition.host);
                continue;
            }
        };
        for endpoint in endpoints {
            if protocol != ReflectorProtocol::Irtt && endpoint.is_ipv6() {
                continue;
            }
            insert_reflector(&mut resolved, endpoint, definition.clone())?;
        }
    }
    Ok(resolved)
}

fn insert_reflector(
    reflectors: &mut HashMap<SocketAddr, ReflectorDefinition>,
    endpoint: SocketAddr,
    definition: ReflectorDefinition,
) -> anyhow::Result<()> {
    if let Some(existing) = reflectors.get_mut(&endpoint) {
        let same_country = match (&existing.country, &definition.country) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => true,
        };
        let same_protocols = existing
            .protocols
            .iter()
            .all(|p| definition.protocols.contains(p))
            && definition
                .protocols
                .iter()
                .all(|p| existing.protocols.contains(p));
        if !same_country
            || !same_protocols
            || existing.irtt_port.unwrap_or(2112) != definition.irtt_port.unwrap_or(2112)
            || existing.irtt_hmac != definition.irtt_hmac
        {
            anyhow::bail!("conflicting reflector definitions for resolved endpoint {endpoint}");
        }
        if existing.country.is_none() {
            existing.country = definition.country;
        }
    } else {
        reflectors.insert(endpoint, definition);
    }
    Ok(())
}

fn setup_reflectors(settings: &Settings) -> anyhow::Result<ReflectorSetup> {
    let protocol = settings.advanced_settings.measurement_type;
    let configured = resolve_reflectors(
        &settings.reflectors.entries,
        protocol,
        &settings.reflectors.countries,
        true,
    )?;
    let bootstrap = resolve_reflectors(&settings.reflectors.bootstrap, protocol, &[], false)?;
    let mut reflectors = bootstrap.clone();
    for (endpoint, definition) in &configured {
        insert_reflector(&mut reflectors, *endpoint, definition.clone())?;
    }
    if reflectors.is_empty() {
        anyhow::bail!("no usable reflectors for measurement protocol {protocol}");
    }
    let mut pool = configured.keys().copied().collect::<Vec<_>>();
    pool.sort();
    let mut initial = bootstrap.keys().copied().collect::<Vec<_>>();
    initial.sort();
    if initial.is_empty() {
        initial = pool
            .iter()
            .take(settings.reflectors.num_reflectors as usize)
            .copied()
            .collect();
    }
    let reselection_enabled = pool.len() > settings.reflectors.num_reflectors as usize;

    Ok(ReflectorSetup {
        peers: Arc::new(RwLock::new(initial)),
        active_streams: Arc::new(RwLock::new(Vec::new())),
        endpoint_streams: Arc::new(RwLock::new(HashMap::new())),
        pool,
        reflectors,
        reselection_enabled,
    })
}

fn spawn_task<F, E>(
    rt: &tokio::runtime::Runtime,
    error_tx: &UnboundedSender<anyhow::Error>,
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
    error_tx: &UnboundedSender<anyhow::Error>,
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

async fn wait_for_exit(error_rx: &mut UnboundedReceiver<anyhow::Error>) -> anyhow::Result<()> {
    loop {
        match tokio::time::timeout(Duration::from_secs(1), error_rx.recv()).await {
            Ok(Some(error)) => {
                return Err(anyhow::anyhow!("worker exited with error: {error}"));
            }
            Ok(None) => {
                return Ok(());
            }
            Err(_) => {
                if SHUTDOWN.load(Ordering::Relaxed) {
                    info!("Received shutdown signal");
                    return Ok(());
                }
            }
        }
    }
}

fn run(settings: &Settings) -> anyhow::Result<()> {
    #[cfg(not(feature = "irtt"))]
    if settings.advanced_settings.measurement_type == ReflectorProtocol::Irtt {
        anyhow::bail!(
            "IRTT measurement selected, but this binary was built without the `irtt` feature"
        );
    }
    let rt = tokio::runtime::Runtime::new()?;

    if settings.advanced_settings.dry_run {
        info!("*** MONITORING MODE ACTIVE — qdisc rates will NOT be changed ***");
    }

    let start_time = Instant::now();

    let ReflectorSetup {
        peers: reflector_peers,
        active_streams,
        endpoint_streams,
        pool: reflector_pool,
        reflectors,
        reselection_enabled,
    } = setup_reflectors(settings)?;

    let (control_snapshot_tx, control_snapshot_rx) = watch::channel(None);
    let (error_tx, mut error_rx) = tokio::sync::mpsc::unbounded_channel::<anyhow::Error>();
    let (icmp_binding_tx, icmp_binding_rx) = watch::channel(Vec::new());
    #[cfg(feature = "irtt")]
    let (irtt_binding_tx, irtt_binding_rx) = watch::channel(Vec::new());
    let (measurement_tx, measurement_rx) = tokio::sync::mpsc::unbounded_channel();
    let (reflector_tx, reflector_rx) = tokio::sync::mpsc::unbounded_channel();
    let (reselect_tx, reselect_rx) = tokio::sync::mpsc::channel(1);

    let source = match settings.advanced_settings.measurement_type {
        ReflectorProtocol::IcmpEcho => MeasurementSource::IcmpEcho,
        ReflectorProtocol::IcmpTimestamps => MeasurementSource::IcmpTimestamp,
        ReflectorProtocol::Irtt => MeasurementSource::Irtt,
    };
    let controller = Controller::new(
        reflector_rx,
        icmp_binding_tx,
        #[cfg(feature = "irtt")]
        irtt_binding_tx,
        reflectors,
        Arc::clone(&active_streams),
        Arc::clone(&endpoint_streams),
        source,
        Duration::from_secs_f64(settings.advanced_settings.tick_interval),
    );
    let icmp_engine = {
        let _guard = rt.enter();
        IcmpEngine::new(
            start_time,
            &settings.network,
            measurement_tx.clone(),
            icmp_binding_rx,
        )?
    };
    #[cfg(feature = "irtt")]
    let (irtt_task, irtt_engine) = {
        let _guard = rt.enter();
        IrttEngine::new(
            Duration::from_secs_f64(settings.advanced_settings.tick_interval),
            &settings.network,
            measurement_tx,
            irtt_binding_rx,
        )?
    };

    spawn_task(&rt, &error_tx, icmp_engine.run());
    #[cfg(feature = "irtt")]
    spawn_task(&rt, &error_tx, irtt_engine.run(irtt_task));

    let dropped = Arc::new(AtomicU32::new(0));

    let (metrics_tx, metrics_thread_handle) = if settings.observability.enabled {
        let (tx, rx) = tokio::sync::mpsc::channel(1000);
        let metrics = Metrics {
            settings: settings.clone(),
            metrics_rx: rx,
            metrics_dropped: Arc::clone(&dropped),
        };
        let handle = spawn_task(&rt, &error_tx, metrics.run());
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

    let mut baseliner = Baseliner {
        settings: settings.clone(),
        reselect_trigger: reselect_tx.clone(),
        start_time,
        stats_rx: measurement_rx,
        control_tx: control_snapshot_tx,
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
            snapshot_rx: control_snapshot_rx.clone(),
            reflector_peers_lock: Arc::clone(&reflector_peers),
            reflector_pool,
            endpoint_streams,
            trigger_channel: reselect_rx,
            metrics: event_metrics,
            reflector_tx,
        };
        spawn_task(&rt, &error_tx, reflector_selector.run());
    }

    // Give the baseliner time to collect initial samples before adjusting rates.
    sleep(Duration::from_secs(10));

    let mut ratecontroller = Ratecontroller::new(
        settings.clone(),
        control_snapshot_rx,
        active_streams,
        reselect_tx,
        rate_metrics,
    )?;

    spawn_worker("ratecontroller", &error_tx, move || ratecontroller.run())?;

    // Drop the original sender so the channel disconnects if all workers exit cleanly.
    drop(error_tx);

    let result = rt.block_on(wait_for_exit(&mut error_rx));

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
        let _ = rt.block_on(handle);
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
