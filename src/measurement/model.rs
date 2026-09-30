use std::{
    net::IpAddr,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MeasurementStreamId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeasurementSource {
    IcmpEcho,
    IcmpTimestamp,
    Irtt,
    TcpSeqAck,
    TcpTimestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MeasurementStream {
    pub id: MeasurementStreamId,
    pub peer: IpAddr,
    pub source: MeasurementSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OneWayClock {
    Uncalibrated,
    Calibrated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SignedDuration {
    nanos: i64,
}

impl SignedDuration {
    pub const fn from_nanos(nanos: i64) -> Self {
        Self { nanos }
    }

    pub const fn from_millis(millis: i64) -> Self {
        Self {
            nanos: millis * 1_000_000,
        }
    }

    pub const fn as_nanos(self) -> i64 {
        self.nanos
    }

    pub fn as_secs_f64(self) -> f64 {
        self.nanos as f64 / 1_000_000_000.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OneWayLatency {
    pub uplink: SignedDuration,
    pub downlink: SignedDuration,
    pub clock: OneWayClock,
}

#[derive(Debug, Clone, Copy)]
pub struct MeasurementObservation {
    pub stream: MeasurementStream,

    /// Local monotonic time at which measurement started.
    pub started_at: Instant,

    /// Local monotonic time at which the result was observed.
    pub observed_at: Instant,

    /// Directional timing, when the measurement mechanism can provide it.
    pub one_way: Option<OneWayLatency>,
}

impl MeasurementObservation {
    pub fn rtt(&self) -> Duration {
        self.observed_at.duration_since(self.started_at)
    }

    pub fn rtt_ms(&self) -> f64 {
        self.rtt().as_secs_f64() * 1000.0
    }
}

pub struct MeasurementLoss {
    pub stream: MeasurementStream,
    pub started_at: Instant,
    pub detected_at: Instant,
}

pub enum MeasurementEvent {
    Observation(MeasurementObservation),
    Loss(MeasurementLoss),
}
