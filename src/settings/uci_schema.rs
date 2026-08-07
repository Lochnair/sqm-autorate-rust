// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct UciOptionMapping {
    pub(crate) uci_option: &'static str,
    pub(crate) config_field: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct UciSectionMapping {
    /// UCI section type, corresponding to `@section_type[0]`.
    pub(crate) uci_section: &'static str,

    /// Top-level config-rs / Serde settings key.
    pub(crate) config_section: &'static str,

    pub(crate) options: &'static [UciOptionMapping],
}

pub(crate) trait UciSectionSchema {
    const UCI_OPTIONS: &'static [UciOptionMapping];
}

pub(crate) trait UciConfigSchema {
    const UCI_PACKAGE: &'static str;
    const UCI_SECTIONS: &'static [UciSectionMapping];
}
