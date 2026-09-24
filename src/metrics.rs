// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use crate::settings::Settings;
use crate::settings::{MeasurementType, ObservabilityProtocol};
use crate::time::Time;
use flume::{Receiver, RecvTimeoutError, Sender};
use log::{error, info, warn};
use rustix::time::ClockId;
use std::fmt::Write;
use std::net::{IpAddr, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

const MAX_RECONNECT_BACKOFF: u64 = 60;

struct Endpoint {
    host: String,
    port: u16,
    backoff: u64,
    next_attempt: Option<Instant>,
}

impl Endpoint {
    fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            backoff: 1,
            next_attempt: None,
        }
    }

    fn failed(&mut self) -> u64 {
        let delay = self.backoff;
        self.next_attempt = Some(Instant::now() + Duration::from_secs(delay));
        self.backoff = (self.backoff * 2).min(MAX_RECONNECT_BACKOFF);
        delay
    }

    fn connected(&mut self) {
        self.backoff = 1;
        self.next_attempt = None;
    }
}

enum Transport {
    Udp {
        socket: Option<UdpSocket>,
        endpoint: Endpoint,
    },
    Tcp {
        stream: Option<TcpStream>,
        endpoint: Endpoint,
    },
}

impl Transport {
    fn new_udp(host: &str, port: u16) -> Self {
        Transport::Udp {
            socket: None,
            endpoint: Endpoint::new(host, port),
        }
    }

    fn new_tcp(host: &str, port: u16) -> Self {
        Transport::Tcp {
            stream: None,
            endpoint: Endpoint::new(host, port),
        }
    }

    fn send(&mut self, data: &str) {
        match self {
            Transport::Udp { socket, endpoint } => {
                if socket.is_none() {
                    if endpoint.next_attempt.is_some_and(|at| Instant::now() < at) {
                        return;
                    }
                    let opened = UdpSocket::bind("0.0.0.0:0").and_then(|socket| {
                        socket.set_write_timeout(Some(Duration::from_millis(100)))?;
                        socket.connect((endpoint.host.as_str(), endpoint.port))?;
                        Ok(socket)
                    });
                    match opened {
                        Ok(opened) => {
                            *socket = Some(opened);
                            endpoint.connected();
                        }
                        Err(error) => {
                            let delay = endpoint.failed();
                            warn!(
                                "Metrics UDP connect to {}:{} failed: {error}; retrying in {delay}s",
                                endpoint.host, endpoint.port
                            );
                            return;
                        }
                    }
                }
                if let Some(error) = socket
                    .as_ref()
                    .and_then(|socket| socket.send(data.as_bytes()).err())
                {
                    *socket = None;
                    let delay = endpoint.failed();
                    warn!("Metrics UDP send failed: {error}; retrying in {delay}s");
                }
            }
            Transport::Tcp { stream, endpoint } => {
                if stream.is_none() {
                    if endpoint.next_attempt.is_some_and(|at| Instant::now() < at) {
                        return;
                    }
                    match TcpStream::connect((endpoint.host.as_str(), endpoint.port)) {
                        Ok(s) => {
                            s.set_write_timeout(Some(Duration::from_millis(500))).ok();
                            info!(
                                "Connected to metrics collector at {}:{}",
                                endpoint.host, endpoint.port
                            );
                            *stream = Some(s);
                            endpoint.connected();
                        }
                        Err(e) => {
                            let delay = endpoint.failed();
                            warn!(
                                "Metrics TCP connect to {}:{} failed: {e}; retrying in {delay}s",
                                endpoint.host, endpoint.port
                            );
                            return;
                        }
                    }
                }

                let write_error = stream
                    .as_mut()
                    .and_then(|s| std::io::Write::write_all(s, data.as_bytes()).err());
                if let Some(e) = write_error {
                    warn!("TCP send failed: {}", e);
                    *stream = None;
                    endpoint.failed();
                }
            }
        }
    }
}

fn escape_tag_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, ',' | '=' | ' ') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

pub enum Metric {
    Ping {
        reflector: IpAddr,
        measurement_type: MeasurementType,
        rtt: f64,
        up_time: f64,
        down_time: f64,
    },
    Rate {
        dl_rate: f64,
        ul_rate: f64,
        rx_load: f64,
        tx_load: f64,
        delta_delay_down: f64,
        delta_delay_up: f64,
    },
    Baseline {
        reflector: IpAddr,
        baseline_up_ewma: f64,
        baseline_down_ewma: f64,
        recent_up_ewma: f64,
        recent_down_ewma: f64,
    },
    Event {
        name: &'static str,
        reason: &'static str,
        reflector: Option<IpAddr>,
        tags: &'static [(&'static str, &'static str)],
    },
    Dropped {
        count: u32,
    },
}

#[derive(Clone)]
pub struct MetricsSender {
    tx: Option<Sender<(Metric, u64)>>,
    dropped: Arc<AtomicU32>,
}

impl MetricsSender {
    pub fn new(tx: Sender<(Metric, u64)>, dropped: Arc<AtomicU32>) -> Self {
        Self {
            tx: Some(tx),
            dropped,
        }
    }

    pub fn disabled() -> Self {
        Self {
            tx: None,
            dropped: Arc::new(AtomicU32::new(0)),
        }
    }

    pub fn send(&self, metric: Metric) {
        if let Some(ref tx) = self.tx {
            let ts = Time::new(ClockId::Realtime).as_nanos();
            if tx.try_send((metric, ts)).is_err() {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

pub struct Metrics {
    pub settings: Settings,
    pub metrics_dropped: Arc<AtomicU32>,
    pub metrics_rx: Receiver<(Metric, u64)>,
    pub shutdown_requested: Arc<AtomicBool>,
}

impl Metrics {
    pub fn run(self) -> anyhow::Result<()> {
        let host = match &self.settings.observability.host {
            Some(h) => h.clone(),
            None => {
                error!("Observability host not configured - metrics exporter disabled");
                return Ok(());
            }
        };

        let port = self.settings.observability.port;
        let batch_size = self.settings.observability.batch_size;
        let timeout = Duration::from_millis(self.settings.observability.batch_timeout_ms);
        let host_tag = &self.settings.observability.host_tag.clone();

        let mut transport = match self.settings.observability.protocol {
            ObservabilityProtocol::Udp => {
                info!("Metrics exporter configured for UDP to {}:{}", host, port);
                Transport::new_udp(&host, port)
            }
            ObservabilityProtocol::Tcp => {
                info!("Metrics exporter configured for TCP to {}:{}", host, port);
                Transport::new_tcp(&host, port)
            }
        };

        let mut batch = Vec::with_capacity(batch_size);
        let mut shutdown_deadline = None;
        let mut last_flush = Instant::now();

        loop {
            if self.shutdown_requested.load(Ordering::Relaxed) {
                let deadline = shutdown_deadline
                    .get_or_insert_with(|| Instant::now() + Duration::from_millis(400));
                if Instant::now() >= *deadline {
                    if !batch.is_empty() {
                        self.flush(&mut transport, &batch, host_tag);
                    }
                    break;
                }
            }

            match self
                .metrics_rx
                .recv_timeout(timeout.min(Duration::from_millis(100)))
            {
                Ok(metric) => batch.push(metric),
                Err(RecvTimeoutError::Timeout) => {
                    if last_flush.elapsed() < timeout && shutdown_deadline.is_none() {
                        continue;
                    }
                    let dropped = self.metrics_dropped.swap(0, Ordering::Relaxed);
                    if dropped > 0 {
                        let ts = Time::new(ClockId::Realtime).as_nanos();
                        batch.push((Metric::Dropped { count: dropped }, ts));
                    }
                    if !batch.is_empty() {
                        self.flush(&mut transport, &batch, host_tag);
                        batch.clear();
                    }
                    last_flush = Instant::now();
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    if !batch.is_empty() {
                        self.flush(&mut transport, &batch, host_tag);
                    }
                    break;
                }
            }

            // greedily drain without blocking
            while let Ok(metric) = self.metrics_rx.try_recv() {
                batch.push(metric);
                if batch.len() >= batch_size {
                    break;
                }
            }

            if batch.len() >= batch_size {
                self.flush(&mut transport, &batch, host_tag);
                batch.clear();
                last_flush = Instant::now();
            }
        }

        Ok(())
    }

    fn flush(&self, transport: &mut Transport, batch: &[(Metric, u64)], host_tag: &str) {
        let host_tag = escape_tag_value(host_tag);
        match transport {
            /*
             * the UDP receive buffer might be too small on the receiver to accept batched data,
             * and it'll get cutoff in the middle of a record. so send metrics directly on UDP
             */
            Transport::Udp { .. } => {
                let mut data = String::with_capacity(300);
                for (metric, timestamp_ns) in batch {
                    data.clear();
                    Self::write_lines(metric, *timestamp_ns, &host_tag, &mut data);
                    transport.send(&data);
                }
            }
            Transport::Tcp { .. } => {
                let mut data = String::with_capacity(batch.len() * 300);
                for (metric, timestamp_ns) in batch {
                    Self::write_lines(metric, *timestamp_ns, &host_tag, &mut data);
                }
                transport.send(&data);
            }
        }
    }

    fn write_lines(metric: &Metric, timestamp_ns: u64, host_tag: &str, out: &mut String) {
        match metric {
            Metric::Ping {
                reflector,
                measurement_type,
                rtt,
                up_time,
                down_time,
            } => {
                writeln!(
                    out,
                    "sqm_ping,host={host_tag},reflector={reflector},type={measurement_type} \
                    rtt={rtt:.3},up_time={up_time:.3},down_time={down_time:.3} {timestamp_ns}"
                )
                .unwrap_or(());
            }
            Metric::Rate {
                dl_rate,
                ul_rate,
                rx_load,
                tx_load,
                delta_delay_down,
                delta_delay_up,
            } => {
                writeln!(out,
                    "sqm_rate,host={host_tag},direction=download \
                    rate_kbps={dl_rate:.0},load={rx_load:.4},delta_delay={delta_delay_down:.3} {timestamp_ns}"
                ).unwrap_or(());
                writeln!(out,
                    "sqm_rate,host={host_tag},direction=upload \
                    rate_kbps={ul_rate:.0},load={tx_load:.4},delta_delay={delta_delay_up:.3} {timestamp_ns}"
                ).unwrap_or(());
            }
            Metric::Baseline {
                reflector,
                baseline_up_ewma,
                baseline_down_ewma,
                recent_up_ewma,
                recent_down_ewma,
            } => {
                writeln!(out,
                    "sqm_baseline,host={host_tag},reflector={reflector},direction=up \
                    baseline_ewma={baseline_up_ewma:.3},recent_ewma={recent_up_ewma:.3} {timestamp_ns}"
                ).unwrap_or(());
                writeln!(out,
                    "sqm_baseline,host={host_tag},reflector={reflector},direction=down \
                    baseline_ewma={baseline_down_ewma:.3},recent_ewma={recent_down_ewma:.3} {timestamp_ns}"
                ).unwrap_or(());
            }
            Metric::Event {
                name,
                reason,
                reflector,
                tags,
            } => {
                let mut tag_str = format!("host={host_tag},type={name}");

                if let Some(reflector) = reflector {
                    write!(tag_str, ",reflector={reflector}").unwrap_or(());
                }

                if !reason.is_empty() {
                    write!(tag_str, ",reason={reason}").unwrap_or(());
                }

                for (k, v) in *tags {
                    write!(tag_str, ",{k}={v}").unwrap_or(());
                }

                writeln!(out, "sqm_event,{tag_str} count=1i {timestamp_ns}").unwrap_or(());
            }
            Metric::Dropped { count } => {
                writeln!(
                    out,
                    "sqm_metrics_dropped,host={host_tag} count={count}i {timestamp_ns}"
                )
                .unwrap_or(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_protocol_uses_bare_timestamps_and_escaped_tags() {
        let reflector = "192.0.2.1".parse().unwrap();
        let metrics = [
            Metric::Ping {
                reflector,
                measurement_type: MeasurementType::Icmp,
                rtt: 2.0,
                up_time: 1.0,
                down_time: 1.0,
            },
            Metric::Rate {
                dl_rate: 100.0,
                ul_rate: 50.0,
                rx_load: 0.2,
                tx_load: 0.3,
                delta_delay_down: 1.0,
                delta_delay_up: 2.0,
            },
            Metric::Baseline {
                reflector,
                baseline_up_ewma: 1.0,
                baseline_down_ewma: 2.0,
                recent_up_ewma: 1.0,
                recent_down_ewma: 2.0,
            },
            Metric::Event {
                name: "started",
                reason: "",
                reflector: None,
                tags: &[],
            },
            Metric::Dropped { count: 1 },
        ];
        let host = escape_tag_value("a b,c=d");
        for metric in metrics {
            let mut output = String::new();
            Metrics::write_lines(&metric, 123, &host, &mut output);
            for line in output.lines() {
                assert!(line.contains("host=a\\ b\\,c\\=d"), "{line}");
                assert!(line.ends_with(" 123"), "{line}");
            }
        }
    }
}
