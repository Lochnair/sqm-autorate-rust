// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use std::{
    fmt,
    io::{self, Write},
    sync::{OnceLock, mpsc},
    thread,
    time::{Duration, Instant},
};

use log::{LevelFilter, Metadata, Record};

use crate::time::{ClockSample, Realtime};

static LOGGER: OnceLock<SimpleLogger> = OnceLock::new();

enum Message {
    Line(String),
    Finish(Option<String>, mpsc::SyncSender<()>),
}

struct SimpleLogger {
    sender: mpsc::SyncSender<Message>,
}

impl log::Log for SimpleLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let now = ClockSample::<Realtime>::now();
            message(format_args!(
                "{} {:5} {}:{}: {}",
                now.as_secs_f64(),
                record.level(),
                record.file().unwrap_or("<unknown>"),
                record.line().unwrap_or(0),
                record.args()
            ));
        }
    }

    // Best effort: never wait for a blocked writer, including during shutdown.
    fn flush(&self) {}
}

/// Unfiltered startup messages and configuration warnings share the same
/// lossy queue. Warnings remain visible even when the configured level is error.
pub(crate) fn message(args: fmt::Arguments<'_>) {
    if let Some(logger) = LOGGER.get() {
        // Full and disconnected queues both discard the incoming message.
        let _ = logger.sender.try_send(Message::Line(format!("{args}\n")));
    }
}

/// Called only after preparation fails or App cleanup completes. One deadline
/// covers queue admission, pending writes, the final error, and flushing.
pub(crate) fn finish(error: Option<&anyhow::Error>) {
    let deadline = Instant::now() + Duration::from_secs(2);
    let Some(logger) = LOGGER.get() else { return };
    let (done, completion) = mpsc::sync_channel(1);
    let mut message = Message::Finish(error.map(|error| format!("Error: {error:#}\n")), done);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        match logger.sender.try_send(message) {
            Ok(()) => break,
            Err(mpsc::TrySendError::Disconnected(_)) => return,
            Err(mpsc::TrySendError::Full(pending)) => {
                message = pending;
                // std's bounded sender has no stable timed send. Retry only on
                // this exit path, under the same deadline as the acknowledgment.
                thread::park_timeout(remaining.min(Duration::from_millis(1)));
            }
        }
    }
    let _ = completion.recv_timeout(deadline.saturating_duration_since(Instant::now()));
}

pub(crate) fn init() -> anyhow::Result<()> {
    let (sender, receiver) = mpsc::sync_channel::<Message>(1024);
    let worker = thread::Builder::new()
        .name("unravel-logger".to_owned())
        .spawn(move || {
            let mut stdout = io::stdout().lock();
            while let Ok(message) = receiver.recv() {
                match message {
                    Message::Line(line) => {
                        if stdout.write_all(line.as_bytes()).is_err() {
                            break;
                        }
                    }
                    Message::Finish(error, done) => {
                        if let Some(error) = error
                            && stdout.write_all(error.as_bytes()).is_err()
                        {
                            break;
                        }
                        if stdout.flush().is_ok() {
                            let _ = done.try_send(());
                        }
                        break;
                    }
                }
            }
        })?;
    // Process-lifetime facility: detaching is intentional. A stopped stdout
    // reader must not delay CAKE restoration or indefinitely prevent exit.
    // finish() may time out; the worker is never joined.
    drop(worker);

    log::set_logger(LOGGER.get_or_init(|| SimpleLogger { sender }))?;
    log::set_max_level(LevelFilter::Error);
    Ok(())
}
