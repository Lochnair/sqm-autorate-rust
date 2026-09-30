use std::{
    collections::HashMap,
    io,
    net::IpAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use flume::Sender;
use icmp_socket2::{
    IcmpSocket, IcmpSocket4, Icmpv4Message, Icmpv4Packet,
    packet::{IcmpPacketBuildError, WithEchoRequest, WithTimestampRequest},
};
use thiserror::Error;

use crate::{
    SHUTDOWN,
    measurement::model::{
        MeasurementEvent, MeasurementLoss, MeasurementObservation, MeasurementSource,
        MeasurementStream, MeasurementStreamId, OneWayClock, OneWayLatency, SignedDuration,
    },
    settings::NetworkSettings,
    time::{ClockSample, Realtime},
};

pub struct IcmpBinding {
    pub stream: MeasurementStream,
    pub interval: Duration,

    next_probe_at: Instant,
}

impl IcmpBinding {
    pub fn new(stream: MeasurementStream, interval: Duration) -> Self {
        Self {
            stream,
            interval,
            next_probe_at: Instant::now(),
        }
    }
}

#[derive(Debug, Error)]
pub enum IcmpError {
    #[error("I/O error")]
    Io(#[from] io::Error),

    #[error("failed to build ICMP packet")]
    PacketBuild(#[from] IcmpPacketBuildError),

    #[error("invalid ICMP packet: {0}")]
    InvalidPacket(String),

    #[error("unsupported measurement source: {0:?}")]
    UnsupportedSource(MeasurementSource),

    #[error("probe interval must be non-zero for stream {0:?}")]
    InvalidInterval(MeasurementStreamId),

    #[error(
        "no matching probe found for peer {:?}, source: {:?}, sequence: {}",
        .0.0,
        .0.1,
        .0.2
    )]
    NoMatchingProbe(InFlightProbeKey),

    #[error("wrong identifier: expected {expected}, found {found}")]
    WrongIdentifier { expected: u16, found: u16 },

    #[error("unsupported ICMP peer address: {0}")]
    UnsupportedPeer(IpAddr),

    #[error("event channel closed")]
    EventChannelClosed,
}

#[derive(Debug, Clone, Copy)]
struct InFlightProbe {
    stream: MeasurementStream,
    sent_at: Instant,
}

type InFlightProbeKey = (IpAddr, MeasurementSource, u16);
type InFlightProbes = Mutex<HashMap<InFlightProbeKey, InFlightProbe>>;

struct IcmpSender {
    socket: IcmpSocket4,
    identifier: u16,
    next_sequence: u16,
    bindings: HashMap<MeasurementStreamId, IcmpBinding>,
    inflight: Arc<InFlightProbes>,
}

struct IcmpReceiver {
    socket: IcmpSocket4,
    identifier: u16,
    inflight: Arc<InFlightProbes>,
    observation_tx: Sender<MeasurementEvent>,
}

pub struct IcmpEngine {
    sender: IcmpSender,
    receiver: IcmpReceiver,
}

impl IcmpEngine {
    pub fn new(
        #[allow(unused)] settings: NetworkSettings,
        observation_tx: Sender<MeasurementEvent>,
    ) -> Result<Self, IcmpError> {
        #[allow(unused_mut)]
        let mut rx_socket = IcmpSocket4::new()?;

        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        if let Some(device) = &settings.measurement_bind_device {
            IcmpSocket::bind_device(&mut rx_socket, device)?;
        }

        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        if let Some(mark) = settings.measurement_mark {
            IcmpSocket::set_mark(&mut rx_socket, mark)?;
        }

        #[cfg(target_os = "freebsd")]
        if let Some(fib) = settings.measurement_fib {
            IcmpSocket::set_fib(&mut rx_socket, fib)?;
        }

        rx_socket.enable_receive_metadata()?;
        let tx_socket = rx_socket.try_clone()?;
        let identifier = (std::process::id() & 0xffff) as u16;
        let inflight: Arc<InFlightProbes> = Arc::new(Mutex::new(HashMap::new()));

        Ok(Self {
            sender: IcmpSender {
                socket: tx_socket,
                identifier,
                next_sequence: 0,
                bindings: HashMap::new(),
                inflight: Arc::clone(&inflight),
            },
            receiver: IcmpReceiver {
                socket: rx_socket,
                identifier,
                inflight,
                observation_tx,
            },
        })
    }

    pub fn set_bindings(
        &mut self,
        bindings: impl IntoIterator<Item = IcmpBinding>,
    ) -> Result<(), IcmpError> {
        self.sender.set_bindings(bindings)
    }

    pub fn run(self) -> Result<(), IcmpError> {
        let Self { sender, receiver } = self;

        let sender_handle = thread::spawn(move || {
            let result = sender.run();
            SHUTDOWN.store(true, Ordering::Relaxed);
            result
        });
        let sender_thread = sender_handle.thread().clone();

        let receiver_handle = thread::spawn(move || {
            let result = receiver.run();
            SHUTDOWN.store(true, Ordering::Relaxed);
            sender_thread.unpark();
            result
        });

        let sender_result = sender_handle.join().expect("ICMP sender thread panicked");

        SHUTDOWN.store(true, Ordering::Relaxed);

        let receiver_result = receiver_handle
            .join()
            .expect("ICMP receiver thread panicked");

        sender_result?;
        receiver_result?;

        Ok(())
    }
}

impl IcmpSender {
    fn set_bindings(
        &mut self,
        bindings: impl IntoIterator<Item = IcmpBinding>,
    ) -> Result<(), IcmpError> {
        let now = Instant::now();
        let mut new_bindings = HashMap::new();

        for mut binding in bindings {
            match binding.stream.source {
                MeasurementSource::IcmpEcho | MeasurementSource::IcmpTimestamp => {}
                source => return Err(IcmpError::UnsupportedSource(source)),
            }

            if binding.interval.is_zero() {
                return Err(IcmpError::InvalidInterval(binding.stream.id));
            }

            binding.next_probe_at = now;
            new_bindings.insert(binding.stream.id, binding);
        }

        self.bindings = new_bindings;

        Ok(())
    }

    fn run(mut self) -> Result<(), IcmpError> {
        while !SHUTDOWN.load(Ordering::Relaxed) {
            let now = Instant::now();

            self.send_due_probes(now)?;

            let Some(deadline) = self.next_deadline() else {
                return Ok(());
            };

            let wait = deadline.saturating_duration_since(Instant::now());
            if !wait.is_zero() {
                thread::park_timeout(wait);
            }
        }

        Ok(())
    }

    fn send_due_probes(&mut self, now: Instant) -> Result<(), IcmpError> {
        let due = self
            .bindings
            .iter()
            .filter_map(|(&stream_id, binding)| (binding.next_probe_at <= now).then_some(stream_id))
            .collect::<Vec<_>>();

        for stream_id in due {
            self.send_probe(stream_id)?;

            let binding = self
                .bindings
                .get_mut(&stream_id)
                .expect("due binding disappeared");

            while binding.next_probe_at <= now {
                binding.next_probe_at += binding.interval;
            }
        }

        Ok(())
    }

    fn send_probe(&mut self, stream_id: MeasurementStreamId) -> Result<(), IcmpError> {
        let stream = self
            .bindings
            .get(&stream_id)
            .expect("binding disappeared")
            .stream;

        let IpAddr::V4(peer) = stream.peer else {
            return Err(IcmpError::UnsupportedPeer(stream.peer));
        };

        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);

        let packet = match stream.source {
            MeasurementSource::IcmpEcho => {
                Icmpv4Packet::with_echo_request(self.identifier, sequence, Vec::new())?
            }
            MeasurementSource::IcmpTimestamp => {
                let originate = ClockSample::<Realtime>::now().as_time_since_midnight() as u32;
                Icmpv4Packet::with_timestamp_request(self.identifier, sequence, originate, 0, 0)?
            }
            source => return Err(IcmpError::UnsupportedSource(source)),
        };

        let sent_at = Instant::now();
        let key = (stream.peer, stream.source, sequence);
        let mut inflight = self.inflight.lock().expect("ICMP inflight mutex poisoned");
        inflight.insert(key, InFlightProbe { stream, sent_at });

        if let Err(err) = self.socket.send_to(peer, packet) {
            return Err(err.into());
        }

        Ok(())
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.bindings
            .values()
            .map(|binding| binding.next_probe_at)
            .min()
    }
}

impl IcmpReceiver {
    fn run(mut self) -> Result<(), IcmpError> {
        const RECEIVE_TIMEOUT: Duration = Duration::from_millis(50);
        const HOUSEKEEPING_INTERVAL: Duration = Duration::from_millis(500);

        self.socket.set_timeout(Some(RECEIVE_TIMEOUT));

        let mut next_housekeeping = Instant::now() + HOUSEKEEPING_INTERVAL;

        while !SHUTDOWN.load(Ordering::Relaxed) {
            match self.socket.rcv_from_with_meta() {
                Ok(result) => {
                    match self.handle_reply(
                        result.packet,
                        result.peer.as_socket().unwrap().ip(),
                        result.received_at,
                        result.kernel_rx_timestamp,
                    ) {
                        Ok(observation) => {
                            self.observation_tx
                                .send(MeasurementEvent::Observation(observation))
                                .map_err(|_| IcmpError::EventChannelClosed)?;
                        }
                        Err(
                            IcmpError::WrongIdentifier { .. }
                            | IcmpError::NoMatchingProbe { .. }
                            | IcmpError::InvalidPacket(_),
                        ) => {
                            // Not one of ours, malformed, or a late reply.
                        }
                        Err(err) => return Err(err),
                    }
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(err) => return Err(err.into()),
            }

            let now = Instant::now();

            if now >= next_housekeeping {
                self.expire_inflight(now)?;
                next_housekeeping = now + HOUSEKEEPING_INTERVAL;
            }
        }

        Ok(())
    }

    fn handle_reply(
        &mut self,
        packet: Icmpv4Packet,
        peer: IpAddr,
        observed_at: Instant,
        kernel_rx_timestamp: Option<SystemTime>,
    ) -> Result<MeasurementObservation, IcmpError> {
        match packet.message {
            Icmpv4Message::EchoReply {
                identifier,
                sequence,
                ..
            } => {
                if identifier != self.identifier {
                    return Err(IcmpError::WrongIdentifier {
                        expected: self.identifier,
                        found: identifier,
                    });
                }

                let key = (peer, MeasurementSource::IcmpEcho, sequence);
                let probe = {
                    let mut inflight = self.inflight.lock().expect("ICMP inflight mutex poisoned");

                    inflight
                        .remove(&key)
                        .ok_or(IcmpError::NoMatchingProbe(key))?
                };

                Ok(MeasurementObservation {
                    stream: probe.stream,
                    started_at: probe.sent_at,
                    observed_at,
                    one_way: None,
                })
            }
            Icmpv4Message::TimestampReply {
                identifier,
                sequence,
                originate,
                receive,
                transmit,
            } => {
                const NON_STANDARD_TIMESTAMP: u32 = 1 << 31;

                if identifier != self.identifier {
                    return Err(IcmpError::WrongIdentifier {
                        expected: self.identifier,
                        found: identifier,
                    });
                }

                if receive & NON_STANDARD_TIMESTAMP != 0 || transmit & NON_STANDARD_TIMESTAMP != 0 {
                    return Err(IcmpError::InvalidPacket(
                        "non-standard RFC 792 timestamp".into(),
                    ));
                }

                let key = (peer, MeasurementSource::IcmpTimestamp, sequence);
                let probe = {
                    let mut inflight = self.inflight.lock().expect("ICMP inflight mutex poisoned");

                    inflight
                        .remove(&key)
                        .ok_or(IcmpError::NoMatchingProbe(key))?
                };

                let now_ms = if let Some(timestamp) = kernel_rx_timestamp {
                    ClockSample::<Realtime>::from(timestamp).as_time_since_midnight()
                } else {
                    ClockSample::<Realtime>::now().as_time_since_midnight()
                };

                let uplink_ms = timestamp_delta(receive as i64, originate as i64);
                let downlink_ms = timestamp_delta(now_ms, transmit as i64);

                Ok(MeasurementObservation {
                    stream: probe.stream,
                    started_at: probe.sent_at,
                    observed_at,
                    one_way: Some(OneWayLatency {
                        uplink: SignedDuration::from_millis(uplink_ms),
                        downlink: SignedDuration::from_millis(downlink_ms),
                        clock: OneWayClock::Uncalibrated,
                    }),
                })
            }
            _ => Err(IcmpError::InvalidPacket(
                "unsupported ICMP message type".to_string(),
            )),
        }
    }

    fn expire_inflight(&self, now: Instant) -> Result<(), IcmpError> {
        const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

        let expired = {
            let mut inflight = self.inflight.lock().expect("ICMP inflight mutex poisoned");
            let mut expired = Vec::new();

            inflight.retain(|_, probe| {
                if now.saturating_duration_since(probe.sent_at) >= PROBE_TIMEOUT {
                    expired.push(*probe);
                    false
                } else {
                    true
                }
            });

            expired
        };

        for probe in expired {
            self.observation_tx
                .send(MeasurementEvent::Loss(MeasurementLoss {
                    stream: probe.stream,
                    started_at: probe.sent_at,
                    detected_at: now,
                }))
                .map_err(|_| IcmpError::EventChannelClosed)?;
        }

        Ok(())
    }
}

const MS_PER_DAY: i64 = 86_400_000;

fn timestamp_delta(later: i64, earlier: i64) -> i64 {
    let delta = later - earlier;
    if delta < -MS_PER_DAY / 2 {
        delta + MS_PER_DAY
    } else if delta > MS_PER_DAY / 2 {
        delta - MS_PER_DAY
    } else {
        delta
    }
}
