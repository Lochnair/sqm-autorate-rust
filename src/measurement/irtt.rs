use std::{collections::HashMap, net::SocketAddr, time::Instant};

use flume::{Receiver, Sender};
use irtt_client::{
    ClientConfig, ClientEvent,
    managed::{
        ManagedClient, ManagedClientConfig, ManagedClientHandle, ManagedClientTask,
        ManagedCommandAcknowledgement, ManagedCommandApplyError, ManagedCommandError,
        ManagedCompletionPolicy, ManagedConfigError, ManagedEndReason, ManagedEvent,
        ManagedLifecycle, ManagedSubscribeError, ManagedTargetConfig, ManagedTargetEndReason,
        TargetId,
    },
};
use log::{debug, trace, warn};
use thiserror::Error;
use tokio::sync::broadcast::error::RecvError;

use crate::measurement::model::{
    MeasurementEvent, MeasurementLoss, MeasurementObservation, MeasurementStream,
    MeasurementStreamId, OneWayClock, OneWayLatency, ProbeSchedule, SignedDuration,
};

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

pub struct IrttBinding {
    stream: MeasurementStream,
    schedule: ProbeSchedule,
    port: u16,
}

impl IrttBinding {
    pub fn new(stream: MeasurementStream, schedule: ProbeSchedule, port: u16) -> Self {
        Self {
            stream,
            schedule,
            port,
        }
    }
}

pub struct IrttEngine {
    client: ManagedClientHandle,

    bindings: HashMap<MeasurementStreamId, IrttBinding>,

    origin: Instant,

    binding_rx: Receiver<Vec<IrttBinding>>,
    observation_tx: Sender<MeasurementEvent>,
}

impl IrttEngine {
    pub fn new(
        origin: Instant,
        observation_tx: Sender<MeasurementEvent>,
        binding_rx: Receiver<Vec<IrttBinding>>,
    ) -> Result<(ManagedClientTask, Self), IrttError> {
        let config = ManagedClientConfig {
            client: ClientConfig {
                request: irtt_client::SessionRequest {
                    duration: None,
                    ..Default::default()
                },
                ..Default::default()
            },
            completion: ManagedCompletionPolicy::ExplicitStop,
            ..Default::default()
        };
        let (task, handle) = ManagedClient::task(config, Vec::new())?;

        Ok((
            task,
            Self {
                client: handle,
                bindings: HashMap::new(),
                origin,
                binding_rx,
                observation_tx,
            },
        ))
    }

    async fn set_bindings(&mut self, bindings: Vec<IrttBinding>) -> Result<(), IrttError> {
        let targets = bindings
            .iter()
            .map(|binding| {
                ManagedTargetConfig::new(
                    TargetId::new(binding.stream.id.0.to_string()),
                    SocketAddr::new(binding.stream.peer, binding.port).to_string(),
                )
            })
            .collect();

        self.client.update_targets(targets)?.await?;

        self.bindings = bindings
            .into_iter()
            .map(|binding| (binding.stream.id, binding))
            .collect();

        Ok(())
    }

    pub async fn run(mut self) -> Result<(), IrttError> {
        let mut events = self.client.subscribe()?;

        loop {
            tokio::select! {
                bindings = self.binding_rx.recv_async() => {
                    let bindings = bindings
                        .map_err(|_| IrttError::BindingChannelClosed)?;

                    self.set_bindings(bindings).await?;
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
                                    ..
                                } => {
                                    let Some(stream) = stream else {
                                        continue;
                                    };

                                    let observation = MeasurementObservation {
                                        stream,
                                        started_at: sent_at.mono,
                                        observed_at: received_at.mono,
                                        one_way: one_way.and_then(map_one_way),
                                    };

                                    self.observation_tx
                                        .send_async(MeasurementEvent::Observation(observation))
                                        .await
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
                                        .send_async(MeasurementEvent::Loss(loss))
                                        .await
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

fn map_one_way(sample: irtt_client::OneWayDelaySample) -> Option<OneWayLatency> {
    let uplink = i64::try_from(sample.client_to_server?.as_nanos()).ok()?;

    let downlink = i64::try_from(sample.server_to_client?.as_nanos()).ok()?;

    Some(OneWayLatency {
        uplink: SignedDuration::from_nanos(uplink),
        downlink: SignedDuration::from_nanos(downlink),
        clock: OneWayClock::Uncalibrated,
    })
}
