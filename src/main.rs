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

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Starting sqm-autorate-rust version {}", VERSION);

    let settings = Settings::load()?;

    log::init(settings.output.log_level)?;
    warn_platform_limitations();

    App::prepare(settings).await?.run().await
}
