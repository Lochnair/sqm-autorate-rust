// SPDX-FileCopyrightText: 2022-Present Charles Corrigan mailto:chas-iot@runegate.org (github @chas-iot)
// SPDX-FileCopyrightText: 2022-Present Daniel Lakeland mailto:dlakelan@street-artists.org (github @dlakelan)
// SPDX-FileCopyrightText: 2022-Present Mark Baker mailto:mark@vpost.net (github @Fail-Safe)
// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use crate::MeasurementType;
use crate::SHUTDOWN;
use crate::metrics::{Metric, MetricsSender};
use crate::util::{MutexExt, RwLockExt};
use flume::Sender;
use icmp_socket2::socket::IcmpSocket;
use icmp_socket2::{IcmpSocket4, Icmpv4Packet};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use std::{io, thread};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum PingError {
    #[error("Couldn't parse number")]
    InvalidNumber(#[from] io::Error),
    #[error("Error parsing packet")]
    InvalidPacket(String),
    #[error("Invalid packet type")]
    InvalidType(String),
    #[error("Wrong ICMP identifier (expected {expected:?}, found {found:?})")]
    WrongID { expected: u16, found: u16 },
}

pub struct PingReply {
    pub reflector: IpAddr,
    pub measurement_type: MeasurementType,
    pub seq: u16,
    pub rtt: f64,
    pub current_time: i64,
    pub down_time: f64,
    pub up_time: f64,
    pub originate_timestamp: i64,
    pub receive_timestamp: i64,
    pub transmit_timestamp: i64,
    pub last_receive_time_s: Instant,
}

#[derive(Clone, Copy, Debug)]
pub struct InFlightProbe {
    pub sent_at: Instant,
    pub originate_timestamp: i64,
}

pub type InFlightProbeKey = (IpAddr, MeasurementType, u16);
pub type InFlightProbeCache = Arc<Mutex<HashMap<InFlightProbeKey, InFlightProbe>>>;
const INFLIGHT_PROBE_TTL: Duration = Duration::from_secs(30);

fn open_socket(type_: MeasurementType) -> io::Result<IcmpSocket4> {
    match type_ {
        MeasurementType::Icmp | MeasurementType::IcmpTimestamps => IcmpSocket4::new(),
    }
}

pub trait PingListener {
    fn listen(
        &mut self,
        id: u16,
        type_: MeasurementType,
        reflectors_lock: Arc<RwLock<Vec<IpAddr>>>,
        inflight: InFlightProbeCache,
        stats_tx: Sender<PingReply>,
        ping_metrics: MetricsSender,
    ) -> anyhow::Result<()> {
        let socket = &mut open_socket(type_)?;
        socket.set_timeout(Some(Duration::from_millis(500)));
        let mut last_receive_warning = None;

        loop {
            if SHUTDOWN.load(Ordering::Relaxed) {
                info!("Ping listener shutting down");
                return Ok(());
            }
            let (pkt, sender) = match socket.rcv_from() {
                Ok(val) => val,
                Err(error) => {
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) {
                        continue;
                    }
                    if error.raw_os_error().is_some() {
                        if last_receive_warning
                            .is_none_or(|at: Instant| at.elapsed() >= Duration::from_secs(30))
                        {
                            warn!("ICMP receive failed: {error}");
                            last_receive_warning = Some(Instant::now());
                        }
                        thread::sleep(Duration::from_millis(200));
                    }
                    continue;
                }
            };

            let addr: IpAddr = sender.as_socket().unwrap().ip();

            let reflectors = reflectors_lock.read_anyhow()?;
            if !reflectors.contains(&addr) {
                continue;
            }

            let reply = match self.parse_packet(id, addr, type_, pkt) {
                Ok(val) => val,
                Err(_) => {
                    // parse_packet will throw an error if it's an unknown protocol etc.
                    // so just quietly move on
                    continue;
                }
            };

            let key = (addr, type_, reply.seq);
            let Some(probe) = inflight.lock_anyhow()?.remove(&key) else {
                continue;
            };
            if reply.originate_timestamp != probe.originate_timestamp {
                continue;
            }

            ping_metrics.send(Metric::Ping {
                reflector: reply.reflector,
                measurement_type: reply.measurement_type,
                rtt: reply.rtt,
                up_time: reply.up_time,
                down_time: reply.down_time,
            });

            debug!(
                "Type: {:4}  | Reflector IP: {:>15}  | Seq: {:5}  | Current time: {:8}  |  Originate: {:8}  |  Received time: {:8}  |  Transmit time : {:8}  |  RTT: {:8}  | UL time: {:5}  | DL time: {:5}",
                "ICMP",
                addr.to_string(),
                reply.seq,
                reply.current_time,
                reply.originate_timestamp,
                reply.receive_timestamp,
                reply.transmit_timestamp,
                reply.rtt,
                reply.up_time,
                reply.down_time
            );
            if let Err(e) = stats_tx.send(reply) {
                warn!("Stats channel closed, stopping listener: {}", e);
                break Ok(());
            }
        }
    }

    fn parse_packet(
        &self,
        id: u16,
        reflector: IpAddr,
        measurement_type: MeasurementType,
        pkt: Icmpv4Packet,
    ) -> Result<PingReply, PingError>;
}

pub trait PingSender {
    fn send(
        &mut self,
        id: u16,
        type_: MeasurementType,
        reflectors_lock: Arc<RwLock<Vec<IpAddr>>>,
        inflight: InFlightProbeCache,
        tick_interval: f64,
    ) -> anyhow::Result<()> {
        let mut socket = open_socket(type_)?;

        let mut seq: u16 = 0;
        let tick_duration = Duration::from_secs_f64(tick_interval);
        let mut last_send_warning = None;

        loop {
            if SHUTDOWN.load(Ordering::Relaxed) {
                info!("Ping sender shutting down");
                return Ok(());
            }
            // Release the read lock before sending so reselection can update peers.
            let reflectors_unlocked = reflectors_lock.read_anyhow()?;
            let reflectors: Vec<_> = reflectors_unlocked
                .iter()
                .filter_map(|reflector| match reflector {
                    IpAddr::V4(addr) => Some(*addr),
                    IpAddr::V6(_) => None,
                })
                .collect();
            drop(reflectors_unlocked);

            if reflectors.is_empty() {
                thread::sleep(tick_duration);
                continue;
            }

            inflight
                .lock_anyhow()?
                .retain(|_, probe| probe.sent_at.elapsed() < INFLIGHT_PROBE_TTL);

            let sleep_duration = Duration::from_secs_f64(tick_interval / reflectors.len() as f64);

            for &addr in &reflectors {
                let (packet, originate_timestamp) = self.craft_packet(id, seq);
                let sent_at = Instant::now();
                let key = (IpAddr::V4(addr), type_, seq);
                inflight.lock_anyhow()?.insert(
                    key,
                    InFlightProbe {
                        sent_at,
                        originate_timestamp,
                    },
                );
                if let Err(e) = socket.send_to(addr, packet) {
                    inflight.lock_anyhow()?.remove(&key);
                    if matches!(
                        e.raw_os_error(),
                        Some(
                            libc::ENETUNREACH
                                | libc::EHOSTUNREACH
                                | libc::ENETDOWN
                                | libc::ENOBUFS
                                | libc::EADDRNOTAVAIL
                        )
                    ) {
                        if last_send_warning
                            .is_none_or(|at: Instant| at.elapsed() >= Duration::from_secs(30))
                        {
                            warn!("ICMP send failed during network outage: {e}");
                            last_send_warning = Some(Instant::now());
                        }
                    } else {
                        return Err(e.into());
                    }
                }
                thread::sleep(sleep_duration);
            }

            seq = seq.wrapping_add(1);
        }
    }

    fn craft_packet(&self, id: u16, seq: u16) -> (icmp_socket2::packet::Icmpv4Packet, i64);
}
