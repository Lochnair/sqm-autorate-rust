//! Best-effort CSV diagnostics, independent of the metrics exporter.
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;
use tokio::{fs::File, io::AsyncWriteExt, sync::mpsc};

use crate::{
    settings::OutputSettings,
    time::{ClockSample, Realtime},
};

const QUEUE_CAPACITY: usize = 64;
const HISTORY_INTERVAL: Duration = Duration::from_secs(300);
const STATS_HEADER: &str = "times,timens,rxload,txload,deltadelaydown,deltadelayup,dlrate,uprate\n";
const HISTORY_HEADER: &str = "time,counter,upspeed,downspeed\n";

#[derive(Debug)]
pub(crate) struct DirectionStatistics {
    pub load: f64,
    pub delta_ms: f64,
    pub rate_kbps: f64,
}

#[derive(Debug)]
pub(crate) struct SpeedHistory {
    pub download: Option<Vec<f64>>,
    pub upload: Option<Vec<f64>>,
}

#[derive(Debug)]
pub(crate) struct DiagnosticRecord {
    pub time: SystemTime,
    pub download: Option<DirectionStatistics>,
    pub upload: Option<DirectionStatistics>,
    pub history: Option<SpeedHistory>,
}

pub(crate) struct DiagnosticsSender {
    tx: mpsc::Sender<DiagnosticRecord>,
    last_history: Instant,
}

impl DiagnosticsSender {
    pub fn publish(&mut self, now: Instant, record: impl FnOnce(bool) -> DiagnosticRecord) {
        let Ok(permit) = self.tx.try_reserve() else {
            return;
        };
        let history_due = now.duration_since(self.last_history) >= HISTORY_INTERVAL;
        permit.send(record(history_due));
        if history_due {
            self.last_history = now;
        }
    }
}

pub(crate) struct CsvWriter {
    output: OutputSettings,
    rx: mpsc::Receiver<DiagnosticRecord>,
}

impl CsvWriter {
    pub fn new(output: &OutputSettings) -> Option<(DiagnosticsSender, Self)> {
        if output.suppress_statistics {
            return None;
        }
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        Some((
            DiagnosticsSender {
                tx,
                last_history: Instant::now(),
            },
            Self {
                output: output.clone(),
                rx,
            },
        ))
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        let mut history = create(&self.output.speed_hist_file, HISTORY_HEADER).await?;
        let mut stats = create(&self.output.stats_file, STATS_HEADER).await?;
        while let Some(record) = self.rx.recv().await {
            if let Err(error) = stats.write_all(format_statistics(&record).as_bytes()).await {
                log::warn!("Failed to write statistics: {error}");
            }
            if let Some(snapshot) = &record.history
                && let Err(error) = history
                    .write_all(format_history(record.time, snapshot).as_bytes())
                    .await
            {
                log::warn!("Failed to write speed history file: {error}");
            }
        }
        // Normal application shutdown still follows the existing worker-abort policy.
        stats.flush().await?;
        history.flush().await?;
        Ok(())
    }
}

async fn create(path: &str, header: &str) -> anyhow::Result<File> {
    let mut file = File::create(path)
        .await
        .with_context(|| format!("Creating CSV diagnostics {path}"))?;
    file.write_all(header.as_bytes()).await?;
    file.flush().await?;
    Ok(file)
}

fn format_statistics(record: &DiagnosticRecord) -> String {
    let time = ClockSample::<Realtime>::from(record.time);
    let down = record.download.as_ref();
    let up = record.upload.as_ref();
    format!(
        "{},{},{},{},{},{},{},{}\n",
        time.secs(),
        time.nsecs(),
        down.map_or(f64::NAN, |s| s.load),
        up.map_or(f64::NAN, |s| s.load),
        down.map_or(f64::NAN, |s| s.delta_ms),
        up.map_or(f64::NAN, |s| s.delta_ms),
        down.map_or_else(|| "NaN".to_owned(), |s| (s.rate_kbps as u64).to_string()),
        up.map_or_else(|| "NaN".to_owned(), |s| (s.rate_kbps as u64).to_string()),
    )
}

fn format_history(time: SystemTime, history: &SpeedHistory) -> String {
    use std::fmt::Write;
    let time = ClockSample::<Realtime>::from(time).as_secs_f64();
    let down = history.download.as_deref().unwrap_or_default();
    let up = history.upload.as_deref().unwrap_or_default();
    let mut rows = String::new();
    for i in 0..down.len().max(up.len()) {
        writeln!(
            rows,
            "{},{},{},{}",
            time,
            i,
            up.get(i).copied().unwrap_or(f64::NAN),
            down.get(i).copied().unwrap_or(f64::NAN)
        )
        .expect("writing to String");
    }
    rows
}
