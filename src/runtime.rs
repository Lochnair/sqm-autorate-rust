use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use log::{error, warn};
use tokio::{sync::watch, time};

use crate::{
    platform::{InterfaceStats, Platform},
    settings::Settings,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Download,
    Upload,
}

pub(crate) struct Shaper {
    interface: String,

    pub(crate) base_kbps: f64,
    pub(crate) min_kbps: f64,
    pub(crate) max_kbps: Option<f64>,
}

#[derive(Clone, Copy, Debug)]
pub struct TrafficRates {
    pub _sampled_at: Instant,
    pub _generation: u16,
    pub rx_kbps: f64,
    pub tx_kbps: f64,
}

pub struct Runtime {
    platform: Arc<Platform>,

    traffic_interface: String,
    sample_interval: Duration,
    traffic_tx: watch::Sender<Option<TrafficRates>>,

    download: Option<Shaper>,
    upload: Option<Shaper>,
    dry_run: bool,
}

impl Runtime {
    pub(super) fn new(
        platform: Arc<Platform>,
        settings: Settings,
        traffic_tx: watch::Sender<Option<TrafficRates>>,
    ) -> Self {
        Runtime {
            platform,
            traffic_interface: settings.network.upload_interface.clone(),
            sample_interval: Duration::from_secs_f64(settings.advanced_settings.tick_interval),
            traffic_tx,
            download: Some(Shaper {
                interface: settings.network.download_interface.clone(),
                base_kbps: settings.network.download_base_kbits,
                min_kbps: settings.network.download_min_kbits(),
                max_kbps: None,
            }),
            upload: Some(Shaper {
                interface: settings.network.upload_interface.clone(),
                base_kbps: settings.network.upload_base_kbits,
                min_kbps: settings.network.upload_min_kbits(),
                max_kbps: None,
            }),
            dry_run: settings.advanced_settings.dry_run,
        }
    }

    pub(super) async fn run(self: Arc<Self>) -> anyhow::Result<()> {
        let mut generation = 0u16;

        let mut previous = self
            .platform
            .read_stats(&self.traffic_interface)
            .ok()
            .map(|stats| (Instant::now(), stats));

        let mut ticker = time::interval_at(
            time::Instant::now() + self.sample_interval,
            self.sample_interval,
        );

        ticker.set_missed_tick_behavior(time::MissedTickBehavior::Delay);

        loop {
            ticker.tick().await;
            self.sample_traffic(&mut previous, &mut generation);
        }
    }

    pub fn restore_shaper(&self) -> anyhow::Result<()> {
        for direction in [Direction::Download, Direction::Upload] {
            let Some(shaper) = self.shaper(direction) else {
                continue;
            };

            if let Err(e) = self.set_rate(direction, shaper.base_kbps) {
                error!("Unable to restore shaper for {direction:?}: {e:?}");
            }
        }

        Ok(())
    }

    fn sample_traffic(
        &self,
        previous: &mut Option<(Instant, InterfaceStats)>,
        generation: &mut u16,
    ) {
        let stats = match self.platform.read_stats(&self.traffic_interface) {
            Ok(stats) => stats,
            Err(err) => {
                log::warn!("Failed to read interface statistics: {err}");

                if previous.take().is_some() {
                    *generation = generation.wrapping_add(1);
                }

                self.traffic_tx.send_replace(None);
                return;
            }
        };

        let now = Instant::now();

        let Some((last_at, last_stats)) = previous.replace((now, stats)) else {
            self.traffic_tx.send_replace(None);
            return;
        };

        if stats.rx_bytes < last_stats.rx_bytes || stats.tx_bytes < last_stats.tx_bytes {
            *generation = generation.wrapping_add(1);
            self.traffic_tx.send_replace(None);
            return;
        }

        let elapsed = now.duration_since(last_at).as_secs_f64();

        const KBITS_PER_BYTE: f64 = 8.0 / 1_000.0;

        let rates = TrafficRates {
            _sampled_at: now,
            _generation: *generation,
            rx_kbps: (stats.rx_bytes - last_stats.rx_bytes) as f64 * KBITS_PER_BYTE / elapsed,
            tx_kbps: (stats.tx_bytes - last_stats.tx_bytes) as f64 * KBITS_PER_BYTE / elapsed,
        };

        self.traffic_tx.send_replace(Some(rates));
    }

    pub(super) fn shaper(&self, direction: Direction) -> Option<&Shaper> {
        match direction {
            Direction::Download => self.download.as_ref(),
            Direction::Upload => self.upload.as_ref(),
        }
    }

    pub(super) fn set_rate(&self, direction: Direction, rate_kbps: f64) -> anyhow::Result<()> {
        let shaper = self
            .shaper(direction)
            .ok_or_else(|| anyhow::anyhow!("No shaper configured for {direction:?}"))?;

        anyhow::ensure!(
            rate_kbps.is_finite() && rate_kbps >= 0.0,
            "Invalid requested shaper rate: {rate_kbps}"
        );

        if let Some(max_kbps) = shaper.max_kbps
            && rate_kbps > max_kbps
        {
            warn!(
                "Law requested {direction:?} rate above configured maximum: \
                 {rate_kbps} > {max_kbps} kbps"
            );
        }

        if rate_kbps < shaper.min_kbps {
            warn!(
                "Law requested {direction:?} rate below configured minimum: \
                 {rate_kbps} < {} kbps",
                shaper.min_kbps
            );
        }

        self.platform
            .set_rate(&shaper.interface, rate_kbps.round() as u64, self.dry_run)?;

        Ok(())
    }
}
