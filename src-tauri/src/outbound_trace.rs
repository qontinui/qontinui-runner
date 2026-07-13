//! Per-command outbound-call trace (P5 of plan
//! 2026-07-08-ui-bridge-reach-and-verify-gated-flows).
//!
//! Turns "reproduce the call by hand to see what it sent" into "read what the
//! command actually did". A Tauri command that makes an outbound HTTP call can
//! [`record`] a **redacted** trace of it; the UI Bridge observe tier
//! (`mcp/ui_bridge/gated_flow.rs`) surfaces the traces produced during its own
//! round-trip.
//!
//! # What is captured — and what is NEVER captured
//!
//! [`OutboundTrace`] carries `{ method, host, path, status, bearer_kind,
//! response_shape }` and nothing else. It **never** carries the bearer token
//! (only its *kind* — `"access"`/`"id"`/`"device"`), and **never** the request
//! or response body (only a [`response_shape`](shape_of) descriptor: top-level
//! key names mapped to type names, plus array lengths). Query strings are
//! dropped from `path` for the same reason — they routinely carry tokens.
//!
//! # Opt-in
//!
//! Recording only happens where a call site explicitly calls [`record`], and
//! traces are only *surfaced* for commands in
//! [`crate::ui_bridge_invoke::traces_outbound`]. Both halves must agree, so a
//! trace can never leak by default.
//!
//! # Correlation
//!
//! The outbound call happens inside the Tauri command, which the frontend
//! dispatches — there is no request id threaded across the IPC boundary to
//! correlate against. Instead every trace is stamped with a monotonically
//! increasing sequence number ([`current_seq`]). The observe handler snapshots
//! the sequence *before* it emits the invoke request and then asks for traces
//! for that command with a greater sequence ([`drain_since`]), which bounds a
//! trace to the round-trip that caused it. This is best-effort: a concurrent
//! invocation of the *same* command could interleave, so the result is a list,
//! not a single trace.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use serde_json::Value;

/// How many traces to retain. Small: the traces are a debugging aid read
/// moments after the call, not an audit log.
const RING_CAPACITY: usize = 64;

/// A redacted record of one outbound HTTP call made by a Tauri command.
///
/// Never contains the bearer token or any request/response body — see the
/// module docs.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OutboundTrace {
    /// HTTP method, e.g. `"GET"`.
    pub method: String,
    /// Host only, e.g. `"api.qontinui.io"` — no scheme, no credentials.
    pub host: String,
    /// Path only, e.g. `"/api/v1/operations/github/repos"`. The query string is
    /// deliberately dropped (it routinely carries tokens).
    pub path: String,
    /// HTTP status code the upstream returned.
    pub status: u16,
    /// Which bearer the runner presented: `"access"` | `"id"` | `"device"` |
    /// `"none"`. NEVER the token itself.
    pub bearer_kind: String,
    /// Type-only descriptor of the response body — key names and types, never
    /// values. `None` for a non-JSON or error response body.
    pub response_shape: Option<Value>,
}

/// The trace ring, paired with the command that produced each entry and the
/// sequence number that orders it.
struct Entry {
    seq: u64,
    command: &'static str,
    trace: OutboundTrace,
}

fn ring() -> &'static Mutex<VecDeque<Entry>> {
    static RING: OnceLock<Mutex<VecDeque<Entry>>> = OnceLock::new();
    RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(RING_CAPACITY)))
}

fn seq_counter() -> &'static AtomicU64 {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    &SEQ
}

/// The current sequence value. Snapshot this *before* triggering a command, then
/// pass it to [`drain_since`] to collect only the traces that command produced.
pub fn current_seq() -> u64 {
    seq_counter().load(Ordering::SeqCst)
}

/// Record one outbound call made by `command`.
///
/// `command` is `&'static str` so only in-tree call sites can register — a
/// trace can never be attributed to a caller-supplied name.
pub fn record(command: &'static str, trace: OutboundTrace) {
    let seq = seq_counter().fetch_add(1, Ordering::SeqCst) + 1;
    let Ok(mut ring) = ring().lock() else {
        // A poisoned ring must never take down a command's real work — the
        // trace is a debugging aid, not a correctness dependency.
        return;
    };
    if ring.len() == RING_CAPACITY {
        ring.pop_front();
    }
    ring.push_back(Entry {
        seq,
        command,
        trace,
    });
}

/// Traces recorded for `command` with a sequence greater than `since_seq`,
/// oldest first. Non-destructive (the ring is a fixed-size window, so entries
/// age out rather than being consumed) — the name mirrors the caller's intent
/// of "everything since I started".
pub fn drain_since(command: &str, since_seq: u64) -> Vec<OutboundTrace> {
    let Ok(ring) = ring().lock() else {
        return Vec::new();
    };
    ring.iter()
        .filter(|e| e.command == command && e.seq > since_seq)
        .map(|e| e.trace.clone())
        .collect()
}

/// Split a URL into `(host, path)`, dropping scheme, any userinfo, and the
/// query string. Falls back to `("unknown", <input>)` only when the URL cannot
/// be parsed — in which case the query is still stripped.
pub fn split_url(url: &str) -> (String, String) {
    match reqwest::Url::parse(url) {
        Ok(u) => (
            u.host_str().unwrap_or("unknown").to_string(),
            u.path().to_string(),
        ),
        Err(_) => {
            let no_query = url.split('?').next().unwrap_or(url);
            ("unknown".to_string(), no_query.to_string())
        }
    }
}

/// A type-only descriptor of a JSON value: key names and type names, never
/// values. Arrays report their length (a count is a non-secret outcome fact —
/// it is exactly what the `repo_count` projection already exposes).
///
/// `{"connected": true, "repos": [{...}, {...}]}` → `{"connected": "bool",
/// "repos": "array[2]"}`.
pub fn shape_of(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), Value::String(type_name_of(v)));
            }
            Value::Object(out)
        }
        other => Value::String(type_name_of(other)),
    }
}

fn type_name_of(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::Number(_) => "number".to_string(),
        Value::String(_) => "string".to_string(),
        Value::Array(a) => format!("array[{}]", a.len()),
        Value::Object(o) => format!("object[{}]", o.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(status: u16) -> OutboundTrace {
        OutboundTrace {
            method: "GET".to_string(),
            host: "api.qontinui.io".to_string(),
            path: "/api/v1/operations/github/repos".to_string(),
            status,
            bearer_kind: "access".to_string(),
            response_shape: None,
        }
    }

    #[test]
    fn drain_since_returns_only_traces_after_the_snapshot() {
        record("cmd_a", trace(200));
        let start = current_seq();
        record("cmd_a", trace(201));
        record("cmd_a", trace(202));

        let got = drain_since("cmd_a", start);
        let statuses: Vec<u16> = got.iter().map(|t| t.status).collect();
        // The pre-snapshot 200 is excluded; both post-snapshot traces are in order.
        assert_eq!(statuses, vec![201, 202]);
    }

    #[test]
    fn drain_since_filters_by_command() {
        let start = current_seq();
        record("cmd_b", trace(200));
        record("cmd_c", trace(500));

        let got = drain_since("cmd_b", start);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].status, 200);
        // A different command's trace never bleeds across.
        assert!(drain_since("cmd_b", start).iter().all(|t| t.status != 500));
    }

    #[test]
    fn split_url_drops_scheme_and_query() {
        let (host, path) = split_url("https://api.qontinui.io/api/v1/repos?token=SECRET&x=1");
        assert_eq!(host, "api.qontinui.io");
        assert_eq!(path, "/api/v1/repos");
        // The token in the query string must not survive anywhere.
        assert!(!path.contains("SECRET"));
        assert!(!host.contains("SECRET"));
    }

    #[test]
    fn split_url_strips_query_even_when_unparseable() {
        let (host, path) = split_url("not a url?token=SECRET");
        assert_eq!(host, "unknown");
        assert!(!path.contains("SECRET"));
    }

    #[test]
    fn shape_of_reports_types_and_lengths_never_values() {
        let body = serde_json::json!({
            "connected": true,
            "repos": [
                {"name": "acme/app", "url": "https://github.com/acme/app"},
                {"name": "acme/lib", "url": "https://github.com/acme/lib"}
            ],
            "login": "octocat"
        });
        let shape = shape_of(&body);
        assert_eq!(
            shape,
            serde_json::json!({
                "connected": "bool",
                "repos": "array[2]",
                "login": "string"
            })
        );
        // No response VALUES survive the shape descriptor.
        let rendered = shape.to_string();
        assert!(!rendered.contains("octocat"));
        assert!(!rendered.contains("acme/app"));
        assert!(!rendered.contains("github.com"));
    }

    #[test]
    fn ring_is_bounded() {
        let start = current_seq();
        for _ in 0..(RING_CAPACITY + 10) {
            record("cmd_ring", trace(200));
        }
        // Never grows past capacity, even though more were recorded.
        assert!(drain_since("cmd_ring", start).len() <= RING_CAPACITY);
    }
}
