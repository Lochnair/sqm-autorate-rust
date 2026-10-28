use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, RwLock},
    time::Duration,
};

use flume::{Receiver, SendError, Sender};
use thiserror::Error;

#[cfg(feature = "irtt")]
use crate::measurement::irtt::IrttBinding;
use crate::{
    measurement::{
        icmp::IcmpBinding,
        model::{MeasurementSource, MeasurementStream, MeasurementStreamId, ProbeSchedule},
    },
    settings::ReflectorDefinition,
};

#[derive(Debug, Error)]
pub enum ControllerError {
    #[error("ICMP channel error")]
    IcmpChannelError(#[from] SendError<Vec<IcmpBinding>>),
    #[cfg(feature = "irtt")]
    #[error("IRTT channel error")]
    IrttChannelError(#[from] SendError<Vec<IrttBinding>>),
}

pub struct Controller {
    reflector_rx: Receiver<Vec<SocketAddr>>,
    icmp_binding_tx: Sender<Vec<IcmpBinding>>,
    #[cfg(feature = "irtt")]
    irtt_binding_tx: Sender<Vec<IrttBinding>>,
    reflectors: HashMap<SocketAddr, ReflectorDefinition>,
    active_streams: Arc<RwLock<Vec<MeasurementStream>>>,
    endpoint_streams: Arc<RwLock<HashMap<SocketAddr, MeasurementStream>>>,
    source: MeasurementSource,
    period: Duration,
    next_stream_id: u64,
    stream_ids: HashMap<(SocketAddr, MeasurementSource), MeasurementStreamId>,
}

impl Controller {
    #[allow(
        clippy::too_many_arguments,
        reason = "Constructor wires the controller channels and shared stream state"
    )]
    pub fn new(
        reflector_rx: Receiver<Vec<SocketAddr>>,
        icmp_binding_tx: Sender<Vec<IcmpBinding>>,
        #[cfg(feature = "irtt")] irtt_binding_tx: Sender<Vec<IrttBinding>>,
        reflectors: HashMap<SocketAddr, ReflectorDefinition>,
        active_streams: Arc<RwLock<Vec<MeasurementStream>>>,
        endpoint_streams: Arc<RwLock<HashMap<SocketAddr, MeasurementStream>>>,
        source: MeasurementSource,
        period: Duration,
    ) -> Self {
        Self {
            reflector_rx,
            icmp_binding_tx,
            #[cfg(feature = "irtt")]
            irtt_binding_tx,
            reflectors,
            active_streams,
            endpoint_streams,
            source,
            period,
            next_stream_id: 0,
            stream_ids: HashMap::new(),
        }
    }

    pub async fn run(mut self, initial_reflectors: Vec<SocketAddr>) -> Result<(), ControllerError> {
        self.update_reflectors(initial_reflectors).await?;
        while let Ok(reflectors) = self.reflector_rx.recv_async().await {
            self.update_reflectors(reflectors).await?;
        }
        Ok(())
    }

    fn stream_for(&mut self, endpoint: SocketAddr) -> MeasurementStream {
        let key = (endpoint, self.source);
        let id = *self.stream_ids.entry(key).or_insert_with(|| {
            let id = MeasurementStreamId(self.next_stream_id);
            self.next_stream_id += 1;
            id
        });
        MeasurementStream {
            id,
            peer: endpoint.ip(),
            source: self.source,
        }
    }

    async fn update_reflectors(
        &mut self,
        endpoints: Vec<SocketAddr>,
    ) -> Result<(), ControllerError> {
        let count = endpoints.len();
        let mut icmp = Vec::new();
        #[cfg(feature = "irtt")]
        let mut irtt = Vec::new();
        let mut streams = Vec::with_capacity(count);
        let mut endpoint_streams = HashMap::new();
        for (index, endpoint) in endpoints.into_iter().enumerate() {
            let Some(_definition) = self.reflectors.get(&endpoint) else {
                continue;
            };
            #[cfg(feature = "irtt")]
            let (port, hmac) = {
                (
                    _definition.irtt_port.unwrap_or(2112),
                    _definition.irtt_hmac.clone(),
                )
            };
            let stream = self.stream_for(endpoint);
            let schedule = ProbeSchedule {
                period: self.period,
                offset: evenly_spaced_offset(self.period, index, count),
            };
            match self.source {
                MeasurementSource::IcmpEcho | MeasurementSource::IcmpTimestamp => {
                    icmp.push(IcmpBinding::new(stream, schedule))
                }
                #[cfg(feature = "irtt")]
                MeasurementSource::Irtt => irtt.push(IrttBinding::new(stream, port, hmac)),
                #[cfg(not(feature = "irtt"))]
                MeasurementSource::Irtt => unreachable!("IRTT feature is checked before startup"),
                _ => unreachable!("unsupported reflector measurement source"),
            }
            streams.push(stream);
            endpoint_streams.insert(endpoint, stream);
        }
        *self
            .active_streams
            .write()
            .expect("active stream lock poisoned") = streams;
        *self
            .endpoint_streams
            .write()
            .expect("endpoint stream lock poisoned") = endpoint_streams;
        self.icmp_binding_tx.send_async(icmp).await?;
        #[cfg(feature = "irtt")]
        self.irtt_binding_tx.send_async(irtt).await?;
        Ok(())
    }
}

fn evenly_spaced_offset(period: Duration, index: usize, count: usize) -> Duration {
    let nanos = period.as_nanos() * index as u128 / count as u128;
    Duration::new(
        (nanos / 1_000_000_000) as u64,
        (nanos % 1_000_000_000) as u32,
    )
}
