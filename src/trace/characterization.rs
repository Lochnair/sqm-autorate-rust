// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use super::reader::{TraceCaptureV1, TraceHeaderV1};
use super::v1::{Direction, Event, RateRequestReason, Record};
use std::mem;
use thiserror::Error;

#[derive(Clone, Debug)]
pub(super) struct CharacterizationV1 {
    pub(super) header: TraceHeaderV1,
    pub(super) initialization: Record,
    pub(super) startup_rates: Record,
    pub(super) evaluations: Vec<EvaluationCaseV1>,
    pub(super) trailing_ping_replies: Vec<Record>,
}

#[derive(Clone, Debug)]
pub(super) struct EvaluationCaseV1 {
    pub(super) ping_replies_before: Vec<Record>,
    pub(super) boundary: Record,
    pub(super) download_calculation: Option<Record>,
    pub(super) upload_calculation: Option<Record>,
    pub(super) random_choices: Vec<Record>,
    pub(super) expected_rates: Record,
    pub(super) deferred_ping_replies: Vec<Record>,
}

#[derive(Debug, Error)]
pub(super) enum CharacterizationErrorV1 {
    #[error("trace has no controller initialization")]
    MissingInitialization,
    #[error("trace has no startup requested-rates record")]
    MissingStartupRates,
    #[error("record {seq} is structurally unexpected: {detail}")]
    UnexpectedRecord { seq: u64, detail: &'static str },
    #[error("evaluation {evaluation_id} is incomplete at EOF")]
    IncompleteEvaluation { evaluation_id: u64 },
    #[error("evaluation {evaluation_id} has invalid structure: {detail}")]
    InvalidEvaluation {
        evaluation_id: u64,
        detail: &'static str,
    },
}

struct OpenEvaluation {
    evaluation_id: u64,
    ping_replies_before: Vec<Record>,
    boundary: Record,
    download_calculation: Option<Record>,
    upload_calculation: Option<Record>,
    random_choices: Vec<Record>,
    deferred_ping_replies: Vec<Record>,
}

pub(super) fn build_characterization_v1(
    capture: &TraceCaptureV1,
) -> Result<CharacterizationV1, CharacterizationErrorV1> {
    let mut initialization = None;
    let mut startup_rates = None;
    let mut evaluations = Vec::new();
    let mut pending_ping_replies = Vec::new();
    let mut open = None::<OpenEvaluation>;

    for record in &capture.records {
        match &record.event {
            Event::PingReply { .. } => {
                if let Some(evaluation) = &mut open {
                    evaluation.deferred_ping_replies.push(record.clone());
                } else {
                    pending_ping_replies.push(record.clone());
                }
            }
            Event::ControllerInitialized { .. } => {
                if initialization.is_some() || open.is_some() || !evaluations.is_empty() {
                    return unexpected(record, "duplicate or late controller initialization");
                }
                initialization = Some(record.clone());
            }
            Event::ControlEvaluation { evaluation_id, .. } => {
                if initialization.is_none() || startup_rates.is_none() {
                    return unexpected(record, "evaluation precedes controller startup");
                }
                if open.is_some() {
                    return unexpected(record, "evaluation begins before the previous one ends");
                }
                if evaluations
                    .last()
                    .is_none_or(|previous: &EvaluationCaseV1| {
                        evaluation_id_of(&previous.boundary).wrapping_add(1) != *evaluation_id
                    })
                    && (!evaluations.is_empty() || *evaluation_id != 0)
                {
                    return invalid(*evaluation_id, "evaluation ids are not contiguous");
                }
                open = Some(OpenEvaluation {
                    evaluation_id: *evaluation_id,
                    ping_replies_before: mem::take(&mut pending_ping_replies),
                    boundary: record.clone(),
                    download_calculation: None,
                    upload_calculation: None,
                    random_choices: Vec::new(),
                    deferred_ping_replies: Vec::new(),
                });
            }
            Event::RateCalculation {
                evaluation_id,
                direction,
            } => {
                let evaluation = matching_open(&mut open, record, *evaluation_id)?;
                let calculation = match direction {
                    Direction::Download => &mut evaluation.download_calculation,
                    Direction::Upload => &mut evaluation.upload_calculation,
                };
                if calculation.replace(record.clone()).is_some() {
                    return invalid(*evaluation_id, "duplicate directional calculation");
                }
            }
            Event::RandomSafeRateChoice {
                evaluation_id,
                direction,
                ..
            } => {
                let evaluation = matching_open(&mut open, record, *evaluation_id)?;
                let has_calculation = match direction {
                    Direction::Download => evaluation.download_calculation.is_some(),
                    Direction::Upload => evaluation.upload_calculation.is_some(),
                };
                if !has_calculation {
                    return invalid(*evaluation_id, "random choice precedes its calculation");
                }
                evaluation.random_choices.push(record.clone());
            }
            Event::RequestedRates {
                evaluation_id: None,
                reason: RateRequestReason::Startup,
                ..
            } => {
                if startup_rates.is_some() || open.is_some() || !evaluations.is_empty() {
                    return unexpected(record, "duplicate or late startup rates");
                }
                startup_rates = Some(record.clone());
            }
            Event::RequestedRates {
                evaluation_id: Some(evaluation_id),
                reason,
                ..
            } => {
                let evaluation = matching_open(&mut open, record, *evaluation_id)?;
                match reason {
                    RateRequestReason::Control
                        if evaluation.download_calculation.is_none()
                            || evaluation.upload_calculation.is_none() =>
                    {
                        return invalid(*evaluation_id, "control evaluation lacks a calculation");
                    }
                    RateRequestReason::NoReflectorData
                        if evaluation.download_calculation.is_some()
                            || evaluation.upload_calculation.is_some()
                            || !evaluation.random_choices.is_empty() =>
                    {
                        return invalid(
                            *evaluation_id,
                            "no-reflector evaluation contains calculation metadata",
                        );
                    }
                    RateRequestReason::Startup => {
                        return invalid(*evaluation_id, "startup rates have an evaluation id");
                    }
                    _ => {}
                }
                let evaluation = open.take().expect("matching evaluation checked above");
                evaluations.push(EvaluationCaseV1 {
                    ping_replies_before: evaluation.ping_replies_before,
                    boundary: evaluation.boundary,
                    download_calculation: evaluation.download_calculation,
                    upload_calculation: evaluation.upload_calculation,
                    random_choices: evaluation.random_choices,
                    expected_rates: record.clone(),
                    deferred_ping_replies: evaluation.deferred_ping_replies,
                });
            }
            Event::RequestedRates { .. } => {
                return unexpected(record, "requested-rates reason and evaluation id disagree");
            }
            Event::Header { .. } | Event::End { .. } => {
                return unexpected(record, "header or end leaked from the trace reader");
            }
        }
    }

    if let Some(evaluation) = open {
        return Err(CharacterizationErrorV1::IncompleteEvaluation {
            evaluation_id: evaluation.evaluation_id,
        });
    }

    Ok(CharacterizationV1 {
        header: capture.header.clone(),
        initialization: initialization.ok_or(CharacterizationErrorV1::MissingInitialization)?,
        startup_rates: startup_rates.ok_or(CharacterizationErrorV1::MissingStartupRates)?,
        evaluations,
        trailing_ping_replies: pending_ping_replies,
    })
}

fn matching_open<'a>(
    open: &'a mut Option<OpenEvaluation>,
    record: &Record,
    evaluation_id: u64,
) -> Result<&'a mut OpenEvaluation, CharacterizationErrorV1> {
    let Some(evaluation) = open else {
        return unexpected(record, "evaluation-scoped record has no open evaluation");
    };
    if evaluation.evaluation_id != evaluation_id {
        return invalid(
            evaluation_id,
            "record refers to a different open evaluation",
        );
    }
    Ok(evaluation)
}

fn evaluation_id_of(record: &Record) -> u64 {
    let Event::ControlEvaluation { evaluation_id, .. } = record.event else {
        unreachable!("builder only stores control-evaluation boundaries")
    };
    evaluation_id
}

fn unexpected<T>(record: &Record, detail: &'static str) -> Result<T, CharacterizationErrorV1> {
    Err(CharacterizationErrorV1::UnexpectedRecord {
        seq: record.seq,
        detail,
    })
}

fn invalid<T>(evaluation_id: u64, detail: &'static str) -> Result<T, CharacterizationErrorV1> {
    Err(CharacterizationErrorV1::InvalidEvaluation {
        evaluation_id,
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::reader::{CaptureCompletionV1, read_trace_v1};
    use crate::trace::v1::{RateRequestReason, Timestamp};
    use std::collections::HashSet;
    use std::io::Cursor;

    const LIVE_FIXTURE: &str = include_str!("../../tests/fixtures/trace-v1/speedtests.jsonl");

    fn record(seq: u64, event: Event) -> Record {
        Record {
            seq,
            timestamp: Timestamp {
                mono_ns: seq,
                unix_us: seq as i64,
            },
            event,
        }
    }

    fn jsonl(records: &[Record]) -> String {
        records
            .iter()
            .map(|record| format!("{}\n", serde_json::to_string(record).unwrap()))
            .collect()
    }

    fn initialization(seq: u64) -> Record {
        record(
            seq,
            Event::ControllerInitialized {
                previous_rx_bytes: 10,
                previous_tx_bytes: 20,
                download_prev_mono_ns: 1,
                upload_prev_mono_ns: 2,
                download_history_index: 0,
                upload_history_index: 0,
                download_safe_rates_kbit: vec![100.0],
                upload_safe_rates_kbit: vec![200.0],
            },
        )
    }

    fn requested(seq: u64, evaluation_id: Option<u64>, reason: RateRequestReason) -> Record {
        record(
            seq,
            Event::RequestedRates {
                evaluation_id,
                reason,
                download_kbit: 100,
                upload_kbit: 200,
                download_requested: true,
                upload_requested: true,
            },
        )
    }

    fn ping(seq: u64, reflector: &str) -> Record {
        record(
            seq,
            Event::PingReply {
                reflector: reflector.parse().unwrap(),
                down_time_ms: 1.0,
                up_time_ms: 2.0,
            },
        )
    }

    fn boundary(seq: u64, evaluation_id: u64) -> Record {
        record(
            seq,
            Event::ControlEvaluation {
                evaluation_id,
                loop_mono_ns: seq,
                rx_bytes: 30,
                tx_bytes: 40,
                active_reflectors: vec!["192.0.2.1".parse().unwrap()],
            },
        )
    }

    fn calculation(seq: u64, evaluation_id: u64, direction: Direction) -> Record {
        record(
            seq,
            Event::RateCalculation {
                evaluation_id,
                direction,
            },
        )
    }

    fn synthetic_capture(records: &[Record]) -> TraceCaptureV1 {
        let mut all = vec![record(0, crate::trace::v1::tests::header_event())];
        all.extend_from_slice(records);
        let input = jsonl(&all);
        read_trace_v1(Cursor::new(input)).unwrap()
    }

    #[test]
    fn freezes_evaluation_before_interleaved_ping_replies() {
        let capture = synthetic_capture(&[
            initialization(1),
            requested(2, None, RateRequestReason::Startup),
            ping(3, "192.0.2.1"),
            boundary(4, 0),
            calculation(5, 0, Direction::Download),
            ping(6, "192.0.2.2"),
            calculation(7, 0, Direction::Upload),
            requested(8, Some(0), RateRequestReason::Control),
            ping(9, "192.0.2.3"),
            boundary(10, 1),
            calculation(11, 1, Direction::Download),
            calculation(12, 1, Direction::Upload),
            requested(13, Some(1), RateRequestReason::Control),
        ]);

        let characterization = build_characterization_v1(&capture).unwrap();

        assert_eq!(characterization.header.seq, 0);
        assert!(characterization.trailing_ping_replies.is_empty());
        assert_eq!(
            characterization.evaluations[0].ping_replies_before[0].seq,
            3
        );
        assert_eq!(
            characterization.evaluations[0].deferred_ping_replies[0].seq,
            6
        );
        assert_eq!(
            characterization.evaluations[1].ping_replies_before[0].seq,
            9
        );
        assert!(
            characterization.evaluations[1]
                .deferred_ping_replies
                .is_empty()
        );
    }

    #[test]
    fn live_fixture_parses_as_an_incomplete_observed_prefix() {
        let capture = read_trace_v1(Cursor::new(LIVE_FIXTURE)).unwrap();

        assert_eq!(capture.completion, CaptureCompletionV1::IncompleteEof);
        assert_eq!(capture.header.program_version, "0.4.1");
        assert_eq!(capture.records.last().unwrap().seq, 1515);
    }

    #[test]
    fn live_fixture_covers_useful_legacy_decisions() {
        let capture = read_trace_v1(Cursor::new(LIVE_FIXTURE)).unwrap();
        let characterization = build_characterization_v1(&capture).unwrap();

        assert!(matches!(
            characterization.initialization.event,
            Event::ControllerInitialized { .. }
        ));
        assert!(matches!(
            characterization.startup_rates.event,
            Event::RequestedRates {
                reason: RateRequestReason::Startup,
                ..
            }
        ));
        assert!(characterization.evaluations.iter().any(|evaluation| {
            let Event::ControlEvaluation {
                active_reflectors, ..
            } = &evaluation.boundary.event
            else {
                unreachable!();
            };
            active_reflectors.len() > 5
        }));

        let stochastic_index = characterization
            .evaluations
            .iter()
            .position(|evaluation| !evaluation.random_choices.is_empty())
            .expect("fixture must exercise the legacy stochastic branch");
        let stochastic = &characterization.evaluations[stochastic_index];
        assert_eq!(evaluation_id_of(&stochastic.boundary), 150);
        assert!(matches!(
            stochastic.random_choices.as_slice(),
            [Record {
                event: Event::RandomSafeRateChoice {
                    direction: Direction::Download,
                    index: 92,
                    ..
                },
                ..
            }]
        ));
        let Event::RequestedRates {
            download_kbit,
            download_requested,
            ..
        } = stochastic.expected_rates.event
        else {
            unreachable!();
        };
        assert_eq!(download_kbit, 256_374);
        assert!(download_requested);
        let Event::RequestedRates {
            download_kbit: previous_download_kbit,
            ..
        } = characterization.evaluations[stochastic_index - 1]
            .expected_rates
            .event
        else {
            unreachable!();
        };
        assert!(download_kbit < previous_download_kbit);

        let distinct_rates = characterization
            .evaluations
            .iter()
            .map(|evaluation| match evaluation.expected_rates.event {
                Event::RequestedRates {
                    download_kbit,
                    upload_kbit,
                    ..
                } => (download_kbit, upload_kbit),
                _ => unreachable!(),
            })
            .collect::<HashSet<_>>();
        assert!(distinct_rates.len() > 3);
    }

    #[test]
    fn live_expected_rates_have_one_unambiguous_evaluation() {
        let capture = read_trace_v1(Cursor::new(LIVE_FIXTURE)).unwrap();
        let characterization = build_characterization_v1(&capture).unwrap();
        let mut ids = HashSet::new();

        for evaluation in &characterization.evaluations {
            let evaluation_id = evaluation_id_of(&evaluation.boundary);
            let Event::RequestedRates {
                evaluation_id: Some(expected_id),
                reason: RateRequestReason::Control,
                ..
            } = evaluation.expected_rates.event
            else {
                panic!("fixture evaluation must have normal control output");
            };
            assert_eq!(expected_id, evaluation_id);
            assert!(ids.insert(evaluation_id));
            assert!(evaluation.download_calculation.is_some());
            assert!(evaluation.upload_calculation.is_some());
        }
    }
}
