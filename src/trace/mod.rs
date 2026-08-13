// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

pub(crate) mod v1;

#[cfg(all(feature = "trace", test))]
mod characterization;
#[cfg(all(feature = "trace", test))]
mod reader;

#[cfg(feature = "trace")]
mod writer;

use crate::settings::Settings;
use std::time::Instant;

#[cfg(feature = "trace")]
use std::time::SystemTime;

#[cfg(feature = "trace")]
use flume::Sender;
#[cfg(feature = "trace")]
use log::warn;
#[cfg(feature = "trace")]
use std::fs::File;
#[cfg(feature = "trace")]
use std::io::{BufWriter, Write};
#[cfg(feature = "trace")]
use std::path::Path;
#[cfg(feature = "trace")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "trace")]
use std::sync::{Arc, Mutex};
#[cfg(feature = "trace")]
use std::thread::JoinHandle;

const TRACE_PATH_ENV: &str = "SQMA_TRACE_V1_PATH";
#[cfg(feature = "trace")]
const QUEUE_CAPACITY: usize = 4_096;

#[derive(Clone, Default)]
pub(crate) struct Recorder {
    #[cfg(feature = "trace")]
    inner: Option<Arc<Inner>>,
}

#[cfg(feature = "trace")]
struct Inner {
    origin: Instant,
    records: Sender<v1::Record>,
    next_seq: Mutex<u64>,
    healthy: Arc<AtomicBool>,
    writer: Mutex<Option<JoinHandle<()>>>,
}

pub(crate) struct OrderedRecorder<'a> {
    #[cfg(feature = "trace")]
    inner: Option<&'a Inner>,
    #[cfg(feature = "trace")]
    next_seq: Option<&'a mut u64>,
    #[cfg(not(feature = "trace"))]
    marker: std::marker::PhantomData<&'a ()>,
}

impl Recorder {
    pub(crate) fn from_env(origin: Instant, settings: &Settings) -> Self {
        let Some(path) = std::env::var_os(TRACE_PATH_ENV) else {
            return Self::disabled();
        };

        #[cfg(feature = "trace")]
        {
            let header = v1::Event::Header {
                format: v1::FORMAT_NAME.to_owned(),
                version: v1::FORMAT_VERSION,
                program_version: env!("CARGO_PKG_VERSION").to_owned(),
                control: v1::ControlConfig::from_settings(settings),
            };
            Self::start_path(Path::new(&path), origin, header)
        }

        #[cfg(not(feature = "trace"))]
        {
            let _ = (origin, settings, path);
            log::warn!(
                "{TRACE_PATH_ENV} is set, but this binary was built without the `trace` feature"
            );
            Self::disabled()
        }
    }

    pub(crate) fn disabled() -> Self {
        Self::default()
    }

    pub(crate) fn is_enabled(&self) -> bool {
        #[cfg(feature = "trace")]
        {
            self.inner
                .as_ref()
                .is_some_and(|inner| inner.healthy.load(Ordering::Acquire))
        }

        #[cfg(not(feature = "trace"))]
        {
            false
        }
    }

    pub(crate) fn monotonic_ns(&self, instant: Instant) -> u64 {
        #[cfg(feature = "trace")]
        {
            self.inner
                .as_ref()
                .map_or(0, |inner| v1::monotonic_ns(inner.origin, instant))
        }

        #[cfg(not(feature = "trace"))]
        {
            let _ = instant;
            0
        }
    }

    pub(crate) fn record_at(&self, instant: Instant, event: v1::Event) -> Option<u64> {
        self.linearize(|recorder| recorder.record_at(instant, event))
    }

    pub(crate) fn linearize<R>(&self, callback: impl FnOnce(&mut OrderedRecorder<'_>) -> R) -> R {
        #[cfg(feature = "trace")]
        {
            if self
                .inner
                .as_ref()
                .is_some_and(|inner| inner.healthy.load(Ordering::Acquire))
            {
                let inner = self
                    .inner
                    .as_ref()
                    .expect("healthy trace recorder must have inner state");
                match inner.next_seq.lock() {
                    Ok(mut next_seq) => {
                        if inner.healthy.load(Ordering::Acquire) {
                            let mut recorder = OrderedRecorder {
                                inner: Some(inner),
                                next_seq: Some(&mut next_seq),
                            };
                            return callback(&mut recorder);
                        }
                    }
                    Err(_) => {
                        if inner.healthy.swap(false, Ordering::AcqRel) {
                            warn!("Legacy control trace sequencing lock failed; tracing disabled");
                        }
                    }
                }
            }

            let mut recorder = OrderedRecorder {
                inner: None,
                next_seq: None,
            };
            callback(&mut recorder)
        }

        #[cfg(not(feature = "trace"))]
        {
            let mut recorder = OrderedRecorder {
                marker: std::marker::PhantomData,
            };
            callback(&mut recorder)
        }
    }

    pub(crate) fn finish(&self) {
        #[cfg(feature = "trace")]
        if let Some(inner) = self.inner.as_ref() {
            self.linearize(|recorder| {
                recorder.record_at(Instant::now(), v1::Event::End { clean: true });
                inner.healthy.store(false, Ordering::Release);
            });

            let writer = match inner.writer.lock() {
                Ok(mut writer) => writer.take(),
                Err(_) => {
                    warn!("Legacy control trace writer lock failed during shutdown");
                    None
                }
            };
            let writer_panicked = writer.map(|writer| writer.join().is_err()).unwrap_or(false);
            if writer_panicked {
                warn!("Legacy control trace writer panicked during shutdown");
            }
        }
    }

    #[cfg(feature = "trace")]
    fn start_path(path: &Path, origin: Instant, header: v1::Event) -> Self {
        let file = match File::options()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
        {
            Ok(file) => file,
            Err(error) => {
                warn!(
                    "Failed to open legacy control trace at {}: {error}; tracing disabled",
                    path.display()
                );
                return Self::disabled();
            }
        };

        Self::start_writer(BufWriter::new(file), origin, header)
    }

    #[cfg(feature = "trace")]
    fn start_writer<W>(writer: W, origin: Instant, header: v1::Event) -> Self
    where
        W: Write + Send + 'static,
    {
        let (records, record_rx) = flume::bounded(QUEUE_CAPACITY);
        let healthy = Arc::new(AtomicBool::new(true));
        let header = v1::Record {
            seq: 0,
            timestamp: v1::timestamp(origin, origin, SystemTime::now()),
            event: header,
        };
        let writer = match writer::spawn(writer, record_rx, header, Arc::clone(&healthy)) {
            Ok(writer) => writer,
            Err(error) => {
                warn!("Failed to start legacy control trace writer: {error}; tracing disabled");
                return Self::disabled();
            }
        };

        Self {
            inner: Some(Arc::new(Inner {
                origin,
                records,
                next_seq: Mutex::new(1),
                healthy,
                writer: Mutex::new(Some(writer)),
            })),
        }
    }
}

impl OrderedRecorder<'_> {
    pub(crate) fn is_enabled(&self) -> bool {
        #[cfg(feature = "trace")]
        {
            self.inner.is_some()
        }

        #[cfg(not(feature = "trace"))]
        {
            false
        }
    }

    pub(crate) fn record_at(&mut self, instant: Instant, event: v1::Event) -> Option<u64> {
        #[cfg(feature = "trace")]
        {
            let inner = self.inner?;
            let next_seq = self.next_seq.as_deref_mut()?;
            let seq = *next_seq;
            let record = v1::Record {
                seq,
                timestamp: v1::timestamp(inner.origin, instant, SystemTime::now()),
                event,
            };

            if inner.records.send(record).is_err() {
                if inner.healthy.swap(false, Ordering::AcqRel) {
                    warn!("Legacy control trace channel closed; tracing disabled");
                }
                return None;
            }

            *next_seq = (*next_seq).saturating_add(1);
            Some(seq)
        }

        #[cfg(not(feature = "trace"))]
        {
            let _ = (instant, event);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_recorder_is_a_no_op() {
        let recorder = Recorder::disabled();

        assert!(!recorder.is_enabled());
        assert_eq!(
            recorder.record_at(Instant::now(), v1::Event::End { clean: true }),
            None
        );
        recorder.finish();
    }

    #[cfg(feature = "trace")]
    mod enabled {
        use super::*;
        use std::io;
        use std::net::{IpAddr, Ipv4Addr};
        use std::path::PathBuf;
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::thread;

        static NEXT_PATH_ID: AtomicU64 = AtomicU64::new(0);

        struct FailingWriter;

        impl Write for FailingWriter {
            fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("injected writer failure"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::other("injected writer failure"))
            }
        }

        fn temp_path() -> PathBuf {
            let id = NEXT_PATH_ID.fetch_add(1, Ordering::Relaxed);
            std::env::temp_dir().join(format!(
                "sqm-autorate-trace-v1-{}-{id}.jsonl",
                std::process::id()
            ))
        }

        fn ping_event(reflector: Ipv4Addr, value: f64) -> v1::Event {
            v1::Event::PingReply {
                reflector: IpAddr::V4(reflector),
                down_time_ms: value,
                up_time_ms: value,
            }
        }

        #[test]
        fn writer_preserves_global_sequence_order_from_multiple_producers() {
            let path = temp_path();
            let origin = Instant::now();
            let recorder = Recorder::start_path(&path, origin, v1::tests::header_event());
            let barrier = Arc::new(Barrier::new(3));
            let mut producers = Vec::new();

            for producer in 1..=2 {
                let recorder = recorder.clone();
                let barrier = Arc::clone(&barrier);
                producers.push(thread::spawn(move || {
                    barrier.wait();
                    for value in 0..50 {
                        recorder.record_at(
                            origin,
                            ping_event(Ipv4Addr::new(192, 0, 2, producer), value as f64),
                        );
                    }
                }));
            }

            barrier.wait();
            for producer in producers {
                producer.join().unwrap();
            }
            recorder.finish();

            let contents = std::fs::read_to_string(&path).unwrap();
            let records = contents
                .lines()
                .map(|line| super::reader::parse_record_v1(line).unwrap())
                .collect::<Vec<_>>();

            assert_eq!(records.len(), 102);
            assert!(matches!(
                &records.first().unwrap().event,
                v1::Event::Header { .. }
            ));
            assert!(matches!(
                &records.last().unwrap().event,
                v1::Event::End { clean: true }
            ));
            for (expected, record) in records.iter().enumerate() {
                assert_eq!(record.seq, expected as u64);
            }

            std::fs::remove_file(path).unwrap();
        }

        #[test]
        fn writer_failure_disables_recorder_without_propagating() {
            let recorder =
                Recorder::start_writer(FailingWriter, Instant::now(), v1::tests::header_event());

            recorder.record_at(Instant::now(), ping_event(Ipv4Addr::new(192, 0, 2, 1), 1.0));
            recorder.finish();

            assert!(!recorder.is_enabled());
        }

        #[test]
        fn open_failure_returns_disabled_recorder() {
            let path = temp_path().join("missing-parent").join("trace.jsonl");
            let recorder = Recorder::start_path(&path, Instant::now(), v1::tests::header_event());

            assert!(!recorder.is_enabled());
            recorder.record_at(Instant::now(), v1::Event::End { clean: true });
            recorder.finish();
        }
    }
}
