// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use crate::measurement::model::MeasurementSource;
use crate::runtime::Direction;
use crate::settings::Settings;
use crate::settings::{ObservabilityProtocol, ObservabilitySettings};
use crate::time::{ClockSample, Realtime};
use log::{error, info, warn};
use std::fmt::Write;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;

const MAX_RECONNECT_BACKOFF: u64 = 60;

enum Transport {
    Udp(UdpSocket),
    Tcp {
        stream: Option<TcpStream>,
        host: String,
        port: u16,
        reconnect_backoff: u64,
    },
}

impl Transport {
    async fn new_udp(host: &str, port: u16) -> anyhow::Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket.connect((host, port)).await?;
        Ok(Transport::Udp(socket))
    }

    fn new_tcp(host: &str, port: u16) -> Self {
        Transport::Tcp {
            stream: None,
            host: host.to_string(),
            port,
            reconnect_backoff: 1,
        }
    }

    async fn send(&mut self, data: &str) {
        match self {
            Transport::Udp(socket) => {
                match tokio::time::timeout(Duration::from_millis(100), socket.send(data.as_bytes()))
                    .await
                {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => warn!("UDP send failed: {e}"),
                    Err(_) => warn!("UDP send timed out"),
                }
            }
            Transport::Tcp {
                stream,
                host,
                port,
                reconnect_backoff,
            } => {
                if stream.is_none() {
                    match TcpStream::connect((host.as_str(), *port)).await {
                        Ok(s) => {
                            info!("Connected to metrics collector at {}:{}", host, port);
                            *stream = Some(s);
                            *reconnect_backoff = 1;
                        }
                        Err(e) => {
                            warn!(
                                "Failed to connect to {}:{} - {}, backoff {}s",
                                host, port, e, reconnect_backoff
                            );
                            *reconnect_backoff =
                                (*reconnect_backoff * 2).min(MAX_RECONNECT_BACKOFF);
                            return;
                        }
                    }
                }

                let full_data = format!("{}\n", data);

                if let Some(s) = stream.as_mut() {
                    let result = tokio::time::timeout(
                        Duration::from_millis(500),
                        s.write_all(full_data.as_bytes()),
                    )
                    .await;

                    if !matches!(result, Ok(Ok(()))) {
                        warn!("TCP send failed: {result:?}");
                        *stream = None;
                        *reconnect_backoff = (*reconnect_backoff * 2).min(MAX_RECONNECT_BACKOFF);
                    }
                }
            }
        }
    }
}

pub enum Metric {
    Ping {
        reflector: IpAddr,
        measurement_type: MeasurementSource,
        rtt: f64,
        up_time: f64,
        down_time: f64,
    },
    Rate {
        direction: Direction,
        rate_kbps: f64,
        load: f64,
        delta_delay: f64,
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
    tx: Option<mpsc::Sender<(Metric, u64)>>,
    dropped: Arc<AtomicU32>,
    export_ping_metrics: bool,
    export_baseline_metrics: bool,
    export_rate_metrics: bool,
    export_events: bool,
}

impl MetricsSender {
    pub fn new(
        tx: mpsc::Sender<(Metric, u64)>,
        dropped: Arc<AtomicU32>,
        settings: &ObservabilitySettings,
    ) -> Self {
        Self {
            tx: Some(tx),
            dropped,
            export_ping_metrics: settings.export_ping_metrics,
            export_baseline_metrics: settings.export_baseline_metrics,
            export_rate_metrics: settings.export_rate_metrics,
            export_events: settings.export_events,
        }
    }

    pub fn disabled() -> Self {
        Self {
            tx: None,
            dropped: Arc::new(AtomicU32::new(0)),
            export_ping_metrics: false,
            export_baseline_metrics: false,
            export_rate_metrics: false,
            export_events: false,
        }
    }

    pub fn send(&self, metric: Metric) {
        let enabled = match &metric {
            Metric::Ping { .. } => self.export_ping_metrics,
            Metric::Baseline { .. } => self.export_baseline_metrics,
            Metric::Rate { .. } => self.export_rate_metrics,
            Metric::Event { .. } => self.export_events,
            Metric::Dropped { .. } => true,
        };
        if !enabled {
            return;
        }

        if let Some(ref tx) = self.tx {
            let ts = ClockSample::<Realtime>::now().as_nanos();
            if tx.try_send((metric, ts)).is_err() {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

pub struct Metrics {
    pub settings: Settings,
    pub metrics_dropped: Arc<AtomicU32>,
    pub metrics_rx: mpsc::Receiver<(Metric, u64)>,
}

impl Metrics {
    pub async fn run(mut self) -> anyhow::Result<()> {
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
                Transport::new_udp(&host, port).await?
            }
            ObservabilityProtocol::Tcp => {
                info!("Metrics exporter configured for TCP to {}:{}", host, port);
                Transport::new_tcp(&host, port)
            }
        };

        let mut batch = Vec::with_capacity(batch_size);

        loop {
            // block until first metric or timeout
            tokio::select! {
                metric = self.metrics_rx.recv() => {
                    match metric {
                        Some(metric) => batch.push(metric),
                        None => break,
                    }
                }

                _ = tokio::time::sleep(timeout) => {
                    let dropped = self.metrics_dropped.swap(0, Ordering::Relaxed);
                    if dropped > 0 {
                        let ts = ClockSample::<Realtime>::now().as_nanos();
                        batch.push((Metric::Dropped { count: dropped }, ts));
                    }
                    if !batch.is_empty() {
                        self.flush(&mut transport, &batch, host_tag).await;
                        batch.clear();
                    }
                    continue;
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
                self.flush(&mut transport, &batch, host_tag).await;
                batch.clear();
            }
        }

        Ok(())
    }

    async fn flush(&self, transport: &mut Transport, batch: &[(Metric, u64)], host_tag: &str) {
        match transport {
            /*
             * the UDP receive buffer might be too small on the receiver to accept batched data,
             * and it'll get cutoff in the middle of a record. so send metrics directly on UDP
             */
            Transport::Udp(_) => {
                let mut data = String::with_capacity(300);
                for (metric, timestamp_ns) in batch {
                    data.clear();
                    self.write_lines(metric, *timestamp_ns, host_tag, &mut data);
                    transport.send(&data).await;
                }
            }
            Transport::Tcp { .. } => {
                let mut data = String::with_capacity(batch.len() * 300);
                for (metric, timestamp_ns) in batch {
                    self.write_lines(metric, *timestamp_ns, host_tag, &mut data);
                }
                transport.send(&data).await;
            }
        }
    }

    fn write_lines(&self, metric: &Metric, timestamp_ns: u64, host_tag: &str, out: &mut String) {
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
                    rtt={rtt:.3},up_time={up_time:.3},down_time={down_time:.3} {timestamp_ns}i"
                )
                .unwrap_or(());
            }
            Metric::Rate {
                direction,
                rate_kbps,
                load,
                delta_delay,
            } => {
                let direction = match direction {
                    Direction::Download => "download",
                    Direction::Upload => "upload",
                };
                writeln!(out,
                    "sqm_rate,host={host_tag},direction={direction} \
                    rate_kbps={rate_kbps:.0},load={load:.4},delta_delay={delta_delay:.3} {timestamp_ns}i"
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
                    baseline_ewma={baseline_up_ewma:.3},recent_ewma={recent_up_ewma:.3} {timestamp_ns}i"
                ).unwrap_or(());
                writeln!(out,
                    "sqm_baseline,host={host_tag},reflector={reflector},direction=down \
                    baseline_ewma={baseline_down_ewma:.3},recent_ewma={recent_down_ewma:.3} {timestamp_ns}i"
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
