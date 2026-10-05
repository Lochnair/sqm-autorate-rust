use std::{collections::HashMap, net::IpAddr, time::Duration};

use flume::{Receiver, SendError, Sender};
use thiserror::Error;

use crate::measurement::{
    icmp::IcmpBinding,
    model::{MeasurementSource, MeasurementStream, MeasurementStreamId, ProbeSchedule},
};

#[derive(Debug, Error)]
pub enum ControllerError {
    #[error("ICMP channel error")]
    IcmpChannelError(#[from] SendError<Vec<IcmpBinding>>),
}

pub struct Controller {
    reflector_rx: Receiver<Vec<IpAddr>>,
    icmp_binding_tx: Sender<Vec<IcmpBinding>>,

    source: MeasurementSource,
    period: Duration,

    next_stream_id: u64,
    stream_ids: HashMap<(IpAddr, MeasurementSource), MeasurementStreamId>,
}

impl Controller {
    pub fn new(
        reflector_rx: Receiver<Vec<IpAddr>>,
        icmp_binding_tx: Sender<Vec<IcmpBinding>>,
    ) -> Self {
        Self {
            reflector_rx,
            icmp_binding_tx,
            source: MeasurementSource::IcmpTimestamp,
            period: Duration::from_millis(500),
            next_stream_id: 0,
            stream_ids: HashMap::new(),
        }
    }

    pub async fn run(mut self, initial_reflectors: Vec<IpAddr>) -> Result<(), ControllerError> {
        self.update_reflectors(initial_reflectors).await?;

        while let Ok(reflectors) = self.reflector_rx.recv_async().await {
            self.update_reflectors(reflectors).await?;
        }

        Ok(())
    }

    fn stream_for(&mut self, peer: IpAddr) -> MeasurementStream {
        let key = (peer, self.source);

        let id = *self.stream_ids.entry(key).or_insert_with(|| {
            let id = MeasurementStreamId(self.next_stream_id);
            self.next_stream_id += 1;
            id
        });

        MeasurementStream {
            id,
            peer,
            source: self.source,
        }
    }

    async fn update_reflectors(&mut self, reflectors: Vec<IpAddr>) -> Result<(), ControllerError> {
        let count = reflectors.len();

        let bindings = reflectors
            .into_iter()
            .enumerate()
            .map(|(index, peer)| {
                let stream = self.stream_for(peer);

                IcmpBinding::new(
                    stream,
                    ProbeSchedule {
                        period: self.period,
                        offset: evenly_spaced_offset(self.period, index, count),
                    },
                )
            })
            .collect();

        self.icmp_binding_tx
            .send_async(bindings)
            .await
            .map_err(|e| ControllerError::IcmpChannelError(e))
    }
}

fn evenly_spaced_offset(period: Duration, index: usize, count: usize) -> Duration {
    let nanos = period.as_nanos() * index as u128 / count as u128;

    Duration::new(
        (nanos / 1_000_000_000) as u64,
        (nanos % 1_000_000_000) as u32,
    )
}
