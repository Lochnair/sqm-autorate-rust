// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use crate::platform::TrafficControlBackend;
use log::debug;
use std::convert::Infallible;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NoopShaper {
    interface: String,
}

#[derive(Debug, Default)]
pub(crate) struct NoopTrafficControl;

impl TrafficControlBackend for NoopTrafficControl {
    type Error = Infallible;
    type Handle = NoopShaper;

    fn find_shaper(&mut self, interface: &str) -> Result<Self::Handle, Self::Error> {
        Ok(NoopShaper {
            interface: interface.to_string(),
        })
    }

    fn set_rate(
        &mut self,
        shaper: &Self::Handle,
        bandwidth_kbit: u64,
        dry_run: bool,
    ) -> Result<(), Self::Error> {
        debug!(
            "Ignoring traffic-control rate request of {} kbit/s for interface {} (dry_run={})",
            bandwidth_kbit, shaper.interface, dry_run
        );
        Ok(())
    }
}
