//! Heal finished marks stamped `finish_synced` over a write coord never
//! applied, and make the finish read-back observe coord (plan
//! `2026-11-05-finish-synced-is-stamped-on-a-2xx-that-did-not-move-session-status`,
//! Phase 5).
//!
//! ## Why
//!
//! Until Phase 4 (`session::coord_sync_finish_ack`) the drain stamped
//! `finish_synced` on ANY 2xx, and coord `89909b161` answered 2xx to a finished
//! PATCH whose progress write it had swallowed. Every mark stamped that way
//! reads synced locally while coord's row is unfinished — and both re-send
//! doors (`set_finished`'s retry door, the registrar's late delivery) skip a
//! synced mark, so nothing ever re-sends it.
//!
//! ## The rule (D3, narrowed by review F1)
//!
//! A synced mark is re-sent only when the mark's OWN coord row positively
//! reads `session_status` **NULL** — the axis a lost finish leaves untouched
//! on a row nothing else ever moved — and coord's `since` (its
//! `state_started_at`) is null or EARLIER than the mark's `finishedAt`. A
//! `since` at or after the finish is a revival coord recorded, and re-sending
//! would re-finish a revived session (coord's `422 stale_finish_refused`
//! would answer, and the drain never stamps on it).
//!
//! **A row reading `working` is NEVER healed, whatever its `since`**
//! (`working_revival_unprovable`). `coord_report_status` (and any other
//! status writer) can report `working` over a row the lost finish never moved
//! WITHOUT advancing `state_started_at` — it is a same-status re-assertion
//! from coord's point of view — so `working` with a `since` before the finish
//! is no proof that nothing revived the session after it. Any other
//! not-finished word is left alone too (`status_not_null`). Serving the row's
//! `last_progress_at` on coord's work-status would let a `working` row be
//! healed safely (a progress write after the finish IS a revival); that is a
//! follow-up, not this change.
//!
//! Everything that is not that positive reading — an unreadable coord, no
//! row, a row other than the mark's own, an own row the runner cannot
//! resolve, an unparseable `since`, a mark older than [`HEAL_WINDOW_MS`] —
//! changes nothing and is counted by reason in `GET /health` `finishHeal`.
//!
//! The re-send of a SYNCED mark is two steps, both under the lifecycle
//! store's `finish_forward` serializer:
//! [`SessionLifecycleStore::clear_finish_synced`] withdraws the stamp of
//! exactly that mark, then the registrar's serialized late-delivery body
//! (`AiCoordRegistrar::redeliver_finish`, the same
//! `with_unsynced_finished_records` → `enqueue_finished` path
//! `deliver_owed_to` takes) re-reads the mark and queues it. Never a bare
//! `enqueue_finished`: an unmark that wins the race leaves no finished mark to
//! read, so no `Finished` row can follow its `working`.
//!
//! ## Unsynced marks (review F3)
//!
//! The boot sweep also takes in-window UNSYNCED marks — among them a finish
//! the drain ACK-dropped after `FINISHED_UNCONFIRMED_MAX_ATTEMPTS` unconfirmed
//! answers — and re-offers one through the same `redeliver_finish` door when
//! its own row passes the same NULL rule. The read-back does not: its own
//! retry door (`set_finished` on an unsynced mark) already re-offers it.
//!
//! ## What guards the runner's own revivals
//!
//! The heal acts only on a mark still finished LOCALLY, so it must not run
//! over a revival the runner itself made. With the NULL-only rule that is
//! mostly moot — every runner revival reports `working`, which is never
//! healed — but the revival paths also clear the local mark first:
//! `unfinish_session` runs from `set_finished(false)`'s observer after the
//! mark is cleared, and the resume door refuses a finished record
//! (`resume_door::classify` → `finished`). The AI-plane wiring
//! (`AiCoordRegistrar::progress_on_interaction` clearing the mark through
//! `set_finished` first, and the planned-restart drain's `progress_on_drain`
//! sending no `working` for a finished session) is a GUARD with no production
//! records behind it today: lifecycle records live in the terminal plane, and
//! an AI task-run session reaches that store only when the orchestration loop
//! records a worker there.
//!
//! ## Where it runs
//!
//! Once per runner start ([`spawn_boot_sweep`]), and on the finish read-back
//! (`POST /sessions/<id>/finish` with no reason — [`read_back`]), which also
//! reports what coord was observed to say as `coord.observed`.
//!
//! ## The mark's own row (review F2)
//!
//! When the drain's observed ACK stamps a mark it also records the row that
//! answered, and when it ACK-drops an unconfirmed finish it records the row
//! that finish was SENT to, unstamped
//! (`TerminalSessionRecord::finish_coord_session_id`). That is what
//! [`HealPort::own_row`] prefers; it falls back to the in-process resolvers
//! (the registrar's index, the live terminal plane). A mark with neither — one
//! stamped before the field existed whose session no live plane knows — is
//! left as `mark_row_unknown`: UNKNOWN, never guessed onto coord's newest row
//! for the harness id, which could be a later revival on another device.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Value as JsonValue};
use tracing::{info, warn};
use uuid::Uuid;

use super::session_lifecycle_store::{FinishSync, SessionLifecycleStore, TerminalSessionRecord};
use super::tracking_health::SessionWorkStatus;
use crate::claude_session::coord_register::AiCoordRegistrar;
use crate::mcp::session_work_status::{self, ObservedRow, StatusFetch, MAX_IDS};

/// Marks finished longer ago than this are never healed (14 days).
pub const HEAL_WINDOW_MS: i64 = 14 * 24 * 60 * 60 * 1000;

/// How long after runner start the boot sweep waits, so the credential and
/// the drain are up before it reads coord and queues anything.
pub const BOOT_SWEEP_DELAY: Duration = Duration::from_secs(60);

/// Why the heal left a mark alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HealSkip {
    /// The mark is older than [`HEAL_WINDOW_MS`].
    OutOfWindow,
    /// The mark's own coord row is neither stored nor resolvable in-process.
    MarkRowUnknown,
    /// coord's work-status could not be read.
    CoordUnreadable,
    /// coord served no row for this session.
    NoCoordRow,
    /// coord's row is not the mark's own (or names none).
    DifferentRow,
    /// coord's own row reads `finished` — the healthy case.
    CoordFinished,
    /// coord's own row reads `working`: a revival `since` cannot rule out.
    WorkingRevivalUnprovable,
    /// coord's own row reads some other not-finished word.
    StatusNotNull,
    /// coord's `since` is at or after the finish: a revival coord recorded.
    RevivedAfterFinish,
    /// coord served a `since` the runner cannot parse — UNKNOWN.
    SinceUnreadable,
}

impl HealSkip {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OutOfWindow => "out_of_window",
            Self::MarkRowUnknown => "mark_row_unknown",
            Self::CoordUnreadable => "coord_unreadable",
            Self::NoCoordRow => "no_coord_row",
            Self::DifferentRow => "different_row",
            Self::CoordFinished => "coord_finished",
            Self::WorkingRevivalUnprovable => "working_revival_unprovable",
            Self::StatusNotNull => "status_not_null",
            Self::RevivedAfterFinish => "revived_after_finish",
            Self::SinceUnreadable => "since_unreadable",
        }
    }
}

/// What the heal decides for one mark. Pure output of [`heal_decision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealDecision {
    /// coord's own row reads NULL with no later revival: re-send to `row`.
    Resend { row: Uuid },
    /// Change nothing, for this reason.
    Leave(HealSkip),
}

/// What coord's work-status read said about one session.
#[derive(Debug, Clone, Copy)]
pub enum Observation<'a> {
    /// The read failed or was degraded.
    Unreadable,
    /// The read succeeded; `None` = coord served no row for the session.
    Row(Option<&'a ObservedRow>),
}

/// D3 (narrowed), pure: decide one mark finished at `finished_at` (unix ms),
/// whose own coord row is `own_row`, against coord's observation, at
/// `now_ms`. Only a NULL `session_status` with no later `since` re-sends.
pub fn heal_decision(
    finished_at: i64,
    own_row: Option<Uuid>,
    obs: Observation<'_>,
    now_ms: i64,
) -> HealDecision {
    if now_ms.saturating_sub(finished_at) > HEAL_WINDOW_MS {
        return HealDecision::Leave(HealSkip::OutOfWindow);
    }
    let Some(own) = own_row else {
        return HealDecision::Leave(HealSkip::MarkRowUnknown);
    };
    let row = match obs {
        Observation::Unreadable => return HealDecision::Leave(HealSkip::CoordUnreadable),
        Observation::Row(None) => return HealDecision::Leave(HealSkip::NoCoordRow),
        Observation::Row(Some(row)) => row,
    };
    let answered = row
        .coord_session_id
        .as_deref()
        .and_then(|s| Uuid::parse_str(s.trim()).ok());
    if answered != Some(own) {
        return HealDecision::Leave(HealSkip::DifferentRow);
    }
    match row.session_status.as_deref().map(SessionWorkStatus::parse) {
        Some(SessionWorkStatus::Finished) => {
            return HealDecision::Leave(HealSkip::CoordFinished)
        }
        Some(SessionWorkStatus::Working) => {
            return HealDecision::Leave(HealSkip::WorkingRevivalUnprovable)
        }
        Some(_) => return HealDecision::Leave(HealSkip::StatusNotNull),
        None => {}
    }
    match (row.since.as_deref(), row.since_ms) {
        (Some(_), None) => HealDecision::Leave(HealSkip::SinceUnreadable),
        (_, Some(since)) if since >= finished_at => {
            HealDecision::Leave(HealSkip::RevivedAfterFinish)
        }
        _ => HealDecision::Resend { row: own },
    }
}

/// The two runner capabilities the heal needs, so it is testable without a
/// live registrar. Implemented by [`AiCoordRegistrar`].
pub trait HealPort: Send + Sync {
    /// The coord row `rec`'s finished mark addresses: the row stored with an
    /// observed stamp (`finish_coord_session_id`) first, else the in-process
    /// resolution.
    fn own_row(&self, rec: &TerminalSessionRecord) -> Option<Uuid>;
    /// Re-send the mark through the serialized late-delivery body; `None`
    /// when the mark is no longer finished-and-unsynced at `finished_at`.
    fn redeliver(&self, csid: &str, finished_at: i64, row: Uuid) -> Option<FinishSync>;
}

impl HealPort for AiCoordRegistrar {
    fn own_row(&self, rec: &TerminalSessionRecord) -> Option<Uuid> {
        rec.finish_coord_session_id
            .or_else(|| self.finish_row_for(rec))
    }
    fn redeliver(&self, csid: &str, finished_at: i64, row: Uuid) -> Option<FinishSync> {
        self.redeliver_finish(csid, finished_at, row)
    }
}

/// What the heal did to one mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealOutcome {
    /// The stamp was withdrawn and the finish re-queued to `row`.
    Resent { row: Uuid },
    /// An UNSYNCED mark's finish was re-offered to `row` (boot sweep only).
    Reoffered { row: Uuid },
    /// The stamp was withdrawn (or the mark was already unsynced) but the
    /// re-send stayed local-only; the retry door re-offers it.
    NotQueued { row: Uuid, reason: &'static str },
    /// The mark changed between the read and the heal (unmarked, restamped,
    /// or its sync state moved) — nothing was sent.
    MarkChanged,
    /// Left alone.
    Left(HealSkip),
}

impl HealOutcome {
    fn reason(&self) -> &'static str {
        match self {
            Self::Resent { .. } => "resent",
            Self::Reoffered { .. } => "reoffered",
            Self::NotQueued { .. } => "not_queued",
            Self::MarkChanged => "mark_changed",
            Self::Left(skip) => skip.as_str(),
        }
    }
}

/// coord's work-status rows for a set of sessions, possibly assembled from
/// several bounded reads: a session whose read was degraded is UNREADABLE,
/// never "no row".
#[derive(Debug, Default)]
pub struct Observations {
    rows: HashMap<String, ObservedRow>,
    unreadable: HashSet<String>,
}

impl Observations {
    /// Fold in one read made for `ids`. A degraded read makes every id
    /// unreadable; a read whose input was truncated (coord's cap, or the
    /// runner's own clamp) makes every id it served no row for unreadable —
    /// such an id may simply not have been asked about, so it is never read
    /// as `no_coord_row`.
    pub fn absorb(&mut self, ids: &[String], fetch: &StatusFetch) {
        if fetch.degraded {
            self.unreadable.extend(ids.iter().cloned());
            return;
        }
        for id in ids {
            match fetch.rows_by_session_id.get(id) {
                Some(row) => {
                    self.rows.insert(id.clone(), row.clone());
                }
                None if fetch.input_truncated => {
                    self.unreadable.insert(id.clone());
                }
                None => {}
            }
        }
    }

    /// One read made for `ids`.
    pub fn from_fetch(ids: &[String], fetch: &StatusFetch) -> Self {
        let mut o = Self::default();
        o.absorb(ids, fetch);
        o
    }

    fn get(&self, csid: &str) -> Observation<'_> {
        if self.unreadable.contains(csid) {
            Observation::Unreadable
        } else {
            Observation::Row(self.rows.get(csid))
        }
    }
}

/// Run the heal over `records` against one work-status read made for exactly
/// those records' ids (synced marks only — the read-back's arm).
pub fn apply_heal(
    store: &SessionLifecycleStore,
    port: Option<&dyn HealPort>,
    records: &[TerminalSessionRecord],
    fetch: &StatusFetch,
    now_ms: i64,
) -> Vec<(String, HealOutcome)> {
    let ids: Vec<String> = records.iter().map(|r| r.claude_session_id.clone()).collect();
    apply_heal_observed(
        store,
        port,
        records,
        &Observations::from_fetch(&ids, fetch),
        now_ms,
        false,
    )
}

/// Run the heal over `records`. Synced finished marks are healed (stamp
/// withdrawn, finish re-sent); with `include_unsynced`, in-window unsynced
/// finished marks passing the same rule are re-offered. Records every outcome
/// in the `/health` counters and returns them.
pub fn apply_heal_observed(
    store: &SessionLifecycleStore,
    port: Option<&dyn HealPort>,
    records: &[TerminalSessionRecord],
    obs: &Observations,
    now_ms: i64,
    include_unsynced: bool,
) -> Vec<(String, HealOutcome)> {
    let mut out = Vec::new();
    for rec in records {
        let Some(at) = rec.finished_at else { continue };
        if !rec.finish_synced && !include_unsynced {
            continue;
        }
        let own_row = port.and_then(|p| p.own_row(rec));
        let decision = heal_decision(at, own_row, obs.get(&rec.claude_session_id), now_ms);
        let outcome = match (decision, rec.finish_synced) {
            (HealDecision::Leave(skip), _) => HealOutcome::Left(skip),
            (HealDecision::Resend { row }, true) => {
                match (store.clear_finish_synced(&rec.claude_session_id, at), port) {
                    (true, Some(p)) => redelivered(p, rec, at, row, false),
                    // Unreachable (a Resend needs an own row, which needs a
                    // port) — and never a send without the serialized door.
                    (true, None) => HealOutcome::NotQueued {
                        row,
                        reason: "no_forwarder",
                    },
                    (false, _) => HealOutcome::MarkChanged,
                }
            }
            (HealDecision::Resend { row }, false) => match port {
                Some(p) => redelivered(p, rec, at, row, true),
                None => HealOutcome::NotQueued {
                    row,
                    reason: "no_forwarder",
                },
            },
        };
        match &outcome {
            HealOutcome::Resent { row } | HealOutcome::Reoffered { row } => info!(
                claude_session_id = %rec.claude_session_id,
                coord_session_id = %row,
                outcome = outcome.reason(),
                "finish_heal: coord's own row reads session_status NULL — the finish is \
                 re-sent through the serialized late-delivery door"
            ),
            HealOutcome::NotQueued { row, reason } => warn!(
                claude_session_id = %rec.claude_session_id,
                coord_session_id = %row,
                reason,
                "finish_heal: the re-send stayed local-only"
            ),
            _ => {}
        }
        note_outcome(&outcome);
        out.push((rec.claude_session_id.clone(), outcome));
    }
    out
}

/// Re-send `rec`'s mark through the port's serialized door.
fn redelivered(
    p: &dyn HealPort,
    rec: &TerminalSessionRecord,
    at: i64,
    row: Uuid,
    reoffer: bool,
) -> HealOutcome {
    match p.redeliver(&rec.claude_session_id, at, row) {
        Some(FinishSync::Queued { coord_session_id }) if reoffer => HealOutcome::Reoffered {
            row: coord_session_id,
        },
        Some(FinishSync::Queued { coord_session_id }) => HealOutcome::Resent {
            row: coord_session_id,
        },
        Some(FinishSync::LocalOnly(r)) => HealOutcome::NotQueued {
            row,
            reason: r.as_str(),
        },
        None => HealOutcome::MarkChanged,
    }
}

/// One sweep over every finished synced mark, plus (with `include_unsynced`)
/// every unsynced one, or only `only`: read work-status for the in-window
/// ones, then [`apply_heal_observed`].
pub async fn heal_sweep(
    store: &SessionLifecycleStore,
    port: Option<&dyn HealPort>,
    only: Option<&str>,
    include_unsynced: bool,
) -> Vec<(String, HealOutcome)> {
    heal_sweep_with(store, port, only, include_unsynced, |ids| async move {
        session_work_status::fetch(&ids).await
    })
    .await
}

/// [`heal_sweep`] over an explicit work-status reader (the test seam). Ids
/// are read in chunks of at most [`MAX_IDS`] — coord's own per-request cap —
/// so an id past the cap is never misread as `no_coord_row`.
pub async fn heal_sweep_with<F, Fut>(
    store: &SessionLifecycleStore,
    port: Option<&dyn HealPort>,
    only: Option<&str>,
    include_unsynced: bool,
    fetch: F,
) -> Vec<(String, HealOutcome)>
where
    F: Fn(Vec<String>) -> Fut,
    Fut: Future<Output = StatusFetch>,
{
    let now_ms = Utc::now().timestamp_millis();
    let candidates: Vec<TerminalSessionRecord> = store
        .all_records()
        .into_iter()
        .filter(|r| r.finished_at.is_some() && (r.finish_synced || include_unsynced))
        .filter(|r| only.is_none_or(|id| r.claude_session_id == id))
        .collect();
    let ids: Vec<String> = candidates
        .iter()
        .filter(|r| {
            r.finished_at
                .is_some_and(|at| now_ms.saturating_sub(at) <= HEAL_WINDOW_MS)
        })
        .map(|r| r.claude_session_id.clone())
        .collect();
    let mut obs = Observations::default();
    for chunk in ids.chunks(MAX_IDS) {
        let read = fetch(chunk.to_vec()).await;
        obs.absorb(chunk, &read);
    }
    apply_heal_observed(store, port, &candidates, &obs, now_ms, include_unsynced)
}

/// Spawn the once-per-runner-start sweep, after [`BOOT_SWEEP_DELAY`].
pub fn spawn_boot_sweep(
    store: std::sync::Weak<SessionLifecycleStore>,
    registrar: std::sync::Weak<AiCoordRegistrar>,
) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(BOOT_SWEEP_DELAY).await;
        let (Some(store), Some(reg)) = (store.upgrade(), registrar.upgrade()) else {
            return;
        };
        let outcomes =
            heal_sweep(&store, Some(reg.as_ref() as &dyn HealPort), None, true).await;
        note_sweep();
        let resent = outcomes
            .iter()
            .filter(|(_, o)| {
                matches!(o, HealOutcome::Resent { .. } | HealOutcome::Reoffered { .. })
            })
            .count();
        info!(
            marks = outcomes.len(),
            resent, "finish_heal: boot sweep over synced finished marks complete"
        );
    });
}

// ---------------------------------------------------------------------------
// The finish read-back
// ---------------------------------------------------------------------------

/// What the finish read-back observed of coord, and what the heal did.
#[derive(Debug, Clone)]
pub struct ReadBack {
    /// coord's row for the session; `None` when unreadable or absent.
    pub observed: Option<ObservedRow>,
    /// When the read was made.
    pub observed_at: DateTime<Utc>,
    /// Why `observed` is `None`.
    pub unobserved_reason: Option<String>,
    /// The mark's own coord row: the stored sent/confirmed row
    /// (`finish_coord_session_id`) first, then the in-process resolution
    /// ([`HealPort::own_row`]).
    pub own_row: Option<Uuid>,
    /// The heal's outcome, for a finished synced mark.
    pub heal: Option<HealOutcome>,
}

/// Read coord for `csid` and heal its mark if coord's own row contradicts it.
pub async fn read_back(
    store: &SessionLifecycleStore,
    port: Option<&dyn HealPort>,
    csid: &str,
) -> ReadBack {
    let fetch = session_work_status::fetch(&[csid.to_string()]).await;
    read_back_from(store, port, csid, &fetch, Utc::now())
}

/// [`read_back`] over an explicit fetch result and clock. Pure over the store.
pub fn read_back_from(
    store: &SessionLifecycleStore,
    port: Option<&dyn HealPort>,
    csid: &str,
    fetch: &StatusFetch,
    now: DateTime<Utc>,
) -> ReadBack {
    let rec = store.get(csid);
    let own_row = rec.as_ref().and_then(|r| port.and_then(|p| p.own_row(r)));
    let (observed, unobserved_reason) = if fetch.degraded {
        (None, Some(fetch.note.clone()))
    } else {
        match fetch.rows_by_session_id.get(csid) {
            Some(row) => (Some(row.clone()), None),
            None => (
                None,
                Some("coord's work-status served no row for this session".to_string()),
            ),
        }
    };
    let heal = rec
        .filter(|r| r.finished_at.is_some() && r.finish_synced)
        .and_then(|r| {
            apply_heal(store, port, &[r], fetch, now.timestamp_millis())
                .into_iter()
                .next()
                .map(|(_, o)| o)
        });
    note_read_back();
    ReadBack {
        observed,
        observed_at: now,
        unobserved_reason,
        own_row,
        heal,
    }
}

/// Add `coord.observed` (and the heal's outcome) to a finish read-back
/// response built by `FinishOutcome::response_json`, and make
/// `session.finishSynced` observe coord:
///
/// - coord readable: `true` only when the stored stamp is `true` AND coord's
///   row reads `finished` AND that row is the mark's own — the stored
///   sent/confirmed row first, then the in-process resolution — (or, when
///   neither knows the own row, only the status can be checked);
/// - coord unreadable: the stored value, unchanged — itself an observed
///   signal since Phase 4 — with `coord.observed: null` and
///   `coord.observedReason` naming why.
///
/// `stored` is the record as it stands AFTER the heal.
pub fn decorate_read_back(
    resp: &mut JsonValue,
    rb: &ReadBack,
    stored: Option<&TerminalSessionRecord>,
) {
    let stored_synced = stored.is_some_and(|r| r.finish_synced);
    let observed = rb.observed.as_ref().map(|row| {
        json!({
            "sessionStatus": row.session_status,
            "since": row.since,
            "coordSessionId": row.coord_session_id,
            "observedAt": rb.observed_at,
        })
    });
    let finish_synced = match &rb.observed {
        Some(row) => {
            let finished = row
                .session_status
                .as_deref()
                .map(SessionWorkStatus::parse)
                .is_some_and(|s| s == SessionWorkStatus::Finished);
            let own = match rb.own_row {
                Some(own) => {
                    row.coord_session_id
                        .as_deref()
                        .and_then(|s| Uuid::parse_str(s.trim()).ok())
                        == Some(own)
                }
                None => true,
            };
            stored_synced && finished && own
        }
        None => stored_synced,
    };
    if let Some(session) = resp.get_mut("session").and_then(JsonValue::as_object_mut) {
        session.insert("finishSynced".to_string(), JsonValue::Bool(finish_synced));
    }
    let Some(coord) = resp.get_mut("coord").and_then(JsonValue::as_object_mut) else {
        return;
    };
    coord.insert("observed".to_string(), observed.unwrap_or(JsonValue::Null));
    coord.insert(
        "observedReason".to_string(),
        rb.unobserved_reason
            .as_ref()
            .map(|r| JsonValue::String(r.clone()))
            .unwrap_or(JsonValue::Null),
    );
    coord.insert(
        "heal".to_string(),
        rb.heal
            .as_ref()
            .map(|o| json!({ "outcome": o.reason() }))
            .unwrap_or(JsonValue::Null),
    );
    if let Some(HealOutcome::Resent { row }) = &rb.heal {
        // The heal re-queued the finish: the read-back now reports that write.
        coord.insert("queued".to_string(), JsonValue::Bool(true));
        coord.insert(
            "coordSessionId".to_string(),
            JsonValue::String(row.to_string()),
        );
        coord.insert("reason".to_string(), JsonValue::Null);
    }
}

// ---------------------------------------------------------------------------
// `GET /health` `finishHeal`
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct HealHealth {
    sweeps: u64,
    last_sweep_at: Option<DateTime<Utc>>,
    read_backs: u64,
    by_outcome: BTreeMap<&'static str, u64>,
}

fn health_cell() -> &'static Mutex<HealHealth> {
    static CELL: OnceLock<Mutex<HealHealth>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HealHealth::default()))
}

fn note_outcome(o: &HealOutcome) {
    if let Ok(mut h) = health_cell().lock() {
        *h.by_outcome.entry(o.reason()).or_insert(0) += 1;
    }
}

fn note_sweep() {
    if let Ok(mut h) = health_cell().lock() {
        h.sweeps += 1;
        h.last_sweep_at = Some(Utc::now());
    }
}

fn note_read_back() {
    if let Ok(mut h) = health_cell().lock() {
        h.read_backs += 1;
    }
}

/// `GET /health` `finishHeal`: boot sweeps and read-backs run in this process,
/// and every synced mark they judged, counted by outcome (`resent`,
/// `not_queued`, `mark_changed`, or the [`HealSkip`] reason it was left for).
/// `bootSweeps: 0` with `lastSweepAt: null` means the boot sweep has not
/// completed yet — UNKNOWN, not "nothing to heal".
pub fn heal_health_json() -> JsonValue {
    health_cell()
        .lock()
        .map(|h| {
            json!({
                "bootSweeps": h.sweeps,
                "lastSweepAt": h.last_sweep_at,
                "readBacks": h.read_backs,
                "byOutcome": h.by_outcome,
            })
        })
        .unwrap_or(JsonValue::Null)
}

#[cfg(test)]
#[path = "finish_heal_tests.rs"]
mod tests;
