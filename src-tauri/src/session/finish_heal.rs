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
//! ## The rule (D3)
//!
//! A synced mark is re-sent only when, for the mark's OWN coord row, coord's
//! work-status read POSITIVELY says not `finished`, and coord's `since` (its
//! `state_started_at`) is null or EARLIER than the mark's `finishedAt` — i.e.
//! coord has recorded no revival after the finish. A `since` at or after the
//! finish is a revival coord saw; re-sending would re-finish a revived session
//! (coord's `422 stale_finish_refused` would answer, and the drain never
//! stamps on it). Everything that is not that positive reading — an
//! unreadable coord, no row, a row other than the mark's own, an own row the
//! runner cannot resolve, an unparseable `since`, a mark older than
//! [`HEAL_WINDOW_MS`] — changes nothing and is counted by reason in `GET
//! /health` `finishHeal`.
//!
//! The re-send is two steps, both under the lifecycle store's
//! `finish_forward` serializer: [`SessionLifecycleStore::clear_finish_synced`]
//! withdraws the stamp of exactly that mark, then the registrar's
//! serialized late-delivery body (`AiCoordRegistrar::redeliver_finish`, the
//! same `with_unsynced_finished_records` → `enqueue_finished` path
//! `deliver_owed_to` takes) re-reads the mark and queues it. Never a bare
//! `enqueue_finished`: an unmark that wins the race leaves no finished mark to
//! read, so no `Finished` row can follow its `working`.
//!
//! ## Why the heal cannot re-finish a revival the runner made
//!
//! It acts only on a mark that is still finished LOCALLY, so it is sound only
//! if every runner-side revival clears the local mark before it reports
//! `working`. They do: `unfinish_session` runs from `set_finished(false)`'s
//! observer, after the mark is cleared; the resume door refuses a finished
//! record (`resume_door::classify` → `finished`); and an interaction
//! (`AiCoordRegistrar::progress_on_interaction`) clears the mark through
//! `set_finished` first, while the planned-restart drain
//! (`progress_on_drain`) sends no `working` for a finished session at all.
//!
//! ## Where it runs
//!
//! Once per runner start ([`spawn_boot_sweep`]), and on the finish read-back
//! (`POST /sessions/<id>/finish` with no reason — [`read_back`]), which also
//! reports what coord was observed to say as `coord.observed`.
//!
//! ## Limit, stated
//!
//! The mark's own row is resolved IN-PROCESS (the registrar's index, the
//! live terminal plane): the record stores no coord row id. A mark whose
//! session neither this process registered nor a live terminal hosts has no
//! resolvable row after a restart, and is left as `mark_row_unknown` — UNKNOWN,
//! never guessed onto coord's newest row for the harness id, which could be a
//! later revival on another device.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Value as JsonValue};
use tracing::{info, warn};
use uuid::Uuid;

use super::session_lifecycle_store::{FinishSync, SessionLifecycleStore, TerminalSessionRecord};
use super::tracking_health::SessionWorkStatus;
use crate::claude_session::coord_register::AiCoordRegistrar;
use crate::mcp::session_work_status::{self, ObservedRow, StatusFetch};

/// Marks finished longer ago than this are never healed (14 days).
pub const HEAL_WINDOW_MS: i64 = 14 * 24 * 60 * 60 * 1000;

/// How long after runner start the boot sweep waits, so the credential and
/// the drain are up before it reads coord and queues anything.
pub const BOOT_SWEEP_DELAY: Duration = Duration::from_secs(60);

/// Why the heal left a synced mark alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HealSkip {
    /// The mark is older than [`HEAL_WINDOW_MS`].
    OutOfWindow,
    /// The mark's own coord row does not resolve in this process.
    MarkRowUnknown,
    /// coord's work-status could not be read.
    CoordUnreadable,
    /// coord served no row for this session.
    NoCoordRow,
    /// coord's row is not the mark's own (or names none).
    DifferentRow,
    /// coord's own row reads `finished` — the healthy case.
    CoordFinished,
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
            Self::RevivedAfterFinish => "revived_after_finish",
            Self::SinceUnreadable => "since_unreadable",
        }
    }
}

/// What the heal decides for one synced mark. Pure output of [`heal_decision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealDecision {
    /// coord's own row positively contradicts the mark: re-send to `row`.
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

/// D3, pure: decide one synced mark finished at `finished_at` (unix ms),
/// whose own coord row is `own_row`, against coord's observation, at
/// `now_ms`.
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
    if row
        .session_status
        .as_deref()
        .map(SessionWorkStatus::parse)
        .is_some_and(|s| s == SessionWorkStatus::Finished)
    {
        return HealDecision::Leave(HealSkip::CoordFinished);
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
    /// The coord row `rec`'s finished mark addresses, resolved in-process.
    fn own_row(&self, rec: &TerminalSessionRecord) -> Option<Uuid>;
    /// Re-send the mark through the serialized late-delivery body; `None`
    /// when the mark is no longer finished-and-unsynced at `finished_at`.
    fn redeliver(&self, csid: &str, finished_at: i64, row: Uuid) -> Option<FinishSync>;
}

impl HealPort for AiCoordRegistrar {
    fn own_row(&self, rec: &TerminalSessionRecord) -> Option<Uuid> {
        self.finish_row_for(rec)
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
    /// The stamp was withdrawn but the re-send stayed local-only (the mark is
    /// now honestly unsynced and the retry door re-offers it).
    NotQueued { row: Uuid, reason: &'static str },
    /// The mark changed between the read and the heal (unmarked, restamped,
    /// or already unsynced) — nothing was sent.
    MarkChanged,
    /// Left alone.
    Left(HealSkip),
}

impl HealOutcome {
    fn reason(&self) -> &'static str {
        match self {
            Self::Resent { .. } => "resent",
            Self::NotQueued { .. } => "not_queued",
            Self::MarkChanged => "mark_changed",
            Self::Left(skip) => skip.as_str(),
        }
    }
}

/// Run the heal over `records` (only finished, SYNCED marks are considered)
/// against one work-status read. Records every outcome in the `/health`
/// counters and returns them.
pub fn apply_heal(
    store: &SessionLifecycleStore,
    port: Option<&dyn HealPort>,
    records: &[TerminalSessionRecord],
    fetch: &StatusFetch,
    now_ms: i64,
) -> Vec<(String, HealOutcome)> {
    let mut out = Vec::new();
    for rec in records {
        let Some(at) = rec.finished_at else { continue };
        if !rec.finish_synced {
            continue;
        }
        let obs = if fetch.degraded {
            Observation::Unreadable
        } else {
            Observation::Row(fetch.rows_by_session_id.get(&rec.claude_session_id))
        };
        let own_row = port.and_then(|p| p.own_row(rec));
        let outcome = match heal_decision(at, own_row, obs, now_ms) {
            HealDecision::Leave(skip) => HealOutcome::Left(skip),
            HealDecision::Resend { row } => {
                match (store.clear_finish_synced(&rec.claude_session_id, at), port) {
                    (true, Some(p)) => match p.redeliver(&rec.claude_session_id, at, row) {
                        Some(FinishSync::Queued { coord_session_id }) => HealOutcome::Resent {
                            row: coord_session_id,
                        },
                        Some(FinishSync::LocalOnly(r)) => HealOutcome::NotQueued {
                            row,
                            reason: r.as_str(),
                        },
                        None => HealOutcome::MarkChanged,
                    },
                    // Unreachable (a Resend needs an own row, which needs a
                    // port) — and never a send without the serialized door.
                    (true, None) => HealOutcome::NotQueued {
                        row,
                        reason: "no_forwarder",
                    },
                    (false, _) => HealOutcome::MarkChanged,
                }
            }
        };
        match &outcome {
            HealOutcome::Resent { row } => info!(
                claude_session_id = %rec.claude_session_id,
                coord_session_id = %row,
                "finish_heal: coord's own row did not read finished — the synced stamp is \
                 withdrawn and the finish re-sent"
            ),
            HealOutcome::NotQueued { row, reason } => warn!(
                claude_session_id = %rec.claude_session_id,
                coord_session_id = %row,
                reason,
                "finish_heal: stamp withdrawn but the re-send stayed local-only"
            ),
            _ => {}
        }
        note_outcome(&outcome);
        out.push((rec.claude_session_id.clone(), outcome));
    }
    out
}

/// One sweep over every finished, synced mark (or only `only`): read
/// work-status for the in-window ones, then [`apply_heal`].
pub async fn heal_sweep(
    store: &SessionLifecycleStore,
    port: Option<&dyn HealPort>,
    only: Option<&str>,
) -> Vec<(String, HealOutcome)> {
    let now_ms = Utc::now().timestamp_millis();
    let candidates: Vec<TerminalSessionRecord> = store
        .all_records()
        .into_iter()
        .filter(|r| r.finished_at.is_some() && r.finish_synced)
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
    let fetch = session_work_status::fetch(&ids).await;
    apply_heal(store, port, &candidates, &fetch, now_ms)
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
        let outcomes = heal_sweep(&store, Some(reg.as_ref() as &dyn HealPort), None).await;
        note_sweep();
        let resent = outcomes
            .iter()
            .filter(|(_, o)| matches!(o, HealOutcome::Resent { .. }))
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
    /// The mark's own coord row, resolved in-process.
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
///   row reads `finished` AND that row is the mark's own (or the own row does
///   not resolve in-process, when only the status can be checked);
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
