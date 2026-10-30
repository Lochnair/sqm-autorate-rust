use std::sync::{Arc, RwLock};

use crate::{
    measurement::model::{MeasurementEvent, MeasurementStream},
    metrics::MetricsSender,
    runtime::{Direction, Runtime, Shaper, TrafficRates},
};
use tokio::sync::{mpsc, watch};

pub struct LawContext {
    pub measurements: mpsc::UnboundedReceiver<MeasurementEvent>,
    pub metrics: MetricsSender,
    pub reselect_trigger: mpsc::Sender<bool>,
    runtime: Arc<Runtime>,
    pub targets: Arc<RwLock<Vec<MeasurementStream>>>,
    pub traffic: watch::Receiver<Option<TrafficRates>>,
}

impl LawContext {
    pub fn new(
        measurements: mpsc::UnboundedReceiver<MeasurementEvent>,
        metrics: MetricsSender,
        reselect_trigger: mpsc::Sender<bool>,
        runtime: Arc<Runtime>,
        targets: Arc<RwLock<Vec<MeasurementStream>>>,
        traffic: watch::Receiver<Option<TrafficRates>>,
    ) -> Self {
        Self {
            measurements,
            metrics,
            reselect_trigger,
            runtime,
            targets,
            traffic,
        }
    }

    pub fn set_rate(&self, direction: Direction, rate_kbps: f64) -> anyhow::Result<()> {
        self.runtime.set_rate(direction, rate_kbps)
    }

    pub fn shaper(&self, direction: Direction) -> Option<&Shaper> {
        self.runtime.shaper(direction)
    }
}
