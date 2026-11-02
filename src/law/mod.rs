// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

mod context;
mod sqma;

pub(crate) use context::LawContext;
pub(crate) use sqma::{ControlSnapshot, SqmaLaw};

#[async_trait::async_trait]
pub trait Law: Send {
    async fn run(self: Box<Self>, ctx: LawContext) -> anyhow::Result<()>;
}
