use std::{collections::HashMap, net::SocketAddr};

use irtt_client::{
    ClientConfig, ClientEvent, NegotiationPolicy, OpenPolicy,
    managed::{
        ManagedClient, ManagedClientConfig, ManagedClientHandle, ManagedClientTask,
        ManagedCommandApplyError, ManagedCommandError, ManagedCompletionPolicy, ManagedConfigError,
        ManagedEndReason, ManagedEvent, ManagedLifecycle, ManagedSubscribeError,
        ManagedTargetConfig, ManagedTargetEndReason, ManagedTargetLifecycle, TargetAuth, TargetId,
    },
};
use log::{debug, trace, warn};
use thiserror::Error;
use tokio::sync::{broadcast::error::RecvError, mpsc::UnboundedSender, watch};

use crate::measurement::model::{
    MeasurementEvent, MeasurementLoss, MeasurementObservation, MeasurementStream,
    MeasurementStreamId, OneWayClock, OneWayLatency, SignedDuration,
};
use crate::settings::NetworkSettings;

#[derive(Debug, Error)]
pub enum IrttError {
    #[error("failed to build IRTT config")]
    InvalidConfig(#[from] ManagedConfigError),

    #[error("event channel closed")]
    EventChannelClosed,

    #[error("binding channel closed")]
    BindingChannelClosed,

    #[error("error while reciving on channel")]
    ChannelRecvError(#[from] RecvError),

    #[error("unable to subscribe to irtt events")]
    SubscriptionError(#[from] ManagedSubscribeError),

    #[error("failed to submit IRTT target update")]
    CommandError(#[from] ManagedCommandError),

    #[error("failed to apply IRTT target update")]
    CommandApplyError(#[from] ManagedCommandApplyError),

    #[error("IRTT managed client failed: {0:?}")]
    ManagedFailure(ManagedEndReason),

    #[error("IRTT managed task was abandoned")]
    ManagedAbandoned,
}

#[derive(Clone)]
pub struct IrttBinding {
    stream: MeasurementStream,
    port: u16,
    hmac: Option<String>,
}

impl IrttBinding {
    pub fn new(stream: MeasurementStream, port: u16, hmac: Option<String>) -> Self {
        Self { stream, port, hmac }
    }
}

pub struct IrttEngine {
    client: ManagedClientHandle,

    bindings: HashMap<MeasurementStreamId, IrttBinding>,
    targets: Vec<ManagedTargetConfig>,

    binding_rx: watch::Receiver<Vec<IrttBinding>>,
    observation_tx: UnboundedSender<MeasurementEvent>,
}

impl IrttEngine {
    pub fn new(
        interval: std::time::Duration,
        settings: &NetworkSettings,
        observation_tx: UnboundedSender<MeasurementEvent>,
        binding_rx: watch::Receiver<Vec<IrttBinding>>,
    ) -> Result<(ManagedClientTask, Self), IrttError> {
        let config = managed_client_config(interval, settings);
        let (task, handle) = ManagedClient::task(config, Vec::new())?;

        Ok((
            task,
            Self {
                client: handle,
                bindings: HashMap::new(),
                targets: Vec::new(),
                binding_rx,
                observation_tx,
            },
        ))
    }

    async fn set_bindings(&mut self, bindings: Vec<IrttBinding>) -> Result<(), IrttError> {
        let targets = bindings
            .iter()
            .map(|binding| {
                let mut target = ManagedTargetConfig::new(
                    TargetId::new(binding.stream.id.0.to_string()),
                    SocketAddr::new(binding.stream.peer, binding.port).to_string(),
                );
                if let Some(key) = binding.hmac.as_ref().filter(|key| !key.trim().is_empty()) {
                    target.auth = TargetAuth::Override(irtt_client::Authentication::Hmac(
                        irtt_client::HmacKey::new(key.as_bytes().to_vec().into_boxed_slice()),
                    ));
                }
                target
            })
            .collect::<Vec<_>>();

        self.client.update_targets(targets.clone())?.await?;
        self.targets = targets;

        self.bindings = bindings
            .into_iter()
            .map(|binding| (binding.stream.id, binding))
            .collect();

        Ok(())
    }

    pub async fn run(mut self, task: ManagedClientTask) -> anyhow::Result<()> {
        let mut task = Box::pin(task);
        let client = self.client.clone();

        // Keep driving the client even while the event loop awaits command receipts.
        let result = tokio::select! {
            biased;
            outcome = task.as_mut() => {
                return match outcome.end_reason {
                    ManagedEndReason::StopRequested => Ok(()),
                    end_reason => Err(IrttError::ManagedFailure(end_reason).into()),
                };
            }
            result = self.run_events() => result,
        };

        // Every event-loop exit stops and joins the driver, including channel errors.
        let stop = client.stop();
        let (outcome, _) = tokio::join!(task, stop);
        result?;
        match outcome.end_reason {
            ManagedEndReason::StopRequested => Ok(()),
            end_reason => Err(IrttError::ManagedFailure(end_reason).into()),
        }
    }

    async fn run_events(&mut self) -> Result<(), IrttError> {
        let mut events = self.client.subscribe()?;
        // Durable status catches failures even if their presentation events were lost.
        // Resubmitting the full desired set restarts terminal targets only.
        let mut retry = tokio::time::interval(std::time::Duration::from_secs(5));
        retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        retry.tick().await;
        loop {
            tokio::select! {
                bindings = self.binding_rx.changed() => {
                    bindings.map_err(|_| IrttError::BindingChannelClosed)?;

                    let bindings = self.binding_rx.borrow_and_update().clone();
                    self.set_bindings(bindings).await?;
                }

                _ = retry.tick() => {
                    if self.client.status().targets.iter().any(|target| {
                        target.desired && target.lifecycle == ManagedTargetLifecycle::Terminal
                    }) {
                        self.client.update_targets(self.targets.clone())?.await?;
                    }
                }

                event = events.recv() => {
                    let event = match event {
                        Ok(event) => event,

                        Err(RecvError::Lagged(n)) => {
                            warn!("IRTT event receiver dropped {n} events");
                            continue;
                        }

                        Err(RecvError::Closed) => {
                            let status = self.client.status();

                            match status.lifecycle {
                                ManagedLifecycle::Completed => return Ok(()),

                                ManagedLifecycle::Failed => {
                                    if let Some(outcome) = &status.final_outcome {
                                        return Err(IrttError::ManagedFailure(
                                            outcome.end_reason.clone()
                                        ));
                                    }

                                    return Err(IrttError::ManagedAbandoned);
                                }

                                _ => return Err(IrttError::ManagedAbandoned),
                            }
                        }
                    };

                    match event {
                        ManagedEvent::Started => {
                            debug!("IRTT managed client started");
                        }

                        ManagedEvent::TargetStateChanged {
                            target,
                            lifecycle,
                        } => {
                            debug!("IRTT target {target:?}: {lifecycle:?}");
                        }

                        ManagedEvent::Client { target, event } => {
                            let stream = target.id.as_str()
                                .parse::<u64>()
                                .ok()
                                .and_then(|id| {
                                    self.bindings.get(&MeasurementStreamId(id))
                                })
                                .map(|binding| binding.stream);

                            match event {
                                ClientEvent::SessionStarted(session) => {
                                    debug!(
                                        "IRTT session started for {}: {}",
                                        target.id,
                                        session.remote
                                    );
                                }

                                ClientEvent::NoTestCompleted(outcome) => {
                                    debug!(
                                        "IRTT no-test completed for {}: {}",
                                        target.id,
                                        outcome.remote
                                    );
                                }

                                ClientEvent::SessionClosed { .. } => {
                                    debug!("IRTT session closed for {}", target.id);
                                }

                                ClientEvent::EchoSent { seq, .. } => {
                                    trace!("IRTT {} sent probe {seq}", target.id);
                                }

                                ClientEvent::EchoReply {
                                    sent_at,
                                    received_at,
                                    one_way,
                                    rtt,
                                    ..
                                } => {
                                    let Some(stream) = stream else {
                                        continue;
                                    };

                                    let observation = MeasurementObservation {
                                        stream,
                                        started_at: sent_at.mono,
                                        observed_at: received_at.mono,
                                        round_trip: i64::try_from(rtt.effective.as_nanos())
                                            .ok().map(SignedDuration::from_nanos),
                                        one_way: one_way.and_then(map_one_way),
                                    };

                                    self.observation_tx
                                        .send(MeasurementEvent::Observation(observation))
                                        .map_err(|_| IrttError::EventChannelClosed)?;
                                }

                                ClientEvent::EchoLoss {
                                    sent_at,
                                    timeout_at,
                                    ..
                                } => {
                                    let Some(stream) = stream else {
                                        continue;
                                    };

                                    let loss = MeasurementLoss {
                                        stream,
                                        started_at: sent_at.mono,
                                        detected_at: timeout_at,
                                    };

                                    self.observation_tx
                                        .send(MeasurementEvent::Loss(loss))
                                        .map_err(|_| IrttError::EventChannelClosed)?;
                                }

                                ClientEvent::DuplicateReply { seq, .. } => {
                                    trace!(
                                        "IRTT {} duplicate reply {seq}",
                                        target.id
                                    );
                                }

                                ClientEvent::LateReply { seq, .. } => {
                                    trace!(
                                        "IRTT {} late reply {seq}",
                                        target.id
                                    );
                                }

                                ClientEvent::Warning { kind, message, .. } => {
                                    debug!(
                                        "IRTT {} warning ({kind:?}): {message}",
                                        target.id
                                    );
                                }
                            }
                        }

                        ManagedEvent::TargetFinished { outcome } => {
                            match &outcome.end_reason {
                                ManagedTargetEndReason::Failed(failure) => {
                                    warn!(
                                        "IRTT target {} failed: {failure:?}",
                                        outcome.target.id
                                    );
                                }

                                reason => {
                                    debug!(
                                        "IRTT target {} finished: {reason:?}",
                                        outcome.target.id
                                    );
                                }
                            }
                        }

                        ManagedEvent::Stopping => {
                            debug!("IRTT managed client stopping");
                        }

                        ManagedEvent::Completed { outcome } => {
                            debug!(
                                "IRTT managed client completed: {:?}",
                                outcome.end_reason
                            );
                            return Ok(());
                        }

                        ManagedEvent::Failed { outcome } => {
                            return Err(IrttError::ManagedFailure(
                                outcome.end_reason.clone()
                            ));
                        }

                        ManagedEvent::Abandoned => {
                            return Err(IrttError::ManagedAbandoned);
                        }
                    }
                }
            }
        }
    }
}

fn managed_client_config(
    interval: std::time::Duration,
    #[allow(unused)] settings: &NetworkSettings,
) -> ManagedClientConfig {
    ManagedClientConfig {
        client: ClientConfig {
            socket: irtt_client::SocketConfig {
                #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
                bind_to_device: settings.measurement_bind_device.clone(),
                #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
                mark: settings.measurement_mark,
                #[cfg(target_os = "freebsd")]
                fib: settings.measurement_fib,
                ..Default::default()
            },
            request: irtt_client::SessionRequest {
                duration: None,
                interval,
                ..Default::default()
            },
            open: OpenPolicy {
                negotiation: NegotiationPolicy::Strict,
                ..Default::default()
            },
            ..Default::default()
        },
        completion: ManagedCompletionPolicy::ExplicitStop,
        ..Default::default()
    }
}

fn map_one_way(sample: irtt_client::OneWayDelaySample) -> Option<OneWayLatency> {
    let uplink = i64::try_from(sample.client_to_server?.as_nanos()).ok()?;

    let downlink = i64::try_from(sample.server_to_client?.as_nanos()).ok()?;

    Some(OneWayLatency {
        uplink: SignedDuration::from_nanos(uplink),
        downlink: SignedDuration::from_nanos(downlink),
        clock: OneWayClock::Uncalibrated,
    })
}
