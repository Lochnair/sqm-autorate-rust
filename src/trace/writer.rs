// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use super::v1::{Event, Record};
use flume::Receiver;
use log::warn;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

pub(super) fn spawn<W>(
    writer: W,
    records: Receiver<Record>,
    header: Record,
    healthy: Arc<AtomicBool>,
) -> io::Result<JoinHandle<()>>
where
    W: Write + Send + 'static,
{
    thread::Builder::new()
        .name("trace-v1-writer".to_owned())
        .spawn(move || {
            if let Err(error) = write_records(writer, records, header) {
                warn!("Legacy control trace writer failed; tracing disabled: {error}");
                healthy.store(false, Ordering::Release);
            }
        })
}

fn write_records<W>(mut writer: W, records: Receiver<Record>, header: Record) -> io::Result<()>
where
    W: Write,
{
    write_record(&mut writer, &header)?;

    while let Ok(record) = records.recv() {
        let clean_end = matches!(record.event, Event::End { clean: true });
        write_record(&mut writer, &record)?;

        if clean_end {
            writer.flush()?;
            return Ok(());
        }
    }

    writer.flush()
}

fn write_record(writer: &mut impl Write, record: &Record) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, record).map_err(io::Error::other)?;
    writer.write_all(b"\n")
}
