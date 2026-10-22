//! `POST /control/sessions/resume {ids | all: true, generation?}` — the runner's own door
//! for re-launching CLOSED sessions on THIS device.
//!
//! Plan `2026-10-06-closed-sessions-whose-work-is-unfinished-are-found-fleet-wide-and-resumed`
//! Phase 4. It is the respawn receiver's materialise flow
//! ([`super::respawn`]) with a LOCAL source: the transcript is already under the
//! row's own `CLAUDE_CONFIG_DIR`, so there is no coord fetch, no handoff bundle
//! and no account re-pin — the session comes back under the account it ran on,
//! in the cwd it ran in. The flow ends in the same shared seam the receiver
//! uses, [`crate::terminal::account_migration::spawn_resumed_pane`] (an argv
//! spawn, so no typed-command retry exists to fail).
//!
//! It is a backend route, deliberately independent of any mounted React
//! frontend (an HTTP door must work on a headless runner).
//!
//! ## It is a credential door
//!
//! The route spawns `claude` processes with the user's account on the user's
//! behalf, so it is on `origin_guard::CREDENTIAL_DOORS`: a browser origin the
//! guard does not trust never reaches it. Inside the handler the request must
//! also be unambiguous — a JSON body naming non-empty `ids`, or the explicit
//! flag `all: true`. An empty or bare body is a 400, never "everything", and a
//! non-JSON content type is a 415, so a cross-site form POST (which can send
//! neither) cannot trigger a fleet-wide resume.
//!
//! ## Why a call cannot double-spawn
//!
//! Three independent guards sit between a request and a spawn: a per-session
//! in-flight set (two concurrent calls naming one id: the second is
//! `skipped(in_flight)`), a process-table read that refuses an id some live
//! `claude` already holds (`skipped(live_process)`), and the batch itself runs
//! in a DETACHED task, so a caller that disconnects mid-batch neither cancels
//! the remaining ids nor leaves a half-verified spawn behind — every id still
//! gets its verdict, in the log if nobody is waiting for the response.
//!
//! ## Per-id flow ([`resume_one`])
//!
//! 0. no other call is already resuming the id, and no live `claude` process
//!    already holds it ([`InFlight`], [`live_holder`]);
//! 1. the id is well-formed, has a lifecycle row, the row is `closed`, not
//!    marked finished, owned by the claude provider, with a known config dir and
//!    a cwd that still exists;
//! 2. the transcript exists locally — `--resume` against nothing would start an
//!    EMPTY conversation wearing the old id, the fabricated half this plan
//!    forbids;
//! 3. the device drain gate, as [`SpawnOrigin::Respawn`], deferred (never
//!    dropped) while the device is drained;
//! 4. the per-session account-hop budget, `MIGRATION_CAP`, shared with the
//!    automatic migration and the respawn receiver (checked last, immediately
//!    before the spawn, because the check consumes a slot);
//! 5. spawn, then VERIFY BY READING THE PROCESS: exactly one `claude --resume
//!    <id>` under the new pane, whose cwd and `CLAUDE_CONFIG_DIR` (where the
//!    platform exposes them) are the ones the row recorded.
//!
//! Every id gets exactly one verdict: `resumed`, `failed(<reason>)` or
//! `skipped(<reason>)`. A refusal before the spawn is a skip; anything that
//! went wrong at or after the spawn is a failure.
//!
//! ## `generation`
//!
//! The rebuild plan's ledger-generation restore shares this route. The ledger
//! carries no generation concept today ([`super::session_ledger`] has none), so
//! the field is ACCEPTED, logged and echoed back, and filters nothing. It is
//! not silently ignored: the report says `generation_applied: false`.
//!
//! The admission-lease path (plan 2026-09-13 spawn-admission) is not landed
//! here; once it is, a mid-batch refusal marks the rest `skipped(admission)`.
//!
//! ## A prepared cohort (plan `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, D7)
//!
//! While the operator restart prepare's record is `restoring`
//! ([`crate::restart_prep::restore::RestoringCohort`]), the core is widened in
//! one way: a cohort row that is `open` but has NO live terminal here — the
//! shape every cohort row has after a restart; a pane whose process exited
//! does not count as live — is admitted alongside `closed`, once the restore
//! set has served THAT row as a placeholder. Until it has, the webview's boot
//! restore owns the row (`skipped(boot_restore_owns_row)`): the restore set
//! left it in `sessions`, and the boot restore may be starting a `claude` the
//! live-process guard cannot see yet. A row's owner is settled by the first
//! read that judged it and never changes after (see `ROW_OWNERS` in
//! [`crate::restart_prep::restore`]), so a row once handed to the boot
//! restore is never admitted here while open.
//! An open row outside the cohort is still `skipped(not_closed)`, an
//! open cohort row whose terminal IS live is `skipped(live_terminal)` — or
//! `skipped(holder_adoption_pending | holder_sweep_pending)` while a PTY
//! holder that kept its pane alive across the restart may still be adopted —
//! and the in-flight and live-process guards are unchanged. A row held for a
//! hand restore is the operator's only (`skipped(held_for_hand_restore)` for
//! an autonomous caller), and only the operator's Restore of a finished
//! cohort row is admitted; its finish mark is lifted once the resume is
//! verified, so a refused or failed Restore leaves it untouched. A cohort row is resumed at
//! the effort it ran at (`--effort <level>`, when that differs from its
//! account's baseline) and the verdict, with the effort read-back, is
//! recorded on the cohort row.
//!
//! ## Origin
//!
//! The core gates on the [`SpawnOrigin`] its caller hands it. This HTTP route
//! passes [`HTTP_ORIGIN`] (`Respawn`, autonomous: deferred while the device is
//! drained); the runner UI's operator twin
//! (`commands::operator_doors::operator_resume_sessions`) passes the operator
//! origin, so a residual drain never defers an operator's Restore click.
//!
//! ## A busy box is a skip
//!
//! A spawn the resource guard refuses (`resource_guard:critical:<lane>: …`)
//! never started anything, so it is `skipped(resource_guard:<lane>)` — a retry
//! once headroom returns — not `failed(spawn: …)`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};
use super::SessionRegistry;
use crate::coord_drain_state::{DrainGate, SpawnOrigin};
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::restart_prep::restore::RestoringCohort;
use crate::terminal::TerminalManager;

/// The origin the HTTP route resumes as: autonomous, so coord's device drain
/// (and a restart prepare's runner-local hold) defers it. The operator twin
/// passes the operator origin instead.
pub const HTTP_ORIGIN: SpawnOrigin = SpawnOrigin::Respawn;

/// Most ids one call will act on. Each resume is verified by a process read, so
/// an unbounded batch would pin the request for minutes.
pub const MAX_IDS: usize = 50;

/// How long to wait for the resumed `claude` to appear before calling the
/// spawn failed.
const VERIFY_DEADLINE: Duration = Duration::from_secs(20);
const VERIFY_POLL: Duration = Duration::from_millis(500);

/// Routes contributed by this module; merged in `mcp_api.rs`. A runner-native
/// route, not part of the UI-Bridge SDK manifest.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/control/sessions/resume", post(post_sessions_resume))
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeRequest {
    /// Claude session ids to resume. Must be non-empty when present; exclusive
    /// with `all`.
    #[serde(default)]
    pub ids: Option<Vec<String>>,
    /// Explicitly resume every closed, unfinished claude session this
    /// device's lifecycle store holds. Without it (or `ids`) the request is
    /// refused: an empty body must never mean "all".
    #[serde(default)]
    pub all: Option<bool>,
    /// The rebuild plan's ledger generation. Accepted and echoed; see the
    /// module docs.
    #[serde(default)]
    pub generation: Option<String>,
}

/// One id's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResumeRow {
    pub id: String,
    /// `resumed` | `failed` | `skipped`.
    pub outcome: &'static str,
    /// Present for `failed` and `skipped`.
    pub reason: Option<String>,
    /// `resumed` | `failed(<reason>)` | `skipped(<reason>)` — the spelling the
    /// plan names, for a reader that wants one string.
    pub verdict: String,
    /// What the process read observed (only on `resumed`, or on a `failed`
    /// that got far enough to observe something).
    pub evidence: Option<ProcessEvidence>,
    /// A restored cohort row only: the level passed as `--effort`, when one
    /// was (see the module docs).
    pub effort_applied: Option<String>,
    /// A restored cohort row only: `effort_restored` |
    /// `effort_not_restored(<reason>)`, from reading the resumed pane's status
    /// row. `None` when no effort was re-applied or the resume did not land.
    pub effort: Option<String>,
}

impl ResumeRow {
    fn resumed(id: &str, evidence: ProcessEvidence) -> Self {
        Self {
            id: id.to_string(),
            outcome: "resumed",
            reason: None,
            verdict: "resumed".to_string(),
            evidence: Some(evidence),
            effort_applied: None,
            effort: None,
        }
    }
    fn failed(id: &str, reason: impl Into<String>, evidence: Option<ProcessEvidence>) -> Self {
        let reason = reason.into();
        Self {
            id: id.to_string(),
            outcome: "failed",
            verdict: format!("failed({reason})"),
            reason: Some(reason),
            evidence,
            effort_applied: None,
            effort: None,
        }
    }
    fn skipped(id: &str, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            id: id.to_string(),
            outcome: "skipped",
            verdict: format!("skipped({reason})"),
            reason: Some(reason),
            evidence: None,
            effort_applied: None,
            effort: None,
        }
    }
}

/// What reading the process table showed about the resumed session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProcessEvidence {
    pub terminal_id: String,
    pub pid: Option<u32>,
    pub cmdline: Option<String>,
    /// `null` = this platform could not say (never a guess).
    pub cwd: Option<String>,
    /// `null` = this platform could not say (never a guess).
    pub claude_config_dir: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ResumeReport {
    pub generation: Option<String>,
    /// `false` — no ledger generation exists to filter by. See module docs.
    pub generation_applied: bool,
    pub requested: usize,
    pub resumed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub results: Vec<ResumeRow>,
}

// ---------------------------------------------------------------------------
// Pure decisions
// ---------------------------------------------------------------------------

/// A row that passed every precondition that needs no I/O beyond the row.
#[derive(Debug, Clone)]
struct Candidate {
    id: String,
    config_dir: String,
    working_dir: String,
    title: String,
    page_id: String,
    zone_index: i32,
    /// Set when the id is a row of the restoring cohort.
    cohort: Option<CohortMember>,
    /// A FINISHED cohort row restored by the operator: the core lifts its
    /// finish mark once the resume is verified (D4) — never for a Restore that
    /// is refused or fails.
    lift_finish: bool,
}

/// What the restoring cohort says about one id (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
struct CohortMember {
    prepare_id: String,
    /// Why the row's recorded terminal rules out a cold resume of an OPEN row:
    /// `live_terminal` (held by this runner with its process running),
    /// `holder_adoption_pending` (a PTY holder kept its pane alive across the
    /// restart and the runner is still adopting it), `holder_alive_not_adopted`
    /// (a live holder the sweep left running with no tab), `holder_sweep_pending`
    /// (the holder census is not in, so none of that can be ruled out yet).
    /// `None` when nothing does.
    terminal_blocker: Option<&'static str>,
    /// The level to pass as `--effort`.
    effort: Option<String>,
    /// Held for a hand restore (D6): only the operator origin restores it.
    held: bool,
    /// Whether the restore set withheld THIS row from the webview's boot
    /// restore by serving it as a placeholder
    /// ([`crate::restart_prep::restore::placeholder_served`]). Until it has,
    /// the boot restore owns the row while it is open.
    placeholder_served: bool,
    /// Whether the restore set handed THIS row to the webview's boot restore
    /// (its settled owner, [`crate::restart_prep::restore::row_owner`]). Such
    /// a row is the boot restore's in EVERY state — a `closed` one included —
    /// so the core never resumes it beside the auto-restore.
    boot_owned: bool,
}

/// The cohort membership of `id`, when `cohort` holds it. `terminal_blocker`
/// judges the row's recorded terminal (see [`CohortMember::terminal_blocker`]).
fn cohort_member(
    cohort: Option<&RestoringCohort>,
    id: &str,
    rec: Option<&TerminalSessionRecord>,
    terminal_blocker: impl Fn(&str) -> Option<&'static str>,
    placeholder_served: bool,
) -> Option<CohortMember> {
    let cohort = cohort?;
    let target = cohort.target(id)?;
    Some(CohortMember {
        prepare_id: cohort.prepare_id.clone(),
        terminal_blocker: rec.and_then(|r| terminal_blocker(&r.terminal_id)),
        effort: target.effort.clone(),
        held: target.held,
        placeholder_served,
        boot_owned: false,
    })
}

/// [`cohort_member`] with [`CohortMember::placeholder_served`] read from what
/// the restore set actually served in this process — per row, so an open row
/// it left to the boot restore is never admitted beside it.
fn served_cohort_member(
    cohort: Option<&RestoringCohort>,
    id: &str,
    rec: Option<&TerminalSessionRecord>,
    terminal_blocker: impl Fn(&str) -> Option<&'static str>,
) -> Option<CohortMember> {
    use crate::restart_prep::restore::{row_owner, RowOwner};
    let owner = cohort.and_then(|c| row_owner(&c.prepare_id, id));
    cohort_member(
        cohort,
        id,
        rec,
        terminal_blocker,
        owner == Some(RowOwner::Operator),
    )
    .map(|m| CohortMember {
        boot_owned: owner == Some(RowOwner::BootRestore),
        ..m
    })
}

/// [`CohortMember::terminal_blocker`] in the running app: the terminal
/// manager, then the PTY holder's adoption state.
fn live_terminal_blocker(tm: &TerminalManager, terminal_id: &str) -> Option<&'static str> {
    use crate::terminal::holder_boot_sweep as sweep;
    // Pending first: an adoption that lands between two reads moves a pane
    // from pending into the manager, never the other way.
    let census = sweep::sweep_status(Duration::ZERO).held;
    if sweep::adoption_pending(terminal_id)
        || census
            .as_ref()
            .is_some_and(|held| held.iter().any(|h| h == terminal_id))
    {
        return Some("holder_adoption_pending");
    }
    // Held AND running: a pane whose process exited keeps its tab, and so
    // its manager entry, until the tab is closed — counting it would refuse
    // every Retry of a restore that failed (the resume door runs `claude` as
    // the pane's own process, so its exit is the pane's). The live-holder guard's process
    // read still stops a second `claude` on the transcript.
    if tm.is_running(terminal_id) {
        return Some("live_terminal");
    }
    if sweep::holder_left_running(terminal_id) {
        return Some("holder_alive_not_adopted");
    }
    if census.is_none() {
        return Some("holder_sweep_pending");
    }
    None
}

/// Why a lifecycle row cannot be resumed, or the candidate it yields.
/// `cohort` is the id's membership in the restoring cohort, which admits an
/// `open` row whose terminal rules nothing out (D7). `operator` is whether the
/// caller is the operator: only the operator restores a row held for a hand
/// restore, and only the operator's Restore lifts a cohort row's finish mark.
fn classify(
    id: &str,
    rec: Option<&TerminalSessionRecord>,
    cohort: Option<&CohortMember>,
    operator: bool,
) -> Result<Candidate, String> {
    if !super::session_id::is_valid_session_id(id) {
        return Err("invalid_id".to_string());
    }
    let Some(rec) = rec else {
        return Err("not_found".to_string());
    };
    if rec.provider != super::session_lifecycle_store::DEFAULT_PROVIDER {
        return Err(format!("provider_{}", rec.provider));
    }
    if cohort.is_some_and(|m| m.boot_owned) {
        return Err("boot_restore_owns_row".to_string());
    }
    match (rec.state.as_str(), cohort) {
        ("closed", _) => {}
        ("open", Some(member)) => {
            if !member.placeholder_served {
                return Err("boot_restore_owns_row".to_string());
            }
            if let Some(blocker) = member.terminal_blocker {
                return Err(blocker.to_string());
            }
        }
        _ => return Err("not_closed".to_string()),
    }
    if cohort.is_some_and(|m| m.held) && !operator {
        return Err("held_for_hand_restore".to_string());
    }
    let lift_finish = rec.finished_at.is_some();
    if lift_finish && !(operator && cohort.is_some()) {
        return Err("finished".to_string());
    }
    let config_dir = rec
        .config_dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "no_config_dir".to_string())?;
    let working_dir = rec
        .working_dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "no_working_dir".to_string())?;
    let short = id.get(..8).unwrap_or(id);
    Ok(Candidate {
        id: id.to_string(),
        config_dir: config_dir.to_string(),
        working_dir: working_dir.to_string(),
        title: rec
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| format!("Resumed {short}")),
        page_id: rec.page_id.clone(),
        zone_index: rec.zone_index,
        cohort: cohort.cloned(),
        lift_finish,
    })
}

/// What a request asked for, once validated.
#[derive(Debug, PartialEq, Eq)]
enum Scope {
    Ids(Vec<String>),
    All,
}

impl ResumeRequest {
    /// An explicit scope or an error — never a default of "everything".
    fn scope(&self) -> Result<Scope, String> {
        let all = self.all == Some(true);
        match (&self.ids, all) {
            (Some(_), true) => Err("`ids` and `all: true` are mutually exclusive".to_string()),
            (Some(ids), false) if ids.iter().all(|i| i.trim().is_empty()) => {
                Err("`ids` is empty — name at least one id, or send `all: true`".to_string())
            }
            (Some(ids), false) => Ok(Scope::Ids(ids.clone())),
            (None, true) => Ok(Scope::All),
            (None, false) => Err("body must name non-empty `ids` or `all: true`".to_string()),
        }
    }
}

/// The ids this call acts on: the caller's, de-duplicated in order, or every
/// closed unfinished claude row for [`Scope::All`].
fn select_ids(scope: &Scope, all: &[TerminalSessionRecord]) -> Vec<String> {
    match scope {
        Scope::Ids(ids) => {
            let mut seen = std::collections::HashSet::new();
            ids.iter()
                .map(|i| i.trim().to_string())
                .filter(|i| !i.is_empty() && seen.insert(i.clone()))
                .collect()
        }
        Scope::All => {
            let mut rows: Vec<&TerminalSessionRecord> = all
                .iter()
                .filter(|r| {
                    r.state == "closed"
                        && r.finished_at.is_none()
                        && r.provider == super::session_lifecycle_store::DEFAULT_PROVIDER
                })
                .collect();
            // Most recently closed first: that is the work most likely still wanted.
            rows.sort_by_key(|r| std::cmp::Reverse(r.closed_at.unwrap_or(0)));
            rows.into_iter()
                .map(|r| r.claude_session_id.clone())
                .collect()
        }
    }
}

/// The admission decision after the I/O preconditions: drain first, then the
/// account-hop budget. `cap_permits` is called ONLY when the drain allows —
/// it records-and-checks, so charging a slot for a deferred attempt would let
/// a drain burn a session's whole 24h budget.
fn admit(gate: DrainGate, cap_permits: impl FnOnce() -> bool) -> Result<(), String> {
    if let DrainGate::Defer { reason, .. } = gate {
        return Err(format!("drain: {reason}"));
    }
    if !cap_permits() {
        return Err("cap_reached".to_string());
    }
    Ok(())
}

/// The skip reason for a resume while a model gateway is declared (or its
/// state is unknown), `None` otherwise. PURE over `gateway_declared`.
fn gateway_refusal(gateway_declared: bool) -> Option<String> {
    gateway_declared.then(|| {
        format!(
            "model_gateway: {}; a resumed pane would run under the gateway's \
             CLAUDE_CONFIG_DIR, where this session's transcript does not exist",
            crate::model_gateway::SUBSCRIPTION_OFF_REASON
        )
    })
}

/// [`gateway_refusal`] over the live gateway state.
fn live_gateway_refusal() -> Option<String> {
    gateway_refusal(crate::model_gateway::gateway_declared())
}

/// Whether `row`, the verdict for a cohort member, is recorded on the cohort
/// row. Everything is, except `skipped(in_flight)`: that is ANOTHER call's
/// restore of the same row, whose own verdict is the one to keep.
fn records_verdict(row: &ResumeRow) -> bool {
    row.reason.as_deref() != Some("in_flight")
}

/// Ids a call is resuming right now ([`InFlight`]). The lifecycle poll asks
/// before it records a cohort row as running, so its observation of the pane
/// a resume is still verifying never pre-empts that resume's own verdict.
pub fn in_flight(id: &str) -> bool {
    IN_FLIGHT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(id)
}

/// `skipped(resource_guard:<lane>)`'s reason for a spawn error that is the
/// resource guard's CRITICAL refusal, `None` for any other spawn error. The
/// refusal's wire is `resource_guard:critical:<lane>: <text>`
/// ([`crate::resource_guard::CRITICAL_REFUSAL_PREFIX`]), where `<lane>` is the
/// metric that refused (`free_commit_bytes` | `thread_count`). Matched
/// anywhere in the error, so a seam that prefixes its own context still reads
/// as the refusal; a lane token that is missing or malformed reads `unknown`.
fn resource_guard_skip(spawn_error: &str) -> Option<String> {
    let (_, rest) = spawn_error.split_once(crate::resource_guard::CRITICAL_REFUSAL_PREFIX)?;
    let lane = rest
        .split(':')
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or("unknown");
    Some(format!("resource_guard:{lane}"))
}

/// The verdict for a spawn that returned `Err(e)` before any pane existed: a
/// resource-guard refusal is a skip, anything else a failure.
fn spawn_error_row(id: &str, e: &str) -> ResumeRow {
    match resource_guard_skip(e) {
        Some(reason) => ResumeRow::skipped(id, reason),
        None => ResumeRow::failed(id, format!("spawn: {e}"), None),
    }
}

/// The skip a process-table read of `id`'s holder imposes, if any: a live
/// `claude` already running the id (`live_process`), or a table that could not
/// be read — which must never read as "nobody holds it". Checked for EVERY
/// candidate, a cohort row admitted while `open` included.
fn holder_skip(id: &str, holder: Result<Option<u32>, String>) -> Option<ResumeRow> {
    match holder {
        Ok(None) => None,
        Ok(Some(pid)) => Some(ResumeRow::skipped(id, format!("live_process(pid {pid})"))),
        Err(why) => Some(ResumeRow::skipped(id, why)),
    }
}

/// One live `claude` process as read from the process table.
#[derive(Debug, Clone)]
struct ClaudeProc {
    pid: u32,
    cmdline: String,
    cwd: Option<String>,
    config_dir: Option<String>,
}

/// `Ok` only when exactly one `claude --resume <id>` is present and every
/// fact the platform could read agrees with the row. `Err(None)` = not there
/// YET (keep polling); `Err(Some(reason))` = wrong, stop.
fn judge(
    procs: &[ClaudeProc],
    cand: &Candidate,
    terminal_id: &str,
) -> Result<ProcessEvidence, Option<String>> {
    let matching: Vec<&ClaudeProc> = procs
        .iter()
        .filter(|p| {
            crate::process_capture::process_tree::parse_session_id_from_cmdline(&p.cmdline)
                .as_deref()
                == Some(cand.id.as_str())
        })
        .collect();
    let p = match matching.as_slice() {
        [] => return Err(None),
        [one] => *one,
        many => return Err(Some(format!("multiple_resume_processes({})", many.len()))),
    };
    if let Some(cwd) = &p.cwd {
        if !same_path(cwd, &cand.working_dir) {
            return Err(Some(format!(
                "cwd_mismatch(want {}, got {cwd})",
                cand.working_dir
            )));
        }
    }
    if let Some(dir) = &p.config_dir {
        if !same_path(dir, &cand.config_dir) {
            return Err(Some(format!(
                "config_dir_mismatch(want {}, got {dir})",
                cand.config_dir
            )));
        }
    }
    Ok(ProcessEvidence {
        terminal_id: terminal_id.to_string(),
        pid: Some(p.pid),
        cmdline: Some(p.cmdline.clone()),
        cwd: p.cwd.clone(),
        claude_config_dir: p.config_dir.clone(),
    })
}

/// Path equality tolerant of trailing separators, `\` vs `/`, and symlinks
/// where both sides resolve. Case-insensitive only where the filesystem is
/// (Windows, macOS): on Linux `/Work` and `/work` are different directories,
/// and calling them equal would pass a resume pinned to the wrong tree.
fn same_path(a: &str, b: &str) -> bool {
    same_path_with(a, b, cfg!(any(windows, target_os = "macos")))
}

fn same_path_with(a: &str, b: &str, case_insensitive: bool) -> bool {
    let norm = |s: &str| {
        let canon = std::fs::canonicalize(s)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| s.to_string());
        let canon = canon.replace('\\', "/").trim_end_matches('/').to_string();
        if case_insensitive {
            canon.to_lowercase()
        } else {
            canon
        }
    };
    norm(a) == norm(b)
}

/// `CLAUDE_CONFIG_DIR` of a live process. Linux only (`/proc/<pid>/environ`);
/// elsewhere `None`, which is "could not say", never a match.
#[cfg(target_os = "linux")]
fn config_dir_of_pid(pid: u32) -> Option<String> {
    let bytes = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    bytes
        .split(|b| *b == 0)
        .filter_map(|kv| std::str::from_utf8(kv).ok())
        .find_map(|kv| kv.strip_prefix("CLAUDE_CONFIG_DIR=").map(str::to_string))
        .filter(|s| !s.is_empty())
}

#[cfg(not(target_os = "linux"))]
fn config_dir_of_pid(_pid: u32) -> Option<String> {
    None
}

// ---------------------------------------------------------------------------
// Double-spawn guards
// ---------------------------------------------------------------------------

/// Session ids a call is resuming RIGHT NOW. Process-wide: two concurrent
/// requests naming one id would otherwise each pass the (read-only)
/// preconditions and each spawn.
static IN_FLIGHT: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(Default::default);

/// RAII claim on one id in [`IN_FLIGHT`]; released on drop, including on a
/// panic or a cancelled future.
struct InFlight(String);

impl InFlight {
    /// `None` when another call already holds `id`.
    fn claim(id: &str) -> Option<Self> {
        let mut g = IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        g.insert(id.to_string()).then(|| Self(id.to_string()))
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
    }
}

/// Pure: the pid of a `claude` among `procs` (`(pid, cmdline)`) that is
/// already running session `id`.
fn holder_of(procs: &[(u32, String)], id: &str) -> Option<u32> {
    procs.iter().find_map(|(pid, cmd)| {
        (crate::process_capture::process_tree::parse_session_id_from_cmdline(cmd).as_deref()
            == Some(id))
        .then_some(*pid)
    })
}

/// Is any live `claude` on this machine already running session `id`
/// (`--resume <id>` or `--session-id <id>`)? `Err` when the process table
/// could not be read — which must NOT read as "nobody holds it": a resume of a
/// live session is a second process writing one transcript.
async fn live_holder(id: &str) -> Result<Option<u32>, String> {
    use crate::process_capture::process_tree as pt;
    let snap = pt::snapshot_process_table_public().await;
    if snap.names.is_empty() {
        return Err("process_table_unreadable".to_string());
    }
    let pids: Vec<u32> = snap
        .names
        .keys()
        .copied()
        .filter(|p| pt::is_countable_claude(*p, &snap))
        .collect();
    let cmdlines = pt::command_lines_for_pids(&pids).await;
    let procs: Vec<(u32, String)> = cmdlines.into_iter().collect();
    Ok(holder_of(&procs, id))
}

// ---------------------------------------------------------------------------
// I/O: the spawn site
// ---------------------------------------------------------------------------

/// Read the claude processes under `terminal_id`'s pane.
async fn read_claude_procs(
    tm: &Arc<TerminalManager>,
    terminal_id: &str,
) -> Option<Vec<ClaudeProc>> {
    use crate::process_capture::process_tree as pt;
    let root = tm.get(terminal_id)?.child_pid()?;
    let snap = pt::snapshot_process_table_public().await;
    let pids = pt::claude_pids_in_inclusive_subtree(root, &snap);
    let cmdlines = pt::command_lines_for_pids(&pids).await;
    let cwds = pt::working_directories_for_pids(&pids).await;
    Some(
        pids.into_iter()
            .filter_map(|pid| {
                Some(ClaudeProc {
                    pid,
                    cmdline: cmdlines.get(&pid)?.clone(),
                    cwd: cwds.get(&pid).cloned(),
                    config_dir: config_dir_of_pid(pid),
                })
            })
            .collect(),
    )
}

/// Poll the process table until the resumed claude is proven (or disproven).
async fn verify(
    tm: &Arc<TerminalManager>,
    cand: &Candidate,
    terminal_id: &str,
) -> Result<ProcessEvidence, String> {
    let deadline = tokio::time::Instant::now() + VERIFY_DEADLINE;
    loop {
        if tm.get(terminal_id).is_none() {
            return Err("pane_vanished_before_claude_appeared".to_string());
        }
        if let Some(procs) = read_claude_procs(tm, terminal_id).await {
            match judge(&procs, cand, terminal_id) {
                Ok(ev) => return Ok(ev),
                Err(Some(reason)) => return Err(reason),
                Err(None) => {}
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("no_claude_resume_process_observed".to_string());
        }
        tokio::time::sleep(VERIFY_POLL).await;
    }
}

/// Resume one session: gate, budget, spawn, verify. The spawn site registered
/// in `runner_spawn_sites.txt` — the drain gate lives HERE and branches on its
/// result. It gates on the `origin` it is handed: [`HTTP_ORIGIN`] from the
/// route, the operator origin from the twin. The caller holds the id's
/// [`InFlight`] claim across this call AND the recording of its verdict.
async fn resume_one(
    app: &tauri::AppHandle,
    tm: &Arc<TerminalManager>,
    registry: &Arc<SessionRegistry>,
    store: &Arc<SessionLifecycleStore>,
    cand: Candidate,
    origin: SpawnOrigin,
) -> ResumeRow {
    // Under a model gateway the resumed pane runs with the gateway's
    // CLAUDE_CONFIG_DIR (finalize_child_env pins it), where this session's
    // transcript does not exist, so `--resume` would find nothing. Refuse the
    // same way account migration does.
    if let Some(why) = live_gateway_refusal() {
        return ResumeRow::skipped(&cand.id, why);
    }
    // A live claude already running this id: resuming would be a second
    // process on one transcript.
    if let Some(skip) = holder_skip(&cand.id, live_holder(&cand.id).await) {
        return skip;
    }
    // The transcript, local — a missing one must never become `--resume` of
    // nothing.
    let transcript = crate::terminal::transcript::session_transcript_path(
        Path::new(&cand.config_dir),
        &cand.working_dir,
        &cand.id,
    );
    if !transcript.is_file() {
        return ResumeRow::skipped(&cand.id, "no_transcript");
    }
    if !Path::new(&cand.working_dir).is_dir() {
        return ResumeRow::skipped(&cand.id, "working_dir_missing");
    }
    let gate =
        match crate::coord_drain_state::drain_gate_for_work(origin, &format!("resume:{}", cand.id))
        {
            DrainGate::Allow => DrainGate::Allow,
            deferred => deferred,
        };
    // A drain deferral answers first, so a busy box never masks it.
    if let DrainGate::Defer { reason, .. } = &gate {
        return ResumeRow::skipped(&cand.id, format!("drain: {reason}"));
    }
    // A busy box is a skip that must not spend one of the session's
    // `MIGRATION_CAP` slots, so the resource guard is asked BEFORE the
    // budget; the spawn seam's own gate stays the authority (see below).
    if let Err(e) = crate::resource_guard::precheck_spawn("terminal session", false) {
        return spawn_error_row(&cand.id, &e);
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    if let Err(why) = admit(gate, || {
        crate::terminal::account_migration::migration_cap_permits(&cand.id, now_ms)
    }) {
        return ResumeRow::skipped(&cand.id, why);
    }

    let effort = cand.cohort.as_ref().and_then(|m| m.effort.clone());
    let spawn = {
        let (app, tm, registry, cand, effort) = (
            app.clone(),
            tm.clone(),
            registry.clone(),
            cand.clone(),
            effort.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let model_transcript = crate::terminal::transcript::session_transcript_path(
                Path::new(&cand.config_dir),
                &cand.working_dir,
                &cand.id,
            );
            crate::terminal::account_migration::spawn_resumed_pane(
                &app,
                &tm,
                &registry,
                crate::terminal::account_migration::ResumeSpawn {
                    claude_session_id: &cand.id,
                    working_dir: &cand.working_dir,
                    model_transcript,
                    // A restored cohort row comes back at the effort it ran at.
                    effort,
                    config_dir: &cand.config_dir,
                    title: cand.title.clone(),
                    page_id: cand.page_id.clone(),
                    zone_index: cand.zone_index,
                    work_unit_slug: None,
                    correlation_topic: None,
                    intent_repo: None,
                    coord_lineage: Some(crate::commands::terminal::CoordSessionLineage {
                        parent_session_id: None,
                        claude_code_session_id: Some(cand.id.clone()),
                    }),
                    // Creates a new process: respect the spawn-time resource
                    // floor like every other autonomous spawn.
                    resource_override: false,
                    // The source's continuation registration, if any, is not
                    // re-claimed from a closed row.
                    gate_identity: None,
                },
            )
        })
        .await
    };
    let terminal_id = match spawn {
        Ok(Ok((terminal_id, _coord_id))) => terminal_id,
        // Refused before anything started: the box is busy, retry later. The
        // seam's gate can still refuse after the pre-check passed (the box
        // crossed its limit in between); that slot is handed back too.
        Ok(Err(e)) => {
            let row = spawn_error_row(&cand.id, &e);
            if row.outcome == "skipped" {
                crate::terminal::account_migration::migration_cap_refund(&cand.id, now_ms);
            }
            return row;
        }
        Err(e) => return ResumeRow::failed(&cand.id, format!("spawn_task: {e}"), None),
    };
    // A cohort row's resume was attempted: stamp the restore marker now (tier
    // `failed` until the process read below proves it), never at boot — a
    // placeholder nobody has restored yet is not a failed boot restore.
    if cand.cohort.is_some() {
        store.mark_restore_pending(&cand.id, true);
    }
    match verify(tm, &cand, &terminal_id).await {
        Ok(ev) => {
            if cand.cohort.is_some() {
                store.clear_restore_pending(&cand.id);
            }
            // A FINISHED cohort row restored by the operator is live work
            // again: its finish mark is lifted now that the resume is proven,
            // so a refused or failed Restore never touches it.
            if cand.lift_finish {
                if let Err(e) = store.set_finished(&cand.id, false, None) {
                    warn!(id = %cand.id, error = ?e, "resume_door: resumed, but the finish mark could not be lifted");
                }
            }
            let mut row = ResumeRow::resumed(&cand.id, ev);
            if let Some(wanted) = effort {
                let tm = tm.clone();
                let read = move || tm.get(&terminal_id).map(|s| s.grid_screen());
                row.effort = Some(
                    crate::restart_prep::restore::read_back_effort(
                        &wanted,
                        read,
                        crate::restart_prep::restore::EFFORT_READ_BACK_DEADLINE,
                        crate::restart_prep::restore::EFFORT_READ_BACK_POLL,
                    )
                    .await,
                );
                row.effort_applied = Some(wanted);
            }
            row
        }
        Err(reason) => {
            let mut row = ResumeRow::failed(
                &cand.id,
                reason,
                Some(ProcessEvidence {
                    terminal_id,
                    pid: None,
                    cmdline: None,
                    cwd: None,
                    claude_config_dir: None,
                }),
            );
            // The spawn carried the flag even though the resume did not land.
            row.effort_applied = effort;
            row
        }
    }
}

fn tally(generation: Option<String>, requested: usize, results: Vec<ResumeRow>) -> ResumeReport {
    let n = |o: &str| results.iter().filter(|r| r.outcome == o).count();
    ResumeReport {
        generation,
        generation_applied: false,
        requested,
        resumed: n("resumed"),
        failed: n("failed"),
        skipped: n("skipped"),
        results,
    }
}

/// Run the batch to completion, one id at a time. Spawned DETACHED by the
/// handler: it owns everything it needs and is not tied to the request future.
/// Each id is bounded by the process-read verify (20 s) and, for a cohort row
/// whose effort is re-applied, the effort read-back (30 s); the restart
/// restore's queue sends one id per call.
async fn run_batch(
    app: tauri::AppHandle,
    tm: Arc<TerminalManager>,
    registry: Arc<SessionRegistry>,
    store: Arc<SessionLifecycleStore>,
    ids: Vec<String>,
    origin: SpawnOrigin,
) -> Vec<ResumeRow> {
    let mut results = Vec::new();
    let operator = !origin.is_autonomous();
    for id in &ids {
        let rec = store.get(id);
        // Re-read per id: an overlapping call may have restored a row since
        // this batch began, and a restored row is no longer in the cohort.
        let cohort = crate::restart_prep::restore::restoring_cohort();
        let member = served_cohort_member(cohort.as_ref(), id, rec.as_ref(), |t| {
            live_terminal_blocker(&tm, t)
        });
        // One call per id at a time, held through spawn, verify AND the
        // recording of the verdict below — so the lifecycle poll's `running`
        // observation (skipped while the id is in flight) can never land
        // between the resume and its own verdict.
        let mut claim = None;
        let row = match classify(id, rec.as_ref(), member.as_ref(), operator) {
            Err(why) => ResumeRow::skipped(id, why),
            Ok(cand) => match InFlight::claim(&cand.id) {
                None => ResumeRow::skipped(id, "in_flight"),
                Some(c) => {
                    claim = Some(c);
                    resume_one(&app, &tm, &registry, &store, cand, origin).await
                }
            },
        };
        info!(id = %row.id, verdict = %row.verdict, effort = ?row.effort, "resume_door: verdict");
        // A cohort row's verdict also lands on the row itself, except a skip
        // that is another call's in-flight restore of the same row.
        if let Some(m) = member.as_ref().filter(|_| records_verdict(&row)) {
            crate::restart_prep::restore::note_restore(
                &m.prepare_id,
                id,
                &row.verdict,
                row.effort_applied.clone(),
                row.effort.clone(),
            );
        }
        drop(claim);
        results.push(row);
    }
    results
}

/// `true` when the `Content-Type` header names JSON (`application/json`, with
/// or without parameters, any case, or a `+json` suffix type).
fn is_json_content_type(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            let essence = v
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            essence == "application/json" || essence.ends_with("+json")
        })
        .unwrap_or(false)
}

async fn post_sessions_resume(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Result<Json<ApiResponse<ResumeReport>>, (axum::http::StatusCode, Json<ApiResponse<()>>)> {
    use axum::http::StatusCode;

    if !is_json_content_type(&headers) {
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(api_error(
                "Content-Type must be application/json — NOTHING was resumed".to_string(),
            )),
        ));
    }
    let req: ResumeRequest = serde_json::from_slice(&body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "body must be {{ids | all: true, generation?}}: {e}"
            ))),
        )
    })?;
    resume_sessions(state, req, HTTP_ORIGIN).await
}

/// The door's core, shared by the HTTP route ([`HTTP_ORIGIN`]) and the runner
/// UI's operator twin (the operator origin): validate the scope, resolve the
/// app state, and run the batch to completion in a detached task.
pub(crate) async fn resume_sessions(
    state: Arc<ApiState>,
    req: ResumeRequest,
    origin: SpawnOrigin,
) -> Result<Json<ApiResponse<ResumeReport>>, (axum::http::StatusCode, Json<ApiResponse<()>>)> {
    use axum::http::StatusCode;
    use tauri::Manager;

    let scope = req
        .scope()
        .map_err(|why| (StatusCode::BAD_REQUEST, Json(api_error(why))))?;
    if let Scope::Ids(ids) = &scope {
        if ids.len() > MAX_IDS {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!("at most {MAX_IDS} ids per call"))),
            ));
        }
    }
    let unavailable = |what: &str| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error(format!(
                "{what} not available — NOTHING was resumed"
            ))),
        )
    };
    let store = state
        .app_handle
        .try_state::<Arc<SessionLifecycleStore>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| unavailable("lifecycle store"))?;
    let tm = state
        .app_handle
        .try_state::<Arc<TerminalManager>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| unavailable("terminal manager"))?;
    let registry = state
        .app_handle
        .try_state::<Arc<SessionRegistry>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| unavailable("session registry"))?;

    let mut ids = select_ids(&scope, &store.all_records());
    if ids.len() > MAX_IDS {
        warn!(
            n = ids.len(),
            "resume_door: more unfinished sessions than one call takes; acting on the first {MAX_IDS}"
        );
        ids.truncate(MAX_IDS);
    }
    info!(
        requested = ids.len(),
        generation = ?req.generation,
        "resume_door: resuming closed sessions"
    );

    let requested = ids.len();
    // Detached: a caller that hangs up mid-batch must not cancel the ids still
    // to come (nor orphan a spawn between its start and its verification).
    // Every id's verdict is logged by `run_batch` whether or not anyone is
    // still waiting for the response.
    let batch = tokio::spawn(run_batch(
        state.app_handle.clone(),
        tm,
        registry,
        store,
        ids,
        origin,
    ));
    let results = batch.await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "resume batch task failed ({e}) — see the per-id verdicts in the runner log"
            ))),
        )
    })?;
    Ok(Json(ApiResponse::success(tally(
        req.generation,
        requested,
        results,
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coord_drain_state::DeferClass;

    const ID: &str = "0b9f6c1e-1111-4222-8333-444455556666";

    fn rec(state: &str) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: ID.to_string(),
            config_dir: Some("/cfg".to_string()),
            working_dir: Some("/work".to_string()),
            page_id: "default".to_string(),
            zone_index: 2,
            title: None,
            terminal_id: "t".to_string(),
            opened_at: 0,
            last_seen_at: 0,
            state: state.to_string(),
            closed_at: Some(5),
            close_reason: None,
            provider: super::super::session_lifecycle_store::DEFAULT_PROVIDER.to_string(),
            origin: None,
            restore_pending_at: None,
            awaiting_account_since: None,
            confirmed_at: None,
            handle: None,
            account_label: None,
            account_wrapper: None,
            session_name: None,
            name_source: None,
            tenant_id: None,
            task_run_id: None,
            bypass_permissions: None,
            restored_from_boot_at: None,
            restore_tier: None,
            finished_at: None,
            wind_down_outcome: None,
            wind_down_at: None,
            finish_reason: None,
            finish_synced: false,
            spawn_device_default: None,
            adopted_from: None,
        }
    }

    fn reason(r: Result<Candidate, String>) -> String {
        r.expect_err("must be skipped")
    }

    #[test]
    fn a_declared_gateway_refuses_the_resume_with_a_reason() {
        assert_eq!(gateway_refusal(false), None);
        let why = gateway_refusal(true).expect("refused");
        assert!(why.starts_with("model_gateway: "), "{why}");
        assert!(why.contains("transcript does not exist"), "{why}");
    }

    /// Wiring: the live reader follows the gateway state (a recorded gateway
    /// with no declaration reads UNKNOWN, which also refuses).
    #[test]
    fn the_live_gateway_state_reaches_the_resume_refusal() {
        let _amb = crate::test_env::isolated_ambient();
        assert_eq!(live_gateway_refusal(), None);
        let g = crate::model_gateway::ModelGatewaySettings {
            base_url: Some("https://resume-gw.example.com".to_string()),
            api_key_helper: Some("echo resume-key".to_string()),
            ..Default::default()
        }
        .resolve()
        .unwrap()
        .unwrap();
        crate::model_gateway::write_marker_at(&crate::model_gateway::gateway_dir().unwrap(), &g)
            .unwrap();
        assert!(live_gateway_refusal().is_some());
    }

    #[test]
    fn classify_names_every_skip_reason() {
        assert_eq!(
            reason(classify("bad id!", Some(&rec("closed")), None, true)),
            "invalid_id"
        );
        assert_eq!(reason(classify(ID, None, None, true)), "not_found");
        assert_eq!(
            reason(classify(ID, Some(&rec("open")), None, true)),
            "not_closed"
        );
        let mut finished = rec("closed");
        finished.finished_at = Some(9);
        assert_eq!(
            reason(classify(ID, Some(&finished), None, true)),
            "finished"
        );
        let mut other = rec("closed");
        other.provider = "gemini".to_string();
        assert_eq!(
            reason(classify(ID, Some(&other), None, true)),
            "provider_gemini"
        );
        let mut nocfg = rec("closed");
        nocfg.config_dir = Some("  ".to_string());
        assert_eq!(
            reason(classify(ID, Some(&nocfg), None, true)),
            "no_config_dir"
        );
        let mut nowd = rec("closed");
        nowd.working_dir = None;
        assert_eq!(
            reason(classify(ID, Some(&nowd), None, true)),
            "no_working_dir"
        );
        let ok = classify(ID, Some(&rec("closed")), None, true).unwrap();
        assert_eq!((ok.config_dir.as_str(), ok.zone_index), ("/cfg", 2));
    }

    #[test]
    fn select_ids_defaults_to_closed_unfinished_newest_first_and_dedups_explicit() {
        let mut a = rec("closed");
        a.claude_session_id = "a".into();
        a.closed_at = Some(1);
        let mut b = rec("closed");
        b.claude_session_id = "b".into();
        b.closed_at = Some(9);
        let mut fin = rec("closed");
        fin.claude_session_id = "fin".into();
        fin.finished_at = Some(1);
        let mut open = rec("open");
        open.claude_session_id = "open".into();
        let all = [a, b, fin, open];
        assert_eq!(select_ids(&Scope::All, &all), ["b", "a"]);
        let explicit = Scope::Ids(vec![" x ".into(), "x".into(), "y".into()]);
        assert_eq!(select_ids(&explicit, &all), ["x", "y"]);
    }

    fn req(json: serde_json::Value) -> ResumeRequest {
        serde_json::from_value(json).expect("decode")
    }

    #[test]
    fn an_empty_or_bare_request_never_means_all() {
        for bare in [
            serde_json::json!({}),
            serde_json::json!({"generation": "g"}),
            serde_json::json!({"all": false}),
            serde_json::json!({"ids": []}),
            serde_json::json!({"ids": ["  "]}),
        ] {
            assert!(
                req(bare.clone()).scope().is_err(),
                "{bare} must be refused, not widened to all"
            );
        }
        assert_eq!(
            req(serde_json::json!({"all": true})).scope(),
            Ok(Scope::All)
        );
        assert_eq!(
            req(serde_json::json!({"ids": ["a"]})).scope(),
            Ok(Scope::Ids(vec!["a".into()]))
        );
        // Ambiguity is refused too.
        assert!(req(serde_json::json!({"ids": ["a"], "all": true}))
            .scope()
            .is_err());
        // An unknown field (a typo of `all`) is a decode error, not a bare body.
        assert!(serde_json::from_value::<ResumeRequest>(serde_json::json!({"al": true})).is_err());
    }

    #[test]
    fn only_a_json_content_type_is_accepted() {
        use axum::http::{header::CONTENT_TYPE, HeaderMap, HeaderValue};
        let with = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(CONTENT_TYPE, HeaderValue::from_str(v).unwrap());
            h
        };
        assert!(is_json_content_type(&with("application/json")));
        assert!(is_json_content_type(&with(
            "Application/JSON; charset=utf-8"
        )));
        assert!(!is_json_content_type(&with("text/plain")));
        assert!(!is_json_content_type(&with(
            "application/x-www-form-urlencoded"
        )));
        assert!(!is_json_content_type(&HeaderMap::new()));
    }

    #[test]
    fn a_second_claim_on_one_id_is_refused_until_the_first_is_released() {
        let id = "in-flight-test-3f2a-resume-door";
        let first = InFlight::claim(id).expect("first claim");
        assert!(InFlight::claim(id).is_none(), "concurrent call must lose");
        assert!(InFlight::claim("another-id-resume-door").is_some());
        drop(first);
        assert!(InFlight::claim(id).is_some(), "released on drop");
    }

    #[test]
    fn a_live_claude_on_the_id_is_found_by_its_cmdline() {
        let procs = vec![
            (
                7,
                "/bin/claude --resume 11111111-1111-4111-8111-111111111111".to_string(),
            ),
            (9, format!("/bin/claude --session-id {ID}")),
        ];
        assert_eq!(holder_of(&procs, ID), Some(9));
        assert_eq!(holder_of(&procs[..1], ID), None);
        assert_eq!(holder_of(&[], ID), None);
    }

    #[test]
    fn path_case_folds_only_where_the_filesystem_does() {
        assert!(same_path_with("/Work/Proj", "/work/proj", true));
        assert!(!same_path_with("/Work/Proj", "/work/proj", false));
        assert!(same_path_with("/work/proj/", "/work/proj", false));
        assert_eq!(
            same_path("/Work/Proj", "/work/proj"),
            cfg!(any(windows, target_os = "macos"))
        );
    }

    #[test]
    fn drain_defers_without_charging_the_cap() {
        let mut asked = false;
        let gate = DrainGate::Defer {
            reason: "device drained".into(),
            class: DeferClass::Drained,
        };
        let r = admit(gate, || {
            asked = true;
            true
        });
        assert_eq!(r, Err("drain: device drained".to_string()));
        assert!(!asked, "a deferred attempt must not consume a budget slot");
    }

    #[test]
    fn the_cap_admits_three_then_refuses_the_fourth() {
        let id = "cap-test-1b2c3d4e-resume-door";
        let now = chrono::Utc::now().timestamp_millis();
        let permits = || crate::terminal::account_migration::migration_cap_permits(id, now);
        for _ in 0..crate::terminal::account_migration::MIGRATION_CAP {
            assert_eq!(admit(DrainGate::Allow, permits), Ok(()));
        }
        assert_eq!(
            admit(DrainGate::Allow, permits),
            Err("cap_reached".to_string())
        );
    }

    fn cand() -> Candidate {
        classify(ID, Some(&rec("closed")), None, true).unwrap()
    }

    fn proc(cmd: &str, cwd: Option<&str>, cfg: Option<&str>) -> ClaudeProc {
        ClaudeProc {
            pid: 42,
            cmdline: cmd.to_string(),
            cwd: cwd.map(String::from),
            config_dir: cfg.map(String::from),
        }
    }

    #[test]
    fn judge_requires_exactly_one_matching_resume_process() {
        let good = format!("/bin/claude --permission-mode bypassPermissions --resume {ID}");
        // Not there yet: keep polling.
        assert_eq!(judge(&[], &cand(), "t").unwrap_err(), None);
        let other = "/bin/claude --resume 11111111-1111-4111-8111-111111111111";
        assert_eq!(
            judge(&[proc(other, None, None)], &cand(), "t").unwrap_err(),
            None
        );
        // One match, unreadable cwd/config: accepted, fields stay null.
        let ev = judge(&[proc(&good, None, None)], &cand(), "t").unwrap();
        assert_eq!(
            (ev.pid, ev.cwd, ev.claude_config_dir),
            (Some(42), None, None)
        );
        // Two matches: a double spawn is a failure, not a success.
        let two = [proc(&good, None, None), proc(&good, None, None)];
        assert_eq!(
            judge(&two, &cand(), "t").unwrap_err(),
            Some("multiple_resume_processes(2)".to_string())
        );
    }

    #[test]
    fn judge_fails_on_a_wrong_cwd_or_config_dir_but_not_on_unknown() {
        let good = format!("claude --resume {ID}");
        let bad_cwd = judge(&[proc(&good, Some("/elsewhere"), None)], &cand(), "t");
        assert!(bad_cwd.unwrap_err().unwrap().starts_with("cwd_mismatch"));
        let bad_cfg = judge(&[proc(&good, Some("/work"), Some("/other"))], &cand(), "t");
        assert!(bad_cfg
            .unwrap_err()
            .unwrap()
            .starts_with("config_dir_mismatch"));
        assert!(judge(&[proc(&good, Some("/work/"), Some("/cfg"))], &cand(), "t").is_ok());
    }

    // --- Phase 6 (plan 2026-10-21-operator-prepare-for-restart-and-progressive-restore)

    fn restoring(ids: &[&str]) -> RestoringCohort {
        crate::restart_prep::restore::restoring_cohort_for_test(ids)
    }

    /// No terminal rules a cold resume out.
    fn no_blocker(_: &str) -> Option<&'static str> {
        None
    }

    #[test]
    fn an_open_cohort_row_with_no_terminal_resumes_and_outside_the_cohort_is_refused() {
        let cohort = restoring(&[ID]);
        let open = rec("open");
        let member = cohort_member(Some(&cohort), ID, Some(&open), no_blocker, true);
        let cand =
            classify(ID, Some(&open), member.as_ref(), true).expect("admitted in cohort mode");
        assert!(!cand.lift_finish);
        let m = cand
            .cohort
            .expect("the candidate carries its cohort membership");
        assert_eq!(m.prepare_id, "prep-test");
        assert_eq!(m.effort.as_deref(), Some("max"));
        // The route (an autonomous caller) is admitted too: one core (D7).
        assert!(classify(ID, Some(&open), member.as_ref(), false).is_ok());
        // The same open row with no cohort, or with a cohort that does not hold it.
        assert_eq!(reason(classify(ID, Some(&open), None, true)), "not_closed");
        let other = restoring(&["11111111-1111-4111-8111-111111111111"]);
        let not_member = cohort_member(Some(&other), ID, Some(&open), no_blocker, true);
        assert_eq!(not_member, None);
        assert_eq!(
            reason(classify(ID, Some(&open), not_member.as_ref(), true)),
            "not_closed"
        );
        // A closed cohort row is admitted and still carries its effort.
        let closed = rec("closed");
        let member = cohort_member(Some(&cohort), ID, Some(&closed), no_blocker, true);
        let cand = classify(ID, Some(&closed), member.as_ref(), true).unwrap();
        assert_eq!(cand.cohort.and_then(|m| m.effort).as_deref(), Some("max"));
    }

    #[test]
    fn an_open_cohort_row_is_the_boot_restores_until_placeholders_were_served() {
        let cohort = restoring(&[ID]);
        let open = rec("open");
        let unserved = cohort_member(Some(&cohort), ID, Some(&open), no_blocker, false);
        assert_eq!(
            reason(classify(ID, Some(&open), unserved.as_ref(), true)),
            "boot_restore_owns_row"
        );
        // A CLOSED cohort row is no boot restore's: still admitted.
        let closed = rec("closed");
        let unserved = cohort_member(Some(&cohort), ID, Some(&closed), no_blocker, false);
        assert!(classify(ID, Some(&closed), unserved.as_ref(), true).is_ok());
    }

    /// A cohort row id no other test uses: the served set is process-wide.
    fn fresh_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// Review round 6, finding 1: "served" is per ROW, read from the set the
    /// restore set writes — not one process-wide flag. An open cohort row is
    /// the boot restore's until the restore set served THAT row as a
    /// placeholder; serving another row, or the same id under another
    /// prepare, admits nothing.
    #[test]
    fn an_open_cohort_row_is_admitted_only_once_the_restore_set_served_that_row() {
        use crate::restart_prep::restore::{settle_row_owner, RowOwner};
        let (served, left_to_boot) = (fresh_id(), fresh_id());
        let cohort = restoring(&[&served, &left_to_boot]);
        let admit = |id: &str| {
            let mut open = rec("open");
            open.claude_session_id = id.to_string();
            let member = served_cohort_member(Some(&cohort), id, Some(&open), no_blocker);
            classify(id, Some(&open), member.as_ref(), true).map(|_| ())
        };
        assert_eq!(admit(&served), Err("boot_restore_owns_row".to_string()));
        settle_row_owner(&cohort.prepare_id, &served, RowOwner::Operator);
        assert_eq!(admit(&served), Ok(()));
        // The row the restore set left in `sessions` (its terminal looked live)
        // was never served: still the boot restore's while it is open.
        assert_eq!(
            admit(&left_to_boot),
            Err("boot_restore_owns_row".to_string())
        );
        settle_row_owner("another-prepare", &left_to_boot, RowOwner::Operator);
        assert_eq!(
            admit(&left_to_boot),
            Err("boot_restore_owns_row".to_string())
        );
        // A row outside the cohort is no member at all, served or not.
        let outsider = fresh_id();
        settle_row_owner(&cohort.prepare_id, &outsider, RowOwner::Operator);
        assert_eq!(
            served_cohort_member(Some(&cohort), &outsider, None, no_blocker),
            None
        );
        assert_eq!(served_cohort_member(None, &served, None, no_blocker), None);
    }

    /// Phase 7: a row the restore set handed to the boot restore is the boot
    /// restore's in EVERY state. A `closed` one too — the auto-restore may be
    /// resuming it from the same read — so the core never resumes it beside
    /// the auto-restore.
    #[test]
    fn a_row_handed_to_the_boot_restore_is_refused_open_or_closed() {
        use crate::restart_prep::restore::{settle_row_owner, RowOwner};
        let to_boot = fresh_id();
        let cohort = restoring(&[&to_boot]);
        settle_row_owner(&cohort.prepare_id, &to_boot, RowOwner::BootRestore);
        for state in ["open", "closed"] {
            let mut r = rec(state);
            r.claude_session_id = to_boot.clone();
            let member = served_cohort_member(Some(&cohort), &to_boot, Some(&r), no_blocker);
            assert_eq!(
                classify(&to_boot, Some(&r), member.as_ref(), true).map(|_| ()),
                Err("boot_restore_owns_row".to_string()),
                "{state}"
            );
        }
        // A closed row nobody judged is still admitted (the pre-Phase-7 shape).
        let unjudged = fresh_id();
        let cohort = restoring(&[&unjudged]);
        let mut r = rec("closed");
        r.claude_session_id = unjudged.clone();
        let member = served_cohort_member(Some(&cohort), &unjudged, Some(&r), no_blocker);
        assert!(classify(&unjudged, Some(&r), member.as_ref(), true).is_ok());
    }

    /// Review round 6, finding 2: a pane whose child exited stays in the
    /// manager until its tab is closed. It is no live terminal, so a Retry
    /// after a failed restore is not `skipped(live_terminal)`; a pane whose
    /// process runs still is.
    #[test]
    fn an_exited_pane_is_no_live_terminal_but_a_running_one_is() {
        /// Leaves the pane exited when it drops, even on an assertion panic:
        /// `Drop` closes a session it believes alive, and this one has no PTY.
        struct ExitOnDrop(Arc<crate::terminal::session::TerminalSession>);
        impl Drop for ExitOnDrop {
            fn drop(&mut self) {
                self.0.set_alive_for_test(false);
            }
        }
        let tm = TerminalManager::new();
        let terminal_id = format!("term-{}", fresh_id());
        let pane = ExitOnDrop(Arc::new(
            crate::terminal::session::tests::make_test_session(Arc::new(std::sync::Mutex::new(
                Vec::new(),
            ))),
        ));
        tm.insert_for_test(&terminal_id, pane.0.clone());
        assert!(!pane.0.is_alive(), "the test pane starts exited");
        assert!(
            tm.get(&terminal_id).is_some(),
            "an exited pane is still held"
        );
        assert!(!tm.is_running(&terminal_id));
        // Only the `live_terminal` arm is asserted: the holder census and
        // adoption state are process-wide, so an exited pane may still read
        // `holder_sweep_pending` while another test runs a sweep. That arm is
        // what this finding changed.
        assert_ne!(
            live_terminal_blocker(&tm, &terminal_id),
            Some("live_terminal")
        );
        pane.0.set_alive_for_test(true);
        assert!(tm.is_running(&terminal_id));
        assert_eq!(
            live_terminal_blocker(&tm, &terminal_id),
            Some("live_terminal")
        );
    }

    #[test]
    fn an_open_cohort_row_whose_terminal_is_live_or_being_adopted_is_refused() {
        let cohort = restoring(&[ID]);
        let open = rec("open");
        for blocker in [
            "live_terminal",
            "holder_adoption_pending",
            "holder_alive_not_adopted",
            "holder_sweep_pending",
        ] {
            let member = cohort_member(
                Some(&cohort),
                ID,
                Some(&open),
                |t| (t == "t").then_some(blocker),
                true,
            );
            assert_eq!(
                reason(classify(ID, Some(&open), member.as_ref(), true)),
                blocker
            );
        }
        // A CLOSED cohort row's old terminal is not consulted.
        let closed = rec("closed");
        let member = cohort_member(
            Some(&cohort),
            ID,
            Some(&closed),
            |_| Some("live_terminal"),
            true,
        );
        assert!(classify(ID, Some(&closed), member.as_ref(), true).is_ok());
    }

    #[test]
    fn an_admitted_open_cohort_row_with_a_live_holder_is_still_skipped() {
        let cohort = restoring(&[ID]);
        let open = rec("open");
        let member = cohort_member(Some(&cohort), ID, Some(&open), no_blocker, true);
        let cand = classify(ID, Some(&open), member.as_ref(), true).expect("admitted");
        // The live-holder guard runs for every candidate, cohort rows included.
        let skip = holder_skip(&cand.id, Ok(Some(4242))).expect("a live holder skips");
        assert_eq!(skip.verdict, "skipped(live_process(pid 4242))");
        let unreadable = holder_skip(&cand.id, Err("process_table_unreadable".to_string()))
            .expect("an unreadable table skips");
        assert_eq!(unreadable.verdict, "skipped(process_table_unreadable)");
        assert_eq!(holder_skip(&cand.id, Ok(None)), None);
    }

    #[test]
    fn a_held_cohort_row_is_the_operators_to_restore() {
        let mut cohort_rec = rec("open");
        cohort_rec.claude_session_id = ID.to_string();
        let cohort = restoring(&[ID]);
        let mut member =
            cohort_member(Some(&cohort), ID, Some(&cohort_rec), no_blocker, true).unwrap();
        member.held = true;
        assert_eq!(
            reason(classify(ID, Some(&cohort_rec), Some(&member), false)),
            "held_for_hand_restore"
        );
        assert!(classify(ID, Some(&cohort_rec), Some(&member), true).is_ok());
    }

    #[test]
    fn only_the_operators_restore_of_a_finished_cohort_row_lifts_its_mark() {
        let cohort = restoring(&[ID]);
        let mut finished = rec("closed");
        finished.finished_at = Some(9);
        let member = cohort_member(Some(&cohort), ID, Some(&finished), no_blocker, true);
        // The operator: admitted, and the core lifts the mark once the resume
        // is verified.
        let cand = classify(ID, Some(&finished), member.as_ref(), true).expect("admitted");
        assert!(cand.lift_finish);
        // The route never lifts a finish mark, cohort or not.
        assert_eq!(
            reason(classify(ID, Some(&finished), member.as_ref(), false)),
            "finished"
        );
        // Outside a cohort the operator is refused as before.
        assert_eq!(
            reason(classify(ID, Some(&finished), None, true)),
            "finished"
        );
    }

    #[test]
    fn every_cohort_verdict_is_recorded_but_another_calls_in_flight_skip() {
        assert!(records_verdict(&ResumeRow::skipped(ID, "live_terminal")));
        assert!(records_verdict(&ResumeRow::failed(ID, "x", None)));
        assert!(records_verdict(&ResumeRow::skipped(
            ID,
            "resource_guard:thread_count"
        )));
        assert!(!records_verdict(&ResumeRow::skipped(ID, "in_flight")));
    }

    #[test]
    fn in_flight_reads_the_claims() {
        let id = "in-flight-reader-7c1d-resume-door";
        assert!(!in_flight(id));
        let claim = InFlight::claim(id).unwrap();
        assert!(in_flight(id));
        drop(claim);
        assert!(!in_flight(id));
    }

    /// The finish mark of a restored finished cohort row is lifted only once
    /// the resume is VERIFIED: read off `resume_one`'s source, because the
    /// spawn needs a running app. Every early return (each guard, a refused or
    /// failed spawn, a failed verify) therefore leaves the mark untouched.
    #[test]
    fn a_finished_rows_mark_is_lifted_only_after_a_verified_resume() {
        let src = include_str!("resume_door.rs");
        let (_, rest) = src
            .split_once("async fn resume_one(")
            .expect("resume_one exists");
        let (body, _) = rest.split_once("\n}\n").expect("resume_one's body ends");
        let lifts: Vec<_> = body.match_indices("set_finished(").collect();
        assert_eq!(
            lifts.len(),
            1,
            "exactly one finish-mark write in resume_one"
        );
        let (lift_at, _) = lifts[0];
        let (verified_at, _) = body
            .match_indices("Ok(ev) =>")
            .next()
            .expect("the verified arm");
        let (failed_at, _) = body
            .match_indices("Err(reason) =>")
            .next()
            .expect("the failed-verify arm");
        assert!(verified_at < failed_at, "the verified arm comes first");
        assert!(
            verified_at < lift_at && lift_at < failed_at,
            "the finish mark is lifted inside the verified-resume arm only — never in \
             the failed-verify arm, nor after the match"
        );
    }

    /// `resume_one` gates on the origin its caller hands it — never a literal
    /// one — so the route's `HTTP_ORIGIN` is deferred under a drain and the
    /// operator twin's origin is not. Read off the source because the call
    /// needs a running app; the spawn-sites guard accepts a literal `Respawn`,
    /// so nothing else would notice the origin being hardcoded again.
    #[test]
    fn resume_one_gates_on_the_origin_it_is_handed() {
        let src = include_str!("resume_door.rs");
        let (_, rest) = src
            .split_once("async fn resume_one(")
            .expect("resume_one exists");
        let (body, _) = rest.split_once("\n}\n").expect("resume_one's body ends");
        let (_, after_gate) = body
            .split_once("drain_gate_for_work(")
            .expect("resume_one calls the drain gate");
        assert!(
            after_gate.trim_start().starts_with("origin,"),
            "the gate's first argument must be the handed origin: {after_gate}"
        );
        assert!(
            !body.contains("SpawnOrigin::"),
            "resume_one must not name a literal origin"
        );
        // ...and runs the live-holder guard for every candidate it is handed,
        // a cohort row admitted while `open` included.
        assert!(body.contains("holder_skip(&cand.id, live_holder(&cand.id).await)"));
        // The HTTP route hands the core `HTTP_ORIGIN`.
        let (_, route) = src
            .split_once("async fn post_sessions_resume(")
            .expect("the route exists");
        let (route, _) = route.split_once("\n}\n").expect("the route's body ends");
        assert!(route.contains("resume_sessions(state, req, HTTP_ORIGIN)"));
        // The batch holds the id's in-flight claim across the verdict's
        // recording, so the poll's observation cannot land in between.
        let (_, batch) = src.split_once("async fn run_batch(").expect("run_batch");
        let (batch, _) = batch.split_once("\n}\n").expect("run_batch's body ends");
        let (claim_at, _) = batch
            .match_indices("InFlight::claim(")
            .next()
            .expect("claim");
        let (note_at, _) = batch.match_indices("note_restore(").next().expect("note");
        let (drop_at, _) = batch.match_indices("drop(claim)").next().expect("drop");
        assert!(claim_at < note_at && note_at < drop_at);
        // Admission reads the per-row served set, never a passed-in constant.
        assert!(batch.contains("served_cohort_member("));
        assert!(!batch.contains(" cohort_member(") && !batch.contains("::cohort_member("));
    }

    #[test]
    fn a_resource_guard_refusal_is_a_skip_and_any_other_spawn_error_a_failure() {
        let prefix = crate::resource_guard::CRITICAL_REFUSAL_PREFIX;
        for lane in ["free_commit_bytes", "thread_count"] {
            let refusal = format!(
                "{prefix}{lane}: Not starting a new terminal session: the host lane has 1.00 GiB. \
                 The limits live in Settings > Resource Guard."
            );
            let row = spawn_error_row(ID, &refusal);
            assert_eq!(row.outcome, "skipped", "{lane}");
            assert_eq!(row.verdict, format!("skipped(resource_guard:{lane})"));
            // A seam that prefixes its own context still reads as the refusal.
            let wrapped = spawn_error_row(ID, &format!("create terminal: {refusal}"));
            assert_eq!(wrapped.verdict, format!("skipped(resource_guard:{lane})"));
        }
        assert_eq!(
            spawn_error_row(ID, &format!("{prefix}: no token")).verdict,
            "skipped(resource_guard:unknown)"
        );
        let real = spawn_error_row(ID, "pty open failed: os error 24");
        assert_eq!(real.outcome, "failed");
        assert_eq!(real.verdict, "failed(spawn: pty open failed: os error 24)");
    }

    #[test]
    fn a_drain_defers_the_route_and_never_the_operator_twin() {
        use crate::coord_drain_state::{gate_with_local_hold, CoordDrainState};
        let now = chrono::Utc::now();
        let drained = CoordDrainState::Drained {
            until: None,
            reason: Some("operator drain".to_string()),
        };
        let operator = crate::commands::operator_doors::OPERATOR;
        for local_hold in [None, Some("prepare p — hold local")] {
            let route = admit(
                gate_with_local_hold(&drained, HTTP_ORIGIN, local_hold, now),
                || true,
            );
            assert!(
                route.as_ref().is_err_and(|e| e.starts_with("drain: ")),
                "the route under a drain is deferred: {route:?}"
            );
            let twin = admit(
                gate_with_local_hold(&drained, operator, local_hold, now),
                || true,
            );
            assert_eq!(twin, Ok(()), "the operator twin is never deferred");
        }
        // A runner-local restart hold alone (coord clear) defers the route too.
        let held = admit(
            gate_with_local_hold(&CoordDrainState::Clear, HTTP_ORIGIN, Some("hold"), now),
            || true,
        );
        assert!(held.is_err());
    }

    #[test]
    fn the_http_route_resumes_as_an_autonomous_respawn() {
        assert_eq!(HTTP_ORIGIN, SpawnOrigin::Respawn);
        assert!(HTTP_ORIGIN.is_autonomous());
    }

    #[test]
    fn verdict_strings_use_the_plan_spelling() {
        assert_eq!(
            ResumeRow::skipped("i", "cap_reached").verdict,
            "skipped(cap_reached)"
        );
        assert_eq!(ResumeRow::failed("i", "boom", None).verdict, "failed(boom)");
        let report = tally(
            Some("g1".into()),
            3,
            vec![
                ResumeRow::skipped("a", "x"),
                ResumeRow::failed("b", "y", None),
                ResumeRow::skipped("c", "z"),
            ],
        );
        assert_eq!((report.resumed, report.failed, report.skipped), (0, 1, 2));
        assert!(!report.generation_applied);
    }
}
