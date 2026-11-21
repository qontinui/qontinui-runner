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
//!
//! ## Two credentials: the device census, and one session's tenant
//!
//! The bulk reads ([`fetch`], [`resolve_coord_row`]) are a DEVICE census —
//! every terminal-hosted session on this box at once — and present the
//! device's default credential. The Stop-hook verdict's one-session read
//! ([`fetch_one_for`]) is about ONE session, whose row lives in that session's
//! tenant: it presents that tenant's credential, decided by the verdict (plan
//! `2026-11-13-runner-stop-hook-continuation-rules-and-compliance-use-the-device-default-tenant`).
//! Under the default credential, a session homed in another tenant answered
//! `unknown`, so the continuation arm never fired for it. On a multi-bound
//! device the census still reads those sessions as `unknown`; that is a
//! separate gap, recorded on the plan.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;
use tracing::debug;

use crate::mcp::policy_tenant::PolicyTarget;
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
    /// When the work axis last genuinely transitioned (coord's
    /// `state_started_at`) — for a `finished` row, the honest `finished_at`.
    /// RFC 3339, or `null` when coord holds no transition time. coord has sent
    /// the key since the route was created (qontinui-coord `0230dcabc`), so a
    /// missing key means a malformed body; it reads as `None`.
    #[serde(default)]
    since: Option<String>,
    /// WHICH `coord.sessions` row answered: the one coord's pick order names
    /// for the harness id, active first and then newest, because the key is
    /// not unique. coord has always sent it as a non-null UUID
    /// (qontinui-coord `0230dcabc`), so a missing or unparseable one means a
    /// malformed body, settled as unresolved rather than guessed.
    #[serde(default)]
    coord_session_id: Option<String>,
    /// The answering row's liveness axis (`active`, `closed`, ...). Served
    /// since the route was created; a missing key reads as `None` (UNKNOWN).
    #[serde(default)]
    state: Option<String>,
    /// When the answering row was REGISTERED (`coord.sessions.started_at`),
    /// RFC 3339. Additive (plan
    /// `2026-11-07-a-runner-finish-is-addressed-to-a-stale-coord-row-after-a-pane-respawn`,
    /// Phase 3); coord OMITS it when unread, and a coord predating the field
    /// never sends it, so absence is UNKNOWN, never a value.
    #[serde(default)]
    started_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireResponse {
    #[serde(default)]
    statuses: HashMap<String, WireRow>,
    #[serde(default)]
    unknown: Vec<String>,
    #[serde(default)]
    invalid: Vec<String>,
    /// coord cut the INPUT id list (more ids than its per-request cap). Wire
    /// name `input_truncated` since plan
    /// `2026-09-12-fleet-principal-session-routes-disagree-about-being-capped`
    /// Phase 3 (qontinui-coord#2553). The transitional `truncated` alias is
    /// gone now that the rename is deployed — the plan's third step. It says
    /// nothing about a capped RESULT page — that is what `truncated` meant on
    /// the sibling routes, and why coord renamed it.
    #[serde(default)]
    input_truncated: bool,
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
    /// `claude_session_id` → unix millis of coord's `since` (its
    /// `state_started_at`: when the row entered its CURRENT work-axis status),
    /// for EVERY row that resolved a status into [`Self::by_session_id`] AND
    /// carried a parseable `since`. Absent ⇒ the time is UNKNOWN (an older
    /// coord that serves no `since`, a null one, or an unparseable one) —
    /// never "now", never "the epoch". `/restart-readiness` passes it through
    /// per process as `sessionStatusSince`. Never consulted by the restart
    /// verdict.
    pub since_by_session_id: HashMap<String, i64>,
    /// The `finished` subset of [`Self::since_by_session_id`]: unix millis at
    /// which coord's row became `finished` (the legacy `done` included).
    /// Absent ⇒ the time is unknown, and wind-down falls back to not bounding
    /// the idle window by it. Never consulted by the restart verdict.
    ///
    /// For a finish the runner delivered, this is the runner's DECLARED finish
    /// instant on the runner's clock (its `finished` PATCH carries the mark's
    /// `finished_at` as `last_progress_at`), not coord's receipt time — so the
    /// wind-down's floor can sit earlier than it did before that change, by up
    /// to the outbox lag.
    pub finished_at_by_session_id: HashMap<String, i64>,
    /// `claude_session_id` → the row coord served for it, RAW: every row in
    /// `statuses`, including one whose `session_status` is `null` (which the
    /// maps above leave out as UNKNOWN). The finish heal needs that null —
    /// it is exactly what a lost finish leaves (plan
    /// `2026-11-05-finish-synced-is-stamped-on-a-2xx-that-did-not-move-session-status`,
    /// Phase 5). Empty on a degraded read.
    pub rows_by_session_id: HashMap<String, ObservedRow>,
    /// Some ids were NOT asked about: coord cut the input list
    /// (`input_truncated`), or the runner clamped it at [`MAX_IDS`]. An id
    /// absent from the maps above is then UNKNOWN, not "no row".
    pub input_truncated: bool,
    /// `"coord"` when the door answered, `"unavailable"` when it did not,
    /// `"not_needed"` when there was nothing to ask about.
    pub source: &'static str,
    pub degraded: bool,
    /// Names what was actually seen. Empty on a clean read.
    pub note: String,
    pub requested: usize,
    pub resolved: usize,
}

/// One work-status row as coord served it — the status kept raw so a `null`
/// (an unset axis) is told apart from an absent row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedRow {
    /// `None` = coord's row exists and its work axis is unset.
    pub session_status: Option<String>,
    /// coord's `since`, verbatim (RFC 3339), when it served one.
    pub since: Option<String>,
    /// [`Self::since`] as unix millis; `None` when absent or unparseable.
    pub since_ms: Option<i64>,
    /// WHICH `coord.sessions` row answered; `None` on a coord predating it.
    pub coord_session_id: Option<String>,
    /// The answering row's liveness axis (`active`, `closed`, ...); `None`
    /// when coord served none (UNKNOWN).
    pub state: Option<String>,
    /// When the answering row was registered (coord's `started_at`), verbatim
    /// (RFC 3339); `None` when coord served none (UNKNOWN).
    pub started_at: Option<String>,
    /// [`Self::started_at`] as unix millis; `None` when absent or unparseable.
    pub started_at_ms: Option<i64>,
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
            since_by_session_id: HashMap::new(),
            finished_at_by_session_id: HashMap::new(),
            rows_by_session_id: HashMap::new(),
            input_truncated: false,
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
    if body.input_truncated {
        notes.push(
            "coord truncated the INPUT id list (over its per-request id cap), so ids past \
             the cap were not asked about"
                .to_string(),
        );
    }
    (out, notes.join("; "))
}

/// `claude_session_id` → unix millis of coord's `since` for every row that
/// RESOLVED a status — the same rows [`map_from_body`] maps (a non-blank
/// `session_status`, whatever its word) — and carries a parseable RFC 3339
/// `since`. Pure. A body with `sessionBridgeColumnPresent: false` yields
/// nothing, exactly like [`map_from_body`]; a row whose status is unset yields
/// no time either, because a time for an UNKNOWN status is not an observation
/// about anything.
fn since_from_body(body: &WireResponse) -> HashMap<String, i64> {
    if !body.session_bridge_column_present {
        return HashMap::new();
    }
    body.statuses
        .iter()
        .filter(|(_, row)| {
            row.session_status
                .as_deref()
                .is_some_and(|raw| !raw.trim().is_empty())
        })
        .filter_map(|(id, row)| {
            let since = row.since.as_deref()?;
            chrono::DateTime::parse_from_rfc3339(since)
                .ok()
                .map(|dt| (id.clone(), dt.timestamp_millis()))
        })
        .collect()
}

/// The `finished` subset of [`since_from_body`]: every row that reads
/// `finished` (including the legacy `done`) and carries a parseable `since`.
/// Pure.
fn finished_at_from_body(body: &WireResponse) -> HashMap<String, i64> {
    let mut since = since_from_body(body);
    since.retain(|id, _| {
        body.statuses
            .get(id)
            .and_then(|row| row.session_status.as_deref())
            .map(SessionWorkStatus::parse)
            .is_some_and(|s| s == SessionWorkStatus::Finished)
    });
    since
}

/// `claude_session_id` → [`ObservedRow`] for EVERY row coord served, null
/// status included. Pure. A body with `sessionBridgeColumnPresent: false`
/// yields nothing.
fn rows_from_body(body: &WireResponse) -> HashMap<String, ObservedRow> {
    if !body.session_bridge_column_present {
        return HashMap::new();
    }
    body.statuses
        .iter()
        .map(|(id, row)| {
            let since = row
                .since
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let since_ms = since
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.timestamp_millis());
            let started_at = row
                .started_at
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let started_at_ms = started_at
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.timestamp_millis());
            (
                id.clone(),
                ObservedRow {
                    session_status: row
                        .session_status
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                    since,
                    since_ms,
                    coord_session_id: row
                        .coord_session_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                    state: row
                        .state
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                    started_at,
                    started_at_ms,
                },
            )
        })
        .collect()
}

/// A clean [`StatusFetch`] built from a raw coord body — the test seam for
/// consumers that need a real fetch result without the network.
#[cfg(test)]
pub(crate) fn fetch_from_body_for_test(json: serde_json::Value, requested: usize) -> StatusFetch {
    let body: WireResponse = serde_json::from_value(json).expect("decode");
    let (by_session_id, note) = map_from_body(&body);
    StatusFetch {
        since_by_session_id: since_from_body(&body),
        finished_at_by_session_id: finished_at_from_body(&body),
        rows_by_session_id: rows_from_body(&body),
        input_truncated: body.input_truncated || requested > MAX_IDS,
        resolved: by_session_id.len(),
        by_session_id,
        source: "coord",
        degraded: false,
        note,
        requested,
    }
}

/// A degraded [`StatusFetch`] (coord unreadable), for consumers' tests.
#[cfg(test)]
pub(crate) fn degraded_fetch_for_test(note: &str) -> StatusFetch {
    StatusFetch::degraded(1, note.to_string())
}

// ---------------------------------------------------------------------------
// The read
// ---------------------------------------------------------------------------

/// How long a resolved `(coord base, device JWT)` pair is reused. A boot with
/// dozens of unbound transcripts would otherwise run the blocking credential
/// resolution (secure storage, then an OS keychain call) once per transcript.
/// Short, so a rotated or expired token is picked up within the minute; a
/// failure is never cached.
const CREDENTIAL_REUSE: Duration = Duration::from_secs(30);

static CREDENTIAL: std::sync::Mutex<Option<(std::time::Instant, String, String)>> =
    std::sync::Mutex::new(None);

/// Serialises credential RESOLUTION so a burst of concurrent callers does the
/// blocking work once, then reads the cache.
static CREDENTIAL_RESOLVE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn cached_credential_now() -> Option<(String, String)> {
    let g = CREDENTIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    g.as_ref()
        .filter(|(at, _, _)| at.elapsed() < CREDENTIAL_REUSE)
        .map(|(_, b, j)| (b.clone(), j.clone()))
}

/// The coord base + device JWT, reused for [`CREDENTIAL_REUSE`].
async fn cached_credential() -> Result<(String, String), String> {
    if let Some(p) = cached_credential_now() {
        return Ok(p);
    }
    let _one_at_a_time = CREDENTIAL_RESOLVE.lock().await;
    if let Some(p) = cached_credential_now() {
        return Ok(p);
    }
    let parts = tokio::time::timeout(
        CREDENTIAL_TIMEOUT,
        tokio::task::spawn_blocking(crate::mcp::continuation_verdict::coord_client_parts),
    )
    .await;
    let (base, jwt) = match parts {
        Ok(Ok(Ok(p))) => p,
        Ok(Ok(Err(e))) => return Err(format!("coord work-status: no credential ({e})")),
        Ok(Err(e)) => {
            return Err(format!(
                "coord work-status: credential resolution panicked ({e})"
            ))
        }
        Err(_) => {
            return Err(format!(
                "coord work-status: credential resolution timed out after {}s",
                CREDENTIAL_TIMEOUT.as_secs()
            ))
        }
    };
    *CREDENTIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some((std::time::Instant::now(), base.clone(), jwt.clone()));
    Ok((base, jwt))
}

/// One pooled client for every work-status read.
fn shared_client() -> Result<reqwest::Client, reqwest::Error> {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c.clone());
    }
    let c = reqwest::Client::builder().timeout(FETCH_TIMEOUT).build()?;
    Ok(CLIENT.get_or_init(|| c).clone())
}

/// Upper bound on concurrent single-id resolutions ([`resolve_coord_row`]).
/// A boot storm (every unbound transcript at once) otherwise opens one
/// connection per transcript against coord.
pub const MAX_CONCURRENT_RESOLUTIONS: usize = 4;

static RESOLUTION_SLOTS: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(MAX_CONCURRENT_RESOLUTIONS);

/// The work-status door's URL for an already-validated id CSV.
fn work_status_url(base: &str, csv: &str) -> String {
    format!("{base}/coord/sessions/work-status?claude_code_session_ids={csv}")
}

/// Send one work-status request and decode its body. The ONE transport /
/// status / decode path every read here shares, so the census and the
/// one-session read cannot disagree about the door. `note_suffix` is appended
/// to every degraded note (the census's clamp note).
async fn execute_wire(
    client: &reqwest::Client,
    request: reqwest::Request,
    note_suffix: &str,
) -> Result<WireResponse, String> {
    let resp = match client.execute(request).await {
        Ok(r) => r,
        Err(e) => {
            let what = if e.is_timeout() {
                format!("request timed out after {}s", FETCH_TIMEOUT.as_secs())
            } else {
                format!("request failed ({e})")
            };
            return Err(format!("coord work-status: {what}{note_suffix}"));
        }
    };
    let status = resp.status();
    if !status.is_success() {
        return Err(format!(
            "coord work-status: HTTP {}{note_suffix}",
            status.as_u16()
        ));
    }
    resp.json()
        .await
        .map_err(|e| format!("coord work-status: undecodable 2xx body ({e}){note_suffix}"))
}

/// One raw coord read: the decoded body plus the clamp note, or the note of
/// whatever degraded it. Shared by [`fetch`] (the work axis) and
/// [`resolve_coord_rows`] (which row answered) so there is ONE credential /
/// timeout / decode path for the device census.
async fn fetch_wire(ids: &[String]) -> Result<(WireResponse, String), String> {
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
    let (base, jwt) = match cached_credential().await {
        Ok(p) => p,
        Err(e) => return Err(format!("{e}{clamp_note}")),
    };
    let client = match shared_client() {
        Ok(c) => c,
        Err(e) => {
            return Err(format!(
                "coord work-status: client build failed ({e}){clamp_note}"
            ))
        }
    };

    let csv = clamped
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let request = match client
        .get(work_status_url(&base, &csv))
        .bearer_auth(&jwt)
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            return Err(format!(
                "coord work-status: request build failed ({e}){clamp_note}"
            ))
        }
    };
    let body = execute_wire(&client, request, &clamp_note).await?;
    Ok((body, clamp_note))
}

/// Bulk-read the coord work axis for `ids`. **Never fails** — see module docs.
pub async fn fetch(ids: &[String]) -> StatusFetch {
    if ids.is_empty() {
        return StatusFetch {
            by_session_id: HashMap::new(),
            since_by_session_id: HashMap::new(),
            finished_at_by_session_id: HashMap::new(),
            rows_by_session_id: HashMap::new(),
            input_truncated: false,
            source: "not_needed",
            degraded: false,
            note: String::new(),
            requested: 0,
            resolved: 0,
        };
    }

    let requested = ids.len();
    let (body, clamp_note) = match fetch_wire(ids).await {
        Ok(x) => x,
        Err(note) => return StatusFetch::degraded(requested, note),
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
        since_by_session_id: since_from_body(&body),
        finished_at_by_session_id: finished_at_from_body(&body),
        rows_by_session_id: rows_from_body(&body),
        input_truncated: body.input_truncated || requested > MAX_IDS,
        by_session_id,
        source: "coord",
        degraded: false,
        note: note.trim_start_matches("; ").to_string(),
        requested,
        resolved,
    }
}

// ---------------------------------------------------------------------------
// Which coord row answers for a harness session id
// ---------------------------------------------------------------------------

/// What coord said about ONE harness session id — the three negative answers
/// are kept apart because only one of them licenses minting a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowResolution {
    /// coord has a row for this id, the one its pick order names (active
    /// first, then newest); adopt it.
    Existing(uuid::Uuid),
    /// coord resolved NO session for this id in this tenant — the one answer
    /// that licenses minting a fresh row.
    Unknown,
    /// coord could not be asked, or its answer does not settle the question
    /// (degraded read, bridge column absent, id absent from every bucket, an
    /// unparseable row id). Never mint on this: a second row beside an
    /// existing one is the duplicate-row defect.
    Unresolved(String),
}

/// Pure: settle `csid` against a decoded body.
fn resolution_from_body(body: &WireResponse, csid: &str) -> RowResolution {
    if !body.session_bridge_column_present {
        return RowResolution::Unresolved(
            "coord reported sessionBridgeColumnPresent: false".to_string(),
        );
    }
    if let Some(row) = body.statuses.get(csid) {
        return match row
            .coord_session_id
            .as_deref()
            .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok())
        {
            Some(id) => RowResolution::Existing(id),
            None => RowResolution::Unresolved(
                "coord answered a row without a parseable coord_session_id \
                 (coord always sends one, so the body is malformed)"
                    .to_string(),
            ),
        };
    }
    if body.unknown.iter().any(|u| u == csid) {
        return RowResolution::Unknown;
    }
    RowResolution::Unresolved("coord named the id in neither `statuses` nor `unknown`".to_string())
}

/// Ask coord which `coord.sessions` row answers for the harness session id
/// `csid`. Never errors — see [`RowResolution`].
pub async fn resolve_coord_row(csid: &str) -> RowResolution {
    // Bounded: at most MAX_CONCURRENT_RESOLUTIONS reads in flight. The
    // semaphore is never closed, so an acquire error is unreachable; treat it
    // as unresolved rather than panic.
    let Ok(_slot) = RESOLUTION_SLOTS.acquire().await else {
        return RowResolution::Unresolved("resolution semaphore closed".to_string());
    };
    match fetch_wire(&[csid.to_string()]).await {
        Ok((body, _)) => resolution_from_body(&body, csid),
        Err(note) => RowResolution::Unresolved(note),
    }
}

// ---------------------------------------------------------------------------
// One session's raw work axis (the Stop-hook verdict's read)
// ---------------------------------------------------------------------------

/// What coord said about ONE harness session's work axis, with the raw status
/// word kept (the verdict decides on the word, not on [`SessionWorkStatus`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OneStatus {
    /// coord has a row; `None` = its work axis is unset.
    Row(Option<String>),
    /// coord named the id in `unknown` — reachable, and no row.
    NoRow,
    /// coord could not be asked, or did not settle the id (degraded read,
    /// bridge column absent, an `invalid` id, named in no bucket).
    Unresolved(String),
}

/// Pure: settle `csid`'s raw work axis against a decoded body.
fn one_status_from_body(body: &WireResponse, csid: &str) -> OneStatus {
    if !body.session_bridge_column_present {
        return OneStatus::Unresolved(
            "coord reported sessionBridgeColumnPresent: false".to_string(),
        );
    }
    if let Some(row) = body.statuses.get(csid) {
        return OneStatus::Row(
            row.session_status
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        );
    }
    if body.unknown.iter().any(|u| u == csid) {
        return OneStatus::NoRow;
    }
    OneStatus::Unresolved("coord named the id in neither `statuses` nor `unknown`".to_string())
}

// ---------------------------------------------------------------------------
// One session's work axis under ITS tenant's credential
// ---------------------------------------------------------------------------

/// One tenant's resolved credential for the one-session read: the coord base
/// and the `Authorization` value `coord_get_for` attached for that tenant's
/// scope, as of when it was resolved.
#[derive(Clone)]
struct TenantCredential {
    at: std::time::Instant,
    base: String,
    authorization: reqwest::header::HeaderValue,
}

/// Keyed per tenant ([`PolicyTarget::tenant`]). The device census's
/// [`CREDENTIAL`] is ONE slot because it is one credential; the Stop-hook read
/// presents a different tenant's per session, and one slot would hand the last
/// resolved tenant's bearer to the next session's read — which `build_checked`
/// would then refuse, so every alternation would cost a re-resolve. Reused for
/// [`CREDENTIAL_REUSE`]; a failure is never cached.
static TENANT_CREDENTIALS: std::sync::OnceLock<
    std::sync::Mutex<HashMap<Option<uuid::Uuid>, TenantCredential>>,
> = std::sync::OnceLock::new();

fn tenant_credentials() -> &'static std::sync::Mutex<HashMap<Option<uuid::Uuid>, TenantCredential>>
{
    TENANT_CREDENTIALS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// `tenant`'s cached `(base, Authorization)`, while within [`CREDENTIAL_REUSE`]
/// of `now`.
fn tenant_credential_lookup(
    tenant: Option<uuid::Uuid>,
    now: std::time::Instant,
) -> Option<(String, reqwest::header::HeaderValue)> {
    let g = tenant_credentials()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    g.get(&tenant)
        .filter(|c| now.duration_since(c.at) < CREDENTIAL_REUSE)
        .map(|c| (c.base.clone(), c.authorization.clone()))
}

/// Cache `authorization` (resolved at `at` against `base`) as `tenant`'s.
fn tenant_credential_store(
    tenant: Option<uuid::Uuid>,
    at: std::time::Instant,
    base: String,
    mut authorization: reqwest::header::HeaderValue,
) {
    authorization.set_sensitive(true);
    tenant_credentials()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            tenant,
            TenantCredential {
                at,
                base,
                authorization,
            },
        );
}

/// The one-id work-status GET under `target`'s credential — `cached` when this
/// tenant's was resolved within [`CREDENTIAL_REUSE`], else freshly through
/// `coord_http::coord_get_for` — refused before it can be sent unless its
/// bearer claims `target.tenant` ([`policy_tenant::build_checked`]).
///
/// [`policy_tenant::build_checked`]: crate::mcp::policy_tenant::build_checked
fn build_work_status_get(
    client: &reqwest::Client,
    url: String,
    target: PolicyTarget,
    cached: Option<reqwest::header::HeaderValue>,
) -> Result<reqwest::Request, String> {
    let rb = match cached {
        Some(authorization) => client
            .get(url)
            .header(reqwest::header::AUTHORIZATION, authorization),
        None => crate::coord_http::coord_get_for(client, url, target.scope),
    };
    crate::mcp::policy_tenant::build_checked(rb, target).map(|(request, _presented)| request)
}

/// The request for `id` under `target`'s credential, resolving (and caching)
/// that credential when none is reusable. Resolution reads secure storage and
/// may reach the OS keychain, so it runs on a blocking thread under
/// [`CREDENTIAL_TIMEOUT`], serialised with the census's resolution.
async fn tenant_request(
    client: &reqwest::Client,
    id: &str,
    target: PolicyTarget,
) -> Result<reqwest::Request, String> {
    let reuse = |client: &reqwest::Client| {
        tenant_credential_lookup(target.tenant, std::time::Instant::now()).map(
            |(base, authorization)| {
                build_work_status_get(
                    client,
                    work_status_url(&base, id),
                    target,
                    Some(authorization),
                )
            },
        )
    };
    if let Some(built) = reuse(client) {
        return built.map_err(|e| format!("coord work-status: {e}"));
    }
    let _one_at_a_time = CREDENTIAL_RESOLVE.lock().await;
    if let Some(built) = reuse(client) {
        return built.map_err(|e| format!("coord work-status: {e}"));
    }
    let (blocking_client, blocking_id) = (client.clone(), id.to_string());
    let resolved = tokio::time::timeout(
        CREDENTIAL_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            let base = crate::mcp::continuation_verdict::coord_base();
            let request = build_work_status_get(
                &blocking_client,
                work_status_url(&base, &blocking_id),
                target,
                None,
            )?;
            Ok::<_, String>((base, request))
        }),
    )
    .await;
    let (base, request) = match resolved {
        Ok(Ok(Ok(built))) => built,
        Ok(Ok(Err(e))) => return Err(format!("coord work-status: {e}")),
        Ok(Err(e)) => {
            return Err(format!(
                "coord work-status: credential resolution panicked ({e})"
            ))
        }
        Err(_) => {
            return Err(format!(
                "coord work-status: credential resolution timed out after {}s",
                CREDENTIAL_TIMEOUT.as_secs()
            ))
        }
    };
    if let Some(authorization) = request.headers().get(reqwest::header::AUTHORIZATION) {
        tenant_credential_store(
            target.tenant,
            std::time::Instant::now(),
            base,
            authorization.clone(),
        );
    }
    Ok(request)
}

/// Read ONE harness session's work axis over the same door, decode path and
/// timeouts as [`fetch`], under the credential of `target` — the session's own
/// tenant, decided by the Stop-hook verdict
/// ([`crate::mcp::continuation_verdict`]). Never errors — see [`OneStatus`].
pub async fn fetch_one_for(csid: &str, target: PolicyTarget) -> OneStatus {
    // The id is interpolated into a CSV query parameter: only a UUID goes on
    // the wire, so a `,` or `&` in a payload can never reshape the request.
    // Normalised once (hyphenated lowercase), and the SAME string is both sent
    // and looked up, so a braced or uppercase id cannot miss its own row.
    let Ok(id) = uuid::Uuid::parse_str(csid.trim()) else {
        return OneStatus::Unresolved(format!("`{csid}` is not a harness session UUID"));
    };
    let id = id.to_string();
    let client = match shared_client() {
        Ok(c) => c,
        Err(e) => {
            return OneStatus::Unresolved(format!("coord work-status: client build failed ({e})"))
        }
    };
    let request = match tenant_request(&client, &id, target).await {
        Ok(request) => request,
        Err(note) => return OneStatus::Unresolved(note),
    };
    match execute_wire(&client, request, "").await {
        Ok(body) => one_status_from_body(&body, &id),
        Err(note) => OneStatus::Unresolved(note),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// The keys coord's `WorkStatusResponse` serializes, every one on every
    /// response (qontinui-coord `crates/coord/src/session_work_status.rs`,
    /// read at `c84e73784`).
    ///
    /// These three lists are a SNAPSHOT. Nothing checks them against coord, so
    /// re-read `WorkStatusResponse` and `WorkStatusRow` whenever coord changes
    /// either. They live here rather than as `deny_unknown_fields` on the
    /// production structs, because production deliberately ignores keys it
    /// does not read (`state`, `started_at`).
    const RESPONSE_KEYS: &[&str] = &[
        "statuses",
        "unknown",
        "invalid",
        "accepted",
        "input_truncated",
        "sessionBridgeColumnPresent",
    ];
    /// The `WorkStatusRow` keys coord sends on every row.
    const ROW_KEYS_ALWAYS: &[&str] = &["session_status", "since", "coord_session_id", "state"];
    /// The `WorkStatusRow` key coord omits when unset.
    const ROW_KEYS_OPTIONAL: &[&str] = &["started_at"];

    /// Decode a fixture after checking its KEYS against the snapshot above.
    /// Every response key and every always-sent row key must be present, and
    /// no key outside the snapshot may be (`started_at` is the one optional
    /// row key). Values are not checked: the harness ids here are short labels
    /// (`sid`, `a`) where coord sends UUID strings, which this module treats
    /// as opaque keys either way.
    ///
    /// [`WireResponse`] ignores unknown keys and defaults missing ones, so
    /// without this a fixture can model a body coord cannot send and still
    /// pass. Six fixtures did exactly that with `truncated` after
    /// qontinui-runner#2150 dropped its alias (coord pins the key's absence in
    /// `session_fleet.rs`, `the_three_session_routes_share_one_at_cap_vocabulary`).
    /// A fixture that breaks the contract on purpose says so and goes through
    /// [`decode_off_contract`].
    fn body(json: Value) -> WireResponse {
        let top = json.as_object().expect("a fixture is a JSON object");
        for key in top.keys() {
            assert!(
                RESPONSE_KEYS.contains(&key.as_str()),
                "fixture key `{key}` is not a field coord's WorkStatusResponse serves: {json}"
            );
        }
        for key in RESPONSE_KEYS {
            assert!(
                top.contains_key(*key),
                "fixture lacks `{key}`, which coord's WorkStatusResponse always sends: {json}"
            );
        }
        let statuses = top["statuses"]
            .as_object()
            .expect("`statuses` is a JSON object");
        for (id, row) in statuses {
            let row = row.as_object().expect("a statuses row is a JSON object");
            for key in row.keys() {
                assert!(
                    ROW_KEYS_ALWAYS.contains(&key.as_str())
                        || ROW_KEYS_OPTIONAL.contains(&key.as_str()),
                    "fixture row `{id}` key `{key}` is not a field coord's WorkStatusRow serves: {json}"
                );
            }
            for key in ROW_KEYS_ALWAYS {
                assert!(
                    row.contains_key(*key),
                    "fixture row `{id}` lacks `{key}`, which coord's WorkStatusRow always sends: {json}"
                );
            }
        }
        decode_off_contract(json)
    }

    #[test]
    fn one_status_keeps_the_raw_word_and_separates_no_row_from_unresolved() {
        // Off contract on purpose: the rows omit `since`/`state` and the body
        // omits `input_truncated`, to exercise the one-status reader alone.
        let b = decode_off_contract(serde_json::json!({
            "statuses": {
                "a": {"session_status": "Working "},
                "b": {"session_status": null},
                "c": {"session_status": "  "}
            },
            "unknown": ["u"], "invalid": ["bad"], "accepted": 3,
            "sessionBridgeColumnPresent": true
        }));
        assert_eq!(
            one_status_from_body(&b, "a"),
            OneStatus::Row(Some("Working".into()))
        );
        assert_eq!(one_status_from_body(&b, "b"), OneStatus::Row(None));
        assert_eq!(one_status_from_body(&b, "c"), OneStatus::Row(None));
        assert_eq!(one_status_from_body(&b, "u"), OneStatus::NoRow);
        assert!(matches!(
            one_status_from_body(&b, "bad"),
            OneStatus::Unresolved(_)
        ));
        assert!(matches!(
            one_status_from_body(&b, "zzz"),
            OneStatus::Unresolved(_)
        ));

        let no_bridge = decode_off_contract(serde_json::json!({
            "statuses": {"a": {"session_status": "working"}}, "unknown": [],
            "sessionBridgeColumnPresent": false
        }));
        assert!(matches!(
            one_status_from_body(&no_bridge, "a"),
            OneStatus::Unresolved(_)
        ));
    }

    /// Decode with NO contract check. Only for a fixture that sends, on
    /// purpose, a body coord cannot (a missing, extra or retired key) to
    /// exercise a defensive arm. Every caller says why.
    fn decode_off_contract(json: Value) -> WireResponse {
        serde_json::from_value(json).expect("decode")
    }

    /// A `statuses` row carrying every key coord always sends. `n` keeps the
    /// `coord_session_id` distinct, as it is for distinct harness ids.
    fn row(n: u32, session_status: Value, since: Value) -> Value {
        json!({
            "session_status": session_status,
            "since": since,
            "coord_session_id": format!("00000000-0000-7000-9000-{n:012}"),
            "state": "active"
        })
    }

    const ROW: &str = "01a11324-7598-7e22-82e3-7ed378f59c62";

    #[test]
    #[should_panic(expected = "fixture key `truncated` is not a field")]
    fn the_fixture_guard_refuses_a_response_key_coord_does_not_serve() {
        body(json!({
            "statuses": {}, "unknown": [], "invalid": [], "accepted": 0,
            "input_truncated": false, "truncated": false,
            "sessionBridgeColumnPresent": true
        }));
    }

    #[test]
    #[should_panic(expected = "fixture lacks `input_truncated`")]
    fn the_fixture_guard_refuses_a_body_missing_a_key_coord_always_sends() {
        body(json!({
            "statuses": {}, "unknown": [], "invalid": [], "accepted": 0,
            "sessionBridgeColumnPresent": true
        }));
    }

    #[test]
    #[should_panic(expected = "fixture row `sid` key `status` is not a field")]
    fn the_fixture_guard_refuses_a_row_key_coord_does_not_serve() {
        body(json!({
            "statuses": {"sid": {
                "session_status": "working", "since": null,
                "coord_session_id": ROW, "state": "active", "status": "working"
            }},
            "unknown": [], "invalid": [], "accepted": 1, "input_truncated": false,
            "sessionBridgeColumnPresent": true
        }));
    }

    #[test]
    #[should_panic(expected = "fixture row `sid` lacks `state`")]
    fn the_fixture_guard_refuses_a_row_missing_a_key_coord_always_sends() {
        body(json!({
            "statuses": {"sid": {
                "session_status": "working", "since": null, "coord_session_id": ROW
            }},
            "unknown": [], "invalid": [], "accepted": 1, "input_truncated": false,
            "sessionBridgeColumnPresent": true
        }));
    }

    /// The observed row carries coord's `state` and `started_at` (the finish
    /// heal's re-address needs both); an omitted `started_at` reads `None`,
    /// never a value.
    #[test]
    fn an_observed_row_carries_state_and_started_at() {
        let b = body(json!({
            "statuses": {
                "a": {"session_status": null, "since": null, "coord_session_id": ROW,
                      "state": "active", "started_at": "2026-11-05T11:00:00Z"},
                "b": {"session_status": null, "since": null, "coord_session_id": ROW,
                      "state": "closed"}
            },
            "unknown": [], "invalid": [], "accepted": 2, "input_truncated": false,
            "sessionBridgeColumnPresent": true
        }));
        let rows = rows_from_body(&b);
        let a = &rows["a"];
        assert_eq!(a.state.as_deref(), Some("active"));
        assert_eq!(a.started_at.as_deref(), Some("2026-11-05T11:00:00Z"));
        assert_eq!(a.started_at_ms, Some(1_793_876_400_000));
        let b = &rows["b"];
        assert_eq!(b.state.as_deref(), Some("closed"));
        assert_eq!(b.started_at, None);
        assert_eq!(b.started_at_ms, None);
    }

    #[test]
    fn resolution_adopts_the_row_coord_names() {
        let b = body(json!({
            "statuses": {"sid": {
                "session_status": null, "since": null,
                "coord_session_id": ROW, "state": "active"
            }},
            "unknown": [], "invalid": [], "accepted": 1, "input_truncated": false,
            "sessionBridgeColumnPresent": true
        }));
        assert_eq!(
            resolution_from_body(&b, "sid"),
            RowResolution::Existing(uuid::Uuid::parse_str(ROW).unwrap())
        );
    }

    #[test]
    fn resolution_mints_only_on_an_explicit_unknown() {
        let unknown = body(json!({
            "statuses": {}, "unknown": ["sid"], "invalid": [], "accepted": 1,
            "input_truncated": false, "sessionBridgeColumnPresent": true
        }));
        assert_eq!(
            resolution_from_body(&unknown, "sid"),
            RowResolution::Unknown
        );

        // In neither bucket: coord did not settle it — must NOT read as mintable.
        // Off-contract on purpose: coord puts every accepted id in `statuses`
        // or `unknown`, so this exercises only the defensive arm.
        let silent = body(json!({
            "statuses": {}, "unknown": [], "invalid": [], "accepted": 1,
            "input_truncated": false, "sessionBridgeColumnPresent": true
        }));
        assert!(matches!(
            resolution_from_body(&silent, "sid"),
            RowResolution::Unresolved(_)
        ));
        // Bridge column absent: even an `unknown` entry is meaningless.
        let no_bridge = body(json!({
            "statuses": {}, "unknown": ["sid"], "invalid": [], "accepted": 1,
            "input_truncated": false, "sessionBridgeColumnPresent": false
        }));
        assert!(matches!(
            resolution_from_body(&no_bridge, "sid"),
            RowResolution::Unresolved(_)
        ));
    }

    /// Off-contract on purpose: coord has always sent `coord_session_id` as a
    /// non-null UUID, so this row lacks a key and exercises only the defensive
    /// arm.
    #[test]
    fn resolution_refuses_a_row_without_a_parseable_id() {
        let b = decode_off_contract(json!({
            "statuses": {"sid": {"session_status": "working", "since": null, "state": "active"}},
            "unknown": [], "invalid": [], "accepted": 1, "input_truncated": false,
            "sessionBridgeColumnPresent": true
        }));
        assert!(matches!(
            resolution_from_body(&b, "sid"),
            RowResolution::Unresolved(_)
        ));
    }

    #[test]
    fn maps_the_five_canonical_words_and_the_legacy_alias() {
        let (m, note) = map_from_body(&body(json!({
            "statuses": {
                "a": row(1, json!("working"), Value::Null),
                "b": row(2, json!("blocked"), Value::Null),
                "c": row(3, json!("stalled"), Value::Null),
                "d": row(4, json!("waiting_human"), Value::Null),
                "e": row(5, json!("finished"), Value::Null),
                "f": row(6, json!("done"), Value::Null),
            },
            "unknown": [], "invalid": [], "accepted": 6, "input_truncated": false,
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

    /// Two `since` values are off-contract on purpose. coord serializes UTC
    /// RFC 3339 ending in `Z`, so `+00:00` tests leniency and `yesterday` tests
    /// the parse-failure arm. A row with no transition time carries
    /// `"since": null`, which is how coord says it.
    #[test]
    fn finished_at_is_read_only_from_finished_rows_with_a_parseable_since() {
        let b = body(json!({
            "statuses": {
                "done-new": row(1, json!("finished"), json!("2026-09-13T10:00:00Z")),
                "done-alias": row(2, json!("done"), json!("2026-09-13T10:00:01+00:00")),
                "working": row(3, json!("working"), json!("2026-09-13T10:00:00Z")),
                "null-since": row(4, json!("finished"), Value::Null),
                "bad-since": row(5, json!("finished"), json!("yesterday")),
            },
            "unknown": [], "invalid": [], "accepted": 5, "input_truncated": false,
            "sessionBridgeColumnPresent": true
        }));
        let m = finished_at_from_body(&b);
        assert_eq!(m.len(), 2, "{m:?}");
        assert_eq!(m.get("done-new"), Some(&1_789_293_600_000));
        assert_eq!(m.get("done-alias"), Some(&1_789_293_601_000));

        // Off-contract on purpose: with the bridge column absent coord sends
        // `statuses: {}`. A row is sent here so the assertion cannot pass
        // vacuously on an empty map.
        let absent = body(json!({
            "statuses": {"x": row(1, json!("finished"), json!("2026-09-13T10:00:00Z"))},
            "unknown": [], "invalid": [], "accepted": 1, "input_truncated": false,
            "sessionBridgeColumnPresent": false
        }));
        assert!(finished_at_from_body(&absent).is_empty());
    }

    /// `since` is decoded for EVERY resolved status, not only `finished` —
    /// `/restart-readiness` serves it per process as `sessionStatusSince`, so a
    /// revival (`working` again) carries its own transition time. A row with no
    /// resolved status, a null `since` or an unparseable one maps to nothing
    /// (UNKNOWN), and an inexpressible join yields nothing at all. The
    /// `+00:00` and `yesterday` values are off-contract on purpose, as in the
    /// test above.
    #[test]
    fn since_is_read_for_every_resolved_status_with_a_parseable_since() {
        let b = body(json!({
            "statuses": {
                "working": row(1, json!("working"), json!("2026-09-13T10:00:00Z")),
                "waiting": row(2, json!("waiting_human"), json!("2026-09-13T10:00:02+00:00")),
                "finished": row(3, json!("finished"), json!("2026-09-13T10:00:01Z")),
                "unrecognised": row(4, json!("vacationing"), json!("2026-09-13T10:00:03Z")),
                "unset": row(5, Value::Null, json!("2026-09-13T10:00:00Z")),
                "blank": row(6, json!("  "), json!("2026-09-13T10:00:00Z")),
                "null-since": row(7, json!("working"), Value::Null),
                "bad-since": row(8, json!("working"), json!("yesterday")),
            },
            "unknown": [], "invalid": [], "accepted": 8, "input_truncated": false,
            "sessionBridgeColumnPresent": true
        }));
        let m = since_from_body(&b);
        assert_eq!(m.len(), 4, "{m:?}");
        assert_eq!(m.get("working"), Some(&1_789_293_600_000));
        assert_eq!(m.get("waiting"), Some(&1_789_293_602_000));
        assert_eq!(m.get("finished"), Some(&1_789_293_601_000));
        assert_eq!(m.get("unrecognised"), Some(&1_789_293_603_000));
        // The finished subset is still exactly the finished rows.
        let f = finished_at_from_body(&b);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f.get("finished"), Some(&1_789_293_601_000));

        // Off-contract on purpose (coord sends `statuses: {}` here), so the
        // emptiness below is the decoder's doing, not the fixture's.
        let no_bridge = body(json!({
            "statuses": {"x": row(1, json!("working"), json!("2026-09-13T10:00:00Z"))},
            "unknown": [], "invalid": [], "accepted": 1, "input_truncated": false,
            "sessionBridgeColumnPresent": false
        }));
        assert!(since_from_body(&no_bridge).is_empty());
    }

    #[test]
    fn a_null_work_axis_produces_no_entry() {
        // The row EXISTS and its axis is unset. That is UNKNOWN, and an absent
        // map entry is exactly how the caller is told to keep blocking.
        let (m, _) = map_from_body(&body(json!({
            "statuses": {"a": row(1, Value::Null, Value::Null)},
            "unknown": [], "invalid": [], "accepted": 1, "input_truncated": false,
            "sessionBridgeColumnPresent": true
        })));
        assert!(m.is_empty());
    }

    #[test]
    fn unrecognised_status_is_carried_verbatim_and_is_not_finished() {
        let (m, _) = map_from_body(&body(json!({
            "statuses": {"a": row(1, json!("vacationing"), Value::Null)},
            "unknown": [], "invalid": [], "accepted": 1, "input_truncated": false,
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

    /// Off-contract on purpose: with the bridge column absent coord sends
    /// `statuses: {}`. The row is what makes "discards" falsifiable.
    #[test]
    fn bridge_column_absent_discards_the_whole_body() {
        let (m, note) = map_from_body(&body(json!({
            "statuses": {"a": row(1, json!("finished"), Value::Null)},
            "unknown": [], "invalid": [], "accepted": 1, "input_truncated": false,
            "sessionBridgeColumnPresent": false
        })));
        assert!(
            m.is_empty(),
            "a body from an inexpressible join is not evidence"
        );
        assert!(note.contains("sessionBridgeColumnPresent"));
    }

    /// coord sets `input_truncated` only when a bucket hit its cap: 500 valid
    /// ids, or 50 echoed bad tokens (`MAX_IDS`, `MAX_INVALID_ECHOED`). This
    /// body is the second case, with every accepted id in `unknown`, so all
    /// three notes come from one body coord can send.
    #[test]
    fn unknown_and_invalid_ids_are_noted_not_mapped() {
        let invalid: Vec<String> = (0..50).map(|i| format!("not-a-uuid-{i}")).collect();
        let (m, note) = map_from_body(&body(json!({
            "statuses": {},
            "unknown": [
                "00000000-0000-7000-8000-000000000001",
                "00000000-0000-7000-8000-000000000002"
            ],
            "invalid": invalid,
            "accepted": 2, "input_truncated": true,
            "sessionBridgeColumnPresent": true
        })));
        assert!(m.is_empty());
        assert!(note.contains("2 id(s) resolved to no coord session"));
        assert!(note.contains("50 id(s) were not UUIDs"));
        assert!(note.contains("truncated the INPUT id list"));
    }

    /// Phase 3 of plan `2026-09-12-fleet-principal-session-routes-…` renamed
    /// the wire field `truncated` → `input_truncated`, and this build reads
    /// ONLY the new name. A bare `truncated` is not read as the input cap: on
    /// the sibling routes that word meant "more rows exist", which is the
    /// confusion the rename removed.
    ///
    /// The base is 500 accepted ids, every one answered (coord puts each
    /// accepted id in `statuses` or `unknown`). That is the shape of a request
    /// coord cut at its 500-id cap (`input_truncated: true`) and of one that
    /// sent exactly 500 (`false`). The base leaves `input_truncated` out, so
    /// only the first step, which adds it, is a body coord can send.
    #[test]
    fn input_truncated_is_read_and_the_legacy_name_is_not() {
        let base = || {
            let statuses: serde_json::Map<String, Value> = (0..500u32)
                .map(|i| {
                    (
                        format!("00000000-0000-7000-8000-{i:012}"),
                        row(i, json!("working"), Value::Null),
                    )
                })
                .collect();
            json!({
                "statuses": statuses, "unknown": [], "invalid": [], "accepted": 500,
                "sessionBridgeColumnPresent": true
            })
        };
        let mut v = base();
        v["input_truncated"] = json!(true);
        let b = body(v);
        assert!(b.input_truncated);
        assert!(map_from_body(&b).1.contains("truncated the INPUT id list"));

        // Off-contract on purpose: a body carrying only the retired name.
        let mut v = base();
        v["truncated"] = json!(true);
        let b = decode_off_contract(v);
        assert!(
            !b.input_truncated,
            "the legacy `truncated` must not be read"
        );
        assert!(map_from_body(&b).1.is_empty());

        // Off-contract on purpose: coord always sends the key, so absent is
        // the defensive default. It reads false, with no note.
        let b = decode_off_contract(base());
        assert!(!b.input_truncated);
        assert!(map_from_body(&b).1.is_empty());
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

    // ── fetch_one_for: the session's tenant (plan 2026-11-13-runner-stop-hook-…) ──

    /// A JWT-shaped token claiming `tenant`.
    fn bearer_claiming(tenant: uuid::Uuid) -> reqwest::header::HeaderValue {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let jwt = format!(
            "h.{}.s",
            URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&json!({"tenant_id": tenant.to_string()})).unwrap())
        );
        reqwest::header::HeaderValue::from_str(&format!("Bearer {jwt}")).unwrap()
    }

    fn owned(t: uuid::Uuid) -> PolicyTarget {
        PolicyTarget {
            scope: crate::auth::TenantScope::Owned(t),
            tenant: Some(t),
        }
    }

    /// Tests bullet 2. The one-session work-status read for tenant B presents
    /// B's credential: the reusable credential is keyed per tenant, so the
    /// default tenant A's never answers a lookup for B, and a request carrying
    /// A's bearer for target B is refused before it can be sent. The old read
    /// went through ONE process-wide default-credential slot.
    #[test]
    fn the_work_status_read_for_tenant_b_presents_bs_credential_never_as() {
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let now = std::time::Instant::now();
        tenant_credential_store(
            Some(a),
            now,
            "https://ws-a.example".into(),
            bearer_claiming(a),
        );
        tenant_credential_store(
            Some(b),
            now,
            "https://ws-b.example".into(),
            bearer_claiming(b),
        );

        let (base_b, auth_b) = tenant_credential_lookup(Some(b), now).expect("B's own slot");
        assert_eq!(base_b, "https://ws-b.example");
        assert!(auth_b.is_sensitive(), "a cached bearer never prints");
        let request = build_work_status_get(
            &reqwest::Client::new(),
            work_status_url(&base_b, "x"),
            owned(b),
            Some(auth_b),
        )
        .expect("B's credential is presented for B");
        assert_eq!(
            crate::mcp::policy_tenant::presented_claim(request.headers()),
            Some(b)
        );

        let (_, auth_a) = tenant_credential_lookup(Some(a), now).expect("A's own slot");
        let refused = build_work_status_get(
            &reqwest::Client::new(),
            work_status_url(&base_b, "x"),
            owned(b),
            Some(auth_a),
        )
        .expect_err("A's credential is never presented for B");
        assert!(refused.contains(&format!("claims tenant {a}")), "{refused}");

        assert_eq!(
            tenant_credential_lookup(Some(uuid::Uuid::new_v4()), now),
            None
        );
        assert_eq!(
            tenant_credential_lookup(Some(b), now + CREDENTIAL_REUSE + Duration::from_secs(1)),
            None,
            "a credential is reused only for CREDENTIAL_REUSE"
        );
    }

    /// Tests bullet 4 for the work-status read: no usable slot for the
    /// target tenant ⇒ no request, and `fetch_one_for` settles UNRESOLVED
    /// (which the verdict maps to an allow) without sending anything.
    #[tokio::test]
    async fn no_work_status_request_is_sent_for_a_tenant_with_no_usable_credential() {
        let t = uuid::Uuid::new_v4();
        let refused = build_work_status_get(
            &reqwest::Client::new(),
            work_status_url("https://coord.example.com", "x"),
            owned(t),
            None,
        )
        .expect_err("no unauthenticated work-status read for tenant t");
        assert!(
            refused.contains(&format!("no usable credential claiming tenant {t}")),
            "{refused}"
        );
        let one = fetch_one_for("44444444-4444-4444-4444-444444444444", owned(t)).await;
        let OneStatus::Unresolved(note) = &one else {
            panic!("no credential for t is UNRESOLVED, got {one:?}");
        };
        assert!(
            note.contains(&format!("no usable credential claiming tenant {t}")),
            "{note}"
        );
        assert_eq!(
            tenant_credential_lookup(Some(t), std::time::Instant::now()),
            None,
            "a failure is never cached"
        );
    }
}
