// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use super::v1::{self, ControlConfig, Event, Record, Timestamp};
use std::io::{self, BufRead};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct TraceHeaderV1 {
    pub(super) seq: u64,
    pub(super) timestamp: Timestamp,
    pub(super) program_version: String,
    pub(super) control: ControlConfig,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CaptureCompletionV1 {
    Complete,
    IncompleteEof,
    IncompleteEnd,
    TruncatedFinalLine { line: usize },
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct TraceCaptureV1 {
    pub(super) header: TraceHeaderV1,
    pub(super) records: Vec<Record>,
    pub(super) completion: CaptureCompletionV1,
}

#[derive(Debug, Error)]
pub(super) enum TraceReadErrorV1 {
    #[error("failed to read trace line {line}: {source}")]
    Io {
        line: usize,
        #[source]
        source: io::Error,
    },
    #[error("trace line {line} is not a valid v1 record: {source}")]
    MalformedRecord {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("trace is missing its v1 header")]
    MissingHeader,
    #[error("first trace record must be the v1 header")]
    FirstRecordNotHeader,
    #[error("trace header sequence must be 0, got {actual}")]
    InvalidHeaderSequence { actual: u64 },
    #[error("unsupported trace format {actual:?}")]
    InvalidFormat { actual: String },
    #[error("unsupported trace version {actual}")]
    InvalidVersion { actual: u16 },
    #[error("trace line {line} has sequence {actual}, expected {expected}")]
    InvalidSequence {
        line: usize,
        expected: u64,
        actual: u64,
    },
    #[error("trace line {line} contains another header")]
    UnexpectedHeader { line: usize },
    #[error("trace line {line} follows an end record")]
    RecordAfterEnd { line: usize },
}

pub(super) fn read_trace_v1(mut reader: impl BufRead) -> Result<TraceCaptureV1, TraceReadErrorV1> {
    let mut header = None;
    let mut records = Vec::new();
    let mut next_seq = 0;
    let mut completion = None;
    let mut line_number = 0;

    loop {
        let mut line = String::new();
        line_number += 1;
        let bytes_read = reader
            .read_line(&mut line)
            .map_err(|source| TraceReadErrorV1::Io {
                line: line_number,
                source,
            })?;
        if bytes_read == 0 {
            break;
        }

        if completion.is_some() {
            return Err(TraceReadErrorV1::RecordAfterEnd { line: line_number });
        }

        let terminated = line.ends_with('\n');
        let json = line.strip_suffix('\n').unwrap_or(&line);
        let json = json.strip_suffix('\r').unwrap_or(json);
        let record = match parse_record_v1(json) {
            Ok(record) => record,
            Err(source)
                if !terminated
                    && !json.trim().is_empty()
                    && source.is_eof()
                    && header.is_some() =>
            {
                return Ok(TraceCaptureV1 {
                    header: header.expect("header checked above"),
                    records,
                    completion: CaptureCompletionV1::TruncatedFinalLine { line: line_number },
                });
            }
            Err(source) => {
                return Err(TraceReadErrorV1::MalformedRecord {
                    line: line_number,
                    source,
                });
            }
        };

        if record.seq != next_seq {
            if header.is_none() && next_seq == 0 {
                return Err(TraceReadErrorV1::InvalidHeaderSequence { actual: record.seq });
            }
            return Err(TraceReadErrorV1::InvalidSequence {
                line: line_number,
                expected: next_seq,
                actual: record.seq,
            });
        }
        next_seq = next_seq.saturating_add(1);

        if header.is_none() {
            let Event::Header {
                format,
                version,
                program_version,
                control,
            } = record.event
            else {
                return Err(TraceReadErrorV1::FirstRecordNotHeader);
            };
            if format != v1::FORMAT_NAME {
                return Err(TraceReadErrorV1::InvalidFormat { actual: format });
            }
            if version != v1::FORMAT_VERSION {
                return Err(TraceReadErrorV1::InvalidVersion { actual: version });
            }
            header = Some(TraceHeaderV1 {
                seq: record.seq,
                timestamp: record.timestamp,
                program_version,
                control,
            });
            continue;
        }

        match record.event {
            Event::Header { .. } => {
                return Err(TraceReadErrorV1::UnexpectedHeader { line: line_number });
            }
            Event::End { clean } => {
                completion = Some(if clean {
                    CaptureCompletionV1::Complete
                } else {
                    CaptureCompletionV1::IncompleteEnd
                });
            }
            _ => records.push(record),
        }
    }

    let header = header.ok_or(TraceReadErrorV1::MissingHeader)?;
    Ok(TraceCaptureV1 {
        header,
        records,
        completion: completion.unwrap_or(CaptureCompletionV1::IncompleteEof),
    })
}

pub(super) fn parse_record_v1(json: &str) -> Result<Record, serde_json::Error> {
    serde_json::from_str(json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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

    fn header(seq: u64) -> Record {
        record(seq, v1::tests::header_event())
    }

    fn ping(seq: u64) -> Record {
        record(
            seq,
            Event::PingReply {
                reflector: "192.0.2.1".parse().unwrap(),
                down_time_ms: 1.0,
                up_time_ms: 2.0,
            },
        )
    }

    #[test]
    fn parses_clean_v1_trace() {
        let input = jsonl(&[header(0), ping(1), record(2, Event::End { clean: true })]);

        let capture = read_trace_v1(Cursor::new(input)).unwrap();

        assert_eq!(capture.header.seq, 0);
        assert_eq!(capture.records, vec![ping(1)]);
        assert_eq!(capture.completion, CaptureCompletionV1::Complete);
    }

    #[test]
    fn retains_records_before_truncated_final_line() {
        let input = format!(
            "{}{{\"seq\":2,\"timestamp\":{{",
            jsonl(&[header(0), ping(1)])
        );

        let capture = read_trace_v1(Cursor::new(input)).unwrap();

        assert_eq!(capture.records, vec![ping(1)]);
        assert_eq!(
            capture.completion,
            CaptureCompletionV1::TruncatedFinalLine { line: 3 }
        );
    }

    #[test]
    fn marks_valid_eof_without_end_incomplete() {
        let capture = read_trace_v1(Cursor::new(jsonl(&[header(0), ping(1)]))).unwrap();

        assert_eq!(capture.records, vec![ping(1)]);
        assert_eq!(capture.completion, CaptureCompletionV1::IncompleteEof);
    }

    #[test]
    fn rejects_malformed_middle_line() {
        let input = format!("{}not-json\n{}", jsonl(&[header(0)]), jsonl(&[ping(1)]));

        assert!(matches!(
            read_trace_v1(Cursor::new(input)),
            Err(TraceReadErrorV1::MalformedRecord { line: 2, .. })
        ));
    }

    #[test]
    fn rejects_sequence_gap_duplicate_and_out_of_order() {
        let cases = [
            (vec![header(0), ping(2)], 2, 1, 2),
            (vec![header(0), ping(1), ping(1)], 3, 2, 1),
            (vec![header(0), ping(1), ping(2), ping(1)], 4, 3, 1),
        ];

        for (records, line, expected, actual) in cases {
            let input = jsonl(&records);
            assert!(matches!(
                read_trace_v1(Cursor::new(input)),
                Err(TraceReadErrorV1::InvalidSequence {
                    line: seen_line,
                    expected: seen_expected,
                    actual: seen,
                }) if seen_line == line && seen_expected == expected && seen == actual
            ));
        }
    }

    #[test]
    fn rejects_non_truncation_error_on_final_line() {
        let input = format!("{}{{\"seq\":1,]", jsonl(&[header(0)]));

        assert!(matches!(
            read_trace_v1(Cursor::new(input)),
            Err(TraceReadErrorV1::MalformedRecord { line: 2, .. })
        ));
    }

    #[test]
    fn rejects_records_after_clean_end() {
        let input = jsonl(&[header(0), record(1, Event::End { clean: true }), ping(2)]);

        assert!(matches!(
            read_trace_v1(Cursor::new(input)),
            Err(TraceReadErrorV1::RecordAfterEnd { line: 3 })
        ));
    }

    #[test]
    fn validates_format_name_and_version() {
        let Event::Header {
            program_version,
            control,
            ..
        } = v1::tests::header_event()
        else {
            unreachable!();
        };
        let invalid_format = record(
            0,
            Event::Header {
                format: "other".to_owned(),
                version: v1::FORMAT_VERSION,
                program_version: program_version.clone(),
                control: control.clone(),
            },
        );
        let invalid_version = record(
            0,
            Event::Header {
                format: v1::FORMAT_NAME.to_owned(),
                version: v1::FORMAT_VERSION + 1,
                program_version,
                control,
            },
        );

        assert!(matches!(
            read_trace_v1(Cursor::new(jsonl(&[invalid_format]))),
            Err(TraceReadErrorV1::InvalidFormat { .. })
        ));
        assert!(matches!(
            read_trace_v1(Cursor::new(jsonl(&[invalid_version]))),
            Err(TraceReadErrorV1::InvalidVersion { .. })
        ));
    }
}
