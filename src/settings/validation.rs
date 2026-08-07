// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0
//

use super::{AdvancedSettings, NetworkSettings, ObservabilitySettings, OutputSettings, Settings};
use std::error::Error;
use std::fmt::{self, Display, Formatter};

const WARN_DOWNLOAD_MIN_KBITS: f64 = 1_000.0;
const WARN_UPLOAD_MIN_KBITS: f64 = 500.0;
const WARN_HIGH_MIN_PERCENT: f64 = 80.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ValidationSeverity {
    Warning,
    Error,
}

#[derive(Debug)]
struct ValidationIssue {
    severity: ValidationSeverity,
    path: &'static str,
    message: String,
}

#[derive(Debug, Default)]
pub(crate) struct ValidationReport {
    issues: Vec<ValidationIssue>,
}

impl ValidationReport {
    fn warning(&mut self, path: &'static str, message: impl Into<String>) {
        self.issues.push(ValidationIssue {
            severity: ValidationSeverity::Warning,
            path,
            message: message.into(),
        });
    }

    fn error(&mut self, path: &'static str, message: impl Into<String>) {
        self.issues.push(ValidationIssue {
            severity: ValidationSeverity::Error,
            path,
            message: message.into(),
        });
    }

    fn finish(self) -> Result<(), Self> {
        let mut errors = Vec::new();

        for issue in self.issues {
            match issue.severity {
                ValidationSeverity::Warning => {
                    eprintln!("warning: {}: {}", issue.path, issue.message);
                }

                ValidationSeverity::Error => {
                    errors.push(issue);
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(Self { issues: errors })
        }
    }
}

impl Display for ValidationReport {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "settings validation failed:")?;

        for issue in &self.issues {
            write!(f, "\n  {}: {}", issue.path, issue.message)?;
        }

        Ok(())
    }
}

impl Error for ValidationReport {}

impl Settings {
    pub(crate) fn validate(&self) -> Result<(), ValidationReport> {
        let mut report = ValidationReport::default();

        self.network.validate(&mut report);
        self.output.validate(&mut report);
        self.observability.validate(&mut report);
        self.advanced_settings.validate(&mut report);

        report.finish()
    }
}

impl NetworkSettings {
    fn validate(&self, report: &mut ValidationReport) {
        if self.download_interface.trim().is_empty() {
            report.error("network.download_interface", "must not be empty");
        }

        if self.upload_interface.trim().is_empty() {
            report.error("network.upload_interface", "must not be empty");
        }

        let download_base_valid = validate_greater_than_zero_f64(
            report,
            "network.download_base_kbits",
            self.download_base_kbits,
        );

        let upload_base_valid = validate_greater_than_zero_f64(
            report,
            "network.upload_base_kbits",
            self.upload_base_kbits,
        );

        let download_percent_valid = validate_percent(
            report,
            "network.download_min_percent",
            self.download_min_percent,
        );

        let upload_percent_valid = validate_percent(
            report,
            "network.upload_min_percent",
            self.upload_min_percent,
        );

        if download_percent_valid && self.download_min_percent > WARN_HIGH_MIN_PERCENT {
            report.warning(
                "network.download_min_percent",
                format!(
                    "{}% leaves little room for the shaper to reduce the download rate",
                    self.download_min_percent,
                ),
            );
        }

        if upload_percent_valid && self.upload_min_percent > WARN_HIGH_MIN_PERCENT {
            report.warning(
                "network.upload_min_percent",
                format!(
                    "{}% leaves little room for the shaper to reduce the upload rate",
                    self.upload_min_percent,
                ),
            );
        }

        if download_base_valid && download_percent_valid {
            let minimum = self.download_min_kbits();

            if minimum < WARN_DOWNLOAD_MIN_KBITS {
                report.warning(
                    "network.download_min_percent",
                    format!(
                        "results in a minimum download rate of only {minimum:.0} kbit/s \
                         ({:.1}% of {:.0} kbit/s); connectivity may become severely constrained",
                        self.download_min_percent, self.download_base_kbits,
                    ),
                );
            }
        }

        if upload_base_valid && upload_percent_valid {
            let minimum = self.upload_min_kbits();

            if minimum < WARN_UPLOAD_MIN_KBITS {
                report.warning(
                    "network.upload_min_percent",
                    format!(
                        "results in a minimum upload rate of only {minimum:.0} kbit/s \
                         ({:.1}% of {:.0} kbit/s); connectivity may become severely constrained",
                        self.upload_min_percent, self.upload_base_kbits,
                    ),
                );
            }
        }
    }
}

impl OutputSettings {
    fn validate(&self, report: &mut ValidationReport) {
        if self.suppress_statistics {
            return;
        }

        let speed_hist_file = self.speed_hist_file.trim();
        let stats_file = self.stats_file.trim();

        if speed_hist_file.is_empty() {
            report.error(
                "output.speed_hist_file",
                "must not be empty when statistics are enabled",
            );
        }

        if stats_file.is_empty() {
            report.error(
                "output.stats_file",
                "must not be empty when statistics are enabled",
            );
        }

        if !speed_hist_file.is_empty() && !stats_file.is_empty() && speed_hist_file == stats_file {
            report.error(
                "output.stats_file",
                "must not refer to the same file as output.speed_hist_file",
            );
        }
    }
}

impl ObservabilitySettings {
    fn validate(&self, report: &mut ValidationReport) {
        if !self.enabled {
            return;
        }

        if self
            .host
            .as_deref()
            .is_none_or(|host| host.trim().is_empty())
        {
            report.error(
                "observability.host",
                "must be set when observability is enabled",
            );
        }

        if self.port == 0 {
            report.error("observability.port", "must be greater than 0");
        }

        if self.batch_size == 0 {
            report.error("observability.batch_size", "must be greater than 0");
        }

        if self.batch_timeout_ms == 0 {
            report.error("observability.batch_timeout_ms", "must be greater than 0");
        }

        if !self.export_ping_metrics
            && !self.export_rate_metrics
            && !self.export_baseline_metrics
            && !self.export_events
        {
            report.warning(
                "observability.enabled",
                "observability is enabled but all metric and event exports are disabled",
            );
        }
    }
}

impl AdvancedSettings {
    fn validate(&self, report: &mut ValidationReport) {
        validate_zero_or_greater_f64(
            report,
            "advanced_settings.download_delay_ms",
            self.download_delay_ms,
        );

        validate_zero_or_greater_f64(
            report,
            "advanced_settings.upload_delay_ms",
            self.upload_delay_ms,
        );

        let high_load_level_valid = validate_zero_or_greater_f64(
            report,
            "advanced_settings.high_load_level",
            self.high_load_level,
        );

        validate_greater_than_zero_f64(
            report,
            "advanced_settings.min_change_interval",
            self.min_change_interval,
        );

        validate_greater_than_zero_f64(
            report,
            "advanced_settings.tick_interval",
            self.tick_interval,
        );

        if self.num_reflectors < 5 {
            report.error(
                "advanced_settings.num_reflectors",
                format!("must be at least 5, got {}", self.num_reflectors,),
            );
        }

        if self.peer_reselection_time == 0 {
            report.error(
                "advanced_settings.peer_reselection_time",
                "must be greater than 0",
            );
        }

        if self.reflector_list_file.trim().is_empty() {
            report.error("advanced_settings.reflector_list_file", "must not be empty");
        }

        if self.speed_hist_size == 0 {
            report.error(
                "advanced_settings.speed_hist_size",
                "must be greater than 0",
            );
        }

        if high_load_level_valid {
            if self.high_load_level == 0.0 {
                report.warning(
                    "advanced_settings.high_load_level",
                    "0 causes any positive load to be treated as high load",
                );
            } else if self.high_load_level > 1.0 {
                report.warning(
                    "advanced_settings.high_load_level",
                    format!(
                        "{} requires utilisation to exceed the current shaper rate \
                         before the high-load threshold is reached",
                        self.high_load_level,
                    ),
                );
            }
        }
    }
}

fn validate_greater_than_zero_f64(
    report: &mut ValidationReport,
    path: &'static str,
    value: f64,
) -> bool {
    if !value.is_finite() {
        report.error(path, format!("must be finite, got {value}"));
        false
    } else if value <= 0.0 {
        report.error(path, format!("must be greater than 0, got {value}"));
        false
    } else {
        true
    }
}

fn validate_zero_or_greater_f64(
    report: &mut ValidationReport,
    path: &'static str,
    value: f64,
) -> bool {
    if !value.is_finite() {
        report.error(path, format!("must be finite, got {value}"));
        false
    } else if value < 0.0 {
        report.error(path, format!("must be 0 or greater, got {value}"));
        false
    } else {
        true
    }
}

fn validate_percent(report: &mut ValidationReport, path: &'static str, value: f64) -> bool {
    if !value.is_finite() {
        report.error(path, format!("must be finite, got {value}"));
        false
    } else if !(1.0..=100.0).contains(&value) {
        report.error(path, format!("must be between 1 and 100, got {value}"));
        false
    } else {
        true
    }
}
