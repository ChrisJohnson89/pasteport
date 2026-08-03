//! The local control protocol.
//!
//! Newline-delimited JSON over a Unix domain socket: one JSON object per line
//! in each direction. Chosen over something like gRPC because the only clients
//! are on the same machine, the message rate is a handful per keystroke, and a
//! developer can drive it with `nc` while debugging.
//!
//! The socket lives inside the 0700 data directory, so access control is
//! filesystem ownership. There is no network listener and no authentication
//! token, because there is nothing to authenticate: reaching the socket already
//! requires being the user who owns the history.

use serde::{Deserialize, Serialize};

use pasteport_core::{Clip, ClipKind, Pinboard, Stats};

/// A request from a client (the CLI, the macOS app, the GTK app).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Liveness check.
    Ping,
    /// Version, backend, and history statistics.
    Status,
    /// Recent clips, newest first, pinned floated to the top.
    List {
        #[serde(default = "default_limit")]
        limit: usize,
        #[serde(default)]
        offset: usize,
        #[serde(default)]
        kind: Option<ClipKind>,
    },
    Search {
        query: String,
        #[serde(default = "default_limit")]
        limit: usize,
    },
    /// One clip's metadata.
    Get {
        id: i64,
    },
    /// One clip's raw payload, base64 encoded. For images.
    GetBytes {
        id: i64,
    },
    /// Put a stored clip back on the system clipboard and bump its recency.
    Copy {
        id: i64,
    },
    Pin {
        id: i64,
        pinned: bool,
    },
    Delete {
        id: i64,
    },
    Clear {
        #[serde(default)]
        include_pinned: bool,
    },
    /// Read the clipboard right now and store whatever is there.
    CaptureNow,
    /// Apply the retention policy immediately.
    Prune,
    Pinboards,
    PinboardCreate {
        name: String,
    },
    PinboardDelete {
        name: String,
    },
    PinboardAdd {
        name: String,
        id: i64,
    },
    PinboardRemove {
        name: String,
        id: i64,
    },
    PinboardClips {
        name: String,
        #[serde(default = "default_limit")]
        limit: usize,
    },
    /// Ask the daemon to exit cleanly.
    Shutdown,
}

fn default_limit() -> usize {
    50
}

/// A reply. Exactly one per request, on one line.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Pong {
        version: String,
    },
    Status(Box<StatusReport>),
    Clips {
        clips: Vec<Clip>,
    },
    Clip {
        clip: Box<Clip>,
    },
    /// Base64-encoded payload. `None` when the clip has no binary body.
    Bytes {
        base64: Option<String>,
    },
    Pinboards {
        pinboards: Vec<Pinboard>,
    },
    /// Number of rows affected, for `Clear` and `Prune`.
    Count {
        count: usize,
    },
    /// The request was understood but could not be carried out.
    Error {
        message: String,
    },
}

impl Response {
    pub fn error(message: impl std::fmt::Display) -> Self {
        Response::Error {
            message: message.to_string(),
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Response::Error { .. })
    }
}

/// Everything `pasteport status` prints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusReport {
    pub version: String,
    /// Clipboard backend in use, e.g. `macos-nspasteboard`.
    pub backend: String,
    pub uptime_secs: u64,
    pub poll_interval_ms: u64,
    pub stats: Stats,
    pub data_dir: String,
}

/// Encode a request as a protocol line, including the trailing newline.
pub fn encode_line<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let mut s = serde_json::to_string(value)?;
    s.push('\n');
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(req: &Request) {
        let line = encode_line(req).unwrap();
        assert!(line.ends_with('\n'));
        assert!(
            !line[..line.len() - 1].contains('\n'),
            "a request must be one line"
        );
        let back: Request = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(&back, req);
    }

    #[test]
    fn requests_round_trip() {
        for req in [
            Request::Ping,
            Request::Status,
            Request::List {
                limit: 10,
                offset: 5,
                kind: Some(ClipKind::Link),
            },
            Request::Search {
                query: "needle".into(),
                limit: 3,
            },
            Request::Get { id: 7 },
            Request::GetBytes { id: 7 },
            Request::Copy { id: 7 },
            Request::Pin {
                id: 7,
                pinned: true,
            },
            Request::Delete { id: 7 },
            Request::Clear {
                include_pinned: true,
            },
            Request::CaptureNow,
            Request::Prune,
            Request::Pinboards,
            Request::PinboardCreate {
                name: "work".into(),
            },
            Request::PinboardAdd {
                name: "work".into(),
                id: 1,
            },
            Request::PinboardClips {
                name: "work".into(),
                limit: 20,
            },
            Request::Shutdown,
        ] {
            round_trip(&req);
        }
    }

    #[test]
    fn list_defaults_fill_in() {
        let req: Request = serde_json::from_str(r#"{"op":"list"}"#).unwrap();
        assert_eq!(
            req,
            Request::List {
                limit: 50,
                offset: 0,
                kind: None
            }
        );
    }

    #[test]
    fn multiline_payloads_stay_on_one_line() {
        // Clipboard text routinely contains newlines; the framing must survive it.
        let req = Request::Search {
            query: "line one\nline two".into(),
            limit: 5,
        };
        round_trip(&req);
    }

    #[test]
    fn unknown_ops_are_rejected_rather_than_guessed() {
        assert!(serde_json::from_str::<Request>(r#"{"op":"self_destruct"}"#).is_err());
        assert!(serde_json::from_str::<Request>(r#"{}"#).is_err());
    }

    #[test]
    fn responses_round_trip() {
        let responses = [
            Response::Ok,
            Response::Pong {
                version: "0.1.0".into(),
            },
            Response::Count { count: 3 },
            Response::Bytes {
                base64: Some("AAEC".into()),
            },
            Response::Bytes { base64: None },
            Response::error("nope"),
        ];
        for resp in responses {
            let line = encode_line(&resp).unwrap();
            let back: Response = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(back, resp);
        }
    }

    #[test]
    fn error_helper_marks_errors() {
        assert!(Response::error("x").is_error());
        assert!(!Response::Ok.is_error());
    }
}
