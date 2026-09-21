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
//!
//! # One static, private handles in tests
//!
//! The ring and its sequence counter have an identity — [`TraceRing`] — so a
//! test can own a PRIVATE one. Production keeps exactly one instance, the
//! [`RING`] static, and the three public entry points are pure delegations to
//! it. Before the handle existed the ring was a process-global `VecDeque` that
//! every unit test of this binary shared: `ring_is_bounded` pushes
//! `RING_CAPACITY + 10` entries, and a sibling asserting an exact drain was
//! recorded red when the two interleaved under `cargo test`'s parallel threads
//! (finding 2026-09-16) — a red that passed alone and passed under
//! `--test-threads=1`. The window is microseconds wide, so the interleave is
//! rare: measured 2026-09-21 by running this module's tests alone at 8 threads,
//! 0 reds in 100 runs and 2 in 1000 (one per victim — both `drain_since_*`
//! tests, each evicted by the sibling's 74 pushes). A private ring per test
//! cannot interleave with anything, so there is nothing left to serialise and
//! nothing for a future test to forget to lock (plan
//! 2026-09-17-runner-tests-share-in-process-mutable-state, Phase 2).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

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

/// One entry of the trace ring: the command that produced the trace and the
/// sequence number that orders it.
struct Entry {
    seq: u64,
    command: &'static str,
    trace: OutboundTrace,
}

/// The trace ring paired with the sequence counter that stamps its entries.
///
/// Giving the pair an identity is what lets a test own a private instance
/// (see the module docs). Production never constructs one: it charges every
/// call to the single [`RING`] static through the `pub fn` delegations below,
/// and the `_in` functions that do the work are module-private so no call
/// site can choose a ring.
///
/// `const fn new()` so the static needs no `OnceLock`: `Mutex::new` and
/// `VecDeque::new` are both const. `VecDeque::with_capacity` is not, so the
/// former 64-slot pre-allocation is gone — the ring grows to capacity once,
/// on its first 64 records, and never again.
struct TraceRing {
    ring: Mutex<VecDeque<Entry>>,
    seq: AtomicU64,
}

impl TraceRing {
    const fn new() -> Self {
        Self {
            ring: Mutex::new(VecDeque::new()),
            seq: AtomicU64::new(0),
        }
    }
}

/// The process-global trace ring — the one every production call is charged to.
static RING: TraceRing = TraceRing::new();

/// The current sequence value. Snapshot this *before* triggering a command, then
/// pass it to [`drain_since`] to collect only the traces that command produced.
pub fn current_seq() -> u64 {
    // A PURE DELEGATION, and load-bearing as such: the logic lives in the `_in`
    // function, which the tests exercise against a private ring, so the code
    // production calls is the code the tests cover. `the_public_ring_api_only_delegates`
    // pins the shape so a second global cannot creep back into this wrapper.
    current_seq_in(&RING)
}

/// Record one outbound call made by `command`.
///
/// `command` is `&'static str` so only in-tree call sites can register — a
/// trace can never be attributed to a caller-supplied name.
pub fn record(command: &'static str, trace: OutboundTrace) {
    // Pure delegation — see `current_seq`.
    record_in(&RING, command, trace)
}

/// Traces recorded for `command` with a sequence greater than `since_seq`,
/// oldest first. Non-destructive (the ring is a fixed-size window, so entries
/// age out rather than being consumed) — the name mirrors the caller's intent
/// of "everything since I started".
pub fn drain_since(command: &str, since_seq: u64) -> Vec<OutboundTrace> {
    // Pure delegation — see `current_seq`.
    drain_since_in(&RING, command, since_seq)
}

/// [`current_seq`] against an explicit ring.
fn current_seq_in(ring: &TraceRing) -> u64 {
    ring.seq.load(Ordering::SeqCst)
}

/// [`record`] against an explicit ring. Module-private on purpose — the
/// parameter exists so the tests can cover THIS function, the one that does
/// the work, against a ring no sibling test can fill.
fn record_in(ring: &TraceRing, command: &'static str, trace: OutboundTrace) {
    let seq = ring.seq.fetch_add(1, Ordering::SeqCst) + 1;
    let Ok(mut entries) = ring.ring.lock() else {
        // A poisoned ring must never take down a command's real work — the
        // trace is a debugging aid, not a correctness dependency.
        return;
    };
    if entries.len() == RING_CAPACITY {
        entries.pop_front();
    }
    entries.push_back(Entry {
        seq,
        command,
        trace,
    });
}

/// [`drain_since`] against an explicit ring. Module-private for the same reason
/// as [`record_in`].
fn drain_since_in(ring: &TraceRing, command: &str, since_seq: u64) -> Vec<OutboundTrace> {
    let Ok(entries) = ring.ring.lock() else {
        return Vec::new();
    };
    entries
        .iter()
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
        // A private ring: no sibling test can push entries into it, so the
        // exact assertion below cannot be reddened by `ring_is_bounded`'s 74
        // records landing in the same window.
        let ring = TraceRing::new();
        record_in(&ring, "cmd_a", trace(200));
        let start = current_seq_in(&ring);
        record_in(&ring, "cmd_a", trace(201));
        record_in(&ring, "cmd_a", trace(202));

        let got = drain_since_in(&ring, "cmd_a", start);
        let statuses: Vec<u16> = got.iter().map(|t| t.status).collect();
        // The pre-snapshot 200 is excluded; both post-snapshot traces are in order.
        assert_eq!(statuses, vec![201, 202]);
    }

    #[test]
    fn drain_since_filters_by_command() {
        let ring = TraceRing::new();
        let start = current_seq_in(&ring);
        record_in(&ring, "cmd_b", trace(200));
        record_in(&ring, "cmd_c", trace(500));

        let got = drain_since_in(&ring, "cmd_b", start);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].status, 200);
        // A different command's trace never bleeds across.
        assert!(drain_since_in(&ring, "cmd_b", start)
            .iter()
            .all(|t| t.status != 500));
    }

    /// Two rings share nothing — the property every other ring test relies on.
    #[test]
    fn rings_are_independent() {
        let a = TraceRing::new();
        let b = TraceRing::new();
        record_in(&a, "cmd_a", trace(200));
        assert_eq!(current_seq_in(&a), 1);
        assert_eq!(current_seq_in(&b), 0);
        assert!(drain_since_in(&b, "cmd_a", 0).is_empty());
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
        let ring = TraceRing::new();
        let start = current_seq_in(&ring);
        for _ in 0..(RING_CAPACITY + 10) {
            record_in(&ring, "cmd_ring", trace(200));
        }
        // Never grows past capacity, even though more were recorded. Exact,
        // not `<=`: on a private ring the only writer is this loop.
        assert_eq!(
            drain_since_in(&ring, "cmd_ring", start).len(),
            RING_CAPACITY
        );
    }

    // ---- the public API stays a pure delegation to the one static ----

    /// The production half of this file, with the test module cut off — the
    /// same rule as `wedge_diagnostics.rs`'s pins: a pin must never scan
    /// `#[cfg(test)]` code, or its negative assertions match their own string
    /// literals.
    fn prod_part(src: &str) -> &str {
        src.split_once("\n#[cfg(test)]\nmod ")
            .map_or(src, |(before, _)| before)
    }

    /// Strip whole-line comments, then all whitespace, so the pin matches
    /// regardless of how `rustfmt` wrapped a line and never trips on the body
    /// comment that explains the delegation.
    fn squeezed_code(src: &str) -> String {
        src.lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .flat_map(|l| l.chars())
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    /// The raw and squeezed body of the production fn whose signature line
    /// starts with `signature`. Panics — rather than returning an empty slice
    /// that would pass every negative assertion — when the signature is absent.
    fn wrapper_body<'a>(prod: &'a str, signature: &str) -> (&'a str, String) {
        let start = prod.find(signature).unwrap_or_else(|| {
            panic!(
                "this pin could not find `{signature}` in the production half of \
                 this module. If the wrapper was renamed or its signature \
                 reformatted, update this pin in the same change — otherwise it \
                 silently stops guarding anything."
            )
        });
        let body_start = start
            + prod[start..]
                .find('{')
                .expect("the wrapper must have a body");
        let body_end = body_start
            + prod[body_start..]
                .find("\n}\n")
                .expect("the wrapper's body must be closed at column 0");
        let raw = &prod[body_start..body_end];
        let squeezed = squeezed_code(raw);
        // Both bounds, loose on top: a collapsed slice asserts nothing, and a
        // slice that ran away past several fns is a parse failure rather than
        // an edit. A wrapper that merely grew a body of its own lands well
        // inside the bound, so it is named by the delegation assertion in the
        // caller, not mis-reported as a parse error here (a 3-line own body
        // squeezes to ~140 chars; a real delegation to ~20–40).
        assert!(
            (10..1000).contains(&squeezed.len()),
            "the pin sliced a {}-char body out of `{signature}` — it is \
             mis-parsing, and in one direction or the other that leaves it \
             vacuous. Raw slice:\n{raw}",
            squeezed.len()
        );
        (raw, squeezed)
    }

    /// **The public API is three one-line delegations to `&RING`, and the
    /// production half declares exactly one static.** Population-independent,
    /// which no runtime assertion against the global ring can be: the `_in`
    /// functions are what the ring tests cover, so a wrapper that grows its own
    /// `lock()`/`fetch_add` body — or a second `static` beside `RING` — is code
    /// nothing covers and the shared-state class this handle removed.
    ///
    /// `include_str!` rather than a directory walk: the compiler resolves it,
    /// so this pin cannot scan the wrong tree and pass vacuously.
    #[test]
    fn the_public_ring_api_only_delegates() {
        const SRC: &str = include_str!("outbound_trace.rs");
        let prod = prod_part(SRC);

        for (signature, expected) in [
            ("pub fn current_seq() -> u64", "current_seq_in(&RING)"),
            (
                "pub fn record(command: &'static str, trace: OutboundTrace)",
                "record_in(&RING,command,trace)",
            ),
            (
                "pub fn drain_since(command: &str, since_seq: u64) -> Vec<OutboundTrace>",
                "drain_since_in(&RING,command,since_seq)",
            ),
        ] {
            let (raw, body) = wrapper_body(prod, signature);
            assert_eq!(
                body,
                format!("{{{expected}"),
                "`{signature}` is no longer a pure delegation to `{expected}`. If \
                 this is a deliberate signature change and the wrapper is STILL a \
                 one-line delegation, update this pin's expected spelling in the \
                 same change. Body is now:\n{raw}"
            );
        }

        // ONE static. A `static SEQ` or an `OnceLock` ring reintroduced anywhere
        // in the production half — inside a fn body included — is the second
        // global this handle exists to prevent.
        let statics: Vec<&str> = prod
            .lines()
            .map(str::trim_start)
            .filter(|l| l.starts_with("static "))
            .collect();
        assert_eq!(
            statics,
            vec!["static RING: TraceRing = TraceRing::new();"],
            "the production half must declare exactly one static, `RING`. A \
             second one is shared mutable state that every test of this binary \
             would see again; give it a field on `TraceRing` instead. Found:\n{}",
            statics.join("\n")
        );
        // Comments stripped first: `TraceRing`'s own doc names `OnceLock` in
        // prose to explain why there is none.
        assert!(
            !squeezed_code(prod).contains("OnceLock"),
            "`OnceLock` is back in the production half — `TraceRing::new` is \
             `const`, so the static needs no lazy init"
        );
    }
}
