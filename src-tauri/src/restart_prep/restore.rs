//! Phase 6 — the backend restore of a prepared cohort (plan
//! `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, D4–D7).
//!
//! A runner rebuilt over a prepare record moves it to `restoring` (the boot
//! step in [`super::prepare`]). From then until the cohort is dismissed, the
//! cohort is restored by hand, one row at a time, through the backend resume
//! core (`session::resume_door`) — never auto-resumed by the boot restore.
//! This module is what that core and the boot side read:
//!
//! * [`RestoringCohort`] — the rows of a `restoring` record, each with the
//!   effort it is resumed at. The resume core admits an `open` row with no live
//!   terminal only when it is one of these (D7), and passes
//!   [`RestoreTarget::effort`] as `--effort <level>` (D5).
//! * [`effort_to_apply`] — the re-apply rule: the level the session RAN at,
//!   when it was read and differs from the account's baseline.
//! * [`read_back_effort`] — after a verified resume, read the pane's status
//!   row and say whether the effort came back: [`EFFORT_RESTORED`] or
//!   `effort_not_restored(<reason>)`. Never a guess: an indicator that was not
//!   seen is a failure to confirm, not a success.
//! * [`note_restore_on`] — the per-row restore verdict, recorded on the cohort
//!   row itself so the panel and the placeholders read one record.
//! * The boot side (`commands::terminal::terminal_session_list_open`) reads
//!   [`RestoringCohort`] to return the waiting rows as placeholders, NOT in
//!   the auto-restore set (D6). A crash boot with no record is unchanged.
//! * [`note_running`] — the lifecycle poll's observation that a cohort row is
//!   running again (a pane the PTY holder kept alive across the restart and
//!   the runner adopted, or a session resumed by another path). A row that is
//!   back — resumed, or observed running — leaves the waiting set for good, so
//!   a later crash boot auto-restores it like any other session.
//!
//! Restore on a FINISHED cohort row lifts its finish mark (D4); the resume
//! core does that itself, for the operator origin only, once the resume is
//! verified — a refused or failed Restore never touches the mark.
//!
//! [`dismiss_on`] ends a restore the operator does not want to finish: the
//! record moves to `done`, which lifts the placeholder shield for every row
//! still waiting (held, finished, or never restorable) — `done` means every
//! row is back, or the cohort was dismissed.
//!
//! The withheld-boot-restore shield's `in_active_cohort` arm
//! (`session_lifecycle_store::withheld_boot_restore_candidate`) reads
//! [`withheld_cohort_ids`] once per lifecycle poll tick, so a placeholder is
//! never orphan-closed while it waits for its Restore click.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::warn;

use super::cohort::CohortRow;
use super::effort::{self, EffortSource};
use super::prepare::RestartPrep;
use super::{rfc3339, PrepState, PrepareRecord};
use crate::terminal::graceful_exit::ScreenText;

/// The read-back verdict when the pane's status row shows the re-applied level.
pub const EFFORT_RESTORED: &str = "effort_restored";

/// The read-back verdict when the screen never showed the status row but the
/// verified `claude` process's own argv carries `--effort <wanted>`. Claude
/// Code 2.1.310 paints the effort row only transiently, so the screen alone
/// can no longer confirm a re-applied level; the argv of the process the
/// resume verified proves the flag reached the CLI that is running.
pub const EFFORT_RESTORED_BY_ARGV: &str = "effort_restored_by_argv";

/// How long the read-back waits for the resumed CLI to paint its status row.
pub const EFFORT_READ_BACK_DEADLINE: Duration = Duration::from_secs(30);
/// ...when the verified argv already carries the flag: a short look for a
/// contradicting status row, not a 30 s wait per row of a one-at-a-time queue.
pub const EFFORT_READ_BACK_ARGV_DEADLINE: Duration = Duration::from_secs(3);

/// Whether `cmdline` (a process command line, space-joined) passes
/// `--effort <wanted>` (or `--effort=<wanted>`), compared as levels.
pub fn argv_carries_effort(cmdline: &str, wanted: &str) -> bool {
    let Some(want) = effort::rank(wanted) else {
        return false;
    };
    let mut words = cmdline.split_whitespace();
    while let Some(w) = words.next() {
        let value = if w == "--effort" {
            words.next()
        } else {
            w.strip_prefix("--effort=")
        };
        if let Some(v) = value {
            if effort::rank(v) == Some(want) {
                return true;
            }
        }
    }
    false
}

/// The read-back verdict, given whether the verified argv carried the flag:
/// the screen's answer, except that a row never seen (`indicator_not_seen`)
/// on a process whose argv carries the level is [`EFFORT_RESTORED_BY_ARGV`].
/// A row showing ANOTHER level still reads `effort_not_restored(shows_…)`.
pub fn effort_verdict_with_argv(screen_verdict: String, argv_carries: bool) -> String {
    if argv_carries && screen_verdict == effort_not_restored("indicator_not_seen") {
        EFFORT_RESTORED_BY_ARGV.to_string()
    } else {
        screen_verdict
    }
}
/// How often the read-back looks.
pub const EFFORT_READ_BACK_POLL: Duration = Duration::from_millis(1000);

/// `effort_not_restored(<reason>)`.
pub fn effort_not_restored(reason: &str) -> String {
    format!("effort_not_restored({reason})")
}

/// One restore of a cohort row, as the resume core answered it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowRestore {
    pub at_ms: i64,
    pub at: String,
    /// The resume core's verdict, verbatim: `resumed`, `failed(<reason>)` or
    /// `skipped(<reason>)` — `skipped(resource_guard:<lane>)` when the box was
    /// too busy to spawn, which is a retry, not a failure.
    pub verdict: String,
    /// The level passed as `--effort`, when one was.
    pub effort_applied: Option<String>,
    /// `effort_restored` | `effort_not_restored(<reason>)`; `None` when no
    /// effort was re-applied or the resume did not land.
    pub effort: Option<String>,
}

/// The verdict [`note_running`] records: the lifecycle poll observed the
/// row's session alive in a live terminal.
pub const VERDICT_RUNNING: &str = "running";

impl RowRestore {
    /// The row's session is back: the resume landed (verified by a process
    /// read), or the lifecycle poll observed it alive in a live terminal
    /// ([`VERDICT_RUNNING`]). A Restore that merely FOUND a live terminal or a
    /// live process (`skipped(live_terminal)`, `skipped(live_process(..))`) is
    /// not proof — a pane left by a failed attempt, a `claude` of another
    /// instance — and a pending holder adoption is not back yet either.
    pub fn is_back(&self) -> bool {
        self.verdict == "resumed" || self.verdict == VERDICT_RUNNING
    }

    /// The poll's observation, which the resume core's own verdict for the
    /// same row may replace (it carries the effort read-back).
    fn is_observation(&self) -> bool {
        self.verdict == VERDICT_RUNNING
    }
}

/// Whether a new verdict `next` may be written over `row`'s current one: a
/// row that is not back takes any verdict; a row the poll merely observed
/// running takes the resume core's verdict for it (never another
/// observation); a row whose resume landed takes nothing.
fn may_record(row: &CohortRow, next: &RowRestore) -> bool {
    match &row.restore {
        None => true,
        Some(current) if !current.is_back() => true,
        Some(current) => current.is_observation() && !next.is_observation(),
    }
}

/// Whether `row`'s session is back (see [`RowRestore::is_back`]).
fn restored(row: &CohortRow) -> bool {
    row.restore.as_ref().is_some_and(RowRestore::is_back)
}

/// What one cohort row is resumed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreTarget {
    /// The level to pass as `--effort`, or `None` to let the account's own
    /// default apply ([`effort_to_apply`]).
    pub effort: Option<String>,
    /// Held for a hand restore (D6): only the operator restores it.
    pub held: bool,
    /// The row hit its account's usage limit during the prepare (D3): the
    /// restore keeps it on that account, so the resumed pane's repainted
    /// limit text must not migrate it
    /// ([`crate::terminal::usage_limit::guard_restored_limit_hit`]).
    pub limit_hit: bool,
    /// When that account's exhausted window resets (unix millis), if known.
    pub limit_reset_at: Option<i64>,
}

/// The level a row is resumed at: the effort it RAN at, when that was read
/// and differs from the baseline a fresh session on that account gets anyway.
/// An EXPLICITLY set level (a transcript `/effort` record) is always
/// re-applied, even when it equals the baseline: the baseline may be the CLI
/// default [`effort::CLI_DEFAULT_EFFORT`] while the model's own default differs
/// (a Sonnet session starts at `xhigh` on 2.1.310), so "equal to the baseline"
/// does not prove the resumed session would come back at it. `None` — no flag
/// — when the effort was not read (never a guessed level), or when a level
/// that may only be a default equals the baseline. A baseline that is itself
/// unknown cannot be equal, so a read effort is re-applied rather than
/// dropped.
///
/// Only a canonical level ever comes out ([`effort::rank`]): the value lands
/// in an argv, and a record edited on disk must not be able to put anything
/// else there.
pub fn effort_to_apply(row: &CohortRow) -> Option<String> {
    if row.effort.source == EffortSource::Unknown {
        return None;
    }
    let level = row.effort.level.as_deref()?;
    let canonical = effort::EFFORT_LEVELS.get(effort::rank(level)?)?;
    if row.effort.source.is_explicit() {
        return Some((*canonical).to_string());
    }
    let same_as_baseline = row
        .baseline
        .baseline
        .as_deref()
        .and_then(effort::rank)
        .is_some_and(|b| effort::EFFORT_LEVELS.get(b) == Some(canonical));
    (!same_as_baseline).then(|| (*canonical).to_string())
}

/// The rows of a `restoring` record still waiting to be restored, keyed by
/// Claude session id. A row whose restore already landed is no longer one of
/// them: it is a live session again, and a later crash must auto-restore it
/// like any other rather than turn it back into a placeholder. Nor is a row
/// that is no restore candidate at all
/// ([`CohortRow::restore_candidate`]): the operator closed it during the
/// prepare, so it was not open when the runner restarted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoringCohort {
    pub prepare_id: String,
    targets: BTreeMap<String, RestoreTarget>,
}

impl RestoringCohort {
    /// The restoring cohort of `rec`; `None` unless `rec` is `restoring`. A
    /// prepare still in its window (the runner was not rebuilt), a cancelled
    /// one and a finished one have no cohort to restore.
    pub fn of(rec: &PrepareRecord) -> Option<Self> {
        (rec.state == PrepState::Restoring).then(|| Self {
            prepare_id: rec.prepare_id.clone(),
            targets: rec
                .cohort
                .rows
                .iter()
                .filter(|row| !restored(row) && row.restore_candidate())
                .map(|row| {
                    (
                        row.session.claude_session_id.clone(),
                        RestoreTarget {
                            effort: effort_to_apply(row),
                            held: row.held,
                            limit_hit: row.state_at_prepare
                                == super::cohort::StateAtPrepare::LimitHit,
                            limit_reset_at: row.limit_reset_at,
                        },
                    )
                })
                .collect(),
        })
    }

    pub fn contains(&self, claude_session_id: &str) -> bool {
        self.targets.contains_key(claude_session_id)
    }

    pub fn target(&self, claude_session_id: &str) -> Option<&RestoreTarget> {
        self.targets.get(claude_session_id)
    }

    pub fn ids(&self) -> HashSet<String> {
        self.targets.keys().cloned().collect()
    }
}

/// Who restores a cohort row in this process: the operator (the restore set
/// served it as a placeholder, so the resume core admits it while `open`) or
/// the webview's boot restore (the restore set left it in `sessions`, which
/// the webview auto-resumes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowOwner {
    Operator,
    BootRestore,
}

/// The owner of each cohort row a `placeholders: true` read judged, keyed by
/// `(prepare_id, claude_session_id)`.
///
/// **The rule: a row's FIRST judgment is final for the life of the process.**
/// Each read still judges every row from its own liveness verdict (a row whose
/// terminal runs is the boot restore's, one whose terminal does not is a
/// placeholder), but [`settle_row_owner`] keeps the owner the first judgment
/// recorded, and the restore set builds both lists from the settled owner:
///
/// * a row the boot restore was given is never served later, even once its
///   terminal is gone — it stays in `sessions` for the auto-restore and its
///   own guards, and the resume core refuses it in EVERY state, `closed`
///   included (`boot_restore_owns_row`). When it is not live and the
///   auto-restore cannot take it — outside the restorable set, or its PTY
///   holder was left running unadopted — the restore set lists it under
///   `placeholders` with a blocker instead of in `sessions`: it is seen and
///   counted, and Dismiss releases it to Past Sessions;
/// * a row once served stays the operator's — a Restore that failed can be
///   retried — and while a later read finds it live it is in NEITHER list:
///   not auto-resumed, and not offered (the resume core refuses a live
///   terminal anyway).
///
/// So no row is ever both in a read's `sessions` and admitted by the resume
/// core, across any number of reads. A `placeholders: false` read judges no
/// cohort row and records nothing. Two webview readers pass `placeholders:
/// true` (Phase 7): the boot restore, the one reader that auto-resumes (and
/// only from `sessions`), and the cohort's periodic re-read, which discards
/// `sessions`. A `placeholders: false` reader must never auto-resume.
///
/// A row first judged live is the boot restore's because its session was
/// running when this process first looked — a PTY holder kept it across the
/// restart — so it is no placeholder: the lifecycle poll records it back
/// ([`note_running`]) once its pane is adopted. If the adoption fails and the
/// holder ends, the boot restore's holder-sweep re-run brings it back, exactly
/// as a crash boot would: the session did not stop for the planned restart,
/// it died after it. If the holder is left running unadopted, the row is shown
/// blocked (above).
static ROW_OWNERS: std::sync::Mutex<BTreeMap<(String, String), RowOwner>> =
    std::sync::Mutex::new(BTreeMap::new());

fn row_owners() -> std::sync::MutexGuard<'static, BTreeMap<(String, String), RowOwner>> {
    ROW_OWNERS.lock().unwrap_or_else(|p| p.into_inner())
}

/// Settle the owner of row `claude_session_id` of prepare `prepare_id`: the
/// owner already recorded for it, else `judged`, which is recorded. See
/// `ROW_OWNERS` for the rule.
pub fn settle_row_owner(prepare_id: &str, claude_session_id: &str, judged: RowOwner) -> RowOwner {
    *row_owners()
        .entry((prepare_id.to_string(), claude_session_id.to_string()))
        .or_insert(judged)
}

/// The settled owner of row `claude_session_id` of prepare `prepare_id`, or
/// `None` when no `placeholders: true` read has judged it yet.
pub fn row_owner(prepare_id: &str, claude_session_id: &str) -> Option<RowOwner> {
    row_owners()
        .get(&(prepare_id.to_string(), claude_session_id.to_string()))
        .copied()
}

/// Whether the row `claude_session_id` of prepare `prepare_id` is the
/// operator's: the restore set served it as a placeholder in this process
/// (see `ROW_OWNERS`).
pub fn placeholder_served(prepare_id: &str, claude_session_id: &str) -> bool {
    row_owner(prepare_id, claude_session_id) == Some(RowOwner::Operator)
}

/// The live record's restoring cohort. `None` with no controller (every unit
/// test), no record, or a record that is not `restoring` — the crash-boot and
/// never-prepared paths, which are unchanged.
pub fn restoring_cohort() -> Option<RestoringCohort> {
    let rec = super::prepare::install_live()?.record()?;
    RestoringCohort::of(&rec)
}

/// The ids of `rec`'s cohort rows that the boot AUTO-restore must leave
/// alone, because this process restores them by hand (D6):
///
/// * the rows of a `restoring` record still waiting ([`RestoringCohort`]);
/// * every restore candidate of a record still in its window on a runner
///   that restarted since the prepare (`same_build_restart`, or
///   [`PrepareRecord::restarted_since`]). That record is on its way to
///   `restoring`: the boot step moves it there once the rebuilt runner's
///   release settles, and Release hold on a restarted runner moves it there
///   too — never to `cancelled` — so its rows will be restored by hand. Until
///   then the record itself says so, which closes the gap in which a drain
///   lift folded BEFORE the boot step decided (the boot read, a heartbeat,
///   an expired hold) re-ran the deferred boot restore with no restoring
///   cohort yet and auto-resumed every row at once.
///
/// Empty otherwise — no record, or a prepare whose sessions are still
/// running in this process — so an unplanned crash with no prepare record
/// auto-restores exactly as before.
pub fn withheld_ids_of(rec: &PrepareRecord, same_build_restart: bool) -> HashSet<String> {
    if let Some(cohort) = RestoringCohort::of(rec) {
        return cohort.ids();
    }
    if rec.in_window() && (same_build_restart || rec.restarted_since()) {
        return rec
            .cohort
            .rows
            .iter()
            .filter(|row| row.restore_candidate())
            .map(|row| row.session.claude_session_id.clone())
            .collect();
    }
    HashSet::new()
}

/// [`withheld_ids_of`] on the live record, empty with no controller or
/// record. Read once per lifecycle poll tick by the withheld-boot-restore
/// shield, and by the restore set before a cohort is `restoring`.
pub fn withheld_cohort_ids() -> HashSet<String> {
    let Some(ctl) = super::prepare::install_live() else {
        return HashSet::new();
    };
    ctl.record()
        .map(|rec| withheld_ids_of(&rec, ctl.same_build_restart()))
        .unwrap_or_default()
}

/// Record a restore verdict on the cohort row `claude_session_id` of prepare
/// `prepare_id`, while that record is `restoring`. `Ok(false)` when the record
/// is no longer restoring or carries no such row (nothing written), and when
/// the row may not take it ([`may_record`]) — a later call's stale skip never
/// overwrites a landed restore, and the poll's observation never overwrites
/// the resume core's verdict (with its effort read-back), though that verdict
/// does replace the observation. Once every row is back the record moves to `done`; a row
/// that is never restored (held, Finished) keeps it `restoring` until the
/// cohort is dismissed.
pub fn note_restore_on(
    ctl: &RestartPrep,
    prepare_id: &str,
    claude_session_id: &str,
    restore: RowRestore,
) -> Result<bool, String> {
    let applies = ctl.record().is_some_and(|r| {
        r.prepare_id == prepare_id
            && r.state == PrepState::Restoring
            && r.cohort.rows.iter().any(|row| {
                row.session.claude_session_id == claude_session_id && may_record(row, &restore)
            })
    });
    if !applies {
        return Ok(false);
    }
    ctl.update(prepare_id, |r| {
        if r.state != PrepState::Restoring {
            return;
        }
        if let Some(row) = r
            .cohort
            .rows
            .iter_mut()
            .find(|row| row.session.claude_session_id == claude_session_id)
            .filter(|row| may_record(row, &restore))
        {
            row.restore = Some(restore);
        }
        if r.cohort
            .rows
            .iter()
            .all(|row| restored(row) || !row.restore_candidate())
        {
            r.state = PrepState::Done;
        }
    })
    .map(|_| true)
}

/// Dismiss the restoring cohort `prepare_id`: the record moves to `done`, so
/// no row is a placeholder or held against the orphan close any more — every
/// row still waiting stays in Past Sessions, resumable by hand. Idempotent on
/// a record that is already `done`; errs on any other state or prepare.
///
/// The ONE dismiss, whichever surface asked (the panel, the strip): it also
/// clears the restore-pending marker (`clear_pending`) of every cohort row
/// that is not back. The webview's restore queue stamps that marker before
/// each attempt and clears it only on a landed restore, so a row whose
/// restore failed, was refused or waited on a busy box would otherwise keep
/// it — and a marked record is never orphan-closed by the lifecycle poll,
/// long after the cohort that explained it is gone.
pub fn dismiss_on(
    ctl: &RestartPrep,
    prepare_id: &str,
    clear_pending: &dyn Fn(&str),
) -> Result<PrepareRecord, String> {
    let current = ctl
        .record()
        .filter(|r| r.prepare_id == prepare_id)
        .ok_or_else(|| format!("no restart cohort {prepare_id} to dismiss"))?;
    let rec = match current.state {
        PrepState::Done => current,
        PrepState::Restoring => ctl.update(prepare_id, |r| {
            if r.state == PrepState::Restoring {
                r.state = PrepState::Done;
            }
        })?,
        other => {
            return Err(format!(
                "restart cohort {prepare_id} is {other:?}, not restoring — nothing to dismiss"
            ))
        }
    };
    for row in rec.cohort.rows.iter().filter(|row| !restored(row)) {
        clear_pending(&row.session.claude_session_id);
    }
    Ok(rec)
}

/// Set cohort row `claude_session_id`'s `held` flag by hand (the panel's held
/// toggle, D6): a held row is skipped by every bulk restore and restored only
/// by its own button. Allowed while the prepare is in its window or the cohort
/// is restoring — the two states in which a later restore can read the flag.
/// Marks the row `held_by_operator`, so a re-capture that first reads the
/// row's effort never recomputes the operator's choice. Idempotent; persists
/// and announces `restart-prep-changed` through [`RestartPrep::update`].
///
/// Serialized with the notice tick (`ops`): the tick replaces the whole cohort
/// with the one it re-captured at its start, so a toggle written while it ran
/// would be lost.
pub async fn set_held_on(
    ctl: &RestartPrep,
    prepare_id: &str,
    claude_session_id: &str,
    held: bool,
) -> Result<PrepareRecord, String> {
    let _op = ctl.ops.lock().await;
    let current = ctl
        .record()
        .filter(|r| r.prepare_id == prepare_id)
        .ok_or_else(|| format!("no restart prepare {prepare_id} — nothing to hold"))?;
    if !matches!(
        current.state,
        PrepState::Preparing | PrepState::Ready | PrepState::Restoring
    ) {
        return Err(format!(
            "restart prepare {prepare_id} is {:?} — a held flag no longer means anything",
            current.state
        ));
    }
    if !current
        .cohort
        .rows
        .iter()
        .any(|row| row.session.claude_session_id == claude_session_id)
    {
        return Err(format!(
            "session {claude_session_id} is not in restart prepare {prepare_id}'s cohort"
        ));
    }
    let mut applied = false;
    let rec = ctl.update(prepare_id, |r| {
        // Re-checked on the current record: a dismiss or the last restore
        // verdict does not take `ops`, and may have ended the cohort since.
        if !matches!(
            r.state,
            PrepState::Preparing | PrepState::Ready | PrepState::Restoring
        ) {
            return;
        }
        if let Some(row) = r
            .cohort
            .rows
            .iter_mut()
            .find(|row| row.session.claude_session_id == claude_session_id)
        {
            row.held = held;
            row.held_by_operator = true;
            applied = true;
        }
    })?;
    if applied {
        Ok(rec)
    } else {
        Err(format!(
            "restart prepare {prepare_id} ended while the held flag was being set — nothing \
             changed"
        ))
    }
}

/// Reason recorded on a row un-finished by hand after the notice loop last
/// read it as finished.
pub const UNFINISHED_SINCE_LAST_READ: &str =
    "un-finished by hand since the last read — not re-read yet";

/// The work axis of cohort row `claude_session_id` changed by hand
/// (`terminal_session_set_finished`): carry it onto the row at once. The
/// notice loop re-derives `pause_state` each tick, but only while the prepare
/// is in its window — on a restoring cohort (a placeholder's Finish /
/// Unfinish) nothing re-reads it, so an un-finished row kept reading
/// `pause_state: finished`. An un-finish clears a `finished` pause state to
/// UNKNOWN-until-read (`None`, with [`UNFINISHED_SINCE_LAST_READ`]) rather
/// than guessing what the session is doing now. `Ok(false)` when there is no
/// record, no such row, or nothing changed.
///
/// Serialized with the notice tick (`ops`), as [`set_held_on`] is: the tick
/// replaces the whole cohort with the one it re-captured at its start, so a
/// mark written while it ran would be overwritten by its stale copy.
///
/// `finished` is what the caller set, but two quick toggles spawn two calls
/// that can reach `ops` out of order. So under the lock the lifecycle store's
/// CURRENT mark (`store_finished`) wins whenever it can be read; `finished` is
/// only the fallback for an unreadable store.
pub async fn note_finished_on(
    ctl: &RestartPrep,
    claude_session_id: &str,
    finished: bool,
    store_finished: &(dyn Fn(&str) -> Option<bool> + Send + Sync),
) -> Result<bool, String> {
    let _op = ctl.ops.lock().await;
    let finished = store_finished(claude_session_id).unwrap_or(finished);
    let Some(rec) = ctl.record() else {
        return Ok(false);
    };
    let changes = |row: &CohortRow| {
        row.finished != Some(finished)
            || (!finished && row.pause_state == Some(super::cohort::PauseState::Finished))
    };
    if !rec
        .cohort
        .rows
        .iter()
        .any(|row| row.session.claude_session_id == claude_session_id && changes(row))
    {
        return Ok(false);
    }
    ctl.update(&rec.prepare_id, |r| {
        if let Some(row) = r
            .cohort
            .rows
            .iter_mut()
            .find(|row| row.session.claude_session_id == claude_session_id)
        {
            row.finished = Some(finished);
            if !finished && row.pause_state == Some(super::cohort::PauseState::Finished) {
                row.pause_state = None;
                row.pause_reason = Some(UNFINISHED_SINCE_LAST_READ.to_string());
            }
        }
    })
    .map(|_| true)
}

/// [`note_finished_on`] on the live controller. A failed write is logged: the
/// lifecycle record, which the next re-capture reads, already holds the mark.
pub async fn note_finished(claude_session_id: &str, finished: bool) {
    let Some(ctl) = super::prepare::install_live() else {
        return;
    };
    let store_finished = |id: &str| -> Option<bool> {
        use tauri::Manager;
        let app = crate::tauri_app_handle::current()?;
        let store = app.try_state::<std::sync::Arc<
            crate::session::session_lifecycle_store::SessionLifecycleStore,
        >>()?;
        store.get(id).map(|r| r.finished_at.is_some())
    };
    match note_finished_on(ctl, claude_session_id, finished, &store_finished).await {
        // One line either way, so a row that keeps a stale state after an
        // un-finish says which: never reached, no row / nothing to change, or
        // carried.
        Ok(changed) => tracing::info!(
            session = claude_session_id,
            requested = finished,
            store_finished = ?store_finished(claude_session_id),
            changed,
            "restart_prep: finish mark carried onto the cohort row"
        ),
        Err(e) => warn!(
            session = claude_session_id,
            error = %e,
            "restart_prep: finish mark not carried onto the cohort row"
        ),
    }
}

/// Carry a finish / un-finish made through ANY door onto the restart cohort
/// row: spawned, because it waits for an in-flight notice tick (`ops`). Every
/// door that sets the lifecycle store's finish mark calls this — the Tauri
/// `terminal_session_set_finished` (the placeholder's Finish / Unfinish) and
/// `POST /sessions/{id}/finish` (the agent and CLI door) — so no door leaves
/// a restoring cohort row reading `finished` after an un-finish.
pub fn carry_finish_onto_cohort(claude_session_id: &str, finished: bool) {
    let id = claude_session_id.to_string();
    tauri::async_runtime::spawn(async move {
        note_finished(&id, finished).await;
    });
}

/// [`set_held_on`] on the live controller (the Tauri `restart_prep_set_held`).
pub async fn set_held(
    prepare_id: &str,
    claude_session_id: &str,
    held: bool,
) -> Result<PrepareRecord, String> {
    let ctl =
        super::prepare::install_live().ok_or("restart prepare is not available in this build")?;
    set_held_on(ctl, prepare_id, claude_session_id, held).await
}

/// [`dismiss_on`] on the live controller (the Tauri `restart_prep_dismiss`).
pub fn dismiss(prepare_id: &str, clear_pending: &dyn Fn(&str)) -> Result<PrepareRecord, String> {
    let ctl =
        super::prepare::install_live().ok_or("restart prepare is not available in this build")?;
    dismiss_on(ctl, prepare_id, clear_pending)
}

/// [`note_restore_on`] on the live controller. A failed write is logged: the
/// verdict is also in the resume core's reply and its log line.
pub fn note_restore(
    prepare_id: &str,
    claude_session_id: &str,
    verdict: &str,
    effort_applied: Option<String>,
    effort: Option<String>,
) {
    let Some(ctl) = super::prepare::install_live() else {
        return;
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    let restore = RowRestore {
        at_ms: now_ms,
        at: rfc3339(now_ms),
        verdict: verdict.to_string(),
        effort_applied,
        effort,
    };
    if let Err(e) = note_restore_on(ctl, prepare_id, claude_session_id, restore) {
        warn!(
            session = claude_session_id,
            error = %e,
            "restart_prep: restore verdict not recorded on the cohort row"
        );
    }
}

/// The lifecycle poll observed cohort row `claude_session_id` alive in a live
/// terminal: record it as back ([`VERDICT_RUNNING`]) so it leaves the waiting
/// set. A no-op when there is no restoring cohort, the row is not in it, or it
/// is back already — so a poll tick that sees it again writes nothing.
pub fn note_running(claude_session_id: &str) {
    let Some(ctl) = super::prepare::install_live() else {
        return;
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    let in_flight = crate::session::resume_door::in_flight(claude_session_id);
    if let Err(e) = note_running_on(ctl, claude_session_id, in_flight, now_ms) {
        warn!(
            session = claude_session_id,
            error = %e,
            "restart_prep: running observation not recorded on the cohort row"
        );
    }
}

/// [`note_running`] on `ctl`. `in_flight` is whether a resume of the row is
/// in progress right now: its own verdict, effort read-back included, is
/// the one to record, so the observation of its still-verifying pane is
/// skipped. `Ok(false)` when nothing was written.
pub fn note_running_on(
    ctl: &RestartPrep,
    claude_session_id: &str,
    in_flight: bool,
    now_ms: i64,
) -> Result<bool, String> {
    if in_flight {
        return Ok(false);
    }
    let Some(cohort) = ctl.record().as_ref().and_then(RestoringCohort::of) else {
        return Ok(false);
    };
    if !cohort.contains(claude_session_id) {
        return Ok(false);
    }
    note_restore_on(
        ctl,
        &cohort.prepare_id,
        claude_session_id,
        RowRestore {
            at_ms: now_ms,
            at: rfc3339(now_ms),
            verdict: VERDICT_RUNNING.to_string(),
            effort_applied: None,
            effort: None,
        },
    )
}

/// PURE: what one read of the pane says about the re-applied `wanted` level.
/// `Ok(())` only when the status row shows exactly `wanted`; `Err(None)` when
/// no indicator is on screen yet (keep looking); `Err(Some(seen))` when it
/// shows another level.
pub fn judge_effort(wanted: &str, screen: &ScreenText) -> Result<(), Option<&'static str>> {
    match effort::parse_screen(&screen.lines) {
        Some(seen) if effort::rank(seen) == effort::rank(wanted) => Ok(()),
        Some(seen) => Err(Some(seen)),
        None => Err(None),
    }
}

/// Read the resumed pane's status row until it confirms `wanted` or
/// `deadline` passes. `read` returns the pane's grid, or `None` when the pane
/// is gone. The answer is [`EFFORT_RESTORED`] or
/// `effort_not_restored(<reason>)`, where the reason is `pane_vanished`,
/// `shows_<level>` (the last level seen, when it was another one), or
/// `indicator_not_seen`.
pub async fn read_back_effort(
    wanted: &str,
    mut read: impl FnMut() -> Option<ScreenText>,
    deadline: Duration,
    poll: Duration,
) -> String {
    let until = tokio::time::Instant::now() + deadline;
    let mut last_other: Option<&'static str> = None;
    loop {
        let Some(screen) = read() else {
            return effort_not_restored("pane_vanished");
        };
        match judge_effort(wanted, &screen) {
            Ok(()) => return EFFORT_RESTORED.to_string(),
            Err(Some(other)) => last_other = Some(other),
            Err(None) => {}
        }
        if tokio::time::Instant::now() >= until {
            return match last_other {
                Some(other) => effort_not_restored(&format!("shows_{other}")),
                None => effort_not_restored("indicator_not_seen"),
            };
        }
        tokio::time::sleep(poll).await;
    }
}

/// A `restoring` cohort over `ids` (prepare `prep-test`), each row captured at
/// `max` effort off its pane on the CLI-default `high` baseline — so each is
/// re-applied as `--effort max` — for the tests of the resume core and the
/// boot side.
#[cfg(test)]
pub(crate) fn restoring_cohort_for_test(ids: &[&str]) -> RestoringCohort {
    use super::effort::Effort;
    let ctl_dir = tempfile::tempdir().expect("tempdir");
    let mut rec = super::prepare::active_controller_for_test(ctl_dir.path(), ids, 0)
        .record()
        .expect("seeded record");
    for row in &mut rec.cohort.rows {
        row.effort = Effort {
            level: Some("max".to_string()),
            source: EffortSource::Screen,
        };
    }
    rec.state = PrepState::Restoring;
    RestoringCohort::of(&rec).expect("a restoring record has a cohort")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::restart_prep::cohort::{build_row, Cohort, RowFacts};
    use crate::restart_prep::effort::{Baseline, DefaultSource, Effort};
    use crate::restart_prep::prepare::active_controller_for_test;
    use crate::session::session_ledger::LedgerEntry;

    fn entry(id: &str) -> LedgerEntry {
        LedgerEntry {
            claude_session_id: id.to_string(),
            terminal_id: format!("term-{id}"),
            page_id: "page-a".to_string(),
            zone_index: 1,
            title: None,
            session_name: None,
            account_label: None,
            config_dir: None,
            working_dir: None,
            restorable: None,
            worktree_path: None,
            plan_slug: None,
            work_unit_id: None,
            wip_state: None,
            wip_ref: None,
            custody_session_mismatch: false,
            name_source: None,
            resume_config_dir: None,
            resume_command: None,
            provider: None,
            last_seen_at: None,
            finished: false,
        }
    }

    fn row_with(level: Option<&str>, source: EffortSource, baseline: Option<&str>) -> CohortRow {
        let mut row = build_row(entry("s"), RowFacts::default());
        row.effort = Effort {
            level: level.map(str::to_string),
            source,
        };
        row.baseline = Baseline {
            baseline: baseline.map(str::to_string),
            default_source: if baseline.is_some() {
                DefaultSource::CliDefault
            } else {
                DefaultSource::Unknown
            },
        };
        row
    }

    fn screen(lines: &[&str]) -> ScreenText {
        ScreenText {
            lines: lines.iter().map(|l| l.to_string()).collect(),
            cursor_row: u16::try_from(lines.len().saturating_sub(1)).unwrap_or(0),
            cursor_col: 2,
        }
    }

    #[test]
    fn effort_is_re_applied_only_when_read_and_different_from_the_baseline() {
        use EffortSource::*;
        let cases: &[(Option<&str>, EffortSource, Option<&str>, Option<&str>)] = &[
            // Above the baseline: re-applied.
            (Some("max"), Screen, Some("high"), Some("max")),
            // Below it too — the session comes back as it ran.
            (Some("low"), Screen, Some("high"), Some("low")),
            // Equal to the baseline: the account default brings it back.
            (Some("high"), Screen, Some("high"), None),
            (Some("HIGH"), Screen, Some("high"), None),
            // The account's own default, read from settings: equal by definition.
            (Some("medium"), AccountDefault, Some("medium"), None),
            // An unknown baseline cannot be equal: the read level is re-applied.
            (Some("xhigh"), Screen, None, Some("xhigh")),
            // An unread effort is never guessed.
            (None, Unknown, Some("high"), None),
            (Some("max"), Unknown, Some("high"), None),
            // Not a level at all: nothing reaches the argv.
            (
                Some("--dangerously-skip-permissions"),
                Screen,
                Some("high"),
                None,
            ),
            (
                Some("--dangerously-skip-permissions"),
                Transcript,
                Some("high"),
                None,
            ),
            // An explicit `/effort` from the transcript: re-applied even when
            // it equals the baseline — the CLI-default baseline is not the
            // model's own default (Sonnet starts at xhigh on 2.1.310).
            (Some("high"), Transcript, Some("high"), Some("high")),
            (Some("max"), Transcript, Some("high"), Some("max")),
            (Some("low"), Transcript, None, Some("low")),
        ];
        for (level, source, baseline, want) in cases {
            let row = row_with(*level, *source, *baseline);
            assert_eq!(
                effort_to_apply(&row).as_deref(),
                *want,
                "{level:?} from {source:?} over baseline {baseline:?}"
            );
        }
    }

    /// The body of the function `fn_sig` in `src`, up to its closing `\n}\n`.
    fn fn_body<'a>(src: &'a str, fn_sig: &str) -> &'a str {
        let (_, rest) = src.split_once(fn_sig).expect("function present");
        rest.split_once("\n}\n").expect("function ends").0
    }

    /// E2E re-run: an un-finish after the restart left the cohort row at
    /// `pause_state: finished`. Every door that sets the lifecycle finish mark
    /// must carry it onto the cohort row — the placeholder's Tauri command and
    /// the HTTP finish route alike (they need a running app, so this reads
    /// their source).
    #[test]
    fn every_finish_door_carries_the_mark_onto_the_cohort_row() {
        let tauri_door = fn_body(
            include_str!("../commands/terminal.rs"),
            "pub fn terminal_session_set_finished(",
        );
        let http_door = fn_body(
            include_str!("../mcp/sessions.rs"),
            "async fn finish_session(",
        );
        for (name, body) in [("tauri", tauri_door), ("http", http_door)] {
            assert!(
                body.contains("carry_finish_onto_cohort("),
                "the {name} finish door does not carry the mark onto the cohort row"
            );
        }
    }

    /// The lifecycle store could not be read: the caller's value stands.
    fn no_store(_: &str) -> Option<bool> {
        None
    }

    /// Re-review finding 2: two quick toggles (finish, then un-finish) spawn
    /// two calls that can take `ops` out of order. The late, stale "finish"
    /// must not leave the row finished while the store says it is not.
    #[tokio::test]
    async fn out_of_order_toggles_end_on_the_stores_current_mark() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = active_controller_for_test(dir.path(), &["s1"], 0);
        // The store's mark after both toggles: NOT finished.
        let store_now = |_: &str| Some(false);
        // The un-finish lands first, then the stale finish.
        note_finished_on(&ctl, "s1", false, &store_now)
            .await
            .unwrap();
        note_finished_on(&ctl, "s1", true, &store_now)
            .await
            .unwrap();
        assert_eq!(ctl.record().unwrap().cohort.rows[0].finished, Some(false));
    }

    #[tokio::test]
    async fn an_unfinish_clears_a_stale_finished_pause_state() {
        use crate::restart_prep::cohort::PauseState;
        let dir = tempfile::tempdir().unwrap();
        let ctl = active_controller_for_test(dir.path(), &["s1", "s2"], 0);
        let prepare_id = ctl.record().unwrap().prepare_id;
        ctl.update(&prepare_id, |r| {
            r.state = PrepState::Restoring;
            for row in &mut r.cohort.rows {
                row.finished = Some(true);
                row.pause_state = Some(PauseState::Finished);
            }
        })
        .unwrap();
        assert_eq!(
            note_finished_on(&ctl, "s1", false, &no_store).await,
            Ok(true)
        );
        let rec = ctl.record().unwrap();
        let s1 = &rec.cohort.rows[0];
        assert_eq!(s1.finished, Some(false));
        assert_eq!(s1.pause_state, None, "not `finished` any more");
        assert_eq!(s1.pause_reason.as_deref(), Some(UNFINISHED_SINCE_LAST_READ));
        // The other row is untouched; a repeat or an unknown row writes nothing.
        assert_eq!(rec.cohort.rows[1].pause_state, Some(PauseState::Finished));
        assert_eq!(
            note_finished_on(&ctl, "s1", false, &no_store).await,
            Ok(false)
        );
        assert_eq!(
            note_finished_on(&ctl, "nope", false, &no_store).await,
            Ok(false)
        );
        // Finishing again records the mark; the tick derives the state.
        assert_eq!(
            note_finished_on(&ctl, "s1", true, &no_store).await,
            Ok(true)
        );
        assert_eq!(ctl.record().unwrap().cohort.rows[0].finished, Some(true));
    }

    /// A notice tick holds `ops` while it re-captures and replaces the whole
    /// cohort: the finish mark must wait for it, not race it.
    #[tokio::test]
    async fn a_finish_mark_waits_for_an_in_flight_notice_tick() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = active_controller_for_test(dir.path(), &["s1"], 0);
        let tick = ctl.ops.lock().await;
        let blocked = tokio::time::timeout(
            Duration::from_millis(50),
            note_finished_on(&ctl, "s1", true, &no_store),
        )
        .await;
        assert!(blocked.is_err(), "wrote while the tick held ops");
        assert_ne!(ctl.record().unwrap().cohort.rows[0].finished, Some(true));
        drop(tick);
        assert_eq!(
            note_finished_on(&ctl, "s1", true, &no_store).await,
            Ok(true)
        );
        assert_eq!(ctl.record().unwrap().cohort.rows[0].finished, Some(true));
    }

    #[test]
    fn argv_effort_confirms_a_level_the_screen_no_longer_shows() {
        let cmd = "claude --resume 1512e91d --effort max --model claude-opus-5-5";
        assert!(argv_carries_effort(cmd, "max"));
        assert!(argv_carries_effort(cmd, "MAX"));
        assert!(!argv_carries_effort(cmd, "high"));
        assert!(argv_carries_effort("claude --effort=xhigh -r x", "xhigh"));
        assert!(!argv_carries_effort("claude --resume x", "max"));
        assert!(!argv_carries_effort("claude --resume x --effort", "max"));
        assert!(!argv_carries_effort(cmd, "turbo"));
        let unseen = effort_not_restored("indicator_not_seen");
        assert_eq!(
            effort_verdict_with_argv(unseen.clone(), true),
            EFFORT_RESTORED_BY_ARGV
        );
        assert_eq!(effort_verdict_with_argv(unseen.clone(), false), unseen);
        // The screen showing ANOTHER level is never overruled by the argv.
        let other = effort_not_restored("shows_high");
        assert_eq!(effort_verdict_with_argv(other.clone(), true), other);
        assert_eq!(
            effort_verdict_with_argv(EFFORT_RESTORED.to_string(), true),
            EFFORT_RESTORED
        );
    }

    fn record(state: PrepState, ids: &[&str]) -> PrepareRecord {
        let rows = ids
            .iter()
            .map(|id| {
                let mut r = build_row(entry(id), RowFacts::default());
                r.effort = Effort {
                    level: Some("max".to_string()),
                    source: EffortSource::Screen,
                };
                r
            })
            .collect();
        let mut rec = PrepareRecord::new(
            "prep-r".to_string(),
            1_000,
            Cohort {
                captured_at_ms: 1_000,
                captured_at: String::new(),
                reason: "restart-prep".to_string(),
                rows,
            },
        );
        rec.state = state;
        rec
    }

    #[test]
    fn only_a_restoring_record_has_a_cohort_to_restore() {
        for state in [
            PrepState::Preparing,
            PrepState::Ready,
            PrepState::Done,
            PrepState::Cancelled,
        ] {
            assert_eq!(
                RestoringCohort::of(&record(state, &["a"])),
                None,
                "{state:?}"
            );
        }
        let c = RestoringCohort::of(&record(PrepState::Restoring, &["a", "b"])).unwrap();
        assert!(c.contains("a") && c.contains("b") && !c.contains("z"));
        assert_eq!(c.ids().len(), 2);
        assert_eq!(c.target("a").unwrap().effort.as_deref(), Some("max"));
        // Zero rows is a cohort with nothing in it, not an absent one.
        let empty = RestoringCohort::of(&record(PrepState::Restoring, &[])).unwrap();
        assert!(empty.ids().is_empty());
    }

    /// Finding 3 (review): from boot until the boot step decides, a prepared
    /// record still in its window on a RESTARTED runner withholds its rows from
    /// the auto-restore — derived from the record itself, so a drain lift
    /// folded before the boot step cannot hand them to the boot restore. A
    /// prepare whose sessions still run here (no restart) and no record at all
    /// (an unplanned crash) withhold nothing.
    #[test]
    fn a_restarted_runners_prepare_withholds_its_rows_before_it_restores() {
        let mut rec = record(PrepState::Preparing, &["a", "b"]);
        assert!(
            withheld_ids_of(&rec, false).is_empty(),
            "not restarted: the sessions still run here"
        );
        let both: HashSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        assert_eq!(withheld_ids_of(&rec, true), both, "a same-build restart");
        rec.prepared_build = "an-older-build".to_string();
        assert_eq!(
            withheld_ids_of(&rec, false),
            both,
            "a rebuild, before the boot step"
        );
        rec.state = PrepState::Ready;
        assert_eq!(withheld_ids_of(&rec, false), both);
        rec.hold_until_ms = Some(0);
        assert_eq!(
            withheld_ids_of(&rec, false),
            both,
            "an expired hold withholds too"
        );
        rec.state = PrepState::Restoring;
        rec.cohort.rows[0].restore = Some(RowRestore {
            at_ms: 0,
            at: String::new(),
            verdict: "resumed".to_string(),
            effort_applied: None,
            effort: None,
        });
        assert_eq!(
            withheld_ids_of(&rec, false),
            ["b".to_string()].into_iter().collect(),
            "restoring: the rows still waiting"
        );
        for state in [PrepState::Done, PrepState::Cancelled] {
            rec.state = state;
            assert!(withheld_ids_of(&rec, true).is_empty(), "{state:?}");
        }
    }

    /// Finding 6 (review): a row the operator closed during the prepare is no
    /// restore candidate — not waiting, not withheld — and a cohort whose only
    /// rows left are such rows is done.
    #[test]
    fn a_row_closed_by_the_user_during_the_prepare_is_not_restored() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = active_controller_for_test(dir.path(), &["kept", "closed"], 1_000);
        let prepare_id = ctl.record().unwrap().prepare_id;
        ctl.update(&prepare_id, |r| {
            r.state = PrepState::Restoring;
            let closed = r
                .cohort
                .rows
                .iter_mut()
                .find(|row| row.session.claude_session_id == "closed")
                .unwrap();
            closed.left_capture_at = Some(1_500);
            // What the tab close button writes.
            closed.left_capture_reason = Some("explicit".to_string());
        })
        .unwrap();
        let rec = ctl.record().unwrap();
        let waiting = RestoringCohort::of(&rec).unwrap();
        assert!(waiting.contains("kept") && !waiting.contains("closed"));
        assert!(!withheld_ids_of(&rec, false).contains("closed"));
        assert_eq!(rec.cohort.rows.len(), 2, "the row stays recorded");
        note_restore_on(
            &ctl,
            &prepare_id,
            "kept",
            RowRestore {
                at_ms: 2_000,
                at: String::new(),
                verdict: "resumed".to_string(),
                effort_applied: None,
                effort: None,
            },
        )
        .unwrap();
        assert_eq!(ctl.record().unwrap().state, PrepState::Done);
    }

    #[test]
    fn with_no_live_controller_there_is_no_restoring_cohort() {
        // The crash-boot / never-prepared path: a unit-test build installs no
        // controller, exactly like a runner that has no record.
        assert_eq!(restoring_cohort(), None);
        assert!(withheld_cohort_ids().is_empty());
    }

    #[test]
    fn a_row_is_back_when_resumed_running_or_found_running_and_not_otherwise() {
        let r = |v: &str| RowRestore {
            at_ms: 0,
            at: String::new(),
            verdict: v.to_string(),
            effort_applied: None,
            effort: None,
        };
        for back in ["resumed", VERDICT_RUNNING] {
            assert!(r(back).is_back(), "{back}");
        }
        for waiting in [
            "skipped(live_terminal)",
            "skipped(live_process(pid 7))",
            "skipped(holder_adoption_pending)",
            "skipped(resource_guard:thread_count)",
            "failed(no_claude_resume_process_observed)",
            "skipped(held_for_hand_restore)",
        ] {
            assert!(!r(waiting).is_back(), "{waiting}");
        }
        // A row seen running leaves the waiting set; one merely found with a
        // live terminal does not.
        let mut rec = record(PrepState::Restoring, &["a", "b"]);
        rec.cohort.rows[0].restore = Some(r(VERDICT_RUNNING));
        rec.cohort.rows[1].restore = Some(r("skipped(live_terminal)"));
        let waiting = RestoringCohort::of(&rec).unwrap();
        assert!(!waiting.contains("a") && waiting.contains("b"));
    }

    #[test]
    fn dismissing_a_restoring_cohort_lifts_every_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = active_controller_for_test(dir.path(), &["s1", "s2"], 1_000);
        let prepare_id = ctl.record().unwrap().prepare_id;
        let cleared = std::sync::Mutex::new(Vec::<String>::new());
        let clear = |id: &str| cleared.lock().unwrap().push(id.to_string());
        // Still preparing: there is no restore to dismiss.
        assert!(dismiss_on(&ctl, &prepare_id, &clear).is_err());
        ctl.update(&prepare_id, |r| r.state = PrepState::Restoring)
            .unwrap();
        assert!(dismiss_on(&ctl, "another-prepare", &clear).is_err());
        assert!(
            cleared.lock().unwrap().is_empty(),
            "a refused dismiss clears nothing"
        );
        // s1 came back; s2 is still waiting (its restore failed, say).
        note_restore_on(
            &ctl,
            &prepare_id,
            "s1",
            RowRestore {
                at_ms: 2_000,
                at: String::new(),
                verdict: "resumed".to_string(),
                effort_applied: None,
                effort: None,
            },
        )
        .unwrap();
        let done = dismiss_on(&ctl, &prepare_id, &clear).unwrap();
        assert_eq!(done.state, PrepState::Done);
        // Finding 4 (review): the dismiss itself clears the restore-pending
        // marker of every row not back, whichever surface asked for it.
        assert_eq!(cleared.lock().unwrap().as_slice(), ["s2"]);
        assert_eq!(
            RestoringCohort::of(&done),
            None,
            "no row is waiting any more"
        );
        // Idempotent.
        assert_eq!(
            dismiss_on(&ctl, &prepare_id, &|_| {}).unwrap().state,
            PrepState::Done
        );
        assert_eq!(
            crate::restart_prep::load_from(&dir.path().join("restart-prep.json"))
                .unwrap()
                .state,
            PrepState::Done
        );
    }

    #[test]
    fn the_polls_observation_never_pre_empts_the_resume_cores_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = active_controller_for_test(dir.path(), &["s1", "s2"], 1_000);
        let prepare_id = ctl.record().unwrap().prepare_id;
        // Not restoring yet: the observation writes nothing.
        assert_eq!(note_running_on(&ctl, "s1", false, 2), Ok(false));
        ctl.update(&prepare_id, |r| r.state = PrepState::Restoring)
            .unwrap();
        // A resume of s1 is in flight: its own verdict is the one to keep.
        assert_eq!(note_running_on(&ctl, "s1", true, 2), Ok(false));
        // The poll saw s1 alive first (the new pane registered before the
        // read-back finished): recorded, and the row is back...
        assert_eq!(note_running_on(&ctl, "s1", false, 3), Ok(true));
        assert_eq!(
            note_running_on(&ctl, "s1", false, 4),
            Ok(false),
            "once only"
        );
        // ...yet the resume core's verdict, effort read-back included, still
        // replaces the observation.
        let verdict = RowRestore {
            at_ms: 5,
            at: rfc3339(5),
            verdict: "resumed".to_string(),
            effort_applied: Some("max".to_string()),
            effort: Some(effort_not_restored("indicator_not_seen")),
        };
        assert_eq!(
            note_restore_on(&ctl, &prepare_id, "s1", verdict.clone()),
            Ok(true)
        );
        let row = ctl.record().unwrap().cohort.rows[0].clone();
        assert_eq!(row.restore, Some(verdict));
        // And a later observation never overwrites it.
        assert_eq!(note_running_on(&ctl, "s1", false, 6), Ok(false));
        assert_eq!(
            ctl.record().unwrap().cohort.rows[0]
                .restore
                .as_ref()
                .and_then(|r| r.effort.clone()),
            Some("effort_not_restored(indicator_not_seen)".to_string())
        );
        // A row nobody restored is outside nothing: s2 is still waiting.
        assert!(RestoringCohort::of(&ctl.record().unwrap())
            .unwrap()
            .contains("s2"));
    }

    #[test]
    fn judge_effort_reads_the_status_row() {
        let shows = |l: &str| screen(&["transcript", l, "────", "❯\u{a0}"]);
        assert_eq!(judge_effort("max", &shows("◉ max · /effort")), Ok(()));
        assert_eq!(
            judge_effort("max", &shows("● high · /effort")),
            Err(Some("high"))
        );
        assert_eq!(judge_effort("max", &shows("● max · /eff")), Err(None));
        assert_eq!(judge_effort("max", &screen(&["❯ "])), Err(None));
    }

    #[tokio::test]
    async fn the_read_back_confirms_the_level_once_it_is_painted() {
        let mut reads = vec![
            screen(&["starting…"]),
            screen(&["● high · /effort"]),
            screen(&["◉ max · /effort"]),
        ]
        .into_iter();
        let got = read_back_effort(
            "max",
            move || reads.next(),
            Duration::from_secs(5),
            Duration::from_millis(1),
        )
        .await;
        assert_eq!(got, EFFORT_RESTORED);
    }

    #[tokio::test]
    async fn the_read_back_names_why_it_could_not_confirm() {
        // Another level stays on screen until the deadline.
        let other = read_back_effort(
            "max",
            || Some(screen(&["● high · /effort"])),
            Duration::from_millis(20),
            Duration::from_millis(1),
        )
        .await;
        assert_eq!(other, "effort_not_restored(shows_high)");
        // No indicator ever painted.
        let unseen = read_back_effort(
            "max",
            || Some(screen(&["❯ "])),
            Duration::from_millis(20),
            Duration::from_millis(1),
        )
        .await;
        assert_eq!(unseen, "effort_not_restored(indicator_not_seen)");
        // The pane went away.
        let gone = read_back_effort(
            "max",
            || None,
            Duration::from_secs(5),
            Duration::from_millis(1),
        )
        .await;
        assert_eq!(gone, "effort_not_restored(pane_vanished)");
    }

    #[test]
    fn a_restore_verdict_lands_on_the_row_only_while_restoring() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = active_controller_for_test(dir.path(), &["s1", "s2"], 1_000);
        let prepare_id = ctl.record().unwrap().prepare_id;
        let verdict = |v: &str| RowRestore {
            at_ms: 5,
            at: rfc3339(5),
            verdict: v.to_string(),
            effort_applied: Some("max".to_string()),
            effort: Some(EFFORT_RESTORED.to_string()),
        };
        // Still preparing: nothing is restored yet.
        assert_eq!(
            note_restore_on(&ctl, &prepare_id, "s1", verdict("resumed")),
            Ok(false)
        );
        ctl.update(&prepare_id, |r| r.state = PrepState::Restoring)
            .unwrap();
        assert_eq!(
            note_restore_on(&ctl, &prepare_id, "s1", verdict("resumed")),
            Ok(true)
        );
        assert_eq!(
            note_restore_on(&ctl, &prepare_id, "not-a-row", verdict("resumed")),
            Ok(false)
        );
        assert_eq!(
            note_restore_on(&ctl, "another-prepare", "s1", verdict("resumed")),
            Ok(false)
        );
        let rec = ctl.record().unwrap();
        assert_eq!(
            rec.cohort.rows[0]
                .restore
                .as_ref()
                .map(|r| r.verdict.as_str()),
            Some("resumed")
        );
        assert_eq!(rec.cohort.rows[1].restore, None);
        // Persisted: the record on disk carries it.
        let on_disk = crate::restart_prep::load_from(&dir.path().join("restart-prep.json"))
            .expect("record on disk");
        assert_eq!(on_disk.cohort.rows[0].restore, rec.cohort.rows[0].restore);
        // A later call's stale verdict never overwrites a landed restore.
        assert_eq!(
            note_restore_on(&ctl, &prepare_id, "s1", verdict("skipped(not_closed)")),
            Ok(false)
        );
        assert_eq!(
            ctl.record().unwrap().cohort.rows[0]
                .restore
                .as_ref()
                .map(|r| r.verdict.as_str()),
            Some("resumed")
        );
        let rec = ctl.record().unwrap();
        // A restored row leaves the cohort still waiting; the other stays.
        let waiting = RestoringCohort::of(&rec).expect("still restoring");
        assert!(!waiting.contains("s1") && waiting.contains("s2"));
        // A skip is not a restore: the record keeps restoring.
        note_restore_on(
            &ctl,
            &prepare_id,
            "s2",
            verdict("skipped(resource_guard:thread_count)"),
        )
        .unwrap();
        assert_eq!(ctl.record().unwrap().state, PrepState::Restoring);
        assert!(RestoringCohort::of(&ctl.record().unwrap())
            .unwrap()
            .contains("s2"));
        // The last row restored: the cohort is done.
        note_restore_on(&ctl, &prepare_id, "s2", verdict("resumed")).unwrap();
        assert_eq!(ctl.record().unwrap().state, PrepState::Done);
        assert_eq!(RestoringCohort::of(&ctl.record().unwrap()), None);
    }

    /// The held toggle round-trips: set, persisted, announced, read back by
    /// the restore set; set back, the same. Repeating it is idempotent.
    #[tokio::test]
    async fn restart_prep_the_held_toggle_round_trips_and_is_announced() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("restart-prep.json");
        let prepare_id = active_controller_for_test(dir.path(), &["s1", "s2"], 1_000)
            .record()
            .unwrap()
            .prepare_id;
        let seen = Arc::new(AtomicUsize::new(0));
        let held_seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (seen2, held2) = (seen.clone(), held_seen.clone());
        let ctl = RestartPrep::load(
            path.clone(),
            Some(Box::new(move |c: &super::super::prepare::PrepChanged| {
                seen2.fetch_add(1, Ordering::SeqCst);
                held2.lock().unwrap().push(c.record.cohort.rows[0].held);
            })),
        );
        assert!(
            !ctl.record().unwrap().cohort.rows[0].held,
            "unknown effort: not held"
        );

        let rec = set_held_on(&ctl, &prepare_id, "s1", true).await.unwrap();
        assert!(rec.cohort.rows[0].held && rec.cohort.rows[0].held_by_operator);
        assert!(
            !rec.cohort.rows[1].held && !rec.cohort.rows[1].held_by_operator,
            "only s1"
        );
        let on_disk = crate::restart_prep::load_from(&path).unwrap();
        assert!(on_disk.cohort.rows[0].held, "persisted");

        // Once restoring, the restore set reads the operator's flag.
        ctl.update(&prepare_id, |r| r.state = PrepState::Restoring)
            .unwrap();
        let cohort = RestoringCohort::of(&ctl.record().unwrap()).unwrap();
        assert!(cohort.target("s1").unwrap().held);
        // Released again — still allowed while restoring.
        set_held_on(&ctl, &prepare_id, "s1", false).await.unwrap();
        let cohort = RestoringCohort::of(&ctl.record().unwrap()).unwrap();
        assert!(!cohort.target("s1").unwrap().held);
        assert!(!crate::restart_prep::load_from(&path).unwrap().cohort.rows[0].held);
        // Idempotent.
        let again = set_held_on(&ctl, &prepare_id, "s1", false).await.unwrap();
        assert!(!again.cohort.rows[0].held);

        assert_eq!(seen.load(Ordering::SeqCst), 4, "every write is announced");
        assert_eq!(*held_seen.lock().unwrap(), [true, true, false, false]);
    }

    #[tokio::test]
    async fn restart_prep_the_held_toggle_refuses_what_it_cannot_mean() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = active_controller_for_test(dir.path(), &["s1"], 1_000);
        let prepare_id = ctl.record().unwrap().prepare_id;
        let before = ctl.record().unwrap();
        assert!(set_held_on(&ctl, "another-prepare", "s1", true)
            .await
            .is_err());
        assert!(set_held_on(&ctl, &prepare_id, "not-in-cohort", true)
            .await
            .is_err());
        assert_eq!(ctl.record().unwrap(), before, "a refusal writes nothing");
        for state in [PrepState::Cancelled, PrepState::Done] {
            ctl.update(&prepare_id, |r| r.state = state).unwrap();
            let err = set_held_on(&ctl, &prepare_id, "s1", true)
                .await
                .unwrap_err();
            assert!(err.contains("no longer means anything"), "{state:?}: {err}");
            assert!(!ctl.record().unwrap().cohort.rows[0].held);
        }
    }
}
