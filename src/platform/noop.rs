// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use crate::platform::Platform;
use log::debug;
use std::convert::Infallible;

impl Platform {
    pub(crate) fn validate_shaper(&self, _interface: &str) -> Result<(), Infallible> {
        Ok(())
    }

    pub(crate) fn set_rate(
        &self,
        interface: &str,
        bandwidth_kbit: u64,
        dry_run: bool,
    ) -> Result<(), Infallible> {
        debug!(
            "Ignoring traffic-control rate request of {} kbit/s for interface {} (dry_run={})",
            bandwidth_kbit, interface, dry_run
        );
        Ok(())
    }
}
