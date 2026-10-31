//! `GET /apps/{app_id}/journey/health` — is the journey ledger actually being
//! written?
//!
//! The co-occurrence precedent only `warn!`s when capture fails, so a pipeline
//! that stopped writing looks identical to one with nothing to write. Here
//! every outcome is counted and the state is DERIVED from the counts:
//!
//! - `schema_absent` — the probe found a journey table missing;
//! - `write_failing` — a write (edge, frontier, or an event the queue could
//!   not hold) failed within the last [`RECENT_WINDOW`] attempts, or the probe
//!   itself failed;
//! - `writing` — the probe found both tables and the recent window is clean.
//!
//! **Never `writing` on no evidence.** Until the probe has completed the state
//! is `write_failing` with a `detail` beginning `unknown:` — the shared
//! `LedgerState` vocabulary is closed, so "not yet known" is said in `detail`
//! and in [`JourneyHealthResponse::schema_probe`], never by reading `writing`.
//! The handler runs the probe itself when it has not run yet, so this arm is
//! reached only when the database cannot be asked at all.
//!
//! The counters are process-wide (the ledger lives in one database); the
//! `appId` path segment scopes the route like its `/apps/{app_id}/spec/*`
//! siblings and is echoed back.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::Json;
use qontinui_types::journey::{JourneyLedgerHealth, LedgerState};
use serde::Serialize;

use crate::mcp::types::ApiState;

use super::capture::{journey_schema_state, journey_schema_supported, SchemaProbe};

/// How many recent write attempts the failure count looks back over.
pub(crate) const RECENT_WINDOW: usize = 20;

/// The `schema_absent` detail on an embedded database — plan Phase 0
/// decision 4: the runner applies its vendored schema only to a FRESH
/// embedded database, so an existing one never receives new tables.
pub(crate) const EMBEDDED_SCHEMA_ABSENT_DETAIL: &str = "embedded DB predates the journey tables — they are created only on a fresh database; see plan Phase 0 decision 4";

static EDGES_WRITTEN: AtomicU64 = AtomicU64::new(0);
static FRONTIER_ROWS_CLEARED: AtomicU64 = AtomicU64::new(0);
static FRONTIER_UPSERTS: AtomicU64 = AtomicU64::new(0);
static WRITES_NOT_ATTEMPTED: AtomicU64 = AtomicU64::new(0);
static WRITE_FAILURES_TOTAL: AtomicU64 = AtomicU64::new(0);
static PENDING_OPEN: AtomicU64 = AtomicU64::new(0);
static TIMELINES_WRITTEN: AtomicU64 = AtomicU64::new(0);
static TIMELINES_UNPARSEABLE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Default)]
struct Recent {
    /// `true` = success, newest at the back, at most [`RECENT_WINDOW`].
    outcomes: VecDeque<bool>,
    last_error: Option<String>,
    last_write_at: Option<String>,
    last_prune_at: Option<String>,
    last_prune_deleted: Option<u64>,
    last_prune_skipped: Option<String>,
    last_timeline_error: Option<String>,
    /// Passive capture, PER APP (plan Phase 3), at most [`MAX_PASSIVE_APPS`].
    /// A slot is created only for a relay from a REGISTERED app whose BODY was
    /// accepted (`observe::claim_slot_if_registered`), claimed under one lock
    /// before its event is queued ([`claim_passive_slot`]).
    passive: std::collections::BTreeMap<String, PassiveStats>,
    /// Counts for an app with no slot above — its refusals, and any receipt
    /// that found no slot — kept apart and capped at [`MAX_UNSLOTTED_APPS`]
    /// with least-recently-touched eviction: an app whose every relay is
    /// refused still reads `refused`, while junk ids can never fill
    /// [`Self::passive`] and lock real apps out.
    passive_unslotted: std::collections::BTreeMap<String, Unslotted>,
    /// Unslotted entries evicted so far: after one, an app with no entry
    /// anywhere may have had counts the map no longer holds (UNKNOWN).
    passive_unslotted_evicted: u64,
    /// Monotonic touch counter for the unslotted map's eviction order.
    passive_refusal_seq: u64,
    /// Refusals whose `app_id` path segment was not an identifier, so they
    /// belong to no app's block.
    passive_unattributed_refusals: u64,
}

/// The counts of an app that holds no slot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Unslotted {
    refused: u64,
    /// Closing snapshots refused at the queue reserve ([`record_passive_snapshot_refused`]).
    snapshots_refused: u64,
    last_snapshot_refusal: Option<String>,
    last_refusal: Option<String>,
    /// Receipts that found no slot (only if one was not claimed first).
    received: u64,
    last_received_at: Option<String>,
    touched: u64,
    /// Created after an unslotted entry was evicted: this app's own earlier
    /// entry may have been the one evicted, so its counts may be incomplete.
    counts_incomplete: bool,
}

/// Most apps the unslotted counters track (see
/// [`Recent::passive_unslotted`]).
pub(crate) const MAX_UNSLOTTED_APPS: usize = 256;

/// One app's passive-capture counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PassiveStats {
    pub received: u64,
    /// Relays the ROUTE refused ([`record_passive_refused`]) — never a
    /// closing snapshot (that is [`Self::snapshots_refused`]).
    pub refused: u64,
    /// Closing snapshots of ACCEPTED relays refused at the queue reserve
    /// ([`record_passive_snapshot_refused`]).
    pub snapshots_refused: u64,
    /// Why (and when) the last closing snapshot was refused.
    pub last_snapshot_refusal: Option<String>,
    pub last_received_at: Option<String>,
    pub last_refusal: Option<String>,
    /// The counts may be incomplete: this app's counters were (re)created
    /// after an unslotted entry was evicted, and its own earlier counts may
    /// have been the ones evicted. Refusals then read UNKNOWN, and receipts
    /// a lower bound.
    pub counts_incomplete: bool,
}

/// Most apps passive capture keeps counters for. A relay for a NEW app past
/// this is refused (the route answers 429), so the map cannot grow without
/// bound.
pub(crate) const MAX_PASSIVE_APPS: usize = 256;

/// Journey writes that failed or were not attempted (the schema could not
/// take them), process-wide and monotonic — the explorer compares this at a
/// run's end with its start ([`super::explorer::Unsettled::ledger_write_failures`]).
pub(crate) fn write_failures_total() -> u64 {
    WRITE_FAILURES_TOTAL.load(Ordering::Relaxed) + WRITES_NOT_ATTEMPTED.load(Ordering::Relaxed)
}

fn recent() -> &'static Mutex<Recent> {
    static RECENT: std::sync::OnceLock<Mutex<Recent>> = std::sync::OnceLock::new();
    RECENT.get_or_init(|| Mutex::new(Recent::default()))
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn push_outcome(ok: bool, error: Option<String>) {
    let mut r = recent().lock().unwrap_or_else(|p| p.into_inner());
    r.outcomes.push_back(ok);
    while r.outcomes.len() > RECENT_WINDOW {
        r.outcomes.pop_front();
    }
    if ok {
        r.last_write_at = Some(now_iso());
    }
    if let Some(e) = error {
        r.last_error = Some(e);
    }
}

/// An edge row landed; `frontier_cleared` rows left the frontier with it;
/// `with_timeline` = its `timeline` column was written (plan Phase 4).
pub(crate) fn record_edge_written(frontier_cleared: u64, with_timeline: bool) {
    EDGES_WRITTEN.fetch_add(1, Ordering::Relaxed);
    if with_timeline {
        TIMELINES_WRITTEN.fetch_add(1, Ordering::Relaxed);
    }
    FRONTIER_ROWS_CLEARED.fetch_add(frontier_cleared, Ordering::Relaxed);
    push_outcome(true, None);
}

/// A frontier upsert statement landed, touching `rows` rows.
pub(crate) fn record_frontier_upserted(rows: u64) {
    FRONTIER_UPSERTS.fetch_add(rows, Ordering::Relaxed);
    push_outcome(true, None);
}

/// A write failed: a contract validation refusal, a refused statement, a pool
/// error, or an event the queue could not hold. Never a silent drop.
pub(crate) fn record_write_failure(error: String) {
    tracing::warn!("journey ledger write failed: {}", error);
    WRITE_FAILURES_TOTAL.fetch_add(1, Ordering::Relaxed);
    push_outcome(false, Some(error));
}

/// An execute-with-diff response carried a `timeline` that is not a
/// `ChangeTimeline`. The edge is still recorded, with a NULL timeline; this
/// count is what keeps "no timeline stored" from reading as "none sent".
pub(crate) fn record_timeline_unparseable(error: String) {
    tracing::warn!(
        "journey: execute-with-diff timeline did not parse: {}",
        error
    );
    TIMELINES_UNPARSEABLE.fetch_add(1, Ordering::Relaxed);
    let mut r = recent().lock().unwrap_or_else(|p| p.into_inner());
    r.last_timeline_error = Some(error);
}

/// A write was not attempted because the probe said the schema cannot take
/// it. Counted separately: the state already says why.
pub(crate) fn record_not_written() {
    WRITES_NOT_ATTEMPTED.fetch_add(1, Ordering::Relaxed);
}

/// The worker's count of open pending edges.
pub(crate) fn set_pending_open(n: u64) {
    PENDING_OPEN.store(n, Ordering::Relaxed);
}

/// One retention pass finished.
pub(crate) fn record_prune(deleted: u64) {
    let mut r = recent().lock().unwrap_or_else(|p| p.into_inner());
    r.last_prune_at = Some(now_iso());
    r.last_prune_deleted = Some(deleted);
    r.last_prune_skipped = None;
}

/// A retention pass did not run (schema absent, probe failed, pool error) or
/// failed part-way; `deleted` is what it removed before stopping.
pub(crate) fn record_prune_skipped(reason: String, deleted: u64) {
    let mut r = recent().lock().unwrap_or_else(|p| p.into_inner());
    r.last_prune_at = Some(now_iso());
    r.last_prune_deleted = Some(deleted);
    r.last_prune_skipped = Some(reason);
}

/// Claim `app_id`'s counter slot — existing, or new while there is room —
/// in ONE lock, before the relay's event is queued, so another app cannot
/// take the last slot between the check and the receipt. A new slot takes
/// over the app's unslotted counts. `false` = the map is full (429).
pub(crate) fn claim_passive_slot(app_id: &str) -> bool {
    let mut r = recent().lock().unwrap_or_else(|p| p.into_inner());
    claim_in(&mut r, app_id)
}

/// Does `app_id` hold a counter slot?
#[cfg(test)]
pub(crate) fn passive_slot_held(app_id: &str) -> bool {
    recent()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .passive
        .contains_key(app_id)
}

fn claim_in(r: &mut Recent, app_id: &str) -> bool {
    if r.passive.contains_key(app_id) {
        return true;
    }
    if r.passive.len() >= MAX_PASSIVE_APPS {
        return false;
    }
    // No unslotted entry after an eviction: this app's may have been the one
    // evicted, so the new slot's counts start possibly incomplete.
    let evicted = r.passive_unslotted_evicted > 0;
    let earlier = r.passive_unslotted.remove(app_id).unwrap_or(Unslotted {
        counts_incomplete: evicted,
        ..Default::default()
    });
    r.passive.insert(
        app_id.to_string(),
        PassiveStats {
            received: earlier.received,
            refused: earlier.refused,
            snapshots_refused: earlier.snapshots_refused,
            last_snapshot_refusal: earlier.last_snapshot_refusal,
            last_received_at: earlier.last_received_at,
            last_refusal: earlier.last_refusal,
            counts_incomplete: earlier.counts_incomplete,
        },
    );
    true
}

/// `POST /apps/{app_id}/journey/observe` accepted one relayed interaction.
pub(crate) fn record_passive_received(app_id: &str) {
    let mut r = recent().lock().unwrap_or_else(|p| p.into_inner());
    record_received_in(&mut r, app_id, now_iso());
}

/// [`record_passive_received`] over a [`Recent`]. The app's slot was claimed
/// first ([`claim_passive_slot`]), so it is counted there. Should a receipt
/// ever find no slot, it is counted in the app's unslotted entry instead —
/// never dropped, and never read as zero.
fn record_received_in(r: &mut Recent, app_id: &str, at: String) {
    if let Some(slot) = r.passive.get_mut(app_id) {
        slot.received += 1;
        slot.last_received_at = Some(at);
        return;
    }
    let e = unslotted_entry(r, app_id);
    e.received += 1;
    e.last_received_at = Some(at);
}

/// `app_id`'s unslotted entry, created (evicting the least recently touched
/// one when the map is full) and touched.
///
/// A NEW entry is marked `counts_incomplete` when any entry was evicted
/// BEFORE this call: this app's own earlier entry may have been the one. The
/// evicted ids are not tracked (that set would grow without bound), so once
/// any eviction has happened, EVERY app first seen afterwards reads
/// counts-incomplete for the rest of the process — deliberately: an unknown
/// count is never shown as a known one.
fn unslotted_entry<'a>(r: &'a mut Recent, app_id: &str) -> &'a mut Unslotted {
    let evicted_before = r.passive_unslotted_evicted > 0;
    if !r.passive_unslotted.contains_key(app_id) && r.passive_unslotted.len() >= MAX_UNSLOTTED_APPS
    {
        if let Some(oldest) = r
            .passive_unslotted
            .iter()
            .min_by_key(|(_, e)| e.touched)
            .map(|(k, _)| k.clone())
        {
            r.passive_unslotted.remove(&oldest);
            r.passive_unslotted_evicted += 1;
        }
    }
    r.passive_refusal_seq += 1;
    let touched = r.passive_refusal_seq;
    // Only an eviction BEFORE this call can have taken this app's counts:
    // the one this call may have just made evicted some OTHER app.
    let e = r
        .passive_unslotted
        .entry(app_id.to_string())
        .or_insert_with(|| Unslotted {
            counts_incomplete: evicted_before,
            ..Default::default()
        });
    e.touched = touched;
    e
}

/// `POST /apps/{app_id}/journey/observe` refused a relay (malformed, an
/// unknown field — a `value` or `text` key included — an unknown action
/// type, a bad id, an unregistered app, or no queue room). Counted, so a page relaying a shape the
/// route refuses reads as "refused", not as "never received". `None` = the
/// path's app id itself was refused.
///
/// A refusal never creates a slot in the accepted-relay map (only
/// [`claim_passive_slot`] does, for a registered app's accepted body), so ids
/// that nothing registered or that never sent an acceptable body cannot fill [`MAX_PASSIVE_APPS`] and lock real apps out. It is
/// counted in the app's slot when it has one, else in the separately capped
/// unslotted map ([`MAX_UNSLOTTED_APPS`], least recently refused
/// evicted first); `None` (an app id that is not an identifier) is
/// unattributed.
pub(crate) fn record_passive_refused(app_id: Option<&str>, reason: String) {
    let mut r = recent().lock().unwrap_or_else(|p| p.into_inner());
    record_refusal_in(&mut r, app_id, reason);
}

fn record_refusal_in(r: &mut Recent, app_id: Option<&str>, reason: String) {
    let Some(app_id) = app_id else {
        r.passive_unattributed_refusals += 1;
        return;
    };
    if let Some(slot) = r.passive.get_mut(app_id) {
        slot.refused += 1;
        slot.last_refusal = Some(reason);
        return;
    }
    let e = unslotted_entry(r, app_id);
    e.refused += 1;
    e.last_refusal = Some(reason);
}

/// The closing snapshot of an ACCEPTED relay (taken inside
/// `capture::as_passive`) was refused at the journey queue's reserve, so the
/// relay's pending edge will close `to_node_unobserved`. Counted apart from
/// [`record_passive_refused`]: the relay itself was accepted (and counted in
/// `receivedTotal`), so counting this as a refused relay would make
/// `refusedTotal` count two different things. `reason` becomes
/// `lastClosingSnapshotRefusal`, stamped with the time it was recorded, so a
/// rise in `closingSnapshotsRefusedTotal` says why.
pub(crate) fn record_passive_snapshot_refused(app_id: &str, reason: &str) {
    let mut r = recent().lock().unwrap_or_else(|p| p.into_inner());
    record_snapshot_refusal_in(&mut r, app_id, format!("{} — {reason}", now_iso()));
}

fn record_snapshot_refusal_in(r: &mut Recent, app_id: &str, reason: String) {
    if let Some(slot) = r.passive.get_mut(app_id) {
        slot.snapshots_refused += 1;
        slot.last_snapshot_refusal = Some(reason);
        return;
    }
    // An accepted relay claimed a slot first, so this is not expected; kept,
    // never dropped, should it happen.
    let e = unslotted_entry(r, app_id);
    e.snapshots_refused += 1;
    e.last_snapshot_refusal = Some(reason);
}

/// The passive-capture block for `app_id` from a [`Recent`]: its slot, else
/// its unslotted entry, else nothing — and then its refusal count is
/// known (zero) only while no unslotted entry was ever evicted. A slot or
/// entry created after an eviction carries `counts_incomplete`.
fn passive_health_in(r: &Recent, app_id: &str) -> PassiveCaptureHealth {
    if let Some(stats) = r.passive.get(app_id) {
        return derive_passive_capture(stats.clone(), true, r.passive_unattributed_refusals);
    }
    if let Some(e) = r.passive_unslotted.get(app_id) {
        let stats = PassiveStats {
            refused: e.refused,
            snapshots_refused: e.snapshots_refused,
            last_snapshot_refusal: e.last_snapshot_refusal.clone(),
            last_refusal: e.last_refusal.clone(),
            received: e.received,
            last_received_at: e.last_received_at.clone(),
            counts_incomplete: e.counts_incomplete,
        };
        return derive_passive_capture(stats, true, r.passive_unattributed_refusals);
    }
    // No counters at all: after an eviction they may have been evicted, so
    // the (zero) counts are incomplete, not known.
    let evicted = r.passive_unslotted_evicted > 0;
    derive_passive_capture(
        PassiveStats {
            counts_incomplete: evicted,
            ..Default::default()
        },
        !evicted,
        r.passive_unattributed_refusals,
    )
}

/// Whether passive real-session capture has reached this runner (plan
/// Phase 3). Process-scoped like every counter here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PassiveCaptureState {
    /// At least one relayed interaction was accepted by this process.
    Receiving,
    /// Relays for this app were refused, and none is recorded as accepted.
    /// With `countsComplete` true, every relay that arrived was refused; with
    /// it false the counts are a lower bound, so that is not claimed.
    Refused,
    /// None is recorded as accepted, and none as refused. With
    /// `countsComplete` false, earlier counts may have been evicted, so
    /// neither is known to be zero.
    NeverReceived,
}

/// The `passiveCapture` block of `/journey/health`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PassiveCaptureHealth {
    /// What the counts cover: THIS app (the path's `appId`), since this
    /// runner process started — they reset on restart.
    pub scope: &'static str,
    pub state: PassiveCaptureState,
    /// When the last relayed interaction was accepted; `None` when none was.
    pub last_received_at: Option<String>,
    /// Relays accepted for this app — a LOWER BOUND when
    /// [`Self::counts_complete`] is false.
    pub received_total: u64,
    /// Relays for this app the ROUTE refused (see [`record_passive_refused`]):
    /// each a relayed interaction that was not recorded at all. It counts
    /// nothing else — a closing snapshot refused after its relay was accepted
    /// is [`Self::closing_snapshots_refused_total`].
    /// `None` (JSON `null`) = UNKNOWN: unslotted counters were evicted, and
    /// either this app has no counters, or its counters were created after
    /// an eviction that may have taken its earlier ones — it may have had
    /// refusals this process no longer holds. Never shown as a known count.
    pub refused_total: Option<u64>,
    /// Closing snapshots of ACCEPTED relays refused at the journey queue's
    /// reserve (see [`record_passive_snapshot_refused`]): each a relay that
    /// IS in `receivedTotal` but whose edge closes `to_node_unobserved`.
    /// `None` (JSON `null`) = UNKNOWN, exactly when `refusedTotal` is.
    pub closing_snapshots_refused_total: Option<u64>,
    /// `false` when this app's counts may be incomplete (see
    /// [`Self::refused_total`]): `receivedTotal` is then a lower bound.
    pub counts_complete: bool,
    /// Why the last relay the route refused was refused ([`Self::refused_total`]).
    pub last_refusal: Option<String>,
    /// When and why the last closing snapshot was refused
    /// ([`Self::closing_snapshots_refused_total`]); `None` when none was.
    pub last_closing_snapshot_refusal: Option<String>,
    /// Relays process-wide the observe route refused because their path app
    /// id was not an identifier (so they belong to no app) — that, and
    /// nothing else. A passive journey event refused at the queue reserve
    /// that names no app (no current path produces one: the only events
    /// recorded inside `capture::as_passive` are closing snapshots, which
    /// always carry their app) is not a refused relay, so it is not counted
    /// here: it is a dropped journey event, counted as a journey write
    /// failure (`counters.recentWriteFailures` / the ledger state).
    pub unattributed_refusals: u64,
    pub detail: String,
}

/// [`PassiveCaptureHealth::scope`].
pub(crate) const PASSIVE_SCOPE: &str = "this app, since this runner process started";

/// Derive the passive-capture block. Pure, so both arms are tested.
///
/// `never_received` says only that nothing arrived: whether a page has the
/// interceptor mounted and simply had no human at it, or has none mounted,
/// is NOT knowable from here, and the detail says so rather than guessing.
pub(crate) fn derive_passive_capture(
    stats: PassiveStats,
    refusals_known: bool,
    unattributed_refusals: u64,
) -> PassiveCaptureHealth {
    let PassiveStats {
        received: received_total,
        refused,
        snapshots_refused,
        last_snapshot_refusal,
        last_received_at,
        last_refusal,
        counts_incomplete,
    } = stats;
    let counts_complete = !counts_incomplete;
    let refusals_known = refusals_known && counts_complete;
    let refused_total = refusals_known.then_some(refused);
    let closing_snapshots_refused_total = refusals_known.then_some(snapshots_refused);
    let caveat = if counts_complete {
        ""
    } else {
        " — counts may be INCOMPLETE: this app's counters were created after unslotted \
         counters were evicted, so the receipt count is a lower bound and the refusal count \
         is unknown"
    };
    let (state, detail) = match (received_total, &last_received_at) {
        (n, Some(at)) if n > 0 => (
            PassiveCaptureState::Receiving,
            format!(
                "{n} relayed interaction(s) accepted for this app by this process; the last at \
                 {at} — a quiet page since then is not distinguishable from an unmounted \
                 one{caveat}"
            ),
        ),
        _ if refused > 0 => (
            PassiveCaptureState::Refused,
            format!(
                "{}; last refusal: {}{caveat}",
                if counts_complete {
                    format!(
                        "{refused} relayed interaction(s) for this app arrived and every one \
                         was refused"
                    )
                } else {
                    format!(
                        "At least {refused} relayed interaction(s) for this app were refused, \
                         and none is recorded as accepted"
                    )
                },
                last_refusal.as_deref().unwrap_or("<none recorded>")
            ),
        ),
        _ if !refusals_known => (
            PassiveCaptureState::NeverReceived,
            format!(
                "no relayed interaction for this app is RECORDED as accepted or refused, but \
                 counts for apps with no slot have been evicted since this process started, \
                 so whether any was accepted or refused is UNKNOWN (and \
                 {unattributed_refusals} refusal(s) had an app id that was not an \
                 identifier)"
            ),
        ),
        _ => (
            PassiveCaptureState::NeverReceived,
            "no relayed interaction accepted for this app since this process started — whether \
             no human \
             used a connected page or no page relays to POST /apps/{app_id}/journey/observe is \
             not known from here"
                .to_string(),
        ),
    };
    PassiveCaptureHealth {
        scope: PASSIVE_SCOPE,
        state,
        last_received_at,
        received_total,
        refused_total,
        closing_snapshots_refused_total,
        counts_complete,
        last_refusal,
        last_closing_snapshot_refusal: last_snapshot_refusal,
        unattributed_refusals,
        detail,
    }
}

/// The probe's answer, as the wire string of
/// [`JourneyHealthResponse::schema_probe`].
fn probe_str(probe: Option<&SchemaProbe>) -> &'static str {
    match probe {
        None => "unknown",
        Some(SchemaProbe::Present) => "present",
        Some(SchemaProbe::Absent { .. }) => "absent",
        Some(SchemaProbe::Failed { .. }) => "failed",
    }
}

/// Derive the ledger state. Pure, so every arm is tested.
///
/// `embedded` selects the `schema_absent` detail: on the runner's embedded
/// database the cause is Phase 0 decision 4; on an external one it is simply a
/// missing table.
pub(crate) fn derive_ledger_health(
    probe: Option<&SchemaProbe>,
    recent_failures: usize,
    last_error: Option<&str>,
    edges_written: u64,
    embedded: bool,
) -> JourneyLedgerHealth {
    match probe {
        None => JourneyLedgerHealth {
            state: LedgerState::WriteFailing,
            detail: "unknown: the journey schema probe has not completed (the database could \
                     not be asked), so whether the ledger is being written is not known"
                .to_string(),
        },
        Some(SchemaProbe::Absent { missing }) => JourneyLedgerHealth {
            state: LedgerState::SchemaAbsent,
            detail: if embedded {
                format!("{EMBEDDED_SCHEMA_ABSENT_DETAIL} (missing: {})", missing.join(", "))
            } else {
                format!(
                    "missing table(s) {} in this database — apply qontinui-web migration \
                     journey_01_edge_ledger",
                    missing.join(", ")
                )
            },
        },
        Some(SchemaProbe::Failed { error }) => JourneyLedgerHealth {
            state: LedgerState::WriteFailing,
            detail: format!(
                "the journey schema probe failed ({error}); nothing is written until a probe \
                 succeeds — it is retried automatically after {}s",
                super::capture::PROBE_RETRY_BACKOFF.as_secs()
            ),
        },
        Some(SchemaProbe::Present) if recent_failures > 0 => JourneyLedgerHealth {
            state: LedgerState::WriteFailing,
            detail: format!(
                "{recent_failures} of the last {RECENT_WINDOW} journey writes failed; last error: {}",
                last_error.unwrap_or("<none recorded>")
            ),
        },
        Some(SchemaProbe::Present) => JourneyLedgerHealth {
            state: LedgerState::Writing,
            detail: if edges_written == 0 {
                "journey tables present and readable; no edge written yet by this process"
                    .to_string()
            } else {
                format!("journey tables present; {edges_written} edge(s) written by this process")
            },
        },
    }
}

/// Process-wide counters reported beside the ledger state.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JourneyCounters {
    pub edges_written: u64,
    pub pending_edges_open: u64,
    /// Frontier rows inserted or refreshed.
    pub frontier_upserts: u64,
    /// Frontier rows removed because an edge activated them.
    pub frontier_rows_cleared: u64,
    /// Writes skipped because the probe said the schema cannot take them.
    pub writes_not_attempted: u64,
    pub write_failures_total: u64,
    /// Edges written with a `timeline` (plan Phase 4).
    pub timelines_written: u64,
    /// Execute-with-diff timelines that did not parse and were not stored.
    pub timelines_unparseable: u64,
    pub last_timeline_error: Option<String>,
    pub recent_write_failures: u64,
    pub recent_window: u64,
    pub last_write_at: Option<String>,
    pub last_error: Option<String>,
    pub last_prune_at: Option<String>,
    pub last_prune_deleted: Option<u64>,
    /// Why the last retention pass did not run (or stopped), when it did not.
    pub last_prune_skipped: Option<String>,
}

/// Body of `GET /apps/{app_id}/journey/health`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JourneyHealthResponse {
    pub app_id: String,
    /// The shared ledger block every journey read repeats.
    pub ledger: JourneyLedgerHealth,
    /// `unknown` until the once-per-process probe completes, then `present`,
    /// `absent` or `failed`.
    pub schema_probe: &'static str,
    pub counters: JourneyCounters,
    /// Passive real-session capture (plan Phase 3).
    pub passive_capture: PassiveCaptureHealth,
}

fn is_embedded() -> bool {
    matches!(
        crate::embedded_pg::db_arm(),
        crate::embedded_pg::DbArm::EmbeddedOwned | crate::embedded_pg::DbArm::EmbeddedAttached
    )
}

/// Assemble the health body from the current counters.
pub(crate) fn snapshot_health(app_id: String) -> JourneyHealthResponse {
    let probe = journey_schema_state();
    let probe = probe.as_ref();
    let (recent_failures, counters, passive_capture) = {
        let r = recent().lock().unwrap_or_else(|p| p.into_inner());
        let failures = r.outcomes.iter().filter(|ok| !**ok).count();
        (
            failures,
            JourneyCounters {
                edges_written: EDGES_WRITTEN.load(Ordering::Relaxed),
                pending_edges_open: PENDING_OPEN.load(Ordering::Relaxed),
                frontier_upserts: FRONTIER_UPSERTS.load(Ordering::Relaxed),
                frontier_rows_cleared: FRONTIER_ROWS_CLEARED.load(Ordering::Relaxed),
                writes_not_attempted: WRITES_NOT_ATTEMPTED.load(Ordering::Relaxed),
                write_failures_total: WRITE_FAILURES_TOTAL.load(Ordering::Relaxed),
                timelines_written: TIMELINES_WRITTEN.load(Ordering::Relaxed),
                timelines_unparseable: TIMELINES_UNPARSEABLE.load(Ordering::Relaxed),
                last_timeline_error: r.last_timeline_error.clone(),
                recent_write_failures: failures as u64,
                recent_window: RECENT_WINDOW as u64,
                last_write_at: r.last_write_at.clone(),
                last_error: r.last_error.clone(),
                last_prune_at: r.last_prune_at.clone(),
                last_prune_deleted: r.last_prune_deleted,
                last_prune_skipped: r.last_prune_skipped.clone(),
            },
            passive_health_in(&r, &app_id),
        )
    };
    let ledger = derive_ledger_health(
        probe,
        recent_failures,
        counters.last_error.as_deref(),
        counters.edges_written,
        is_embedded(),
    );
    JourneyHealthResponse {
        app_id,
        ledger,
        schema_probe: probe_str(probe),
        counters,
        passive_capture,
    }
}

/// `GET /apps/{app_id}/journey/health`.
///
/// Runs the schema probe itself when no settled answer is cached (never
/// probed, or a failure older than the retry backoff), so a runner that has
/// not seen an agent action still answers `writing` / `schema_absent` from
/// evidence. When the database cannot be asked (no pooled connection within
/// the pool's own timeout), the probe is left un-run and the answer says
/// `unknown` (or the last failure).
pub async fn get_journey_health(
    State(state): State<Arc<ApiState>>,
    Path(app_id): Path<String>,
) -> Json<JourneyHealthResponse> {
    if !matches!(
        journey_schema_state(),
        Some(SchemaProbe::Present | SchemaProbe::Absent { .. })
    ) {
        if let Ok(conn) = state.app_state.pg_db.pool().get().await {
            let _ = journey_schema_supported(&conn).await;
        }
    }
    Json(snapshot_health(app_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_probe_never_reads_writing() {
        let h = derive_ledger_health(None, 0, None, 0, true);
        assert_ne!(h.state, LedgerState::Writing);
        assert!(h.detail.starts_with("unknown"), "{}", h.detail);
        assert_eq!(probe_str(None), "unknown");
    }

    #[test]
    fn schema_absent_on_embedded_names_phase_0_decision_4() {
        let probe = SchemaProbe::Absent {
            missing: vec![
                "project.journey_edge_observations",
                "project.journey_frontier",
            ],
        };
        let h = derive_ledger_health(Some(&probe), 0, None, 0, true);
        assert_eq!(h.state, LedgerState::SchemaAbsent);
        assert!(
            h.detail.starts_with(EMBEDDED_SCHEMA_ABSENT_DETAIL),
            "{}",
            h.detail
        );
        assert!(h.detail.contains("project.journey_frontier"));
    }

    #[test]
    fn schema_absent_on_external_is_the_plain_missing_table() {
        let probe = SchemaProbe::Absent {
            missing: vec!["project.journey_frontier"],
        };
        let h = derive_ledger_health(Some(&probe), 3, Some("x"), 0, false);
        assert_eq!(
            h.state,
            LedgerState::SchemaAbsent,
            "absent outranks failures"
        );
        assert!(h
            .detail
            .starts_with("missing table(s) project.journey_frontier"));
        assert!(!h.detail.contains("Phase 0"));
    }

    #[test]
    fn recent_failures_read_write_failing_with_the_last_error() {
        let h = derive_ledger_health(
            Some(&SchemaProbe::Present),
            2,
            Some("[23514] check_violation"),
            9,
            true,
        );
        assert_eq!(h.state, LedgerState::WriteFailing);
        assert!(h.detail.contains("2 of the last 20"));
        assert!(h.detail.contains("check_violation"));
    }

    #[test]
    fn a_failed_probe_is_write_failing_not_absent() {
        let h = derive_ledger_health(
            Some(&SchemaProbe::Failed {
                error: "permission denied".into(),
            }),
            0,
            None,
            0,
            true,
        );
        assert_eq!(h.state, LedgerState::WriteFailing);
        assert!(h.detail.contains("permission denied"));
    }

    #[test]
    fn present_and_clean_is_writing() {
        let h = derive_ledger_health(Some(&SchemaProbe::Present), 0, None, 0, true);
        assert_eq!(h.state, LedgerState::Writing);
        assert!(h.detail.contains("no edge written yet"));
        let h = derive_ledger_health(Some(&SchemaProbe::Present), 0, None, 4, false);
        assert!(h.detail.contains("4 edge(s)"));
    }

    #[test]
    fn passive_capture_is_never_received_until_one_is_accepted() {
        let refused = derive_passive_capture(
            PassiveStats {
                refused: 3,
                last_refusal: Some("unknown field `value`".into()),
                ..Default::default()
            },
            true,
            0,
        );
        assert_eq!(refused.state, PassiveCaptureState::Refused);
        assert_eq!(refused.last_received_at, None);
        assert_eq!(
            refused.refused_total,
            Some(3),
            "refusals are reported, not hidden"
        );
        let never = derive_passive_capture(PassiveStats::default(), true, 0);
        assert_eq!(never.state, PassiveCaptureState::NeverReceived);
        assert_eq!(never.refused_total, Some(0));
        assert!(never.detail.contains("not known"), "{}", never.detail);

        let at = "2026-10-28T10:00:00.000Z".to_string();
        let live = derive_passive_capture(
            PassiveStats {
                received: 2,
                last_received_at: Some(at.clone()),
                ..Default::default()
            },
            true,
            0,
        );
        assert_eq!(live.state, PassiveCaptureState::Receiving);
        assert_eq!(live.last_received_at.as_deref(), Some(at.as_str()));
        let wire = serde_json::to_value(&live).unwrap();
        assert_eq!(wire["state"], "receiving");
        assert_eq!(wire["lastReceivedAt"], at.as_str());
        assert_eq!(
            serde_json::to_value(&never).unwrap()["state"],
            "never_received"
        );
    }

    #[test]
    fn passive_counters_are_per_app_and_the_map_is_capped() {
        let mut r = Recent::default();
        for i in 0..MAX_PASSIVE_APPS {
            assert!(claim_in(&mut r, &format!("app-{i}")));
        }
        assert!(!claim_in(&mut r, "one-too-many"));
        assert!(claim_in(&mut r, "app-0"), "known apps keep counting");
        assert_eq!(r.passive.len(), MAX_PASSIVE_APPS);
        // Served per app: one app's interactions are not another's.
        assert!(claim_passive_slot("health-test-app-a"));
        record_passive_received("health-test-app-a");
        let b = snapshot_health("health-test-app-b".into()).passive_capture;
        assert_eq!(b.state, PassiveCaptureState::NeverReceived);
        let a = snapshot_health("health-test-app-a".into()).passive_capture;
        assert_eq!(a.state, PassiveCaptureState::Receiving);
    }

    #[test]
    fn an_always_refused_app_reads_refused_never_zero() {
        // Its interceptor always sends a `value` key: every relay is a 422.
        let mut r = Recent::default();
        for _ in 0..3 {
            record_refusal_in(&mut r, Some("leaky-app"), "unknown field".into());
        }
        let h = passive_health_in(&r, "leaky-app");
        assert_eq!(h.state, PassiveCaptureState::Refused);
        assert_eq!(h.refused_total, Some(3));
        assert_eq!(h.last_refusal.as_deref(), Some("unknown field"));
        assert!(
            r.passive.is_empty(),
            "a refusal takes no accepted-relay slot"
        );
        // Its first accepted relay claims a slot, which takes the refusals.
        assert!(claim_in(&mut r, "leaky-app"));
        record_received_in(&mut r, "leaky-app", "2026-10-29T10:00:00.000Z".into());
        let h = passive_health_in(&r, "leaky-app");
        assert_eq!(h.state, PassiveCaptureState::Receiving);
        assert_eq!(h.refused_total, Some(3));
        assert!(!r.passive_unslotted.contains_key("leaky-app"));
    }

    #[test]
    fn the_last_slot_cannot_be_taken_between_the_claim_and_the_receipt() {
        let mut r = Recent::default();
        for i in 0..(MAX_PASSIVE_APPS - 1) {
            assert!(claim_in(&mut r, &format!("app-{i}")));
        }
        record_refusal_in(&mut r, Some("x"), "bad body".into());
        // X claims the last slot (taking its refusal), THEN Y arrives: full.
        assert!(claim_in(&mut r, "x"));
        assert!(!claim_in(&mut r, "y"), "the map is full");
        // X's receipt lands in X's own slot, with its refusal kept.
        record_received_in(&mut r, "x", "2026-10-29T10:00:00.000Z".into());
        let h = passive_health_in(&r, "x");
        assert_eq!(h.state, PassiveCaptureState::Receiving);
        assert_eq!(h.received_total, 1);
        assert_eq!(h.refused_total, Some(1));
        // Should a receipt ever find no slot, it is kept — never zero.
        record_refusal_in(&mut r, Some("z"), "bad body".into());
        record_received_in(&mut r, "z", "2026-10-29T10:00:01.000Z".into());
        let h = passive_health_in(&r, "z");
        assert_eq!(h.state, PassiveCaptureState::Receiving, "{}", h.detail);
        assert_eq!(h.received_total, 1);
        assert_eq!(h.refused_total, Some(1));
        assert!(!r.passive.contains_key("z"));
    }

    /// X's five refusals are evicted by 256 other ids; X is then re-created,
    /// by a refusal and by an accepted body. Neither may read as a KNOWN
    /// count: the true refusal count is 6, not 1 or 0.
    #[test]
    fn an_evicted_then_recreated_app_never_reads_its_counts_as_known() {
        let evict_x = || {
            let mut r = Recent::default();
            for _ in 0..5 {
                record_refusal_in(&mut r, Some("x"), "bad body".into());
            }
            for i in 0..MAX_UNSLOTTED_APPS {
                record_refusal_in(&mut r, Some(&format!("other-{i}")), "bad body".into());
            }
            assert!(!r.passive_unslotted.contains_key("x"), "x was evicted");
            r
        };
        // Re-created by a refusal.
        let mut r = evict_x();
        record_refusal_in(&mut r, Some("x"), "bad body".into());
        let h = passive_health_in(&r, "x");
        assert_eq!(h.refused_total, None, "1 is not the count: {}", h.detail);
        assert!(!h.counts_complete);
        assert_eq!(h.state, PassiveCaptureState::Refused);
        assert!(h.detail.contains("At least 1"), "{}", h.detail);
        // Re-created by an accepted body (a claimed slot, then a receipt).
        let mut r = evict_x();
        assert!(claim_in(&mut r, "x"));
        let h = passive_health_in(&r, "x");
        assert_eq!(h.refused_total, None, "0 is not the count: {}", h.detail);
        assert!(!h.counts_complete);
        record_received_in(&mut r, "x", "2026-10-29T10:00:00.000Z".into());
        let h = passive_health_in(&r, "x");
        assert_eq!(h.state, PassiveCaptureState::Receiving);
        assert_eq!(h.refused_total, None);
        assert!(!h.counts_complete, "received_total is a lower bound");
        assert!(h.detail.contains("lower bound"), "{}", h.detail);
        let wire = serde_json::to_value(&h).unwrap();
        assert!(wire["refusedTotal"].is_null(), "{wire}");
        assert_eq!(wire["countsComplete"], false, "{wire}");
        // With no eviction, counts are complete and known.
        let mut r = Recent::default();
        record_refusal_in(&mut r, Some("y"), "bad body".into());
        let h = passive_health_in(&r, "y");
        assert_eq!(h.refused_total, Some(1));
        assert!(h.counts_complete);
    }

    /// The app whose own creation makes the FIRST eviction had nothing
    /// evicted before it: its counts are complete. The next new app's are
    /// not (an eviction preceded it).
    #[test]
    fn the_app_that_triggers_the_first_eviction_reads_complete() {
        let mut r = Recent::default();
        for i in 0..MAX_UNSLOTTED_APPS {
            record_refusal_in(&mut r, Some(&format!("other-{i}")), "bad body".into());
        }
        assert_eq!(r.passive_unslotted_evicted, 0);
        record_refusal_in(&mut r, Some("first"), "bad body".into());
        assert_eq!(r.passive_unslotted_evicted, 1, "its creation evicted one");
        let h = passive_health_in(&r, "first");
        assert!(h.counts_complete, "{}", h.detail);
        assert_eq!(h.refused_total, Some(1));
        record_refusal_in(&mut r, Some("second"), "bad body".into());
        assert!(!passive_health_in(&r, "second").counts_complete);
    }

    #[test]
    fn junk_ids_can_never_fill_the_accepted_map_and_evictions_read_unknown() {
        let mut r = Recent::default();
        for i in 0..(MAX_PASSIVE_APPS * 2) {
            record_refusal_in(&mut r, Some(&format!("junk-{i}")), "bad body".into());
        }
        assert!(r.passive.is_empty(), "junk ids hold no accepted-relay slot");
        assert_eq!(r.passive_unslotted.len(), MAX_UNSLOTTED_APPS, "bounded");
        assert_eq!(
            r.passive_unslotted_evicted,
            (MAX_PASSIVE_APPS * 2 - MAX_UNSLOTTED_APPS) as u64
        );
        // A real app can still take a slot.
        assert!(claim_in(&mut r, "real-app"));
        record_received_in(&mut r, "real-app", "2026-10-29T10:00:00.000Z".into());
        assert_eq!(r.passive.len(), 1);
        // An evicted (or never-seen) app's refusal count is UNKNOWN, not 0.
        let h = passive_health_in(&r, "junk-0");
        assert_eq!(h.refused_total, None);
        assert!(
            !h.counts_complete,
            "no counters after an eviction: incomplete"
        );
        assert!(h.detail.contains("UNKNOWN"), "{}", h.detail);
        let wire = serde_json::to_value(&h).unwrap();
        assert!(wire["refusedTotal"].is_null(), "{wire}");
        assert_eq!(wire["countsComplete"], false, "{wire}");
        // An id that is not an identifier is unattributed.
        record_refusal_in(&mut r, None, "bad id".into());
        assert_eq!(r.passive_unattributed_refusals, 1);
    }

    #[test]
    fn a_refused_closing_snapshot_is_its_own_count_never_a_refused_relay() {
        let mut r = Recent::default();
        assert!(claim_in(&mut r, "relaying-app"));
        record_received_in(&mut r, "relaying-app", "2026-10-29T10:00:00.000Z".into());
        record_snapshot_refusal_in(&mut r, "relaying-app", "t0 — reserve".into());
        let h = passive_health_in(&r, "relaying-app");
        assert_eq!(h.state, PassiveCaptureState::Receiving);
        assert_eq!(h.received_total, 1, "the relay itself was accepted");
        assert_eq!(h.refused_total, Some(0), "no relay was refused");
        assert_eq!(h.closing_snapshots_refused_total, Some(1));
        assert_eq!(h.last_refusal, None, "a refused relay is explained apart");
        let wire = serde_json::to_value(&h).unwrap();
        assert_eq!(wire["refusedTotal"], 0, "{wire}");
        assert_eq!(wire["closingSnapshotsRefusedTotal"], 1, "{wire}");
        assert_eq!(
            wire["lastClosingSnapshotRefusal"], "t0 — reserve",
            "a rise in the count says why: {wire}"
        );
        // A route refusal moves only refusedTotal.
        record_refusal_in(&mut r, Some("relaying-app"), "bad body".into());
        let h = passive_health_in(&r, "relaying-app");
        assert_eq!(h.refused_total, Some(1));
        assert_eq!(h.closing_snapshots_refused_total, Some(1));
        // Unknown is first-class: when the counts may be incomplete, both read
        // null — never a known 0.
        let mut r = Recent::default();
        for i in 0..=MAX_UNSLOTTED_APPS {
            record_refusal_in(&mut r, Some(&format!("other-{i}")), "bad body".into());
        }
        assert!(r.passive_unslotted_evicted > 0);
        assert!(claim_in(&mut r, "late-app"));
        record_snapshot_refusal_in(&mut r, "late-app", "t1 — reserve".into());
        let h = passive_health_in(&r, "late-app");
        assert_eq!(h.closing_snapshots_refused_total, None, "{}", h.detail);
        assert_eq!(h.refused_total, None);
        let wire = serde_json::to_value(&h).unwrap();
        assert!(wire["closingSnapshotsRefusedTotal"].is_null(), "{wire}");
    }

    #[test]
    fn the_response_serializes_camel_case_with_the_ledger_block() {
        let body = serde_json::to_value(snapshot_health("qontinui-web".into())).unwrap();
        assert_eq!(body["appId"], "qontinui-web");
        assert!(body["ledger"]["state"].is_string());
        assert!(body["ledger"]["detail"].is_string());
        assert!(body["schemaProbe"].is_string());
        assert!(body["passiveCapture"]["state"].is_string());
        assert_eq!(body["passiveCapture"]["scope"], PASSIVE_SCOPE);
        assert!(body["passiveCapture"].get("lastReceivedAt").is_some());
        for field in [
            "edgesWritten",
            "pendingEdgesOpen",
            "frontierUpserts",
            "lastWriteAt",
            "lastPruneAt",
            "lastPruneDeleted",
            "recentWriteFailures",
        ] {
            assert!(
                body["counters"].get(field).is_some(),
                "missing counters.{field}"
            );
        }
    }
}
