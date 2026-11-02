// SPDX-License-Identifier: GPL-3.0-or-later
//! JSON line protocol between the daemon and the helper executable (one JSON object per line on
//! stdin/stdout). All input is bounded: a line longer than [`MAX_LINE`] bytes is discarded without
//! being buffered, and a request that fails to parse yields an error response, never a panic.
//!
//! Requests carry the protocol version and a caller-chosen `id` echoed in the response. Events
//! (call events, link changes, command outcomes) are pushed unprompted as `{"event": ...}` lines.
use crate::error::AdapterError;
use crate::model::{BtAddress, PROTOCOL_VERSION};
use crate::plan::MacCommand;
use crate::route::RouteRequest;
use bridge_core::calls::CallCommand;
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead};

/// Hard cap on one input line. A dial/DTMF command is a few hundred bytes at most.
pub const MAX_LINE: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Request {
    /// Must equal [`PROTOCOL_VERSION`].
    pub v: u32,
    pub id: u64,
    #[serde(flatten)]
    pub op: Op,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Read-only: authorization, paired devices, capabilities. Never connects.
    Probe,
    /// Same inventory without the capability block.
    Inventory,
    /// Open a link to exactly this paired phone.
    Connect {
        address: BtAddress,
    },
    Disconnect,
    /// An adapter command (adds hold/resume and the audio strategy to the core set).
    Command {
        command: MacCommand,
    },
    /// A `bridge_core::calls::CallCommand` exactly as the core serialises it. Commands with no
    /// public macOS mapping (ringer or microphone mute) are refused, not dropped.
    CoreCommand {
        command: CallCommand,
    },
    /// Read-only CoreAudio endpoint listing.
    AudioDevices,
    /// Move the system defaults to the named endpoints (explicit; restored on link end).
    RouteApply {
        route: RouteRequest,
    },
    RouteRestore,
    Status,
    /// Disconnect (if linked), restore owned routes, then exit.
    Shutdown,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum Response<T: Serialize> {
    Ok {
        v: u32,
        id: u64,
        ok: bool,
        result: T,
    },
    Err {
        v: u32,
        id: u64,
        ok: bool,
        error: AdapterError,
    },
}
impl<T: Serialize> Response<T> {
    pub fn ok(id: u64, result: T) -> Self {
        Self::Ok {
            v: PROTOCOL_VERSION,
            id,
            ok: true,
            result,
        }
    }
    pub fn err(id: u64, error: AdapterError) -> Self {
        Self::Err {
            v: PROTOCOL_VERSION,
            id,
            ok: false,
            error,
        }
    }
}

/// What a bounded read produced.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    Text(String),
    /// The line exceeded the cap; its remainder was skipped.
    TooLong,
    Eof,
}

/// Read one `\n`-terminated line, never holding more than `max` bytes. Invalid UTF-8 is replaced.
pub fn read_bounded_line<R: BufRead>(reader: &mut R, max: usize) -> io::Result<Line> {
    let mut buf: Vec<u8> = Vec::new();
    let mut overflow = false;
    let mut saw_any = false;
    loop {
        let (consumed, done) = {
            let chunk = match reader.fill_buf() {
                Ok(c) => c,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if chunk.is_empty() {
                return Ok(if saw_any && !overflow {
                    Line::Text(String::from_utf8_lossy(&buf).into_owned())
                } else if overflow {
                    Line::TooLong
                } else {
                    Line::Eof
                });
            }
            saw_any = true;
            match chunk.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    if !overflow {
                        if buf.len() + pos > max {
                            overflow = true;
                        } else {
                            buf.extend_from_slice(&chunk[..pos]);
                        }
                    }
                    (pos + 1, true)
                }
                None => {
                    if !overflow {
                        if buf.len() + chunk.len() > max {
                            overflow = true;
                            buf.clear();
                        } else {
                            buf.extend_from_slice(chunk);
                        }
                    }
                    (chunk.len(), false)
                }
            }
        };
        reader.consume(consumed);
        if done {
            return Ok(if overflow {
                Line::TooLong
            } else {
                Line::Text(String::from_utf8_lossy(&buf).into_owned())
            });
        }
    }
}

/// Parse one request line, enforcing the protocol version.
pub fn parse_request(line: &str) -> Result<Request, AdapterError> {
    let req: Request = serde_json::from_str(line).map_err(|e| AdapterError::Protocol {
        detail: format!("invalid request: {}", e.classify_name()),
    })?;
    if req.v != PROTOCOL_VERSION {
        return Err(AdapterError::Protocol {
            detail: format!("unsupported protocol version {}", req.v),
        });
    }
    Ok(req)
}

trait ClassifyName {
    fn classify_name(&self) -> &'static str;
}
impl ClassifyName for serde_json::Error {
    /// Error category only: the message can quote input, which may hold phone numbers.
    fn classify_name(&self) -> &'static str {
        match self.classify() {
            serde_json::error::Category::Io => "io",
            serde_json::error::Category::Syntax => "syntax",
            serde_json::error::Category::Data => "data",
            serde_json::error::Category::Eof => "eof",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_lines_and_eof() {
        let mut r = Cursor::new(b"abc\ndef\n".to_vec());
        assert_eq!(
            read_bounded_line(&mut r, 100).unwrap(),
            Line::Text("abc".into())
        );
        assert_eq!(
            read_bounded_line(&mut r, 100).unwrap(),
            Line::Text("def".into())
        );
        assert_eq!(read_bounded_line(&mut r, 100).unwrap(), Line::Eof);
    }

    #[test]
    fn final_line_without_newline_is_returned() {
        let mut r = Cursor::new(b"tail".to_vec());
        assert_eq!(
            read_bounded_line(&mut r, 100).unwrap(),
            Line::Text("tail".into())
        );
        assert_eq!(read_bounded_line(&mut r, 100).unwrap(), Line::Eof);
    }

    #[test]
    fn oversized_line_is_skipped_without_buffering_and_stream_recovers() {
        let mut data = vec![b'x'; 5000];
        data.push(b'\n');
        data.extend_from_slice(b"ok\n");
        let mut r = io::BufReader::with_capacity(64, Cursor::new(data));
        assert_eq!(read_bounded_line(&mut r, 100).unwrap(), Line::TooLong);
        assert_eq!(
            read_bounded_line(&mut r, 100).unwrap(),
            Line::Text("ok".into())
        );
    }

    #[test]
    fn oversized_final_line_without_newline_reports_too_long() {
        let mut r = io::BufReader::with_capacity(16, Cursor::new(vec![b'y'; 1000]));
        assert_eq!(read_bounded_line(&mut r, 100).unwrap(), Line::TooLong);
        assert_eq!(read_bounded_line(&mut r, 100).unwrap(), Line::Eof);
    }

    #[test]
    fn line_exactly_at_the_cap_is_accepted() {
        let mut data = vec![b'z'; 100];
        data.push(b'\n');
        let mut r = Cursor::new(data);
        match read_bounded_line(&mut r, 100).unwrap() {
            Line::Text(t) => assert_eq!(t.len(), 100),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn invalid_utf8_is_replaced_not_fatal() {
        let mut r = Cursor::new(vec![0xff, 0xfe, b'\n']);
        assert!(matches!(
            read_bounded_line(&mut r, 10).unwrap(),
            Line::Text(_)
        ));
    }

    #[test]
    fn parses_each_operation() {
        let p = |s: &str| parse_request(s).unwrap().op;
        assert_eq!(p(r#"{"v":1,"id":1,"op":"probe"}"#), Op::Probe);
        assert_eq!(p(r#"{"v":1,"id":1,"op":"disconnect"}"#), Op::Disconnect);
        assert!(matches!(
            p(r#"{"v":1,"id":2,"op":"connect","address":"11-22-33-44-55-66"}"#),
            Op::Connect { .. }
        ));
        assert!(matches!(
            p(r#"{"v":1,"id":3,"op":"command","command":{"type":"dial","number":"+1555"}}"#),
            Op::Command {
                command: MacCommand::Dial { .. }
            }
        ));
        assert!(matches!(
            p(r#"{"v":1,"id":3,"op":"core_command","command":{"type":"swap_calls"}}"#),
            Op::CoreCommand {
                command: CallCommand::SwapCalls
            }
        ));
        assert!(matches!(
            p(r#"{"v":1,"id":3,"op":"core_command","command":{"type":"mute_ringer"}}"#),
            Op::CoreCommand {
                command: CallCommand::MuteRinger
            }
        ));
        assert!(matches!(
            p(r#"{"v":1,"id":3,"op":"command","command":{"type":"connect_audio"}}"#),
            Op::Command {
                command: MacCommand::ConnectAudio { .. }
            }
        ));
        assert!(matches!(
            p(
                r#"{"v":1,"id":4,"op":"route_apply","route":{"session":"s","input_uid":"a","output_uid":"b"}}"#
            ),
            Op::RouteApply { .. }
        ));
    }

    #[test]
    fn rejects_bad_version_unknown_op_and_bad_address_without_echoing_input() {
        assert!(matches!(
            parse_request(r#"{"v":2,"id":1,"op":"probe"}"#),
            Err(AdapterError::Protocol { .. })
        ));
        let e = parse_request(r#"{"v":1,"id":1,"op":"explode"}"#).unwrap_err();
        assert!(matches!(e, AdapterError::Protocol { .. }));
        let e =
            parse_request(r#"{"v":1,"id":1,"op":"connect","address":"+15551234567"}"#).unwrap_err();
        let shown = format!("{e}");
        assert!(!shown.contains("5551234567"), "{shown}");
        assert!(parse_request("not json").is_err());
        assert!(parse_request("").is_err());
    }

    #[test]
    fn responses_serialise_with_ok_flag() {
        let ok = serde_json::to_value(Response::ok(7, "x")).unwrap();
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["id"], 7);
        let err = serde_json::to_value(Response::<()>::err(8, AdapterError::NoActiveLink)).unwrap();
        assert_eq!(err["ok"], false);
        assert_eq!(err["error"]["error"], "no_active_link");
    }
}
