//! The coord **WORK** axis for terminal-hosted sessions, read in bulk for
//! `GET /restart-readiness` (plan
//! `2026-09-10-restart-readiness-counts-open-sessions-not-active-ones`).
//!
//! ## Why this exists
//!
//! `/restart-readiness` measured process LIVENESS and nothing else, so on a
//! headless box — where an operator cannot close a finished session — every
//! session ever opened blocked a restart forever. Measured on `merytshost`
//! 2026-09-10: 16 live `claude` processes, 16 counted as blockers, oldest
//! 11h04m, `hasLiveChildren: false` on every one of them, and a verdict that
//! said the same thing whether one session was mid-build or all sixteen had
//! finished hours earlier.
//!
//! `coord.sessions.session_status` is the axis that answers it, and it has been
//! written all along — `/finish-session` sets `finished` on it explicitly so
//! *"a rebuilt runner offers only UNFINISHED sessions for resume"*. The door
//! below is the device-authed bulk read coord shipped for this consumer
//! (`qontinui-coord` `crates/coord/src/session_work_status.rs`, whose module
//! docs say: *"The qontinui-runner knows its sessions by `claude_session_id`
//! and nothing else."*).
//!
//! ## Why coord and not a local mirror
//!
//! Checked on `qontinui-runner@8d90065fe`: `TerminalSessionRecord` carries
//! `state`/`close_reason`/`restore_pending_at`/`confirmed_at` and **no work
//! axis** (`grep -rn 'finished_at\|set_finished\|finish_synced' src/` → zero
//! hits, so plan `2026-09-01-session-finished-marker-and-unfinished-resume`
//! Phase 2 has not shipped), and nothing in the runner had ever called this
//! door. There is no local source to prefer. **When that Phase 2 cache lands,
//! prefer it here and keep this as the fallback** — [`fetch`] is the only
//! seam that would change, and [`crate::session::tracking_health::evaluate`]
//! is already indifferent to where the map came from.
//!
//! ## Bounded, and it can only DEGRADE
//!
//! `/restart-readiness` is a fast local read that gates a destructive
//! operation, and `dev-start.ps1` / the supervisor / an operator all block on
//! it. **A coord round-trip must therefore never make it fail or hang.** So:
//!
//! - a [`CREDENTIAL_TIMEOUT`] of 2 s on resolving the device JWT and a
//!   [`FETCH_TIMEOUT`] of 2 s on the HTTP call — **≈4 s worst case in total**,
//!   and that is the honest number: `AuthManager::get_access_token()` is
//!   SYNCHRONOUS and can reach the OS keychain (bounded there at 3 s by
//!   `auth::KEYCHAIN_CALL_TIMEOUT`), so it is run on `spawn_blocking` under
//!   its own timeout rather than on a tokio worker thread. Timing out on the
//!   credential is a `degraded` result like any other;
//! - an empty id list short-circuits with **zero** coord traffic;
//! - **every** failure — no JWT, transport error, timeout, non-2xx,
//!   undecodable body, `sessionBridgeColumnPresent: false` — yields an EMPTY
//!   map plus `degraded: true` and a note naming what was seen.
//!
//! An empty map is not a special case: with no statuses resolved, every live
//! process blocks, which is bit-for-bit the pre-2026-09-10 verdict. The
//! degradation is reported in the response body rather than escalating the
//! whole verdict to UNKNOWN — escalating would make a coord blip unreadable
//! without making it any safer than fail-closed already is.
//!
//! ## No cache
//!
//! [`crate::mcp::continuation_verdict`] TTL-caches its status read; this does
//! not. A 45 s TTL would let a session finished 10 s ago read as blocking
//! (safe) but equally let a REOPENED session read as finished (not safe). One
//! bulk request per invocation of a human-triggered endpoint is cheap enough
//! that the trade is not worth taking.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;
use tracing::debug;

use crate::session::tracking_health::SessionWorkStatus;

/// Hard client timeout for the work-axis read. See module docs — this sits in
/// front of a human deciding whether to destroy running work.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(2);

/// Hard bound on resolving the device JWT. `AuthManager::get_access_token()`
/// reads secure storage and, on a miss, the OS keychain — **it does not
/// refresh anything** (`auth.rs`), it is synchronous, and the keychain leg has
/// its own 3 s bound. Run under `spawn_blocking` with this timeout so a wedged
/// keychain costs a `degraded` answer rather than a stalled executor thread.
pub const CREDENTIAL_TIMEOUT: Duration = Duration::from_secs(2);

/// coord's own clamp (`session_work_status::MAX_IDS`). Two orders of magnitude
/// above the busiest box measured (16), but a caller must not silently send
/// more than coord will answer: over the cap we send the first [`MAX_IDS`] and
/// SAY the request was clamped — the remainder resolve to no status and
/// therefore block.
pub const MAX_IDS: usize = 500;

/// The route, quoted verbatim in the response so a reader can re-run it.
pub const DOOR: &str = "GET {coord}/coord/sessions/work-status?claude_code_session_ids=<csv>";

// ---------------------------------------------------------------------------
// Wire types (coord's `WorkStatusResponse`)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct WireRow {
    /// `None` = the row exists and its work axis is unset. A real observation,
    /// and still UNKNOWN for our purposes — never "not finished".
    session_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireResponse {
    #[serde(default)]
    statuses: HashMap<String, WireRow>,
    #[serde(default)]
    unknown: Vec<String>,
    #[serde(default)]
    invalid: Vec<String>,
    #[serde(default)]
    truncated: bool,
    /// `false` means the join could not be expressed on that database at all —
    /// so every `statuses` entry it did or did not return is meaningless.
    #[serde(rename = "sessionBridgeColumnPresent", default = "default_true")]
    session_bridge_column_present: bool,
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

/// One work-axis read. **Never an error type** — a failure is a `degraded`
/// result with an empty map, because the caller must still answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusFetch {
    /// `claude_session_id` → the work axis coord served for it. Absent ⇒
    /// UNKNOWN ⇒ blocks.
    pub by_session_id: HashMap<String, SessionWorkStatus>,
    /// `"coord"` when the door answered, `"unavailable"` when it did not,
    /// `"not_needed"` when there was nothing to ask about.
    pub source: &'static str,
    pub degraded: bool,
    /// Names what was actually seen. Empty on a clean read.
    pub note: String,
    pub requested: usize,
    pub resolved: usize,
}

impl StatusFetch {
    /// The lifecycle store did not resolve, so the ids to ask about are not
    /// knowable — the axis was never consulted. Degraded, not `not_needed`.
    pub fn store_unavailable() -> Self {
        Self::degraded(
            0,
            "coord work-status: the SessionLifecycleStore did not resolve, so no session ids              could be assembled and the coord work axis was never consulted",
        )
    }

    /// A degraded result: empty map, cause named, every process blocks.
    fn degraded(requested: usize, note: impl Into<String>) -> Self {
        Self {
            by_session_id: HashMap::new(),
            source: "unavailable",
            degraded: true,
            note: note.into(),
            requested,
            resolved: 0,
        }
    }
}

/// The response block `/restart-readiness` emits so an operator can see WHERE
/// the discounting evidence came from, and whether it was there at all.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionStatusSource {
    /// `"coord"` | `"unavailable"` | `"not_needed"`.
    pub source: &'static str,
    /// The route, so the read is reproducible by hand.
    pub door: &'static str,
    pub requested: usize,
    pub resolved: usize,
    /// `true` when the work axis could NOT be read. Every process then blocks
    /// — the verdict is the pre-work-axis one, and this field is how a reader
    /// tells that apart from "coord said nothing is finished".
    pub degraded: bool,
    /// What was seen. Empty on a clean read.
    pub note: String,
}

impl From<&StatusFetch> for SessionStatusSource {
    fn from(f: &StatusFetch) -> Self {
        Self {
            source: f.source,
            door: DOOR,
            requested: f.requested,
            resolved: f.resolved,
            degraded: f.degraded,
            note: f.note.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Pure shaping (the unit-test surface)
// ---------------------------------------------------------------------------

/// Turn a decoded coord body into the map [`crate::session::tracking_health`]
/// consumes. Pure — no I/O, no clock.
///
/// `sessionBridgeColumnPresent: false` discards the WHOLE body: the join could
/// not be expressed, so nothing it returned is an observation about the work
/// axis. `unknown` / `invalid` ids and rows whose `session_status` is `null`
/// simply produce no entry, which is UNKNOWN, which blocks.
fn map_from_body(body: &WireResponse) -> (HashMap<String, SessionWorkStatus>, String) {
    if !body.session_bridge_column_present {
        return (
            HashMap::new(),
            "coord reported sessionBridgeColumnPresent: false — the work-axis join \
             is not expressible on that database, so no status was observed"
                .to_string(),
        );
    }
    let mut out = HashMap::new();
    for (id, row) in &body.statuses {
        if let Some(raw) = row.session_status.as_deref() {
            if !raw.trim().is_empty() {
                out.insert(id.clone(), SessionWorkStatus::parse(raw));
            }
        }
    }
    let mut notes: Vec<String> = Vec::new();
    if !body.unknown.is_empty() {
        notes.push(format!(
            "{} id(s) resolved to no coord session (unknown — not \"unfinished\")",
            body.unknown.len()
        ));
    }
    if !body.invalid.is_empty() {
        notes.push(format!("{} id(s) were not UUIDs", body.invalid.len()));
    }
    if body.truncated {
        notes.push("coord truncated the request (over its id cap)".to_string());
    }
    (out, notes.join("; "))
}

// ---------------------------------------------------------------------------
// The read
// ---------------------------------------------------------------------------

/// Bulk-read the coord work axis for `ids`. **Never fails** — see module docs.
pub async fn fetch(ids: &[String]) -> StatusFetch {
    if ids.is_empty() {
        return StatusFetch {
            by_session_id: HashMap::new(),
            source: "not_needed",
            degraded: false,
            note: String::new(),
            requested: 0,
            resolved: 0,
        };
    }

    let requested = ids.len();
    let clamped: Vec<&String> = ids.iter().take(MAX_IDS).collect();
    let clamp_note = if requested > MAX_IDS {
        format!(
            "; only the first {MAX_IDS} of {requested} ids were sent (coord's cap) — \
             the remainder have no status and therefore block"
        )
    } else {
        String::new()
    };

    // Reuse the existing device-JWT + coord-base resolution rather than
    // growing a second auth path. It reads the runner's OWN stored device
    // token (secure storage, then the OS keychain) — NOT the routinely stale
    // `~/.qontinui/coord-device-jwt` file — but it neither refreshes nor
    // validates it, so an expired token surfaces below as an HTTP 401 and
    // therefore as `degraded`, never as "nothing is finished".
    //
    // It is SYNCHRONOUS and can block (file I/O, then a keychain call bounded
    // at 3 s), so it runs on a blocking thread under its own timeout. See
    // `CREDENTIAL_TIMEOUT`.
    let parts = tokio::time::timeout(
        CREDENTIAL_TIMEOUT,
        tokio::task::spawn_blocking(crate::mcp::continuation_verdict::coord_client_parts),
    )
    .await;
    let (base, jwt) = match parts {
        Ok(Ok(Ok(p))) => p,
        Ok(Ok(Err(e))) => {
            return StatusFetch::degraded(
                requested,
                format!("coord work-status: no credential ({e}){clamp_note}"),
            )
        }
        Ok(Err(e)) => {
            return StatusFetch::degraded(
                requested,
                format!("coord work-status: credential resolution panicked ({e}){clamp_note}"),
            )
        }
        Err(_) => {
            return StatusFetch::degraded(
                requested,
                format!(
                    "coord work-status: credential resolution timed out after {}s{clamp_note}",
                    CREDENTIAL_TIMEOUT.as_secs()
                ),
            )
        }
    };
    let client = match reqwest::Client::builder().timeout(FETCH_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            return StatusFetch::degraded(
                requested,
                format!("coord work-status: client build failed ({e}){clamp_note}"),
            )
        }
    };

    let csv = clamped
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let url = format!("{base}/coord/sessions/work-status?claude_code_session_ids={csv}");

    let resp = match client.get(&url).bearer_auth(&jwt).send().await {
        Ok(r) => r,
        Err(e) => {
            let what = if e.is_timeout() {
                format!("request timed out after {}s", FETCH_TIMEOUT.as_secs())
            } else {
                format!("request failed ({e})")
            };
            return StatusFetch::degraded(
                requested,
                format!("coord work-status: {what}{clamp_note}"),
            );
        }
    };
    let status = resp.status();
    if !status.is_success() {
        return StatusFetch::degraded(
            requested,
            format!("coord work-status: HTTP {}{clamp_note}", status.as_u16()),
        );
    }
    let body: WireResponse = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            return StatusFetch::degraded(
                requested,
                format!("coord work-status: undecodable 2xx body ({e}){clamp_note}"),
            )
        }
    };

    let (by_session_id, mut note) = map_from_body(&body);
    if !body.session_bridge_column_present {
        // The whole body was discarded — that is a DEGRADED read, not a clean
        // one that happened to find nothing.
        return StatusFetch::degraded(requested, format!("{note}{clamp_note}"));
    }
    note.push_str(&clamp_note);
    let resolved = by_session_id.len();
    if resolved == 0 && note.is_empty() {
        // coord answered, named no unknown or invalid id, and still resolved
        // no status for anything we asked about. That is a REAL observation
        // (every row's work axis is unset) — but it is byte-identical, from
        // the counts alone, to a join that silently matched nothing. Say which
        // question was asked so a reader can tell them apart.
        note = format!(
            "coord answered for all {requested} id(s) and resolved no work-axis status for any of them (every row's axis is unset, or no row matched) — nothing was discounted"
        );
    }
    debug!(
        requested,
        resolved, "restart-readiness: coord work-axis read completed"
    );
    StatusFetch {
        by_session_id,
        source: "coord",
        degraded: false,
        note: note.trim_start_matches("; ").to_string(),
        requested,
        resolved,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn body(json: serde_json::Value) -> WireResponse {
        serde_json::from_value(json).expect("decode")
    }

    #[test]
    fn maps_the_five_canonical_words_and_the_legacy_alias() {
        let (m, note) = map_from_body(&body(serde_json::json!({
            "statuses": {
                "a": {"session_status": "working"},
                "b": {"session_status": "blocked"},
                "c": {"session_status": "stalled"},
                "d": {"session_status": "waiting_human"},
                "e": {"session_status": "finished"},
                "f": {"session_status": "done"},
            },
            "unknown": [], "invalid": [], "accepted": 6, "truncated": false,
            "sessionBridgeColumnPresent": true
        })));
        assert_eq!(m.get("a"), Some(&SessionWorkStatus::Working));
        assert_eq!(m.get("b"), Some(&SessionWorkStatus::Blocked));
        assert_eq!(m.get("c"), Some(&SessionWorkStatus::Stalled));
        assert_eq!(m.get("d"), Some(&SessionWorkStatus::WaitingHuman));
        assert_eq!(m.get("e"), Some(&SessionWorkStatus::Finished));
        // coord's own parser accepts "done" as Finished; so must this mirror.
        assert_eq!(m.get("f"), Some(&SessionWorkStatus::Finished));
        assert!(note.is_empty(), "clean read carries no note: {note}");
    }

    #[test]
    fn a_null_work_axis_produces_no_entry() {
        // The row EXISTS and its axis is unset. That is UNKNOWN, and an absent
        // map entry is exactly how the caller is told to keep blocking.
        let (m, _) = map_from_body(&body(serde_json::json!({
            "statuses": {"a": {"session_status": null}},
            "unknown": [], "invalid": [], "accepted": 1, "truncated": false,
            "sessionBridgeColumnPresent": true
        })));
        assert!(m.is_empty());
    }

    #[test]
    fn unrecognised_status_is_carried_verbatim_and_is_not_finished() {
        let (m, _) = map_from_body(&body(serde_json::json!({
            "statuses": {"a": {"session_status": "vacationing"}},
            "unknown": [], "invalid": [], "accepted": 1, "truncated": false,
            "sessionBridgeColumnPresent": true
        })));
        let got = m.get("a").expect("entry");
        assert_eq!(
            got,
            &SessionWorkStatus::Unrecognised("vacationing".to_string())
        );
        assert_eq!(got.as_wire(), "vacationing");
        assert!(
            crate::session::tracking_health::blocks_restart(Some(got)),
            "a status this build does not know must BLOCK"
        );
    }

    #[test]
    fn bridge_column_absent_discards_the_whole_body() {
        let (m, note) = map_from_body(&body(serde_json::json!({
            "statuses": {"a": {"session_status": "finished"}},
            "unknown": [], "invalid": [], "accepted": 1, "truncated": false,
            "sessionBridgeColumnPresent": false
        })));
        assert!(
            m.is_empty(),
            "a body from an inexpressible join is not evidence"
        );
        assert!(note.contains("sessionBridgeColumnPresent"));
    }

    #[test]
    fn unknown_and_invalid_ids_are_noted_not_mapped() {
        let (m, note) = map_from_body(&body(serde_json::json!({
            "statuses": {},
            "unknown": ["a", "b"], "invalid": ["not-a-uuid"],
            "accepted": 2, "truncated": true,
            "sessionBridgeColumnPresent": true
        })));
        assert!(m.is_empty());
        assert!(note.contains("2 id(s) resolved to no coord session"));
        assert!(note.contains("1 id(s) were not UUIDs"));
        assert!(note.contains("truncated"));
    }

    #[tokio::test]
    async fn empty_ids_costs_no_coord_traffic() {
        let f = fetch(&[]).await;
        assert_eq!(f.source, "not_needed");
        assert!(!f.degraded);
        assert_eq!(f.requested, 0);
        assert!(f.by_session_id.is_empty());
    }

    #[test]
    fn degraded_result_maps_onto_the_response_block() {
        let f = StatusFetch::degraded(16, "coord work-status: request timed out after 2s");
        let src = SessionStatusSource::from(&f);
        assert!(src.degraded);
        assert_eq!(src.source, "unavailable");
        assert_eq!(src.requested, 16);
        assert_eq!(src.resolved, 0);
        assert_eq!(src.door, DOOR);
        assert!(src.note.contains("timed out"));
    }
}
