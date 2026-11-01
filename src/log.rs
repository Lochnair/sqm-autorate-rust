// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use std::sync::OnceLock;

use log::Level;
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};

// Statics are not dropped at process exit. Retaining the guard also retains a
// queue sender: the worker cannot see disconnection or receive Shutdown, so its
// final-flush stderr path is unreachable. Never move this guard into a local.
// Best-effort logging intentionally sacrifices exit flushing to avoid the
// blocking/panicking stdout print in tracing-appender 0.2.5's WorkerGuard::drop.
static LOGGER: OnceLock<(WorkerGuard, Result<(), String>)> = OnceLock::new();

pub fn init(level: Level) -> anyhow::Result<()> {
    let (_, result) = LOGGER.get_or_init(|| {
        let (writer, guard) = NonBlockingBuilder::default()
            .buffered_lines_limit(1024)
            .lossy(true)
            .finish(std::io::stdout());
        let level = match level {
            Level::Error => tracing::Level::ERROR,
            Level::Warn => tracing::Level::WARN,
            Level::Info => tracing::Level::INFO,
            Level::Debug => tracing::Level::DEBUG,
            Level::Trace => tracing::Level::TRACE,
        };
        let result = tracing_subscriber::fmt()
            .with_max_level(level)
            .with_ansi(false)
            .with_target(false)
            .with_file(true)
            .with_line_number(true)
            // Formatter errors must not fall back to synchronous stderr I/O.
            .log_internal_errors(false)
            .with_writer(writer)
            // Also installs the one-way log -> tracing bridge for dependencies.
            .try_init()
            .map_err(|error| error.to_string());
        // Retain the guard even if subscriber/bridge installation failed.
        (guard, result)
    });
    result
        .as_ref()
        .copied()
        .map_err(|error| anyhow::anyhow!("{error}"))
}
