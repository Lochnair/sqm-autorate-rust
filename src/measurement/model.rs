use std::{
    net::IpAddr,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MeasurementStreamId(pub u64);

#[allow(dead_code)]
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

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OneWayClock {
    Uncalibrated,
    Calibrated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeSchedule {
    pub period: Duration,
    pub offset: Duration,
}

impl ProbeSchedule {
    pub(crate) fn next_probe_at(&self, origin: Instant, now: Instant) -> Instant {
        const NANOS_PER_SEC: u128 = 1_000_000_000;

        let first = origin + self.offset;

        if now < first {
            return first;
        }

        let elapsed_ns = now.duration_since(first).as_nanos();
        let period_ns = self.period.as_nanos();
        let remainder = elapsed_ns % period_ns;

        let until_next = if remainder == 0 {
            period_ns
        } else {
            period_ns - remainder
        };

        let until_next = Duration::new(
            (until_next / NANOS_PER_SEC) as u64,
            (until_next % NANOS_PER_SEC) as u32,
        );

        now + until_next
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SignedDuration {
    nanos: i64,
}

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
impl MeasurementObservation {
    pub fn rtt(&self) -> Duration {
        self.observed_at.duration_since(self.started_at)
    }

    pub fn rtt_ms(&self) -> f64 {
        self.rtt().as_secs_f64() * 1000.0
    }
}

#[allow(dead_code)]
pub struct MeasurementLoss {
    pub stream: MeasurementStream,
    pub started_at: Instant,
    pub detected_at: Instant,
}

#[allow(dead_code)]
pub enum MeasurementEvent {
    Observation(MeasurementObservation),
    Loss(MeasurementLoss),
}
