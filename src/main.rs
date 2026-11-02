// SPDX-FileCopyrightText: 2022-Present Charles Corrigan mailto:chas-iot@runegate.org (github @chas-iot)
// SPDX-FileCopyrightText: 2022-Present Daniel Lakeland mailto:dlakelan@street-artists.org (github @dlakelan)
// SPDX-FileCopyrightText: 2022-Present Mark Baker mailto:mark@vpost.net (github @Fail-Safe)
// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

extern crate core;

mod app;
mod controller;
mod diagnostics;
mod error;
mod law;
mod log;
mod measurement;
mod metrics;
mod platform;
mod reflector_selector;
mod reflector_setup;
mod runtime;
mod settings;
mod time;
mod util;

use crate::app::App;
use crate::platform::warn_platform_limitations;
use crate::settings::Settings;
use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> anyhow::Result<ExitCode> {
    // Initialization errors return before any shaper can be modified.
    log::init()?;
    log::message(format_args!("Starting unRAVEL version {}", VERSION));

    let result = async {
        let settings = Settings::load()?;
        ::log::set_max_level(settings.output.log_level.to_level_filter());
        warn_platform_limitations();

        App::prepare(settings).await?.run().await
    }
    .await;

    // App::run has already completed shaper restoration and worker cleanup.
    log::finish(result.as_ref().err());
    Ok(if result.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
