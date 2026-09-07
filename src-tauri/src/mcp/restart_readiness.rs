//! `GET /restart-readiness` — the one surface that answers *"is it safe to
//! restart this runner?"* (plan
//! `2026-08-29-no-single-answer-to-is-it-safe-to-restart-the-runner`, Phase 1).
//!
//! ## The incident this exists to prevent
//!
//! An operator nearly restarted a runner carrying 23 live agent sessions.
//! They reasoned correctly from the evidence the runner offered:
//! `GET /task-runs/running` returned `[]` (it is a port-filtered *workflow*
//! ledger) and `GET /sessions/history` showed one closed row (it is a
//! DISPLAY-only record of *past* terminal sessions). Both answered their own
//! narrow question truthfully; both read as *idle*. The authoritative count
//! was a nested field inside `/health` → `data.sessionTracking`, under a key
//! that reads like a subsystem-health metric.
//!
//! ## Two disjoint session planes — the crux
//!
//! There is no single number, so this endpoint never emits one (D6):
//!
//! | | `terminal_sessions` | `ai_sessions` |
//! |---|---|---|
//! | Population | `claude` processes in the runner's **inclusive process subtree**, cross-referenced against open `SessionLifecycleStore` records — i.e. terminal-hosted agent sessions ([`crate::session::tracking_health`]) | `SessionManager::active_claude_sessions()` — the AI / task-run plane, keyed by `task_run_id` |
//! | `POST /drain` acts on it | **NO** | yes |
//!
//! The census **explicitly exempts** the AI plane (it subtracts precisely the
//! set `drain()` operates on), so a session counted in `terminal_sessions` is
//! by definition a session a drain will not touch. Measured on `merytshost`
//! 2026-08-29: `liveClaudeTotal: 25` while `/task-runs/running` returned `[]`,
//! so a drain right then would have taken its
//! `"drain: no live AI sessions — fast no-op"` branch and reported
//! `drained_sessions: 0` while 25 live agent sessions carried on.
//!
//! ## Three populations, not one (2026-09-07)
//!
//! The census plane above is itself made of **four disjoint classes**
//! ([`crate::session::tracking_health::TrackingHealthReport`]): terminal-hosted,
//! the AI plane, agent-runtime **headless-exempt** children, and the
//! unclassified residue. This endpoint used to surface their SUM as
//! `terminal_sessions.count` and render it as *"N terminal-hosted agent
//! sessions"*. On a headless box that is false about every one of them:
//! measured here 2026-09-07, `count: 9` with `tracked_open_total: 0`,
//! `sessions: []`, `ai_sessions.count: 0` — nine live agent `claude` processes,
//! every detail array empty, and the one label attached to them describing a
//! plane with no members on that box.
//!
//! So `terminal_sessions.count` now means **terminal-hosted only**;
//! `headless_sessions` is its own plane with its own count and per-process
//! detail; and `live_claude` carries the TOTAL so nothing that read the old
//! number loses it. **The verdict is unchanged** — `safe_to_restart` is still
//! `live_claude_total == 0 && ai_sessions.count == 0`. Splitting a count is a
//! labelling fix; it must never become a safety change. Plan
//! `2026-09-07-restart-readiness-counts-headless-exempt-sessions-as-terminal-hosted`.
//!
//! Note the corollary the detail makes visible: these are **processes**, and one
//! agent session routinely fans out into several nested subagent `claude`
//! children (4 top-level vs 9 processes, same measurement). `root_count` is the
//! honest "how many sessions"; `count` is the honest "how many processes".
//!
//! **Hence D3: this endpoint never emits `drain_required`, and never
//! recommends a drain for the terminal plane.** A verdict that said
//! *"unsafe → drain → now safe"* for that population would manufacture a false
//! safe carrying the runner's own authority — strictly worse than the status
//! quo it replaces.
//!
//! ## Fresh, not cached (D5)
//!
//! The verdict calls [`crate::session::tracking_health::compute`] on demand.
//! It never reads `tracking_health::latest()`: `CHECK_INTERVAL` is **600 s**,
//! so the `/health` cache is routinely minutes stale (measured: `lastCheckAt`
//! advanced exactly once across 19 polls, by 600,024 ms), and for the first
//! 120 s after boot the count fields are *absent from the object entirely* —
//! a consumer's `?? 0` reads that as idle during the highest-risk window there
//! is, because boot-restore is re-opening exactly the sessions a restart would
//! destroy. The background census's age is reported in `census` as
//! **observability only**; the verdict does not depend on it.
//!
//! Computing fresh is reuse, not a second census (D1): it is the same
//! `evaluate` body, over the same handles, keyed on the same
//! `primary_boot_unix_millis`. Nothing here counts anything on its own.
//!
//! ## Fail closed
//!
//! `safe_to_restart` is `false` on every unknown — an unreadable process
//! table, an unresolvable `SessionManager`/`TerminalManager`/lifecycle store,
//! an uninitialized PID-reuse reference — with the cause named in `reason` and
//! the affected plane serialized as `null` rather than `0`. This surface is
//! consulted precisely when someone is about to do something destructive.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::mcp::types::ApiState;
use crate::session::session_lifecycle_store::TerminalSessionRecord;
use crate::session::tracking_health::{self, LiveClaudeProcess, TrackingHealthReport};

/// What the subtree cross-reference structurally cannot see. Emitted verbatim
/// on every response so a reader is never invited to infer omniscience from a
/// confident-looking count.
pub const BOUNDARY: &str = "counts `claude` PROCESSES in this runner's inclusive process subtree — each process, so a nested subagent counts alongside the agent that spawned it (`nested_under_claude` marks those, and `root_count` excludes them); a session doing non-`claude` work, or a child that escaped the subtree, is not represented; `cwd` is read from `/proc/<pid>/cwd` and is null on Windows and for any pid whose link could not be resolved; `has_live_children` is a hint that a child process is attached right now, never a verdict that a session is busy or idle";

/// `drain.covers` — the constant, honest scope of `POST /drain`.
pub const DRAIN_COVERS: &str = "ai_sessions only";

/// How many recent task runs to pull when resolving AI-plane `age_s`. One
/// lightweight, deliberately **un**-port-filtered query (the port filter is
/// what made `/task-runs/running` return `[]` during the incident).
const TASK_RUN_AGE_LOOKUP_LIMIT: u32 = 500;

// ---------------------------------------------------------------------------
// Response shape
// ---------------------------------------------------------------------------

/// One live session in either plane.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionEntry {
    pub id: String,
    /// Seconds since this session started, or `null` where no creation
    /// timestamp is reachable. NEVER a fabricated value: `ClaudeSession` has
    /// no creation field at all (only `last_activity_tracker()`, which mutates
    /// on activity and is not a start time), so an AI-plane entry whose
    /// `task_runs` join misses is honestly unknown.
    pub age_s: Option<i64>,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// The terminal-hosted agent-session plane — the population in the incident.
///
/// ⚠ **`count` changed meaning on 2026-09-07.** It was `live_claude_total`, the
/// sum of every live `claude` in the subtree including the headless-exempt
/// ones; it is now **terminal-hosted only**. The old number lives on, unchanged,
/// as [`LiveClaudeTotals::total`].
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TerminalPlane {
    /// Live `claude` PROCESSES claimed by a live tracked terminal. Not
    /// sessions: see `root_count`.
    pub count: usize,
    /// Of `count`, the processes that are NOT nested subagents — the closest
    /// honest analogue of "how many terminal-hosted sessions".
    pub root_count: usize,
    /// **Always `false`.** `drain()` operates on `active_claude_sessions()`,
    /// a set the census explicitly exempts. See D3.
    pub drain_covers_these: bool,
    /// Open `SessionLifecycleStore` records at compute time.
    pub tracked_open_total: usize,
    /// Live `claude` processes NOTHING accounts for — no live terminal, no AI
    /// session, no headless registration. These would be dropped silently by a
    /// restart and are absent from `sessions` below; `unclassified_processes`
    /// now names them instead of leaving only a count.
    pub live_untracked_count: usize,
    /// Open records whose terminal is gone / whose subtree has no live
    /// `claude`. Reported for observability; does NOT affect the verdict.
    pub tracked_dead_count: usize,
    /// Max over the non-`null` `age_s` in `sessions`, or `null`.
    pub oldest_session_age_s: Option<i64>,
    /// The live tracked sessions (open records minus the tracked-dead ones).
    pub sessions: Vec<SessionEntry>,
    /// Per-process detail for the terminal-hosted plane: pid, age, cwd,
    /// children hint. Complements `sessions`, which is record-shaped.
    pub processes: Vec<LiveClaudeProcess>,
    /// Per-process detail for `live_untracked_count`. Previously a count with
    /// no names at all — on a headless box that is the whole of what an
    /// operator could learn about a session a restart would silently destroy.
    pub unclassified_processes: Vec<LiveClaudeProcess>,
}

/// The **headless-exempt** plane: `claude` children the agent runtime owns
/// directly (`register_headless_claude_pid` — direct tokio children, no PTY, no
/// `capture_hint`).
///
/// This plane had no field at all before 2026-09-07: its members were counted
/// inside `terminal_sessions.count` and described as terminal-hosted, which
/// they are not. On a headless box (no display, no terminal panes, no lifecycle
/// records) it is typically the ONLY non-empty plane, and
/// `GET /restart-readiness` is the only window onto it — hence the per-process
/// detail rather than a bare number.
///
/// `drain_covers_these` is **`false`**, exactly as for the terminal plane:
/// `POST /drain` acts on `active_claude_sessions()` only ([`DRAIN_COVERS`]), so
/// D3 holds here too — nothing about this plane may be read as "drain, then
/// restart".
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HeadlessPlane {
    /// Live headless `claude` PROCESSES. Not sessions: see `root_count`.
    pub count: usize,
    /// Of `count`, the processes whose parent is not itself a counted
    /// `claude` — i.e. top-level agent sessions rather than their nested
    /// subagents. Measured here 2026-09-07: `count: 9`, `root_count: 4`.
    pub root_count: usize,
    /// **Always `false`** — see D3 and [`DRAIN_COVERS`].
    pub drain_covers_these: bool,
    /// Max over the non-`null` `age_s` in `processes`, or `null`.
    pub oldest_age_s: Option<i64>,
    /// pid, parent, age, cwd (the agent worktree), and the children hint.
    pub processes: Vec<LiveClaudeProcess>,
}

/// The live-`claude` census total and its four-way split — emitted so that
/// splitting `terminal_sessions.count` regresses no consumer: `total` is the
/// exact number that field carried before 2026-09-07, and it is what
/// `safe_to_restart` reads.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LiveClaudeTotals {
    /// Every live `claude` process in the runner's inclusive subtree.
    pub total: usize,
    pub terminal_hosted: usize,
    pub ai_plane: usize,
    pub headless_exempt: usize,
    /// Claimed by nothing. Blocks a restart like any other live process —
    /// fail-closed: what the runner cannot explain, it does not wave through.
    pub unclassified: usize,
}

/// The AI / task-run plane — `SessionManager::active_claude_sessions()`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AiPlane {
    pub count: usize,
    /// `true` — this is the only plane `POST /drain` acts on.
    pub drain_covers_these: bool,
    /// Subset holding an isolated `worktree()`. A drain writes
    /// `refs/wip/<agent_session_id>` only for these; a session in the shared
    /// cwd is deliberately skipped so a shared checkout is not polluted.
    pub wip_capture_eligible: usize,
    pub oldest_session_age_s: Option<i64>,
    pub sessions: Vec<SessionEntry>,
    /// The census's view of this plane: the live `claude` PROCESSES whose
    /// subtree an AI-plane root claims, with pid/age/cwd/children-hint.
    ///
    /// `sessions` is keyed by `task_run_id` from `SessionManager` and answers
    /// "which runs are open"; this answers "which processes are on the box".
    /// They are different questions and can legitimately differ in length —
    /// one run can own several `claude` processes, and a run whose process has
    /// exited leaves a session entry with no process here.
    pub processes: Vec<LiveClaudeProcess>,
}

/// Drain state, reported — never performed. A drain is TERMINAL (`DRAINING`
/// is never reset), so triggering one on an operator's behalf would silently
/// end the runner's useful life.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DrainInfo {
    pub already_drained: bool,
    pub is_draining: bool,
    /// `true` when `ai_sessions.count == 0` (or a drain already completed) —
    /// i.e. calling `POST /drain` right now would change nothing.
    pub would_be_noop: bool,
    /// Always [`DRAIN_COVERS`].
    pub covers: &'static str,
    pub call: String,
}

/// Age/health of the BACKGROUND `tracking_health` task.
///
/// ⚠ **Observability only.** This endpoint computes its own fresh pass; none
/// of these fields feeds `safe_to_restart` (D5).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CensusInfo {
    /// `checked_at_ms` of the cached report `/health` serves, or `null` before
    /// the first background pass completes.
    pub background_last_check_at: Option<i64>,
    pub background_age_s: Option<i64>,
    pub check_interval_s: u64,
    /// `false` when `background_age_s > 2 * check_interval_s`; `null` while no
    /// background pass has completed (the task's 120 s initial delay may
    /// simply not have elapsed — unknown, not healthy).
    pub periodic_task_healthy: Option<bool>,
}

/// The `GET /restart-readiness` body.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RestartReadiness {
    pub safe_to_restart: bool,
    pub reason: String,
    /// `null` when the plane could not be determined — never `0`.
    pub terminal_sessions: Option<TerminalPlane>,
    /// The agent-runtime headless plane. `null` when it could not be
    /// determined — never `0`. Resolves from the SAME `tracking_health` pass
    /// as `terminal_sessions`, so the two are `Some`/`None` together.
    pub headless_sessions: Option<HeadlessPlane>,
    /// `null` when the plane could not be determined — never `0`.
    pub ai_sessions: Option<AiPlane>,
    /// The census total and its split. `null` on the same unknown as the
    /// terminal plane. `total` is the pre-2026-09-07
    /// `terminal_sessions.count`.
    pub live_claude: Option<LiveClaudeTotals>,
    pub drain: DrainInfo,
    pub census: CensusInfo,
    pub boundary: &'static str,
}

// ---------------------------------------------------------------------------
// Pure shaping + verdict
// ---------------------------------------------------------------------------

fn age_s_from(started_ms: i64, now_ms: i64) -> Option<i64> {
    if started_ms <= 0 {
        return None;
    }
    let secs = (now_ms - started_ms) / 1000;
    // A record stamped in the future is a clock artifact, not an age.
    if secs < 0 {
        None
    } else {
        Some(secs)
    }
}

fn oldest(sessions: &[SessionEntry]) -> Option<i64> {
    sessions.iter().filter_map(|s| s.age_s).max()
}

/// Max over the non-`null` `age_s` of a live-process list.
fn oldest_process(procs: &[LiveClaudeProcess]) -> Option<i64> {
    procs.iter().filter_map(|p| p.age_s).max()
}

/// Shape the terminal plane from one [`tracking_health`] pass.
///
/// `sessions` is the open records MINUS the pass's `tracked_dead` set (a stale
/// row masquerading as a restorable session is not live work).
///
/// ⚠ `count` is the pass's **`terminal_hosted`** class, not `live_claude_total`.
/// Before 2026-09-07 it was the total, which meant a box whose every live
/// `claude` was headless-exempt reported them all here and the reason string
/// called them terminal-hosted. The total is emitted separately by
/// [`live_claude_totals_from`]; the verdict reads that, so the change is
/// labelling only.
pub fn terminal_plane_from(
    report: &TrackingHealthReport,
    open_records: &[TerminalSessionRecord],
    now_ms: i64,
) -> TerminalPlane {
    let dead: std::collections::HashSet<&str> = report
        .tracked_dead
        .iter()
        .map(|d| d.claude_session_id.as_str())
        .collect();

    let sessions: Vec<SessionEntry> = open_records
        .iter()
        .filter(|r| !dead.contains(r.claude_session_id.as_str()))
        .map(|r| SessionEntry {
            id: r.claude_session_id.clone(),
            age_s: age_s_from(r.opened_at, now_ms),
            state: r.state.clone(),
            terminal_id: Some(r.terminal_id.clone()),
            title: r.title.clone(),
        })
        .collect();

    TerminalPlane {
        count: report.terminal_hosted.len(),
        root_count: TrackingHealthReport::root_count(&report.terminal_hosted),
        // D3: never true. `drain()` acts on a set this census subtracts.
        drain_covers_these: false,
        tracked_open_total: report.tracked_open_total,
        live_untracked_count: report.live_untracked.len(),
        tracked_dead_count: report.tracked_dead.len(),
        oldest_session_age_s: oldest(&sessions),
        sessions,
        processes: report.terminal_hosted.clone(),
        unclassified_processes: report.live_untracked.clone(),
    }
}

/// Shape the headless-exempt plane from the SAME pass — no second census.
pub fn headless_plane_from(report: &TrackingHealthReport) -> HeadlessPlane {
    HeadlessPlane {
        count: report.headless_exempt.len(),
        root_count: TrackingHealthReport::root_count(&report.headless_exempt),
        // D3 applies here too: `POST /drain` covers `ai_sessions` only.
        drain_covers_these: false,
        oldest_age_s: oldest_process(&report.headless_exempt),
        processes: report.headless_exempt.clone(),
    }
}

/// The census total plus its four-way split, from the same pass.
///
/// `total` is `live_claude_total` verbatim — the number `terminal_sessions.count`
/// carried before the split, and the number the verdict reads. Emitting it keeps
/// the split a labelling change rather than a loss of information.
pub fn live_claude_totals_from(report: &TrackingHealthReport) -> LiveClaudeTotals {
    LiveClaudeTotals {
        total: report.live_claude_total,
        terminal_hosted: report.terminal_hosted.len(),
        ai_plane: report.ai_plane.len(),
        headless_exempt: report.headless_exempt.len(),
        unclassified: report.live_untracked.len(),
    }
}

/// One AI-plane session as collected from `SessionManager` + the `task_runs`
/// creation-time join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiSessionInput {
    /// `task_run_id` — the AI plane's key.
    pub id: String,
    pub state: String,
    /// Holds an isolated `worktree()`, so a drain could write a WIP ref.
    pub has_worktree: bool,
    /// `task_runs.created_at` in unix millis, or `None` where the join missed.
    pub created_at_ms: Option<i64>,
}

/// `processes` is the census's view of the AI plane (the pass's `ai_plane`
/// class); pass `&[]` where no pass resolved. It is deliberately separate from
/// `entries`, which are `SessionManager` rows — see [`AiPlane::processes`].
pub fn ai_plane_from(
    entries: &[AiSessionInput],
    processes: &[LiveClaudeProcess],
    now_ms: i64,
) -> AiPlane {
    let sessions: Vec<SessionEntry> = entries
        .iter()
        .map(|e| SessionEntry {
            id: e.id.clone(),
            age_s: e.created_at_ms.and_then(|ms| age_s_from(ms, now_ms)),
            state: e.state.clone(),
            terminal_id: None,
            title: None,
        })
        .collect();

    AiPlane {
        count: entries.len(),
        drain_covers_these: true,
        wip_capture_eligible: entries.iter().filter(|e| e.has_worktree).count(),
        oldest_session_age_s: oldest(&sessions),
        sessions,
        processes: processes.to_vec(),
    }
}

/// Shape the background-census observability block. Never feeds the verdict.
pub fn census_info(latest: Option<&TrackingHealthReport>, now_ms: i64) -> CensusInfo {
    let interval_s = tracking_health::CHECK_INTERVAL.as_secs();
    match latest {
        Some(r) => {
            let age_s = ((now_ms - r.checked_at_ms) / 1000).max(0);
            CensusInfo {
                background_last_check_at: Some(r.checked_at_ms),
                background_age_s: Some(age_s),
                check_interval_s: interval_s,
                periodic_task_healthy: Some(age_s <= (interval_s as i64) * 2),
            }
        }
        None => CensusInfo {
            background_last_check_at: None,
            background_age_s: None,
            check_interval_s: interval_s,
            // Unknown, not healthy — the 120 s initial delay may not have
            // elapsed, or the task may have died before its first pass.
            periodic_task_healthy: None,
        },
    }
}

/// Compose the verdict.
///
/// **Fail closed.** Any `unknowns` entry, or either plane missing, forces
/// `safe_to_restart: false` with the cause named. There is no path on which an
/// unknown resolves to `true`.
///
/// **D3.** The reason string never recommends a drain, and no `drain_required`
/// field exists to be set.
#[allow(clippy::too_many_arguments)]
pub fn build_verdict(
    terminal: Option<TerminalPlane>,
    headless: Option<HeadlessPlane>,
    ai: Option<AiPlane>,
    totals: Option<LiveClaudeTotals>,
    unknowns: Vec<String>,
    drain: DrainInfo,
    census: CensusInfo,
) -> RestartReadiness {
    let mut unknowns = unknowns;
    if terminal.is_none() && !unknowns.iter().any(|u| u.contains("terminal")) {
        unknowns.push("the terminal-session plane could not be determined".to_string());
    }
    // The headless plane and the totals come from the SAME `tracking_health`
    // pass as the terminal plane, so either missing on its own is a coding
    // error rather than a live unknown — say so explicitly rather than
    // silently emitting a `null` beside a confident verdict.
    if headless.is_none() && !unknowns.iter().any(|u| u.contains("headless")) {
        unknowns.push("the headless agent-runtime plane could not be determined".to_string());
    }
    if totals.is_none() && !unknowns.iter().any(|u| u.contains("live-`claude` census")) {
        unknowns.push("the live-`claude` census total could not be determined".to_string());
    }
    if ai.is_none() && !unknowns.iter().any(|u| u.contains("AI")) {
        unknowns.push("the AI/task-run plane could not be determined".to_string());
    }

    if !unknowns.is_empty() {
        return RestartReadiness {
            safe_to_restart: false,
            reason: format!("UNKNOWN, so treated as unsafe: {}", unknowns.join("; ")),
            terminal_sessions: terminal,
            headless_sessions: headless,
            ai_sessions: ai,
            live_claude: totals,
            drain,
            census,
            boundary: BOUNDARY,
        };
    }

    // Every plane resolved.
    let t = terminal.expect("checked above");
    let h = headless.expect("checked above");
    let a = ai.expect("checked above");
    let totals = totals.expect("checked above");

    let mut parts: Vec<String> = Vec::new();
    if t.count > 0 {
        parts.push(format!(
            "{} terminal-hosted agent `claude` process{} {} live ({} top-level); no graceful stop path exists for {}, so {} in-flight work will be lost",
            t.count,
            if t.count == 1 { "" } else { "es" },
            if t.count == 1 { "is" } else { "are" },
            t.root_count,
            if t.count == 1 { "it" } else { "them" },
            if t.count == 1 { "its" } else { "their" },
        ));
    }
    if h.count > 0 {
        // NEVER call these terminal-hosted. They are direct children of the
        // runner with no PTY and no lifecycle record BY DESIGN, and on a
        // headless box they are typically the whole population.
        parts.push(format!(
            "{} headless agent `claude` process{} {} live as direct child{} of this runner ({} top-level, {} nested subagent{}); they are NOT terminal-hosted and `POST /drain` does not cover them, so a restart kills them mid-work — see `headless_sessions.processes` for pid, age and cwd",
            h.count,
            if h.count == 1 { "" } else { "es" },
            if h.count == 1 { "is" } else { "are" },
            if h.count == 1 { "" } else { "ren" },
            h.root_count,
            h.count - h.root_count,
            if h.count - h.root_count == 1 { "" } else { "s" },
        ));
    }
    if t.live_untracked_count > 0 {
        parts.push(format!(
            "{} live `claude` process{} match{} no live terminal, no AI session and no headless registration — unclassified, so counted as live work and named in `terminal_sessions.unclassified_processes`",
            t.live_untracked_count,
            if t.live_untracked_count == 1 { "" } else { "es" },
            if t.live_untracked_count == 1 { "es" } else { "" },
        ));
    }
    if a.count > 0 {
        parts.push(format!(
            "{} AI/task-run session{} {} live ({} hold an isolated worktree)",
            a.count,
            if a.count == 1 { "" } else { "s" },
            if a.count == 1 { "is" } else { "are" },
            a.wip_capture_eligible,
        ));
    }

    if a.count == 0 && totals.ai_plane > 0 {
        // The census attributed live processes to an AI-plane root while
        // `SessionManager::active_claude_sessions()` reports no open run — a
        // race, or a leaked child. Either way it is live work, and it must not
        // fall through to a reason string that names nothing.
        parts.push(format!(
            "{} live `claude` process{} sit{} under the AI/task-run plane's roots while no AI session is reported open — a leaked or racing child, counted as live work",
            totals.ai_plane,
            if totals.ai_plane == 1 { "" } else { "es" },
            if totals.ai_plane == 1 { "s" } else { "" },
        ));
    }

    // UNCHANGED verdict: `totals.total` IS the pre-split
    // `terminal_sessions.count` (`live_claude_total`), so this is bit-for-bit
    // the old `t.count == 0 && a.count == 0`. Splitting a count is a labelling
    // fix and must not become a safety change — and every class, including the
    // unclassified residue, still blocks.
    let safe = totals.total == 0 && a.count == 0;
    let reason = if safe {
        "no live agent sessions in any plane".to_string()
    } else if parts.is_empty() {
        // Unreachable given `safe` above, but a reason string is never empty on
        // an unsafe verdict: an operator reading a bare `false` learns nothing.
        format!(
            "{} live `claude` process(es) in the runner's subtree could not be attributed to a named plane",
            totals.total
        )
    } else {
        parts.join("; ")
    };

    RestartReadiness {
        safe_to_restart: safe,
        reason,
        terminal_sessions: Some(t),
        headless_sessions: Some(h),
        ai_sessions: Some(a),
        live_claude: Some(totals),
        drain,
        census,
        boundary: BOUNDARY,
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Parse `task_runs.created_at` (RFC 3339, per `PgDb`'s `to_rfc3339()`) into
/// unix millis. Anything unparseable is `None` — an honest unknown.
fn rfc3339_to_millis(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// `GET /restart-readiness` — see the module docs.
pub async fn restart_readiness_handler(
    State(state): State<Arc<ApiState>>,
) -> Json<RestartReadiness> {
    use tauri::Manager;

    let now_ms = chrono::Utc::now().timestamp_millis();
    let app = &state.app_handle;
    let mut unknowns: Vec<String> = Vec::new();

    // ── Drain state: reported, never performed ────────────────────────────
    let port = state
        .app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);
    let already_drained = crate::drain::already_drained();

    // ── AI plane ──────────────────────────────────────────────────────────
    let ai_raw: Option<Vec<(String, String, bool)>> =
        match app.try_state::<Arc<crate::claude_session::SessionManager>>() {
            Some(sm) => Some(
                sm.active_claude_sessions()
                    .into_iter()
                    .map(|(task_run_id, session)| {
                        (
                            task_run_id,
                            session.state().to_string(),
                            session.worktree().is_some(),
                        )
                    })
                    .collect(),
            ),
            None => {
                unknowns.push(
                    "the AI/task-run plane could not be determined: SessionManager did not resolve"
                        .to_string(),
                );
                None
            }
        };

    // Creation times for the AI plane come from `task_runs.created_at` —
    // `ClaudeSession` carries none. ONE lightweight query, deliberately NOT
    // port-filtered (the port filter is exactly what made `/task-runs/running`
    // read as idle during the incident). A failure here costs `age_s: null`
    // per entry, never a fabricated age and never the verdict.
    let ai_inputs: Option<Vec<AiSessionInput>> = match ai_raw {
        Some(raw) if raw.is_empty() => Some(Vec::new()),
        Some(raw) => {
            let created: std::collections::HashMap<String, i64> = match state
                .app_state
                .pg_db
                .get_recent_task_runs(TASK_RUN_AGE_LOOKUP_LIMIT, None)
                .await
            {
                Ok(runs) => runs
                    .into_iter()
                    .filter_map(|r| rfc3339_to_millis(&r.created_at).map(|ms| (r.id, ms)))
                    .collect(),
                Err(e) => {
                    tracing::debug!(
                        "restart-readiness: task_runs creation-time lookup failed ({e}) — \
                         AI-plane age_s will be null"
                    );
                    std::collections::HashMap::new()
                }
            };
            Some(
                raw.into_iter()
                    .map(|(id, st, has_worktree)| AiSessionInput {
                        created_at_ms: created.get(&id).copied(),
                        id,
                        state: st,
                        has_worktree,
                    })
                    .collect(),
            )
        }
        None => None,
    };

    // ── Terminal + headless planes: ONE fresh tracking_health pass (D5),
    //    never latest(). The pass partitions the live `claude` set, so all
    //    three census-derived planes and the totals come from a single
    //    `compute` — there is no second census here (D1).
    let pass = 'terminal: {
        let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
            unknowns.push(
                "the terminal-session plane could not be determined: TerminalManager did not resolve"
                    .to_string(),
            );
            break 'terminal None;
        };
        let Some(store) =
            app.try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        else {
            unknowns.push(
                "the terminal-session plane could not be determined: SessionLifecycleStore did not resolve"
                    .to_string(),
            );
            break 'terminal None;
        };
        let Some(sm) = app.try_state::<Arc<crate::claude_session::SessionManager>>() else {
            unknowns.push(
                "the terminal-session plane could not be determined: SessionManager did not resolve, so the exempt AI plane cannot be subtracted"
                    .to_string(),
            );
            break 'terminal None;
        };
        // NEVER `now()` here — that reference feeds the PID-reuse guard and
        // substituting it falsely flips live idle sessions to tracked-dead.
        let Some(boot_ms) = tracking_health::primary_boot_unix_millis() else {
            unknowns.push(
                "the terminal-session plane could not be determined: the PID-reuse guard's primary-boot reference is not initialized yet (the runner is still starting)"
                    .to_string(),
            );
            break 'terminal None;
        };

        match tracking_health::compute(tm.inner(), store.inner(), sm.inner(), boot_ms).await {
            Some(pass) => Some(pass),
            None => {
                unknowns.push(
                    "the terminal-session plane could not be determined: the process table is unreadable (snapshot_process_table_public returned an empty parent_map), so live `claude` processes cannot be enumerated"
                        .to_string(),
                );
                None
            }
        }
    };

    let terminal = pass
        .as_ref()
        .map(|p| terminal_plane_from(&p.report, &p.open_records, now_ms));
    let headless = pass.as_ref().map(|p| headless_plane_from(&p.report));
    let totals = pass.as_ref().map(|p| live_claude_totals_from(&p.report));
    // The AI plane joins `SessionManager` rows with the census's own view of
    // that plane's processes. A census that did not resolve costs `processes:
    // []` — an empty DETAIL array beside an explicit unknown in `unknowns`,
    // never a `0` that reads as idle.
    let ai_census: Vec<LiveClaudeProcess> = pass
        .as_ref()
        .map(|p| p.report.ai_plane.clone())
        .unwrap_or_default();
    let ai = ai_inputs.map(|inputs| ai_plane_from(&inputs, &ai_census, now_ms));

    let drain = DrainInfo {
        already_drained,
        is_draining: crate::drain::is_draining(),
        would_be_noop: already_drained || ai.as_ref().map(|p| p.count == 0).unwrap_or(false),
        covers: DRAIN_COVERS,
        call: format!("POST http://127.0.0.1:{port}/drain"),
    };

    let census = census_info(tracking_health::latest().as_ref(), now_ms);

    Json(build_verdict(
        terminal, headless, ai, totals, unknowns, drain, census,
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tracking_health::{
        evaluate, LiveClaudeProcess, TrackedDeadRecord, TrackingHealthReport,
    };
    use std::collections::{HashMap, HashSet};

    // Mirrors `tracking_health::tests::snap_with` — the same synthetic-snapshot
    // harness, so these tests drive the REAL `evaluate` body rather than a
    // hand-written stand-in.
    fn snap_with(
        parent_map: &[(u32, &[u32])],
        creation: &[(u32, i64)],
        names: &[(u32, &str)],
    ) -> crate::process_capture::process_tree::ProcessSnapshot {
        let mut s = crate::process_capture::process_tree::ProcessSnapshot::default();
        for (p, kids) in parent_map {
            s.parent_map.insert(*p, kids.to_vec());
        }
        for (pid, t) in creation {
            s.creation_times.insert(*pid, *t);
        }
        for (pid, n) in names {
            s.names.insert(*pid, n.to_string());
        }
        s
    }

    fn record(claude_session_id: &str, terminal_id: &str, opened_at: i64) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: claude_session_id.to_string(),
            config_dir: None,
            working_dir: Some("D:/work".to_string()),
            page_id: "default".to_string(),
            zone_index: 0,
            title: Some(format!("title-{claude_session_id}")),
            terminal_id: terminal_id.to_string(),
            opened_at,
            last_seen_at: opened_at,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: "claude".to_string(),
            origin: None,
            restore_pending_at: None,
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
        }
    }

    fn idle_drain() -> DrainInfo {
        DrainInfo {
            already_drained: false,
            is_draining: false,
            would_be_noop: true,
            covers: DRAIN_COVERS,
            call: "POST http://127.0.0.1:9876/drain".to_string(),
        }
    }

    /// A census pass with every class empty — an idle box.
    fn empty_report(now_ms: i64) -> TrackingHealthReport {
        TrackingHealthReport {
            checked_at_ms: now_ms,
            live_claude_total: 0,
            tracked_open_total: 0,
            terminal_hosted: vec![],
            ai_plane: vec![],
            headless_exempt: vec![],
            live_untracked: vec![],
            tracked_dead: vec![],
        }
    }

    fn fresh_census(now_ms: i64) -> CensusInfo {
        census_info(Some(&empty_report(now_ms - 60_000)), now_ms)
    }

    /// `build_verdict` over ONE pass, shaping every census-derived plane from
    /// the same report — exactly what the handler does, so a test can never
    /// hand the verdict a set of planes the handler could not produce.
    fn verdict_from(
        report: &TrackingHealthReport,
        open_records: &[TerminalSessionRecord],
        ai: Option<AiPlane>,
        unknowns: Vec<String>,
        drain: DrainInfo,
        census: CensusInfo,
        now_ms: i64,
    ) -> RestartReadiness {
        build_verdict(
            Some(terminal_plane_from(report, open_records, now_ms)),
            Some(headless_plane_from(report)),
            ai,
            Some(live_claude_totals_from(report)),
            unknowns,
            drain,
            census,
        )
    }

    /// **The D3 regression test — the one that matters most.**
    ///
    /// With terminal-hosted agent sessions live the verdict is `false`, and
    /// NOTHING in the response recommends a drain: `drain_covers_these` is
    /// `false`, `would_be_noop` is `true`, the reason never mentions a drain,
    /// and the field `drain_required` does not exist anywhere in the payload.
    ///
    /// Shipping this without the assertion would have produced a *worse*
    /// system than the status quo — an operator who drains, sees
    /// `drained_sessions: 0`, and restarts believing the work was captured.
    #[test]
    fn terminal_sessions_live_is_unsafe_and_never_recommends_a_drain() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        // Runner (1) → shell 5 → claude 10 (tracked "t-5"), shell 6 → claude 11
        // (tracked "t-6"). Two live terminal-hosted agent sessions.
        let snap = snap_with(
            &[(1, &[5, 6]), (5, &[10]), (6, &[11])],
            &[(5, now_s), (6, now_s), (10, now_s), (11, now_s)],
            &[
                (5, "powershell.exe"),
                (6, "powershell.exe"),
                (10, "claude.exe"),
                (11, "claude.exe"),
            ],
        );
        let opened_at = now_ms - 2_231_000;
        let records = vec![
            record("sess-a", "t-5", opened_at),
            record("sess-b", "t-6", now_ms - 60_000),
        ];
        let terminal_pids: HashMap<String, u32> =
            [("t-5".to_string(), 5u32), ("t-6".to_string(), 6u32)]
                .into_iter()
                .collect();

        let report = evaluate(
            &snap,
            1,
            &records,
            &terminal_pids,
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
            now_ms,
            now_ms,
        );
        assert_eq!(report.live_claude_total, 2);

        // The AI plane is genuinely empty — exactly the incident's shape.
        let ai = ai_plane_from(&[], &[], now_ms);

        let v = verdict_from(
            &report,
            &records,
            Some(ai),
            vec![],
            idle_drain(),
            fresh_census(now_ms),
            now_ms,
        );

        assert!(!v.safe_to_restart, "verdict must be unsafe: {v:?}");

        let t = v.terminal_sessions.as_ref().unwrap();
        assert_eq!(t.count, 2);
        assert_eq!(t.sessions.len(), 2);
        assert!(
            !t.drain_covers_these,
            "D3: a drain NEVER covers the terminal plane"
        );
        assert_eq!(t.oldest_session_age_s, Some(2231));

        let a = v.ai_sessions.as_ref().unwrap();
        assert_eq!(a.count, 0);
        assert!(a.drain_covers_these);

        assert!(
            v.drain.would_be_noop,
            "a drain with 0 AI sessions is a no-op"
        );
        assert_eq!(v.drain.covers, "ai_sessions only");

        // The reason must say what is lost, and must NOT point at the drain.
        assert!(
            !v.reason.to_lowercase().contains("drain"),
            "D3: the reason must not recommend a drain: {}",
            v.reason
        );
        assert!(v.reason.contains("no graceful stop path"), "{}", v.reason);
        assert!(v.reason.contains("will be lost"), "{}", v.reason);

        // And the deleted field must not have crept back in.
        let json = serde_json::to_string(&v).unwrap();
        assert!(
            !json.contains("drain_required"),
            "D3: `drain_required` is deleted, not renamed: {json}"
        );
        assert!(json.contains("\"boundary\""));
    }

    /// A genuinely idle runner: both planes empty → `safe_to_restart: true`.
    #[test]
    fn idle_runner_is_safe() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        // Runner (1) → a bare shell, no claude anywhere.
        let snap = snap_with(&[(1, &[5])], &[(5, now_s)], &[(5, "powershell.exe")]);

        let report = evaluate(
            &snap,
            1,
            &[],
            &HashMap::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
            now_ms,
            now_ms,
        );
        assert_eq!(report.live_claude_total, 0);

        let v = verdict_from(
            &report,
            &[],
            Some(ai_plane_from(&[], &[], now_ms)),
            vec![],
            idle_drain(),
            fresh_census(now_ms),
            now_ms,
        );

        assert!(v.safe_to_restart, "{v:?}");
        assert_eq!(v.reason, "no live agent sessions in any plane");
        assert_eq!(v.terminal_sessions.as_ref().unwrap().count, 0);
        assert_eq!(
            v.terminal_sessions.as_ref().unwrap().oldest_session_age_s,
            None
        );
        assert_eq!(v.ai_sessions.as_ref().unwrap().count, 0);
    }

    /// Fail-closed: an unreadable process table (empty `parent_map`) is the
    /// existing fail-OPEN skip for the periodic task and must be fail-CLOSED
    /// here, with the cause named and the plane serialized `null` — never `0`.
    #[test]
    fn empty_process_snapshot_is_unsafe_with_the_cause_named() {
        let now_ms = chrono::Utc::now().timestamp_millis();

        // `compute` returns None on an empty parent_map; the handler turns that
        // into this unknown. Assert the composition end of that contract.
        let cause = "the terminal-session plane could not be determined: the process table is \
                     unreadable (snapshot_process_table_public returned an empty parent_map), so \
                     live `claude` processes cannot be enumerated";
        let v = build_verdict(
            None,
            None,
            Some(ai_plane_from(&[], &[], now_ms)),
            None,
            vec![cause.to_string()],
            idle_drain(),
            fresh_census(now_ms),
        );

        assert!(!v.safe_to_restart, "an unknown must never read as safe");
        assert!(v.reason.starts_with("UNKNOWN, so treated as unsafe:"));
        assert!(
            v.reason.contains("process table is unreadable"),
            "{}",
            v.reason
        );
        assert!(v.terminal_sessions.is_none());

        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(
            json["terminal_sessions"],
            serde_json::Value::Null,
            "an undetermined plane is null, never 0"
        );
        assert_eq!(json["safe_to_restart"], serde_json::Value::Bool(false));
    }

    /// A missing plane with no explicit cause still fails closed and still
    /// names which plane went unknown (no silent `true`).
    #[test]
    fn missing_plane_without_a_stated_cause_still_fails_closed() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let report = empty_report(now_ms);
        let v = verdict_from(
            &report,
            &[],
            None,
            vec![],
            idle_drain(),
            fresh_census(now_ms),
            now_ms,
        );
        assert!(!v.safe_to_restart);
        assert!(v.reason.contains("AI/task-run plane"), "{}", v.reason);
        assert!(v.ai_sessions.is_none());
    }

    /// A stalled background census reports `periodic_task_healthy: false` —
    /// and the verdict is UNAFFECTED by it, because the verdict comes from
    /// this endpoint's own fresh pass (D5).
    #[test]
    fn stale_background_census_flags_the_task_but_not_the_verdict() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let interval_s = tracking_health::CHECK_INTERVAL.as_secs() as i64;

        // Stale: 3x the interval old (> the 2x threshold).
        let stale = TrackingHealthReport {
            checked_at_ms: now_ms - interval_s * 3 * 1000,
            terminal_hosted: vec![],
            ai_plane: vec![],
            headless_exempt: vec![],
            // Deliberately DISAGREES with the fresh pass below: the cache says
            // 7 sessions were live 30 minutes ago. If the verdict ever read the
            // cache, this test would fail.
            live_claude_total: 7,
            tracked_open_total: 7,
            live_untracked: vec![LiveClaudeProcess {
                pid: 42,
                parent_pid: Some(1),
                image: Some("claude.exe".to_string()),
                age_s: Some(90),
                cwd: None,
                has_live_children: false,
                nested_under_claude: false,
            }],
            tracked_dead: vec![TrackedDeadRecord {
                claude_session_id: "ghost".to_string(),
                terminal_id: "t-ghost".to_string(),
                title: None,
            }],
        };
        let census = census_info(Some(&stale), now_ms);
        assert_eq!(census.periodic_task_healthy, Some(false));
        assert_eq!(census.background_age_s, Some(interval_s * 3));
        assert_eq!(census.check_interval_s, interval_s as u64);

        // Fresh pass: genuinely idle.
        let fresh = empty_report(now_ms);
        let v = verdict_from(
            &fresh,
            &[],
            Some(ai_plane_from(&[], &[], now_ms)),
            vec![],
            idle_drain(),
            census,
            now_ms,
        );

        assert!(
            v.safe_to_restart,
            "a stalled BACKGROUND task must not flip the verdict (D5): {v:?}"
        );
        assert_eq!(v.census.periodic_task_healthy, Some(false));
        assert_eq!(v.terminal_sessions.as_ref().unwrap().count, 0);
    }

    /// Before the first background pass the census age is UNKNOWN — `null`,
    /// never "healthy" and never `0`.
    #[test]
    fn census_before_first_background_pass_is_unknown_not_healthy() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let c = census_info(None, now_ms);
        assert_eq!(c.background_last_check_at, None);
        assert_eq!(c.background_age_s, None);
        assert_eq!(c.periodic_task_healthy, None);

        let json = serde_json::to_value(c).unwrap();
        assert_eq!(json["periodic_task_healthy"], serde_json::Value::Null);
    }

    /// Live AI sessions: the plane is reported honestly (count, worktree
    /// eligibility, `age_s` null where the `task_runs` join missed), the
    /// verdict is unsafe, and `would_be_noop` is false.
    #[test]
    fn ai_plane_reports_wip_eligibility_and_null_age_on_a_missed_join() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let inputs = vec![
            AiSessionInput {
                id: "run-1".to_string(),
                state: "processing".to_string(),
                has_worktree: true,
                created_at_ms: Some(now_ms - 300_000),
            },
            AiSessionInput {
                id: "run-2".to_string(),
                state: "ready".to_string(),
                has_worktree: false,
                // Join missed — honestly unknown, NOT 0 and NOT `now`.
                created_at_ms: None,
            },
        ];
        let ai = ai_plane_from(&inputs, &[], now_ms);
        assert_eq!(ai.count, 2);
        assert_eq!(ai.wip_capture_eligible, 1);
        assert_eq!(ai.sessions[0].age_s, Some(300));
        assert_eq!(ai.sessions[1].age_s, None);
        assert_eq!(ai.oldest_session_age_s, Some(300));

        let report = empty_report(now_ms);
        let drain = DrainInfo {
            would_be_noop: false,
            ..idle_drain()
        };
        let v = verdict_from(
            &report,
            &[],
            Some(ai),
            vec![],
            drain,
            fresh_census(now_ms),
            now_ms,
        );
        assert!(!v.safe_to_restart);
        assert!(
            v.reason.contains("2 AI/task-run sessions are live"),
            "{}",
            v.reason
        );
        assert!(!v.drain.would_be_noop);

        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(
            json["ai_sessions"]["sessions"][1]["age_s"],
            serde_json::Value::Null
        );
    }

    /// A tracked-dead record is excluded from the live session list, while a
    /// live-but-untracked `claude` still counts toward `count` (it has no
    /// record to list) and is named in the reason.
    #[test]
    fn tracked_dead_excluded_and_live_untracked_counted() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        // Runner (1) → shell 5 → claude 10 (tracked "t-5"); stray claude 20 with
        // no record; record "sess-ghost" points at a terminal that is gone.
        let snap = snap_with(
            &[(1, &[5, 20]), (5, &[10])],
            &[(5, now_s), (10, now_s), (20, now_s)],
            &[
                (5, "powershell.exe"),
                (10, "claude.exe"),
                (20, "claude.exe"),
            ],
        );
        let records = vec![
            record("sess-live", "t-5", now_ms - 10_000),
            record("sess-ghost", "t-gone", now_ms - 10_000),
        ];
        let terminal_pids: HashMap<String, u32> = [("t-5".to_string(), 5u32)].into_iter().collect();

        let report = evaluate(
            &snap,
            1,
            &records,
            &terminal_pids,
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
            now_ms,
            now_ms,
        );
        let t = terminal_plane_from(&report, &records, now_ms);

        // POST-SPLIT: `count` is terminal-hosted ONLY — pid 10. The untracked
        // stray (pid 20) is no longer folded in here; it stays in
        // `live_untracked_count`, is NAMED in `unclassified_processes`, and
        // still shows up in the total below.
        assert_eq!(t.count, 1, "count is terminal-hosted only");
        assert_eq!(t.root_count, 1);
        assert_eq!(
            live_claude_totals_from(&report).total,
            2,
            "the pre-split number is still emitted, as live_claude.total"
        );
        assert_eq!(
            t.unclassified_processes
                .iter()
                .map(|p| p.pid)
                .collect::<Vec<_>>(),
            vec![20],
            "the stray is NAMED now, not just counted"
        );
        assert_eq!(t.tracked_open_total, 2);
        assert_eq!(t.live_untracked_count, 1);
        assert_eq!(t.tracked_dead_count, 1);
        let ids: Vec<&str> = t.sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["sess-live"],
            "the dead record is not a live session"
        );

        let v = verdict_from(
            &report,
            &records,
            Some(ai_plane_from(&[], &[], now_ms)),
            vec![],
            idle_drain(),
            fresh_census(now_ms),
            now_ms,
        );
        assert!(!v.safe_to_restart);
        // POST-SPLIT wording: the residue is called what it is — unclassified
        // — and the reason points at the array that names it, instead of
        // describing it as "N of those terminal-hosted sessions".
        assert!(v.reason.contains("1 terminal-hosted agent"), "{}", v.reason);
        assert!(
            v.reason.contains("unclassified, so counted as live work"),
            "{}",
            v.reason
        );
        assert!(
            v.reason
                .contains("terminal_sessions.unclassified_processes"),
            "{}",
            v.reason
        );
    }

    /// A record stamped in the future (clock skew) yields `age_s: null`, never
    /// a negative or fabricated age.
    #[test]
    fn future_or_missing_timestamps_yield_null_age() {
        let now_ms = 1_000_000_000i64;
        assert_eq!(age_s_from(0, now_ms), None);
        assert_eq!(age_s_from(-5, now_ms), None);
        assert_eq!(age_s_from(now_ms + 60_000, now_ms), None);
        assert_eq!(age_s_from(now_ms - 5_000, now_ms), Some(5));
    }

    // ------------------------------------------------------------------
    // The population split (plan
    // `2026-09-07-restart-readiness-counts-headless-exempt-sessions-as-terminal-hosted`)
    // ------------------------------------------------------------------

    /// The headless-box fixture, shaped like the live 2026-09-07 measurement:
    /// four direct agent-runtime `claude` children of the runner, two of which
    /// spawned a nested subagent `claude`; no terminals, no lifecycle records,
    /// no AI sessions. Returns the pass report.
    fn headless_box_report(now_ms: i64) -> TrackingHealthReport {
        let now_s = now_ms / 1000;
        let snap = snap_with(
            &[(1, &[10, 11, 12, 13]), (10, &[100, 900]), (11, &[110])],
            &[
                (10, now_s - 4_443),
                (11, now_s - 4_440),
                (12, now_s - 4_400),
                (13, now_s - 4_390),
                (100, now_s - 393),
                (110, now_s - 138),
                (900, now_s - 30),
            ],
            &[
                (10, "claude"),
                (11, "claude"),
                (12, "claude"),
                (13, "claude"),
                (100, "claude"),
                (110, "claude"),
                (900, "cargo"),
            ],
        );
        let cwds: HashMap<u32, String> = [
            (10u32, "/w/01a07bad-a553/qontinui-coord".to_string()),
            (11u32, "/w/01a07bad-ac3f/qontinui-coord".to_string()),
            (12u32, "/w/01a07bad-b2e3/qontinui-runner".to_string()),
            (13u32, "/w/01a07bad-c043/qontinui-runner".to_string()),
            (100u32, "/w/01a07bad-a553/qontinui-coord".to_string()),
            (110u32, "/w/01a07bad-ac3f/qontinui-coord".to_string()),
        ]
        .into_iter()
        .collect();
        let agent_runtime: HashSet<u32> = [10u32, 11, 12, 13].into_iter().collect();

        evaluate(
            &snap,
            1,
            &[],
            &HashMap::new(),
            &agent_runtime,
            &HashSet::new(),
            &cwds,
            now_ms,
            now_ms,
        )
    }

    /// **The regression this plan exists for.**
    ///
    /// On a headless box every live `claude` is headless-exempt. The response
    /// must file them under `headless_sessions`, must NOT call them
    /// terminal-hosted, must still block the restart, and must still keep the
    /// pre-split total available.
    #[test]
    fn all_headless_reason_never_says_terminal_hosted() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let report = headless_box_report(now_ms);
        let v = verdict_from(
            &report,
            &[],
            Some(ai_plane_from(&[], &report.ai_plane, now_ms)),
            vec![],
            idle_drain(),
            fresh_census(now_ms),
            now_ms,
        );

        assert!(
            !v.safe_to_restart,
            "6 live agent processes must block: {v:?}"
        );

        let t = v.terminal_sessions.as_ref().unwrap();
        assert_eq!(t.count, 0, "nothing here is terminal-hosted");
        assert_eq!(t.tracked_open_total, 0);
        assert_eq!(t.live_untracked_count, 0);
        assert!(t.sessions.is_empty());

        let h = v.headless_sessions.as_ref().unwrap();
        assert_eq!(h.count, 6, "the population is the headless plane");
        assert_eq!(h.root_count, 4, "4 agent sessions, 6 processes");
        assert!(!h.drain_covers_these, "D3 holds for this plane too");
        assert_eq!(h.processes.len(), 6);

        let a = v.ai_sessions.as_ref().unwrap();
        assert_eq!(a.count, 0);

        // The pre-split number is not lost.
        let totals = v.live_claude.as_ref().unwrap();
        assert_eq!(totals.total, 6);
        assert_eq!(totals.terminal_hosted, 0);
        assert_eq!(totals.headless_exempt, 6);
        assert_eq!(totals.ai_plane, 0);
        assert_eq!(totals.unclassified, 0);
        assert_eq!(
            totals.terminal_hosted + totals.ai_plane + totals.headless_exempt + totals.unclassified,
            totals.total
        );

        // The reason names the right population and slanders no other. The
        // only occurrence of "terminal-hosted" allowed here is the DENIAL the
        // headless clause carries; the terminal clause ("N terminal-hosted
        // agent `claude` process…") must be absent entirely.
        assert!(
            !v.reason.contains("terminal-hosted agent"),
            "headless children must never be described as terminal-hosted \
             agent sessions: {}",
            v.reason
        );
        assert!(
            v.reason.contains("NOT terminal-hosted"),
            "the denial is the point: {}",
            v.reason
        );
        assert!(
            v.reason.contains("headless agent `claude` process"),
            "{}",
            v.reason
        );
        assert!(
            v.reason.contains("4 top-level, 2 nested subagents"),
            "{}",
            v.reason
        );
        // D3 survives the rewrite. The reason may MENTION a drain only to DENY
        // that it covers this plane — never to recommend one. That denial is
        // load-bearing: the incident's false-safe was exactly an operator
        // draining, seeing `drained_sessions: 0`, and restarting. Assert every
        // occurrence of "drain" sits inside the denial.
        assert_eq!(
            v.reason.matches("drain").count(),
            v.reason
                .matches("`POST /drain` does not cover them")
                .count(),
            "the only permitted mention of a drain is the denial: {}",
            v.reason
        );
        assert!(!v.drain.covers.contains("headless"));
        assert_eq!(v.drain.covers, DRAIN_COVERS);

        // D6/D3 at the wire level: no `drain_required` key anywhere.
        let json = serde_json::to_value(&v).unwrap();
        assert!(
            !serde_json::to_string(&json)
                .unwrap()
                .contains("drain_required"),
            "D3: the field must not exist"
        );
        assert_eq!(json["headless_sessions"]["count"], 6);
        assert_eq!(json["live_claude"]["total"], 6);
    }

    /// The detail an operator on a headless box actually needs: pid, age, cwd
    /// (the agent worktree), and the children hint — for every live process,
    /// not just a number.
    #[test]
    fn headless_processes_carry_pid_age_cwd_and_children_hint() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let report = headless_box_report(now_ms);
        let h = headless_plane_from(&report);

        assert_eq!(
            h.processes.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![10, 11, 12, 13, 100, 110]
        );
        for p in &h.processes {
            assert!(p.age_s.is_some(), "pid {} has no age", p.pid);
            assert!(p.cwd.is_some(), "pid {} has no cwd", p.pid);
        }
        assert_eq!(h.oldest_age_s, Some(4_443));

        let first = &h.processes[0];
        assert_eq!(first.pid, 10);
        assert_eq!(
            first.cwd.as_deref(),
            Some("/w/01a07bad-a553/qontinui-coord")
        );
        assert!(
            first.has_live_children,
            "pid 10 has a `cargo` child — the HINT, not a verdict"
        );
        assert!(!first.nested_under_claude);

        // The two nested subagents are marked as such, and share their
        // parent's worktree — which is exactly how an operator tells a
        // subagent from a session.
        let nested: Vec<u32> = h
            .processes
            .iter()
            .filter(|p| p.nested_under_claude)
            .map(|p| p.pid)
            .collect();
        assert_eq!(nested, vec![100, 110]);
        assert_eq!(
            h.processes[4].cwd, h.processes[0].cwd,
            "subagent 100 shares agent 10's worktree"
        );

        // No CPU field anywhere: this endpoint takes ONE snapshot and a CPU
        // number would require an interval.
        let json = serde_json::to_string(&serde_json::to_value(&h).unwrap()).unwrap();
        for forbidden in ["cpu", "busy", "idle"] {
            assert!(!json.contains(forbidden), "unexpected `{forbidden}` field");
        }
    }

    /// Splitting the count is a LABELLING change: the same population yields
    /// the same `safe_to_restart` whether it lands in one class or four, and
    /// an unclassified process still blocks.
    #[test]
    fn splitting_the_count_does_not_change_the_verdict() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;

        // Same three claude processes, attributed three different ways.
        let snap = snap_with(
            &[(1, &[10, 11, 12])],
            &[(10, now_s), (11, now_s), (12, now_s)],
            &[(10, "claude"), (11, "claude"), (12, "claude")],
        );
        let all: HashSet<u32> = [10u32, 11, 12].into_iter().collect();

        // (a) all headless-exempt, (b) all unclassified.
        let a = evaluate(
            &snap,
            1,
            &[],
            &HashMap::new(),
            &all,
            &HashSet::new(),
            &HashMap::new(),
            now_ms,
            now_ms,
        );
        let b = evaluate(
            &snap,
            1,
            &[],
            &HashMap::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
            now_ms,
            now_ms,
        );
        assert_eq!(a.live_claude_total, b.live_claude_total);
        assert_eq!(a.headless_exempt.len(), 3);
        assert_eq!(b.live_untracked.len(), 3);

        for report in [&a, &b] {
            let v = verdict_from(
                report,
                &[],
                Some(ai_plane_from(&[], &report.ai_plane, now_ms)),
                vec![],
                idle_drain(),
                fresh_census(now_ms),
                now_ms,
            );
            assert!(
                !v.safe_to_restart,
                "3 live claude must block however they are classified: {v:?}"
            );
            assert_eq!(v.live_claude.as_ref().unwrap().total, 3);
            assert!(!v.reason.is_empty(), "an unsafe verdict always says why");
        }

        // And the unclassified residue is NAMED, not merely counted — the
        // fail-closed arm must still be legible to a headless operator.
        let v = verdict_from(
            &b,
            &[],
            Some(ai_plane_from(&[], &[], now_ms)),
            vec![],
            idle_drain(),
            fresh_census(now_ms),
            now_ms,
        );
        assert!(v.reason.contains("unclassified"), "{}", v.reason);
        assert_eq!(
            v.terminal_sessions
                .as_ref()
                .unwrap()
                .unclassified_processes
                .len(),
            3
        );
    }

    /// A missing headless plane is an UNKNOWN, and an unknown never reads as
    /// safe — even when every other plane says idle.
    #[test]
    fn missing_headless_plane_fails_closed() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let report = empty_report(now_ms);
        let v = build_verdict(
            Some(terminal_plane_from(&report, &[], now_ms)),
            None,
            Some(ai_plane_from(&[], &[], now_ms)),
            Some(live_claude_totals_from(&report)),
            vec![],
            idle_drain(),
            fresh_census(now_ms),
        );
        assert!(!v.safe_to_restart);
        assert!(v.reason.contains("headless"), "{}", v.reason);
        assert!(v.headless_sessions.is_none(), "null, never 0");
    }

    /// The boundary statement ships verbatim on every response.
    #[test]
    fn boundary_is_emitted_verbatim() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let report = empty_report(now_ms);
        let v = verdict_from(
            &report,
            &[],
            Some(ai_plane_from(&[], &[], now_ms)),
            vec![],
            idle_drain(),
            fresh_census(now_ms),
            now_ms,
        );
        assert_eq!(v.boundary, BOUNDARY);
        // The statement must stay TRUE of what is emitted, not merely present:
        // it names the process-vs-session distinction, the nested-subagent
        // marker, and the two ways `cwd` can be null.
        for claim in [
            "PROCESSES",
            "nested subagent",
            "nested_under_claude",
            "root_count",
            "cwd",
            "null on Windows",
            "has_live_children",
            "never a verdict",
        ] {
            assert!(v.boundary.contains(claim), "boundary lost `{claim}`");
        }
    }

    #[test]
    fn rfc3339_parsing() {
        assert_eq!(rfc3339_to_millis("1970-01-01T00:00:01Z"), Some(1000),);
        assert_eq!(rfc3339_to_millis("not-a-timestamp"), None);
    }
}
