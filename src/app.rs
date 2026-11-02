// SPDX-FileCopyrightText: 2022-Present Charles Corrigan mailto:chas-iot@runegate.org (github @chas-iot)
// SPDX-FileCopyrightText: 2022-Present Daniel Lakeland mailto:dlakelan@street-artists.org (github @dlakelan)
// SPDX-FileCopyrightText: 2022-Present Mark Baker mailto:mark@vpost.net (github @Fail-Safe)
// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use crate::controller::Controller;
use crate::diagnostics::CsvWriter;
use crate::error::ChannelClosed;
use crate::law::{Law, LawContext, SqmaLaw};
use crate::measurement::icmp::IcmpEngine;
#[cfg(feature = "irtt")]
use crate::measurement::irtt::IrttEngine;
use crate::measurement::model::MeasurementSource;
use crate::metrics::{Metric, Metrics, MetricsSender};
use crate::platform::Platform;
use crate::reflector_selector::ReflectorSelector;
use crate::reflector_setup::{ReflectorSetup, setup_reflectors};
use crate::runtime::{Direction, Runtime};
use crate::settings::{ReflectorProtocol, Settings};
use ::log::{error, info};
use anyhow::Context;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::{
    sync::{mpsc, watch},
    task::{JoinError, JoinSet},
};

const SINK_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
type TaskResult = (&'static str, anyhow::Result<()>);

struct PreparedWorkers {
    icmp: IcmpEngine,
    #[cfg(feature = "irtt")]
    irtt: (irtt_client::managed::ManagedClientTask, IrttEngine),
    controller: Controller,
    initial_reflectors: Vec<SocketAddr>,
    reflector_tx: mpsc::UnboundedSender<Vec<SocketAddr>>,
    law: Box<SqmaLaw>,
    law_ctx: LawContext,
    selector: Option<ReflectorSelector>,
}

pub(crate) struct App {
    runtime: Arc<Runtime>,
    prepared: PreparedWorkers,
    exporter: Option<Metrics>,
    csv_writer: Option<CsvWriter>,
    metrics: MetricsSender,
    dry_run: bool,
}

impl App {
    pub(crate) async fn prepare(settings: Settings) -> anyhow::Result<Self> {
        settings.validate()?;
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
            initial: initial_reflectors,
            pool: reflector_pool,
            reflectors,
            reselection_enabled,
        } = setup_reflectors(&settings)?;

        let active_streams = Arc::new(RwLock::new(Vec::new()));
        let endpoint_streams = Arc::new(RwLock::new(HashMap::new()));
        let interval = Duration::from_secs_f64(settings.advanced_settings.tick_interval);

        let (control_snapshot_tx, control_snapshot_rx) = watch::channel(None);
        let (icmp_binding_tx, icmp_binding_rx) = watch::channel(Vec::new());
        #[cfg(feature = "irtt")]
        let (irtt_binding_tx, irtt_binding_rx) = watch::channel(Vec::new());
        let (measurement_tx, measurement_rx) = mpsc::unbounded_channel();
        let (reflector_tx, reflector_rx) = mpsc::unbounded_channel();
        let (reselect_tx, reselect_rx) = mpsc::channel(1);
        let (traffic_tx, traffic_rx) = watch::channel(None);

        let platform = Arc::new(Platform::default());
        for interface in [
            settings.network.download_interface(),
            settings.network.upload_interface(),
        ]
        .into_iter()
        .flatten()
        {
            platform.validate_shaper(interface)?;
        }

        let runtime = Arc::new(Runtime::new(platform, settings.clone(), traffic_tx)?);

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
            interval,
        );
        let icmp_engine = IcmpEngine::new(
            start_time,
            &settings.network,
            measurement_tx.clone(),
            icmp_binding_rx,
        )?;
        #[cfg(feature = "irtt")]
        let irtt = IrttEngine::new(interval, &settings.network, measurement_tx, irtt_binding_rx)?;

        let (metrics, exporter) = if settings.observability.enabled {
            let dropped = Arc::new(AtomicU32::new(0));
            let (tx, rx) = mpsc::channel(1000);
            let exporter = Metrics::new(settings.clone(), rx, Arc::clone(&dropped)).await?;
            (
                MetricsSender::new(tx, dropped, &settings.observability),
                Some(exporter),
            )
        } else {
            (MetricsSender::disabled(), None)
        };

        let (diagnostics, csv_writer) = CsvWriter::new(&settings.output).await?.unzip();
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
            active_streams,
            traffic_rx,
        );
        let selector = if reselection_enabled {
            Some(ReflectorSelector {
                settings: settings.clone(),
                snapshot_rx: control_snapshot_rx,
                reflector_peers_lock: Arc::new(RwLock::new(initial_reflectors.clone())),
                reflector_pool,
                endpoint_streams,
                trigger_channel: reselect_rx,
                metrics: metrics.clone(),
                reflector_tx: reflector_tx.clone(),
            })
        } else {
            None
        };

        Ok(Self {
            runtime,
            prepared: PreparedWorkers {
                icmp: icmp_engine,
                #[cfg(feature = "irtt")]
                irtt,
                controller,
                initial_reflectors,
                reflector_tx,
                law,
                law_ctx,
                selector,
            },
            exporter,
            csv_writer,
            metrics,
            dry_run: settings.advanced_settings.dry_run,
        })
    }

    pub(crate) async fn run(self) -> anyhow::Result<()> {
        // Register both handlers eagerly: ctrl_c() alone registers only when polled.
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

        let Self {
            runtime,
            prepared,
            exporter,
            csv_writer,
            metrics,
            dry_run,
        } = self;
        let mut workers = JoinSet::new();
        let mut sinks = JoinSet::new();
        if let Some(exporter) = exporter {
            sinks.spawn(async move { ("metrics exporter", exporter.run().await) });
        }
        if let Some(writer) = csv_writer {
            sinks.spawn(async move { ("CSV diagnostics", writer.run().await) });
        }
        metrics.send(Metric::Event {
            name: "starting",
            reason: "",
            reflector: None,
            tags: if dry_run { &[("dry_run", "true")] } else { &[] },
        });

        let mut restore_required = false;
        let result = tokio::select! {
            biased;
            signal = sigint.recv() => signal.context("SIGINT stream closed").map(|_| ()),
            signal = sigterm.recv() => signal.context("SIGTERM stream closed").map(|_| ()),
            result = async {
                restore_required = true;
                for direction in [Direction::Download, Direction::Upload] {
                    let Some(shaper) = runtime.shaper(direction) else {
                        continue;
                    };
                    let rate = shaper.min_kbps;
                    info!("Requesting minimum {direction:?} shaper rate: {rate}");
                    runtime
                        .set_rate(direction, rate)
                        .with_context(|| format!("Initializing {direction:?} shaping"))?;
                }
                info!("Sleeping for 2 seconds to give the shaper a chance to control existing bloat");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {},
                    completed = sinks.join_next(), if !sinks.is_empty() => {
                        return unexpected_completion(completed);
                    }
                }
                prepared.start(Arc::clone(&runtime), &mut workers);
                tokio::select! {
                    completed = workers.join_next() => unexpected_completion(completed),
                    completed = sinks.join_next(), if !sinks.is_empty() => unexpected_completion(completed),
                }
            } => result,
        };
        let cleanup = shutdown(
            &runtime,
            workers,
            sinks,
            metrics,
            restore_required,
            result.is_ok(),
        )
        .await;
        match (result, cleanup) {
            (Err(primary), Err(cleanup)) => {
                Err(primary.context(format!("Shutdown also failed: {cleanup:#}")))
            }
            (Err(primary), Ok(())) => Err(primary),
            (Ok(()), cleanup) => cleanup,
        }
    }
}

impl PreparedWorkers {
    fn start(self, runtime: Arc<Runtime>, workers: &mut JoinSet<TaskResult>) {
        let PreparedWorkers {
            icmp,
            #[cfg(feature = "irtt")]
                irtt: (irtt_task, irtt_engine),
            controller,
            initial_reflectors,
            reflector_tx,
            law,
            law_ctx,
            selector,
        } = self;
        workers.spawn(async move { ("runtime sampler", runtime.run().await) });
        workers.spawn(async move { ("ICMP engine", icmp.run().await) });
        #[cfg(feature = "irtt")]
        workers.spawn(async move { ("IRTT engine", irtt_engine.run(irtt_task).await) });
        workers.spawn(async move { ("control law", law.run(law_ctx).await) });
        workers.spawn(async move {
            let _reflector_keepalive = reflector_tx;
            ("controller", controller.run(initial_reflectors).await)
        });
        if let Some(selector) = selector {
            workers.spawn(async move { ("reflector selector", selector.run().await) });
        }
    }
}

async fn shutdown(
    runtime: &Runtime,
    mut workers: JoinSet<TaskResult>,
    mut sinks: JoinSet<TaskResult>,
    metrics: MetricsSender,
    restore_required: bool,
    signal_teardown: bool,
) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    workers.abort_all();
    while let Some(completed) = workers.join_next().await {
        if let Err(error) = shutdown_completion(completed, signal_teardown) {
            error!("Worker shutdown failed: {error:#}");
            failures.push(format!("{error:#}"));
        }
    }

    if restore_required {
        info!("Restoring shapers to configured base rates");
        if let Err(error) = runtime.restore_shaper() {
            failures.push(format!("Restoring shapers: {error:#}"));
        }
    }

    metrics.send(Metric::Event {
        name: "stopping",
        reason: if signal_teardown { "signal" } else { "error" },
        reflector: None,
        tags: &[],
    });
    drop(metrics);
    let drained = tokio::time::timeout(SINK_DRAIN_TIMEOUT, async {
        while let Some(completed) = sinks.join_next().await {
            if let Err(error) = shutdown_completion(completed, false) {
                error!("Output shutdown failed: {error:#}");
                failures.push(format!("{error:#}"));
            }
        }
    })
    .await;
    if drained.is_err() {
        error!("Output sinks did not drain within {SINK_DRAIN_TIMEOUT:?}; aborting");
        failures.push(format!(
            "Output sinks did not drain within {SINK_DRAIN_TIMEOUT:?}"
        ));
        sinks.abort_all();
        while let Some(completed) = sinks.join_next().await {
            if let Err(error) = shutdown_completion(completed, false) {
                failures.push(format!("{error:#}"));
            }
        }
    }
    anyhow::ensure!(failures.is_empty(), "{}", failures.join("; "));
    Ok(())
}

fn unexpected_completion(completed: Option<Result<TaskResult, JoinError>>) -> anyhow::Result<()> {
    match completed {
        Some(Ok((name, result))) => {
            result.with_context(|| format!("{name} failed"))?;
            anyhow::bail!("{name} exited unexpectedly");
        }
        Some(Err(error)) => Err(error).context("Supervised task failed"),
        None => anyhow::bail!("No supervised tasks remaining"),
    }
}

fn shutdown_completion(
    completed: Result<TaskResult, JoinError>,
    signal_teardown: bool,
) -> anyhow::Result<()> {
    match completed {
        Ok((_, Err(error)))
            if signal_teardown && error.chain().any(|cause| cause.is::<ChannelClosed>()) =>
        {
            Ok(())
        }
        Ok((name, result)) => result.with_context(|| format!("{name} failed")),
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(error).context("Joining task during shutdown"),
    }
}
