// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

#[derive(Debug, thiserror::Error)]
#[error("channel closed")]
pub(crate) struct ChannelClosed;
