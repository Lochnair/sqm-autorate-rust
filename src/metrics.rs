use crate::Config;
use crate::config::{MeasurementType, ObservabilityProtocol};
use crate::time::Time;
use log::{error, info, warn};
use rustix::time::ClockId;
use std::fmt::Write;
use std::net::{IpAddr, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::time::Duration;

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
    fn new_udp(host: &str, port: u16) -> anyhow::Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.set_write_timeout(Some(Duration::from_millis(100)))?;
        socket.connect((host, port))?;
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

    fn send(&mut self, data: &str) {
        match self {
            Transport::Udp(socket) => {
                if let Err(e) = socket.send(data.as_bytes()) {
                    warn!("UDP send failed: {}", e);
                }
            }
            Transport::Tcp {
                stream,
                host,
                port,
                reconnect_backoff,
            } => {
                if stream.is_none() {
                    match TcpStream::connect((host.as_str(), *port)) {
                        Ok(s) => {
                            s.set_write_timeout(Some(Duration::from_millis(500))).ok();
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
                // take the stream out, write, put it back or set None on failure
                if let Some(s) = stream {
                    if let Err(e) = std::io::Write::write_all(s, full_data.as_bytes()) {
                        warn!("TCP send failed: {}", e);
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
    tx: Option<SyncSender<(Metric, u64)>>,
    dropped: Arc<AtomicU32>,
}

impl MetricsSender {
    pub fn new(tx: SyncSender<(Metric, u64)>, dropped: Arc<AtomicU32>) -> Self {
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
    pub config: Config,
    pub metrics_dropped: Arc<AtomicU32>,
    pub metrics_rx: Receiver<(Metric, u64)>,
}

impl Metrics {
    pub fn run(self) -> anyhow::Result<()> {
        let host = match &self.config.observability_host {
            Some(h) => h.clone(),
            None => {
                error!("Observability host not configured - metrics exporter disabled");
                return Ok(());
            }
        };

        let port = self.config.observability_port;
        let batch_size = self.config.observability_batch_size;
        let timeout = Duration::from_millis(self.config.observability_batch_timeout_ms);
        let host_tag = &self.config.observability_host_tag.clone();

        let mut transport = match self.config.observability_protocol {
            ObservabilityProtocol::Udp => {
                info!("Metrics exporter configured for UDP to {}:{}", host, port);
                Transport::new_udp(&host, port)?
            }
            ObservabilityProtocol::Tcp => {
                info!("Metrics exporter configured for TCP to {}:{}", host, port);
                Transport::new_tcp(&host, port)
            }
        };

        let mut batch = Vec::with_capacity(batch_size);

        loop {
            // block until first metric or timeout
            match self.metrics_rx.recv_timeout(timeout) {
                Ok(metric) => batch.push(metric),
                Err(RecvTimeoutError::Timeout) => {
                    let dropped = self.metrics_dropped.swap(0, Ordering::Relaxed);
                    if dropped > 0 {
                        let ts = Time::new(ClockId::Realtime).as_nanos();
                        batch.push((Metric::Dropped { count: dropped }, ts));
                    }
                    if !batch.is_empty() {
                        self.flush(&mut transport, &batch, host_tag);
                        batch.clear();
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }

            // greedily drain without blocking
            loop {
                match self.metrics_rx.try_recv() {
                    Ok(metric) => batch.push(metric),
                    Err(_) => break,
                }
                if batch.len() >= batch_size {
                    break;
                }
            }

            if batch.len() >= batch_size {
                self.flush(&mut transport, &batch, host_tag);
                batch.clear();
            }
        }

        Ok(())
    }

    fn flush(&self, transport: &mut Transport, batch: &[(Metric, u64)], host_tag: &str) {
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
                    transport.send(&data);
                }
            }
            Transport::Tcp { .. } => {
                let mut data = String::with_capacity(batch.len() * 300);
                for (metric, timestamp_ns) in batch {
                    self.write_lines(metric, *timestamp_ns, host_tag, &mut data);
                }
                transport.send(&data);
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
                dl_rate,
                ul_rate,
                rx_load,
                tx_load,
                delta_delay_down,
                delta_delay_up,
            } => {
                writeln!(out,
                    "sqm_rate,host={host_tag},direction=download \
                    rate_kbps={dl_rate:.0},load={rx_load:.4},delta_delay={delta_delay_down:.3} {timestamp_ns}i"
                ).unwrap_or(());
                writeln!(out,
                    "sqm_rate,host={host_tag},direction=upload \
                    rate_kbps={ul_rate:.0},load={tx_load:.4},delta_delay={delta_delay_up:.3} {timestamp_ns}i"
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
