use log::{error, info, warn};

use crate::Config;
use crate::config::{MeasurementType, ObservabilityProtocol};
use std::fmt::Write;
use std::net::{IpAddr, TcpStream, UdpSocket};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread::sleep;
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
        timestamp_ns: u64,
    },
    Rate {
        dl_rate: f64,
        ul_rate: f64,
        rx_load: f64,
        tx_load: f64,
        delta_delay_down: f64,
        delta_delay_up: f64,
        timestamp_ns: u64,
    },
    Baseline {
        reflector: IpAddr,
        baseline_up_ewma: f64,
        baseline_down_ewma: f64,
        recent_up_ewma: f64,
        recent_down_ewma: f64,
        timestamp_ns: u64,
    },
    Event {
        name: &'static str,
        reason: &'static str,
        reflector: Option<IpAddr>,
        timestamp_ns: u64,
    },
}

pub struct Metrics {
    pub config: Config,
    pub metrics_receiver: Receiver<Metric>,
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
            match self.metrics_receiver.recv_timeout(timeout) {
                Ok(metric) => batch.push(metric),
                Err(RecvTimeoutError::Timeout) => {
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
                match self.metrics_receiver.try_recv() {
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

    fn flush(&self, transport: &mut Transport, batch: &[Metric], host_tag: &str) {
        let lines: Vec<String> = batch
            .iter()
            .map(|m| self.to_influx_line(m, host_tag))
            .collect();

        let data = lines.join("\n");
        transport.send(&data);
    }

    pub fn to_influx_line(&self, metric: &Metric, host_tag: &str) -> String {
        match metric {
            Metric::Ping {
                reflector,
                measurement_type,
                rtt,
                up_time,
                down_time,
                timestamp_ns,
            } => format!(
                "sqm_ping,host={host_tag},reflector={reflector},type={measurement_type} \
                         rtt={rtt:.3},up_time={up_time:.3},down_time={down_time:.3} {timestamp_ns}i"
            ),
            Metric::Rate {
                dl_rate,
                ul_rate,
                rx_load,
                tx_load,
                delta_delay_down,
                delta_delay_up,
                timestamp_ns,
            } => format!(
                "sqm_rate,host={host_tag},direction=download \
                         rate_kbps={dl:.0},load={rx_load:.4},delta_delay={delta_delay_down:.3} {timestamp_ns}i\n\
                         sqm_rate,host={host_tag},direction=upload \
                         rate_kbps={ul:.0},load={tx_load:.4},delta_delay={delta_delay_up:.3} {timestamp_ns}i",
                dl = dl_rate,
                ul = ul_rate,
            ),
            Metric::Baseline {
                reflector,
                baseline_up_ewma,
                baseline_down_ewma,
                recent_up_ewma,
                recent_down_ewma,
                timestamp_ns,
            } => format!(
                "sqm_baseline,host={host_tag},reflector={reflector},direction=up baseline_ewma={baseline_up_ewma:.3},recent_ewma={recent_up_ewma:.3} {timestamp_ns}i\n
                sqm_baseline,host={host_tag},reflector={reflector},direction=down baseline_ewma={baseline_down_ewma:.3},recent_ewma={recent_down_ewma:.3} {timestamp_ns}i"
            ),
            Metric::Event {
                name,
                reason,
                reflector,
                timestamp_ns,
            } => {
                let mut tags = format!("host={host_tag},type={name}");

                if let Some(reflector) = reflector {
                    write!(tags, ",reflector={reflector}").unwrap_or(());
                }

                if !reason.is_empty() {
                    write!(tags, ",reason={reason}").unwrap_or(());
                }
                format!("sqm_event,{tags} count=1i {timestamp_ns}")
            },
        }
    }
}
