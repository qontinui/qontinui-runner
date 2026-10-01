//! Control messages and version negotiation.
//!
//! Every control frame's payload is ONE JSON object whose `"type"` field names
//! the message. Requests flow runner → holder, replies holder → runner.
//!
//! ## The frozen envelope (plan D15)
//!
//! These never change shape, in any protocol version, ever:
//!
//! - the framing (`frame`),
//! - `hello {versions}` and its answers `hello_ack {version, holder_build,
//!   holder_pid, child_pid}` / `no_common_version {holder_versions}`,
//! - `census` and its answer `census_reply`,
//! - `prepare_upgrade` (answered `unsupported` in this build),
//! - `unsupported` and `rejected`.
//!
//! Frozen is what lets a runner arbitrarily far ahead still IDENTIFY, COUNT and
//! (on Unix, Phase 8) UPGRADE a holder it can no longer fully drive — the
//! headline scenario of the plan is exactly a newer runner meeting an older,
//! still-running holder. `wire_shape_*` tests below pin the exact bytes, so a
//! later edit cannot move them silently. Readers of envelope messages ignore
//! unknown fields (serde's default), which is the only tolerance granted.
//!
//! ## Negotiation
//!
//! The runner offers every version it speaks; the holder answers with the
//! HIGHEST version both speak ([`negotiate`]), or `no_common_version`. Versions
//! are never compared for equality (D15 replaces Phase 1's original "compared
//! for equality"). The envelope verbs — `census`, `prepare_upgrade` — stay
//! answerable after `no_common_version`; version-scoped verbs do not.
//!
//! ## Versions
//!
//! - **1** (Phase 1) — `ping`. Transport only.
//! - **2** (Phase 2) — the DATA PATH. A version-2 holder owns its pane's PTY.
//!   - `attach {from_offset}` → `attached {...}`, after which the holder
//!     STREAMS, unprompted, on that connection: `KIND_OUTPUT` frames (absolute
//!     offset + raw bytes), `output_lost {from_offset, to_offset}` when bytes
//!     the client asked for have left the holder's ring, and one final
//!     `exit {code, signal}` once the child has exited and every byte before
//!     the exit has been sent.
//!   - Input is a `KIND_DATA` frame of raw bytes, accepted on an attached
//!     connection only. It gets no reply.
//!   - `resize {cols, rows}`, `pause`, `resume`, `kill` and `detach` each get
//!     `ok {verb}`. On an attached connection that reply arrives INTERLEAVED
//!     with the output stream, so a client reads it as one more event.
//!   - `pause` / `resume` mirror the consumer's emission gate and NEVER pause
//!     the PTY read (the runner's one flow-control invariant: "gate emission,
//!     never pause reads" — `RemoteFlowGates`, `terminal/session.rs`). While a
//!     connection is paused the holder keeps reading the PTY into its ring and
//!     withholds frames; on `resume` it continues from that connection's own
//!     next offset, so nothing is skipped unless the ring rolled past it, which
//!     is reported as `output_lost`, never silently.
//!   - `kill` ends the child (SIGHUP to its process group, SIGKILL after a
//!     grace; Windows: terminate). `detach` closes this connection only; the
//!     child keeps running and a later client can attach and resume.
//!
//! A build only ever ADDS a version at the top of [`PROTOCOL_VERSIONS`]; the
//! version a build emits never moves backwards (D15).

use serde::{Deserialize, Serialize};

/// Every protocol version THIS build speaks, ascending. A build only ever adds
/// to this list at the top; a version is dropped only when no holder that needs
/// it can still exist (D15: holders age out with their pane).
pub const PROTOCOL_VERSIONS: &[u32] = &[1, 2];

/// The first version that carries the data path (attach, input, output, exit).
pub const DATA_PATH_VERSION: u32 = 2;

/// The highest version in both lists, or `None`.
pub fn negotiate(offered: &[u32], supported: &[u32]) -> Option<u32> {
    offered
        .iter()
        .copied()
        .filter(|v| supported.contains(v))
        .max()
}

/// A human-readable build identity for `hello_ack.holder_build`.
pub fn holder_build() -> String {
    format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
}

/// The request verbs a holder of this build answers — the POSITIVE ALLOWLIST
/// (plan D5). A `"type"` not in this list is rejected with
/// [`RejectReason::UnknownVerb`] and the connection is closed. Adding a verb
/// means adding it here AND an arm to the server's dispatch; the dispatch's
/// fall-through arm is a rejection, never a default action.
pub const REQUEST_VERBS: &[&str] = &[
    "hello",
    "census",
    "prepare_upgrade",
    "ping",
    "attach",
    "resize",
    "pause",
    "resume",
    "kill",
    "detach",
];

/// Runner → holder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// FROZEN. Must be the first frame on every connection.
    Hello { versions: Vec<u32> },
    /// FROZEN. Answerable after any `hello`, even with no common version.
    Census,
    /// FROZEN. Answerable after any `hello`. Phase 8 builds the handler; this
    /// build answers [`Reply::Unsupported`].
    PrepareUpgrade,
    /// Version 1. A liveness round trip after the handshake.
    Ping,
    /// Version 2. Start streaming this pane's output on this connection.
    /// `from_offset: None` is a fresh consumer: it gets the ring's last
    /// [`ATTACH_TAIL_BYTES`]. `Some(n)` resumes at absolute offset `n` — the
    /// first byte the consumer has NOT yet seen. Answered `attached`.
    Attach { from_offset: Option<u64> },
    /// Version 2. Resize the PTY. Answered `ok`.
    Resize { cols: u16, rows: u16 },
    /// Version 2, attached only. Withhold output frames (never the PTY read).
    Pause,
    /// Version 2, attached only. Resume output frames where they stopped.
    Resume,
    /// Version 2. End the child. Answered `ok`; the `exit` frame follows on
    /// every attached connection.
    Kill,
    /// Version 2. Close this connection; the child keeps running. Answered
    /// `ok`, then the holder closes.
    Detach,
}

impl Request {
    /// The `"type"` string this request serializes with.
    pub fn verb(&self) -> &'static str {
        match self {
            Request::Hello { .. } => "hello",
            Request::Census => "census",
            Request::PrepareUpgrade => "prepare_upgrade",
            Request::Ping => "ping",
            Request::Attach { .. } => "attach",
            Request::Resize { .. } => "resize",
            Request::Pause => "pause",
            Request::Resume => "resume",
            Request::Kill => "kill",
            Request::Detach => "detach",
        }
    }
}

/// How much of the ring a FRESH attach (`from_offset: None`) is sent — the
/// same 64 KiB the remote path ships (`REMOTE_ATTACH_TAIL_BYTES`,
/// `mcp/remote_terminal.rs`), so a local and a remote pane open alike.
pub const ATTACH_TAIL_BYTES: u64 = 64 * 1024;

/// `attached` — version 2. Where this connection's stream begins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachedReply {
    /// Absolute offset of the first stream position this connection covers.
    /// When it is below `ring_start_offset` the bytes in between are gone, and
    /// an `output_lost {start_offset, ring_start_offset}` frame comes first.
    /// A requested offset past `end_offset` (one the holder never produced —
    /// the client's offset belongs to another stream) is clamped to
    /// `end_offset`, so `start_offset < from_offset` says exactly that.
    pub start_offset: u64,
    /// Absolute offset of the oldest byte the holder's ring still holds.
    pub ring_start_offset: u64,
    /// Absolute offset one past the newest byte produced so far.
    pub end_offset: u64,
    pub child_pid: u32,
    pub cols: u16,
    pub rows: u16,
}

/// `exit` — version 2. The child exited and every byte it produced before
/// that has been sent on this connection.
///
/// `code` is `None` whenever the exit code is not known — killed by a signal
/// (then `signal` names it), or (Phase 8, D12a rule 3) an adopted pane whose
/// holder is not the child's parent. `None` must never be rendered as 0 or as
/// a fabricated failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitReply {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

/// `hello_ack` — FROZEN.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloAck {
    /// The negotiated version: the highest one both sides speak.
    pub version: u32,
    /// The holder's build identity.
    pub holder_build: String,
    /// The holder's own pid.
    pub holder_pid: u32,
    /// The pane's child pid. Always `null` in Phase 1 — there is no PTY yet —
    /// but present from day one so the envelope does not move when Phase 2
    /// fills it.
    pub child_pid: Option<u32>,
}

/// `census_reply` — FROZEN. What one holder says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CensusReply {
    pub pane_id: String,
    pub holder_pid: u32,
    pub child_pid: Option<u32>,
    pub holder_build: String,
    /// Every protocol version this holder speaks.
    pub versions: Vec<u32>,
    pub started_at_unix_ms: u64,
}

/// Why a holder refused a frame. Every rejection closes the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// The `"type"` is not in [`REQUEST_VERBS`].
    UnknownVerb,
    /// A frame kind tag this holder does not accept from a client.
    UnknownFrameKind,
    /// A data frame where none is accepted (Phase 1 accepts none at all).
    UnexpectedDataFrame,
    /// The first frame on the connection was not `hello`.
    HandshakeRequired,
    /// A second `hello` on one connection.
    DuplicateHello,
    /// A known verb that the negotiated version (or its absence) does not include.
    VerbNotInVersion,
    /// Not a JSON object with a string `"type"`, or its fields did not parse.
    Malformed,
    /// The OS-level peer check failed (Unix: peer uid is not the holder's).
    PeerNotAuthorized,
    /// The holder is at its connection cap.
    Busy,
    /// A second `attach` on one connection (version 2).
    AlreadyAttached,
    /// An attached-only verb, or an input frame, on a connection that has not
    /// attached (version 2).
    NotAttached,
    /// DESERIALIZE-ONLY: a reason string this build does not know — a newer
    /// holder's. The reason set is part of the frozen `rejected` shape, but it
    /// may GROW, so an older runner must still read the rejection as typed
    /// rather than fail to parse it. Never sent: serializing it is an error.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// Holder → runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    /// FROZEN.
    HelloAck(HelloAck),
    /// FROZEN. The holder speaks none of the offered versions; the connection
    /// stays open for the envelope verbs.
    NoCommonVersion { holder_versions: Vec<u32> },
    /// FROZEN.
    CensusReply(CensusReply),
    /// FROZEN. A known verb this build does not implement. The connection stays
    /// open.
    Unsupported { verb: String, reason: String },
    /// FROZEN. The frame was refused; the holder closes the connection after
    /// sending this.
    Rejected {
        reason: RejectReason,
        detail: String,
    },
    /// Version 1.
    Pong,
    /// Version 2: the answer to `attach`. Output frames follow.
    Attached(AttachedReply),
    /// Version 2: `resize` / `pause` / `resume` / `kill` / `detach` done.
    Ok { verb: String },
    /// Version 2: bytes `[from_offset, to_offset)` left the ring before this
    /// connection was sent them. Streaming continues at `to_offset`.
    OutputLost { from_offset: u64, to_offset: u64 },
    /// Version 2: the child exited. The last frame on the connection.
    Exit(ExitReply),
}

/// Serialize a message to its control-frame payload.
///
/// FALLIBLE, and callers must handle it: [`RejectReason::Unknown`] is
/// deserialize-only (`skip_serializing`), so a `Reply::Rejected` carrying it
/// fails to serialize. Every other message is plain data and serializes.
pub fn to_payload<T: Serialize>(msg: &T) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(msg)
}

/// Parse a request, telling an unknown verb apart from a malformed one.
///
/// This is where the allowlist is enforced: the `"type"` is checked against
/// [`REQUEST_VERBS`] BEFORE any typed parse, so a verb this build does not know
/// can never be steered into a variant by serde's own fallbacks.
pub fn parse_request(payload: &[u8]) -> Result<Request, (RejectReason, String)> {
    let value: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|e| (RejectReason::Malformed, format!("not JSON: {e}")))?;
    let verb = value
        .as_object()
        .and_then(|o| o.get("type"))
        .and_then(|t| t.as_str())
        .ok_or_else(|| {
            (
                RejectReason::Malformed,
                "a control frame must be a JSON object with a string \"type\"".to_string(),
            )
        })?;
    if !REQUEST_VERBS.contains(&verb) {
        return Err((RejectReason::UnknownVerb, format!("unknown verb {verb:?}")));
    }
    serde_json::from_value(value).map_err(|e| (RejectReason::Malformed, e.to_string()))
}

/// Parse a reply.
pub fn parse_reply(payload: &[u8]) -> Result<Reply, serde_json::Error> {
    serde_json::from_slice(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{encode_frame, KIND_CONTROL};

    fn frame_of<T: Serialize>(msg: &T) -> Vec<u8> {
        encode_frame(KIND_CONTROL, &to_payload(msg).unwrap()).unwrap()
    }

    /// Prefix + kind + the exact JSON text.
    fn expected(json: &str) -> Vec<u8> {
        let mut v = ((json.len() + 1) as u32).to_be_bytes().to_vec();
        v.push(KIND_CONTROL);
        v.extend_from_slice(json.as_bytes());
        v
    }

    fn fixture_ack() -> HelloAck {
        HelloAck {
            version: 1,
            holder_build: "qontinui-pty-holder 0.1.0".into(),
            holder_pid: 4242,
            child_pid: None,
        }
    }

    /// WIRE SHAPE — the frozen envelope, byte for byte (plan D15). If this test
    /// fails, a change moved a frozen message: a runner built after that change
    /// can no longer identify or count a holder built before it. Do not update
    /// the expected bytes to make it pass; add a new, version-scoped message
    /// instead.
    #[test]
    fn pty_holder_wire_shape_frozen_envelope() {
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (
                frame_of(&Request::Hello {
                    versions: vec![1, 2],
                }),
                r#"{"type":"hello","versions":[1,2]}"#,
            ),
            (
                frame_of(&Reply::HelloAck(fixture_ack())),
                r#"{"type":"hello_ack","version":1,"holder_build":"qontinui-pty-holder 0.1.0","holder_pid":4242,"child_pid":null}"#,
            ),
            (
                frame_of(&Reply::HelloAck(HelloAck {
                    child_pid: Some(77),
                    ..fixture_ack()
                })),
                r#"{"type":"hello_ack","version":1,"holder_build":"qontinui-pty-holder 0.1.0","holder_pid":4242,"child_pid":77}"#,
            ),
            (
                frame_of(&Reply::NoCommonVersion {
                    holder_versions: vec![1],
                }),
                r#"{"type":"no_common_version","holder_versions":[1]}"#,
            ),
            (frame_of(&Request::Census), r#"{"type":"census"}"#),
            (
                frame_of(&Reply::CensusReply(CensusReply {
                    pane_id: "p1".into(),
                    holder_pid: 4242,
                    child_pid: None,
                    holder_build: "qontinui-pty-holder 0.1.0".into(),
                    versions: vec![1],
                    started_at_unix_ms: 1_790_000_000_000,
                })),
                r#"{"type":"census_reply","pane_id":"p1","holder_pid":4242,"child_pid":null,"holder_build":"qontinui-pty-holder 0.1.0","versions":[1],"started_at_unix_ms":1790000000000}"#,
            ),
            (
                frame_of(&Request::PrepareUpgrade),
                r#"{"type":"prepare_upgrade"}"#,
            ),
            (
                frame_of(&Reply::Unsupported {
                    verb: "prepare_upgrade".into(),
                    reason: "r".into(),
                }),
                r#"{"type":"unsupported","verb":"prepare_upgrade","reason":"r"}"#,
            ),
            (
                frame_of(&Reply::Rejected {
                    reason: RejectReason::UnknownVerb,
                    detail: "d".into(),
                }),
                r#"{"type":"rejected","reason":"unknown_verb","detail":"d"}"#,
            ),
        ];
        for (got, json) in cases {
            assert_eq!(got, expected(json), "frozen envelope moved: {json}");
        }
        // And the prefix/kind bytes spelled out once, literally, for hello.
        let hello = frame_of(&Request::Hello { versions: vec![1] });
        assert_eq!(
            hello[..5],
            [0x00, 0x00, 0x00, 0x20, 0x01],
            "hello prefix is u32 BE len (31 JSON bytes + 1 kind) then kind 0x01"
        );
    }

    /// The reason vocabulary is part of the frozen `rejected` shape.
    #[test]
    fn pty_holder_wire_shape_reject_reasons() {
        let all = [
            (RejectReason::UnknownVerb, "unknown_verb"),
            (RejectReason::UnknownFrameKind, "unknown_frame_kind"),
            (RejectReason::UnexpectedDataFrame, "unexpected_data_frame"),
            (RejectReason::HandshakeRequired, "handshake_required"),
            (RejectReason::DuplicateHello, "duplicate_hello"),
            (RejectReason::VerbNotInVersion, "verb_not_in_version"),
            (RejectReason::Malformed, "malformed"),
            (RejectReason::PeerNotAuthorized, "peer_not_authorized"),
            (RejectReason::Busy, "busy"),
            (RejectReason::AlreadyAttached, "already_attached"),
            (RejectReason::NotAttached, "not_attached"),
        ];
        for (r, s) in all {
            assert_eq!(serde_json::to_string(&r).unwrap(), format!("\"{s}\""));
            let back: RejectReason = serde_json::from_str(&format!("\"{s}\"")).unwrap();
            assert_eq!(back, r);
        }
        // A newer holder's reason reads as typed Unknown, not a parse failure…
        let newer = br#"{"type":"rejected","reason":"quota_exhausted_v9","detail":"d"}"#;
        assert_eq!(
            parse_reply(newer).unwrap(),
            Reply::Rejected {
                reason: RejectReason::Unknown,
                detail: "d".into(),
            }
        );
        // …and Unknown is never put on the wire: to_payload reports it as an
        // error instead of panicking.
        assert!(serde_json::to_string(&RejectReason::Unknown).is_err());
        assert!(to_payload(&Reply::Rejected {
            reason: RejectReason::Unknown,
            detail: "d".into(),
        })
        .is_err());
    }

    /// WIRE SHAPE — the version-2 data-path messages, byte for byte. Not part
    /// of the frozen envelope (a later version may add new messages beside
    /// them), but a v2 holder still running after a runner upgrade must keep
    /// understanding — and being understood by — every runner that offers v2,
    /// so their shape is pinned exactly like the envelope's.
    #[test]
    fn pty_holder_wire_shape_v2_data_path() {
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (
                frame_of(&Request::Attach { from_offset: None }),
                r#"{"type":"attach","from_offset":null}"#,
            ),
            (
                frame_of(&Request::Attach {
                    from_offset: Some(4096),
                }),
                r#"{"type":"attach","from_offset":4096}"#,
            ),
            (
                frame_of(&Request::Resize {
                    cols: 120,
                    rows: 40,
                }),
                r#"{"type":"resize","cols":120,"rows":40}"#,
            ),
            (frame_of(&Request::Pause), r#"{"type":"pause"}"#),
            (frame_of(&Request::Resume), r#"{"type":"resume"}"#),
            (frame_of(&Request::Kill), r#"{"type":"kill"}"#),
            (frame_of(&Request::Detach), r#"{"type":"detach"}"#),
            (
                frame_of(&Reply::Attached(AttachedReply {
                    start_offset: 10,
                    ring_start_offset: 5,
                    end_offset: 99,
                    child_pid: 4243,
                    cols: 80,
                    rows: 24,
                })),
                r#"{"type":"attached","start_offset":10,"ring_start_offset":5,"end_offset":99,"child_pid":4243,"cols":80,"rows":24}"#,
            ),
            (
                frame_of(&Reply::Ok {
                    verb: "resize".into(),
                }),
                r#"{"type":"ok","verb":"resize"}"#,
            ),
            (
                frame_of(&Reply::OutputLost {
                    from_offset: 0,
                    to_offset: 8192,
                }),
                r#"{"type":"output_lost","from_offset":0,"to_offset":8192}"#,
            ),
            (
                frame_of(&Reply::Exit(ExitReply {
                    code: Some(7),
                    signal: None,
                })),
                r#"{"type":"exit","code":7,"signal":null}"#,
            ),
            (
                frame_of(&Reply::Exit(ExitReply {
                    code: None,
                    signal: Some(9),
                })),
                r#"{"type":"exit","code":null,"signal":9}"#,
            ),
        ];
        for (got, json) in cases {
            assert_eq!(got, expected(json), "v2 data-path message moved: {json}");
        }
        // Every message parses back to itself.
        let exit = br#"{"type":"exit","code":null,"signal":null}"#;
        assert_eq!(
            parse_reply(exit).unwrap(),
            Reply::Exit(ExitReply {
                code: None,
                signal: None
            })
        );
        assert_eq!(
            parse_request(br#"{"type":"attach","from_offset":null}"#).unwrap(),
            Request::Attach { from_offset: None }
        );
    }

    /// The version list only grows at the top, and the data path is in it.
    #[test]
    fn pty_holder_versions_ascend_and_include_the_data_path() {
        assert!(PROTOCOL_VERSIONS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(PROTOCOL_VERSIONS.first(), Some(&1), "v1 is still spoken");
        assert!(PROTOCOL_VERSIONS.contains(&DATA_PATH_VERSION));
        // A Phase 1 runner (offers only [1]) still negotiates with this holder.
        assert_eq!(negotiate(&[1], PROTOCOL_VERSIONS), Some(1));
    }

    /// A newer holder may add fields to the envelope replies; an older runner
    /// must still read them. And the frozen bytes above parse back.
    #[test]
    fn pty_holder_envelope_readers_tolerate_added_fields() {
        let newer = br#"{"type":"hello_ack","version":3,"holder_build":"x","holder_pid":1,"child_pid":2,"future_field":{"a":1}}"#;
        assert_eq!(
            parse_reply(newer).unwrap(),
            Reply::HelloAck(HelloAck {
                version: 3,
                holder_build: "x".into(),
                holder_pid: 1,
                child_pid: Some(2),
            })
        );
        let hello = br#"{"type":"hello","versions":[1,7],"client_build":"later"}"#;
        assert_eq!(
            parse_request(hello).unwrap(),
            Request::Hello {
                versions: vec![1, 7]
            }
        );
    }

    #[test]
    fn pty_holder_negotiation_picks_highest_common() {
        assert_eq!(negotiate(&[1, 2, 3], &[2, 3]), Some(3));
        assert_eq!(negotiate(&[3, 1], &[1, 2]), Some(1));
        assert_eq!(negotiate(&[9], &[1, 2]), None);
        assert_eq!(negotiate(&[], PROTOCOL_VERSIONS), None);
        assert_eq!(
            negotiate(PROTOCOL_VERSIONS, PROTOCOL_VERSIONS),
            PROTOCOL_VERSIONS.iter().copied().max()
        );
    }

    #[test]
    fn pty_holder_parse_request_allowlist() {
        let err = parse_request(br#"{"type":"terminal_create","cwd":"/"}"#).unwrap_err();
        assert_eq!(err.0, RejectReason::UnknownVerb);
        // A reply type sent as a request is not a request verb.
        let err = parse_request(br#"{"type":"hello_ack","version":1}"#).unwrap_err();
        assert_eq!(err.0, RejectReason::UnknownVerb);
        for bad in [&b"[1,2]"[..], b"{}", br#"{"type":5}"#, b"\xff\xfe", b""] {
            assert_eq!(parse_request(bad).unwrap_err().0, RejectReason::Malformed);
        }
        // Known verb, wrong fields.
        let err = parse_request(br#"{"type":"hello","versions":"one"}"#).unwrap_err();
        assert_eq!(err.0, RejectReason::Malformed);
        for verb in REQUEST_VERBS {
            let req = match *verb {
                "hello" => Request::Hello { versions: vec![1] },
                "census" => Request::Census,
                "prepare_upgrade" => Request::PrepareUpgrade,
                "ping" => Request::Ping,
                "attach" => Request::Attach {
                    from_offset: Some(3),
                },
                "resize" => Request::Resize { cols: 80, rows: 24 },
                "pause" => Request::Pause,
                "resume" => Request::Resume,
                "kill" => Request::Kill,
                "detach" => Request::Detach,
                other => panic!("REQUEST_VERBS has {other:?} with no Request variant"),
            };
            assert_eq!(req.verb(), *verb);
            assert_eq!(parse_request(&to_payload(&req).unwrap()).unwrap(), req);
        }
    }
}
