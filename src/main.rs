// SPDX-FileCopyrightText: 2022-Present Charles Corrigan mailto:chas-iot@runegate.org (github @chas-iot)
// SPDX-FileCopyrightText: 2022-Present Daniel Lakeland mailto:dlakelan@street-artists.org (github @dlakelan)
// SPDX-FileCopyrightText: 2022-Present Mark Baker mailto:mark@vpost.net (github @Fail-Safe)
// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

extern crate core;

mod controller;
mod diagnostics;
mod law;
mod log;
mod measurement;
mod metrics;
mod platform;
mod reflector_selector;
mod runtime;
mod settings;
mod time;
mod util;

use crate::controller::Controller;
use crate::law::{Law, LawContext, SqmaLaw};
use crate::measurement::icmp::IcmpEngine;
#[cfg(feature = "irtt")]
use crate::measurement::irtt::IrttEngine;
use crate::measurement::model::{MeasurementSource, MeasurementStream};
use crate::metrics::{Metric, Metrics, MetricsSender};
use crate::platform::{Platform, warn_platform_limitations};
use crate::reflector_selector::ReflectorSelector;
use crate::runtime::Runtime;
use crate::settings::{ReflectorDefinition, ReflectorProtocol, Settings};
use crate::util::RwLockExt;
use ::log::{info, warn};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::watch;

struct ReflectorSetup {
    peers: Arc<RwLock<Vec<SocketAddr>>>,
    active_streams: Arc<RwLock<Vec<MeasurementStream>>>,
    endpoint_streams: Arc<RwLock<HashMap<SocketAddr, MeasurementStream>>>,
    pool: Vec<SocketAddr>,
    reflectors: HashMap<SocketAddr, ReflectorDefinition>,
    reselection_enabled: bool,
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

async fn initialize_shaper(settings: &Settings, platform: &Platform) -> anyhow::Result<()> {
    platform.validate_shaper(&settings.network.download_interface)?;
    platform.validate_shaper(&settings.network.upload_interface)?;

    info!(
        "Requesting minimum shaper rates (D/U): {} / {}",
        settings.network.download_min_kbits(),
        settings.network.upload_min_kbits(),
    );

    platform.set_rate(
        &settings.network.download_interface,
        settings.network.download_min_kbits() as u64,
        settings.advanced_settings.dry_run,
    )?;
    platform.set_rate(
        &settings.network.upload_interface,
        settings.network.upload_min_kbits() as u64,
        settings.advanced_settings.dry_run,
    )?;

    info!("Sleeping for 2 seconds to give the shaper a chance to control existing bloat");

    tokio::time::sleep(Duration::from_secs(2)).await;

    Ok(())
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

async fn run(settings: Settings) -> anyhow::Result<()> {
    let mut workers = tokio::task::JoinSet::<anyhow::Result<()>>::new();

    #[cfg(not(feature = "irtt"))]
    if settings.advanced_settings.measurement_type == ReflectorProtocol::Irtt {
        anyhow::bail!(
            "IRTT measurement selected, but this binary was built without the `irtt` feature"
        );
    }

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
    } = setup_reflectors(&settings)?;

    let (control_snapshot_tx, control_snapshot_rx) = watch::channel(None);
    let (icmp_binding_tx, icmp_binding_rx) = watch::channel(Vec::new());
    #[cfg(feature = "irtt")]
    let (irtt_binding_tx, irtt_binding_rx) = watch::channel(Vec::new());
    let (measurement_tx, measurement_rx) = tokio::sync::mpsc::unbounded_channel();
    let (reflector_tx, reflector_rx) = tokio::sync::mpsc::unbounded_channel();
    let (reselect_tx, reselect_rx) = tokio::sync::mpsc::channel(1);
    let (traffic_tx, traffic_rx) = watch::channel(None);

    let platform = Arc::new(Platform::default());
    initialize_shaper(&settings, &platform).await?;

    let runtime = Arc::new(Runtime::new(platform, settings.clone(), traffic_tx));
    workers.spawn(runtime.clone().run());

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
    let icmp_engine = IcmpEngine::new(
        start_time,
        &settings.network,
        measurement_tx.clone(),
        icmp_binding_rx,
    )?;
    #[cfg(feature = "irtt")]
    let (irtt_task, irtt_engine) = IrttEngine::new(
        Duration::from_secs_f64(settings.advanced_settings.tick_interval),
        &settings.network,
        measurement_tx,
        irtt_binding_rx,
    )?;

    workers.spawn(icmp_engine.run());

    #[cfg(feature = "irtt")]
    workers.spawn(irtt_engine.run(irtt_task));

    let dropped = Arc::new(AtomicU32::new(0));

    let metrics_tx = if settings.observability.enabled {
        let (tx, rx) = tokio::sync::mpsc::channel(1000);
        let metrics = Metrics {
            settings: settings.clone(),
            metrics_rx: rx,
            metrics_dropped: Arc::clone(&dropped),
        };
        workers.spawn(metrics.run());
        Some(tx)
    } else {
        None
    };

    let metrics = metrics_tx
        .as_ref()
        .map(|tx| MetricsSender::new(tx.clone(), Arc::clone(&dropped), &settings.observability))
        .unwrap_or_else(MetricsSender::disabled);

    metrics.send(Metric::Event {
        name: "starting",
        reason: "",
        reflector: None,
        tags: if settings.advanced_settings.dry_run {
            &[("dry_run", "true")]
        } else {
            &[]
        },
    });

    let diagnostics = diagnostics::CsvWriter::new(&settings.output).map(|(sender, writer)| {
        workers.spawn(writer.run());
        sender
    });
    let law = Box::new(SqmaLaw::new(
        settings.clone(),
        control_snapshot_tx,
        diagnostics,
    ));
    let law_ctx = LawContext::new(
        measurement_rx,
        metrics.clone(),
        reselect_tx,
        Arc::clone(&runtime),
        Arc::clone(&active_streams),
        traffic_rx,
    );
    workers.spawn(law.run(law_ctx));

    workers.spawn(controller.run(reflector_peers.read_anyhow()?.clone()));

    let main_event_metrics = metrics.clone();

    if reselection_enabled {
        let reflector_selector = ReflectorSelector {
            settings: settings.clone(),
            snapshot_rx: control_snapshot_rx.clone(),
            reflector_peers_lock: Arc::clone(&reflector_peers),
            reflector_pool,
            endpoint_streams,
            trigger_channel: reselect_rx,
            metrics,
            reflector_tx,
        };
        workers.spawn(reflector_selector.run());
    }

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    let result: anyhow::Result<()> = tokio::select! {
        signal = tokio::signal::ctrl_c() => signal.map_err(Into::into),

        _ = sigterm.recv() => Ok(()),

        completed = workers.join_next() => match completed {
            Some(Ok(Err(error))) => Err(error),
            Some(Ok(Ok(()))) => {
                Err(anyhow::anyhow!("Worker exited unexpectedly"))
            }
            Some(Err(error)) => Err(error.into()),
            None => Err(anyhow::anyhow!("No workers remaining")),
        },
    };

    let stopping_reason = if result.is_err() { "error" } else { "signal" };
    main_event_metrics.send(Metric::Event {
        name: "stopping",
        reason: stopping_reason,
        reflector: None,
        tags: &[],
    });

    workers.shutdown().await;

    // Drop all MetricsSender instances and the raw tx so the metrics channel
    // disconnects once all worker threads also drop their copies.
    drop(main_event_metrics);
    drop(metrics_tx);

    runtime.restore_shaper()?;

    result
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Starting sqm-autorate-rust version {}", VERSION);

    let settings = Settings::load()?;
    settings.validate()?;

    log::init(settings.output.log_level)?;
    warn_platform_limitations();

    run(settings).await
}
