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
//! ## Liveness is not activity (2026-09-10)
//!
//! Everything above counts PROCESSES. That answers *"what is running"* and it
//! does not answer *"what is still working"* — and on a headless box the two
//! diverge permanently, because an operator there cannot CLOSE a session. Every
//! session ever opened stays in the process table, so the verdict was
//! permanently `false` and said the same thing whether one session was
//! mid-build or all sixteen had finished hours ago. Measured on `merytshost`
//! 2026-09-10: `live_claude.total: 16`, all terminal-hosted, oldest 11h04m,
//! `hasLiveChildren: false` on every one.
//!
//! `coord.sessions.session_status` is the axis that answers it — coord's own
//! docs call it *"a SECOND, orthogonal axis ALONGSIDE"* liveness — and
//! `/finish-session` has been writing `finished` to it all along. This endpoint
//! now JOINS each terminal-hosted process to the open lifecycle record that
//! claims it and reads that axis in bulk from
//! `GET /coord/sessions/work-status` ([`crate::mcp::session_work_status`]).
//!
//! **The verdict reads `live_claude.blocking`, not `live_claude.total`.** The
//! two differ by exactly `finished_discounted`, and a process is discounted
//! ONLY when its coord row resolved to an explicit `finished`
//! ([`crate::session::tracking_health::blocks_restart`], `false` for nothing
//! else). Absent, unset, unrecognised, ambiguous, non-terminal-hosted and
//! coord-unreachable all still block — so the change moves the verdict in the
//! finished-discounting direction and in no other, and a coord outage
//! reproduces the previous verdict bit-for-bit.
//!
//! ⚠ **Finishing does not terminate anything.** A discounted process is still
//! running, still holds memory, is still in `total`, and a restart still kills
//! it. `safe_to_restart: true` means *"no work worth protecting"*, never
//! *"nothing is running"* — see [`BOUNDARY`], which says so on every response.
//! Plan `2026-09-10-restart-readiness-counts-open-sessions-not-active-ones`.
//!
//! ## Working is not open (2026-09-29)
//!
//! `blocking` still counts every non-`finished` process, because a restart
//! kills an idle session as surely as a working one. But "245 blocking" hides
//! what an operator needs to decide what to finish first. Coord finding
//! `0675c4e4` (merytshost, 2026-09-29) counted 245 live sessions of which
//! Claude Code's own records called 220 `idle` and only 8 had exchanged a
//! message in the last hour — a hand measurement cited here, not something
//! this code measured. `live_claude.by_activity {working, idle, stale, unknown}` splits the
//! same `total` on the ACTIVITY axis ([`crate::session::claude_activity`]):
//! the runner's own pane observation (already taken by the wind-down
//! observer) where it is decisive, else Claude Code's `sessions/<pid>.json`.
//! **Report-only: the verdict does not read it** — an idle session still dies
//! on a restart. Plan
//! `2026-09-29-quiet-is-measured-by-session-existence-and-machine-wide-so-a-24x7-box-never-gets-one`,
//! Phase 6.
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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::mcp::session_work_status::{self, SessionStatusSource};
use crate::mcp::types::ApiState;
use crate::session::claude_activity::{self, ActivityCounts, LivePid, RecordReading};
use crate::session::session_lifecycle_store::TerminalSessionRecord;
use crate::session::tracking_health::{self, LiveClaudeProcess, TrackingHealthReport};
use crate::session::wind_down_observer::{self, ObservedInputs, TerminalObservation};
use qontinui_runner_lib::wind_down::{self, WindDownView};

/// What the subtree cross-reference structurally cannot see. Emitted verbatim
/// on every response so a reader is never invited to infer omniscience from a
/// confident-looking count.
pub const BOUNDARY: &str = "counts `claude` PROCESSES in this runner's inclusive process subtree — each process, so a nested subagent counts alongside the agent that spawned it (`nestedUnderClaude` marks those, and `root_count` excludes them); a session doing non-`claude` work, or a child that escaped the subtree, is not represented; `cwd` is read from `/proc/<pid>/cwd` and is null on Windows and for any pid whose link could not be resolved; `hasLiveChildren` is a hint that a child process is attached right now, never a verdict that a session is busy or idle, and is null when the snapshot never enumerated that pid — null means UNCOMPUTABLE, never \"no children\"; `sessionStatus` is the coord WORK axis (`coord.sessions.session_status`), read fresh per request from `GET /coord/sessions/work-status` — a session marked `finished` is DISCOUNTED from `blocking` but its `claude` PROCESS IS STILL RUNNING, still holds memory, and will still be killed by a restart, so `finished` means \"no work worth protecting\", NEVER \"not running\"; every other status, an unreadable coord, an absent row, an unset axis, an unrecognised value, an ambiguous process->session mapping and every non-terminal-hosted process all count as BLOCKING; a NESTED subagent `claude` is never discounted by its ancestor's declaration (nobody declared IT finished), and a live `claude` whose own lifecycle record has no live terminal at all is invisible to this join and is attributed to whichever live terminal's subtree contains it, or to none; `windDown` (on each top-level terminal-hosted process) and `windDownCandidates` are a wind-down eligibility report — THIS ENDPOINT closes nothing, but since Phase 4 the wind-down executor acts on the same verdict WHILE COORD HOLDS THIS DEVICE DRAINED, so an `eligible` here is a session the runner will graceful-`/exit` on its next 30 s tick if the drain is on; they are computed whether or not the runner is drained, the executor's own extra gates (the drain itself, a wall-clock-jump quarantine, and a per-tick close budget) are NOT reflected here, so `eligible` is a candidacy and never a prediction; and a grid-idle window is only as old as the first observation that saw the pane idle with no grid change since; `live_claude.by_activity` classifies the same `total` processes as working / idle / stale / unknown from the pane observation where decisive, else Claude Code's internal `sessions/<pid>.json` record (a `busy`/`shell` status is `working` only with a transcript message in the last 30 min, else `stale`) — the `safe_to_restart` verdict never reads it (only the quiet-barrier `resume` classification does, per process, and only while a runner-restart barrier is open), an `idle` session still dies on a restart, and a missing, unparseable, ambiguous or unrecognised record is `unknown`, never `idle`";

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
    /// Of `count`, the processes that count as WORK IN FLIGHT — i.e. every one
    /// whose coord work axis is not an explicit `finished`. **This is the
    /// number the verdict reads for this plane.**
    pub blocking_count: usize,
    /// Of `count`, the processes DISCOUNTED because their coord session row
    /// reads `finished`. Reported beside `blocking_count` and never instead of
    /// it (D6): an operator must be able to see what was discounted.
    ///
    /// ⚠ These processes are **still running**. Finishing is metadata; it does
    /// not terminate anything. See [`BOUNDARY`].
    pub finished_count: usize,
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
    /// Live processes counted as WORK IN FLIGHT: `total - finished_discounted`.
    /// **This is what `safe_to_restart` reads**, in place of `total`.
    pub blocking: usize,
    /// Live processes whose coord session row reads `finished`, and which are
    /// therefore discounted from `blocking`. **Only these are ever
    /// discounted** — an absent, unreadable, unset, unrecognised or ambiguous
    /// status blocks, so this number can never be inflated by a coord outage.
    ///
    /// ⚠ Discounted is not gone: the processes are still live, still in
    /// `total`, still named in `terminal_sessions.processes`, and a restart
    /// still kills them.
    pub finished_discounted: usize,
    /// The ACTIVITY axis over the same `total` processes: how many are
    /// `working` right now, sitting `idle` at a turn boundary or a prompt,
    /// `stale` (Claude Code says `busy`/`shell` but nothing has moved for 30
    /// min), or `unknown`. `working + idle + stale + unknown == total` always:
    /// a process that cannot be classified is `unknown`, never `idle`.
    ///
    /// ⚠ **`safe_to_restart` never reads it.** An idle session still dies on a
    /// restart. The one consumer is the quiet-barrier `resume` classification,
    /// which reads the same per-process class ([`process_activity`]) for its
    /// "idle at a turn boundary" condition, and only under an open
    /// `runner-restart` barrier. This exists so an operator can see how many of the
    /// open sessions are actually working and decide what to finish first; see
    /// [`crate::session::claude_activity`] for the evidence and its precedence.
    pub by_activity: ActivityCounts,
}

/// Everything `live_claude.by_activity` reads beyond the census pass itself.
/// The default is "no evidence at all", which classifies every process
/// `unknown` — the honest answer when nothing was observed.
#[derive(Debug, Clone, Default)]
pub struct ActivityEvidence {
    /// pid → the pane observation of the top-level terminal-hosted `claude`
    /// the runner spawned in it. Built by [`pane_observations_by_pid`].
    pub pane_by_pid: HashMap<u32, TerminalObservation>,
    /// pid → Claude Code's own `sessions/<pid>.json` reading. A pid absent
    /// from this map was not read, and classifies like a missing record.
    pub records: HashMap<u32, RecordReading>,
    pub now_ms: i64,
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
    /// Coord's DEVICE drain as this runner reads it (plan
    /// `2026-09-13-drained-runner-never-reaches-idle`, Phase 3) — a different
    /// lever from the local `is_draining` above: coord's drain is reversible
    /// and expiring, and defers this runner's AUTONOMOUS spawns; the local
    /// `POST /drain` is terminal and acts on the AI-session plane. The two are
    /// reported side by side and never merged.
    #[serde(rename = "coordDrain")]
    pub coord_drain: crate::coord_drain_state::CoordDrainSnapshot,
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
    /// WHERE the work-axis evidence came from, and whether it was there at
    /// all. `degraded: true` means the coord read failed and **every** live
    /// process is counted as blocking — the pre-work-axis verdict — rather
    /// than "coord said nothing is finished". The two are indistinguishable
    /// from the counts alone, which is why this block exists.
    pub session_status_source: SessionStatusSource,
    /// How many top-level terminal-hosted processes are wind-down `eligible`
    /// right now — the count of `windDown.eligibility == "eligible"` across
    /// `terminal_sessions.processes` (plan
    /// `2026-09-13-drained-runner-never-reaches-idle`).
    ///
    /// ⚠ **NOT a dry run since Phase 4**, and this is the WIRE contract, so an
    /// external consumer reads it here. THIS ENDPOINT closes nothing and the
    /// count is computed whether or not the runner is drained — but the
    /// wind-down executor acts on the same verdict while the device IS drained.
    /// It is therefore a count of CANDIDATES, not a prediction: the executor
    /// applies further gates of its own (the drain, a wall-clock-jump
    /// quarantine, a per-tick budget, and a re-check immediately before each
    /// close). `null` when the terminal plane could not be determined — never
    /// `0`.
    #[serde(rename = "windDownCandidates")]
    pub wind_down_candidates: Option<usize>,
    pub boundary: &'static str,
    /// Plan `2026-09-29-quiet-on-demand-…` (D5): would a restart RIGHT NOW
    /// lose nothing that the boot restore does not bring back? `true` only
    /// while a `runner-restart` quiet barrier is open (so the autonomous wake
    /// doors defer — D4; `resume.wake_paths_gated` is the gate's own answer),
    /// every plane resolved, the AI plane is empty, and every live agent
    /// session is either `resumable` or `finished` (see `resume`).
    ///
    /// Additive: `safe_to_restart` above is unchanged and still means "no work
    /// in flight at all". The two are never merged.
    pub safe_with_resume: bool,
    /// Why `safe_with_resume` is what it is — including, when no barrier is
    /// open, that it was not computed at all.
    pub safe_with_resume_reason: String,
    /// The resumable classification. `null` unless a `runner-restart` barrier
    /// is open: without one, an idle session can be woken, so nothing is
    /// resumable by construction.
    pub resume: Option<crate::quiet_barrier::resume::ResumeBlock>,
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
        blocking_count: TrackingHealthReport::blocking_count(&report.terminal_hosted),
        finished_count: TrackingHealthReport::finished_count(&report.terminal_hosted),
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
    live_claude_totals_observed(report, &ActivityEvidence::default())
}

/// [`live_claude_totals_from`] with the activity axis classified from
/// `evidence`. Every count except `by_activity` is independent of `evidence`,
/// so the verdict — which reads `blocking` — cannot move with it.
pub fn live_claude_totals_observed(
    report: &TrackingHealthReport,
    evidence: &ActivityEvidence,
) -> LiveClaudeTotals {
    LiveClaudeTotals {
        total: report.live_claude_total,
        terminal_hosted: report.terminal_hosted.len(),
        ai_plane: report.ai_plane.len(),
        headless_exempt: report.headless_exempt.len(),
        unclassified: report.live_untracked.len(),
        // Only the terminal-hosted plane can carry a work axis (it is the only
        // class with a `claude_session_id` coord can be asked about), but the
        // sum is taken over EVERY class so the arithmetic
        // `blocking + finished_discounted == total` holds unconditionally.
        blocking: [
            &report.terminal_hosted,
            &report.ai_plane,
            &report.headless_exempt,
            &report.live_untracked,
        ]
        .iter()
        .map(|l| TrackingHealthReport::blocking_count(l))
        .sum(),
        finished_discounted: [
            &report.terminal_hosted,
            &report.ai_plane,
            &report.headless_exempt,
            &report.live_untracked,
        ]
        .iter()
        .map(|l| TrackingHealthReport::finished_count(l))
        .sum(),
        by_activity: activity_counts(report, evidence),
    }
}

/// Classify every live process in all four census classes — the same
/// population `total` counts, so the four cells always sum to it.
pub fn activity_counts(
    report: &TrackingHealthReport,
    evidence: &ActivityEvidence,
) -> ActivityCounts {
    let mut counts = ActivityCounts::default();
    for list in [
        &report.terminal_hosted,
        &report.ai_plane,
        &report.headless_exempt,
        &report.live_untracked,
    ] {
        for process in list {
            counts.add(process_activity(process, evidence));
        }
    }
    counts
}

/// What Claude Code's own record for this process says on the `waiting`
/// question — a permission prompt or a question. The activity axis counts a
/// `waiting` record as `idle` (correctly: no turn is running), but a pending
/// prompt is LOST on resume, so the quiet-barrier `resume` classification
/// treats it as `waiting_human` exactly as it does the sideband (plan D2).
///
/// Tri-state: a record that could not be read — missing, unparseable,
/// ambiguous, a reused pid, or no entry at all because the read timed out, was
/// skipped while an earlier one was in flight, or was never requested — is
/// `Unreadable`, which resume reports as `unknown` and never resumable.
/// Resume-only: `by_activity` never reads this.
pub fn record_waiting_read(
    process: &LiveClaudeProcess,
    evidence: &ActivityEvidence,
) -> crate::quiet_barrier::resume::RecordWaitingRead {
    use crate::quiet_barrier::resume::RecordWaitingRead;
    match evidence.records.get(&process.pid) {
        Some(RecordReading::Parsed(ev)) if ev.status == "waiting" => RecordWaitingRead::Waiting,
        Some(RecordReading::Parsed(_)) => RecordWaitingRead::NotWaiting,
        Some(RecordReading::Missing) => {
            RecordWaitingRead::Unreadable("no `sessions/<pid>.json` record exists".to_string())
        }
        Some(RecordReading::Unparseable) => {
            RecordWaitingRead::Unreadable("the session record did not parse".to_string())
        }
        Some(RecordReading::Ambiguous) => RecordWaitingRead::Unreadable(
            "two config dirs hold conflicting records for this pid".to_string(),
        ),
        Some(RecordReading::PidReused) => RecordWaitingRead::Unreadable(
            "the record belongs to an earlier process that held this pid".to_string(),
        ),
        None => RecordWaitingRead::Unreadable(
            "the record was not read — the read timed out, was skipped while an earlier read \
             was in flight, or was not requested"
                .to_string(),
        ),
    }
}

/// One live process's class on the activity axis — exactly the value
/// [`activity_counts`] adds for it. The quiet-barrier resume classification
/// (`quiet_barrier::resume`) reads its "idle at a turn boundary" condition
/// from here, so `by_activity` and `resume` share one census and one set of
/// blind spots rather than two idle heuristics that can disagree.
pub fn process_activity(
    process: &LiveClaudeProcess,
    evidence: &ActivityEvidence,
) -> claude_activity::Activity {
    claude_activity::classify(
        evidence.pane_by_pid.get(&process.pid),
        process.has_live_children,
        evidence
            .records
            .get(&process.pid)
            .unwrap_or(&RecordReading::Missing),
        evidence.now_ms,
    )
}

/// pid → pane observation, for the top-level terminal-hosted processes whose
/// pane the wind-down observer actually looked at in this pass. A nested
/// subagent is not its pane's top-level `claude`, so the pane says nothing
/// about it; a process whose pane was not observed gets no entry (never
/// [`TerminalObservation::UNOBSERVABLE`] standing in for one).
pub fn pane_observations_by_pid(
    report: &TrackingHealthReport,
    observed: &ObservedInputs,
) -> HashMap<u32, TerminalObservation> {
    report
        .terminal_hosted
        .iter()
        .filter(|p| !p.nested_under_claude)
        .filter_map(|p| {
            let terminal_id = observed.terminal_for(p.session_id.as_deref()?)?;
            let observation = observed.by_terminal.get(terminal_id)?;
            Some((p.pid, *observation))
        })
        .collect()
}

/// How long the Claude Code record read may take before every undecided
/// process is reported `unknown`. `/restart-readiness` is polled on every Stop
/// turn under a short client timeout; a slow disk must cost the activity
/// split, never the verdict's availability.
pub const RECORD_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Set while a Claude Code record read is running. See
/// [`gather_activity_evidence`].
pub static RECORD_READ_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Clears the in-flight flag when dropped — i.e. when the blocking read
/// finishes or unwinds.
struct InFlightRead(&'static AtomicBool);

impl Drop for InFlightRead {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The live processes whose activity the pane did NOT decide — the only ones
/// whose Claude Code record `by_activity` needs — with the census's process age
/// for the record's identity check. `include_pane_decided` reads EVERY live
/// process's record instead: the quiet-barrier `resume` classification needs
/// the record's own `waiting` status even where the pane decided the activity
/// (see [`record_says_waiting`]). It never changes `by_activity`, because
/// [`claude_activity::classify`] consults the record only when the pane is not
/// decisive.
fn pids_needing_a_record(
    report: &TrackingHealthReport,
    pane_by_pid: &HashMap<u32, TerminalObservation>,
    now_ms: i64,
    include_pane_decided: bool,
) -> Vec<LivePid> {
    [
        &report.terminal_hosted,
        &report.ai_plane,
        &report.headless_exempt,
        &report.live_untracked,
    ]
    .iter()
    .flat_map(|l| l.iter())
    .filter(|p| {
        include_pane_decided
            || pane_by_pid
                .get(&p.pid)
                .and_then(|obs| {
                    claude_activity::classify_from_pane(obs, p.has_live_children, now_ms)
                })
                .is_none()
    })
    .map(|p| LivePid {
        pid: p.pid,
        // From the census's OWN snapshot time, not a later clock read: the
        // age was measured at `checked_at_ms`.
        process_started_ms: p.age_s.map(|age_s| report.checked_at_ms - age_s * 1000),
    })
    .collect()
}

/// Assemble the activity evidence for one pass: the pane observations the
/// wind-down observer already took, plus Claude Code's records for every
/// process the pane could not decide, read from `config_dirs` off the
/// executor and bounded by `read_timeout`. A read that fails or times out
/// costs the records — those processes read `unknown`, never `idle`.
/// `proc_start` is [`claude_activity::proc_start_ticks`] in production.
///
/// `in_flight` admits ONE read at a time: while a read (even an abandoned,
/// timed-out one) is still running, a new call skips the read and reports its
/// undecided processes `unknown`. Production passes
/// [`RECORD_READ_IN_FLIGHT`].
///
/// `read_pane_decided` (true only while a `runner-restart` quiet barrier is
/// open) also reads the records of pane-decided processes, for the resume
/// rule in [`record_says_waiting`]; `by_activity` is identical either way.
pub async fn gather_activity_evidence(
    report: &TrackingHealthReport,
    observed: &ObservedInputs,
    config_dirs: Vec<PathBuf>,
    proc_start: fn(u32) -> Option<String>,
    read_timeout: std::time::Duration,
    in_flight: &'static AtomicBool,
    read_pane_decided: bool,
) -> ActivityEvidence {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let pane_by_pid = pane_observations_by_pid(report, observed);
    let pids = pids_needing_a_record(report, &pane_by_pid, now_ms, read_pane_decided);
    let records = if pids.is_empty() {
        HashMap::new()
    } else if in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        // An earlier read is still running — it timed out and was abandoned,
        // but a blocking thread cannot be cancelled. Starting another would
        // leak one more thread per poll on a hung filesystem.
        tracing::warn!(
            "restart-readiness: a previous Claude Code session-record read is still in \
             flight — skipped; every undecided process reads activity `unknown`"
        );
        HashMap::new()
    } else {
        let guard = InFlightRead(in_flight);
        let read = tokio::task::spawn_blocking(move || {
            // Released when the read ENDS (or unwinds), not when the caller
            // stops waiting for it.
            let _guard = guard;
            claude_activity::read_records(&config_dirs, &pids, &proc_start)
        });
        match tokio::time::timeout(read_timeout, read).await {
            Ok(Ok(records)) => records,
            Ok(Err(e)) => {
                tracing::warn!(
                    "restart-readiness: Claude Code session-record read failed ({e}) — \
                     every undecided process reads activity `unknown`"
                );
                HashMap::new()
            }
            Err(_) => {
                tracing::warn!(
                    "restart-readiness: Claude Code session-record read exceeded {read_timeout:?} — \
                     every undecided process reads activity `unknown`"
                );
                HashMap::new()
            }
        }
    };
    ActivityEvidence {
        pane_by_pid,
        records,
        now_ms,
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
///
/// **The work axis, and the ONE direction the verdict may move.** `safe` reads
/// `totals.blocking` where it used to read `totals.total`. Those differ by
/// exactly `totals.finished_discounted`, and a process is discounted ONLY when
/// its coord session row resolved to an explicit `finished`
/// ([`crate::session::tracking_health::blocks_restart`], which is `false` for
/// nothing else). So a session that blocked before and is not `finished` still
/// blocks, and a coord outage — which resolves no statuses at all — reproduces
/// the previous verdict bit-for-bit. `status_source.degraded` says which of
/// those two worlds produced the number.
///
/// **A failed work-axis read is NOT an `unknowns` entry.** It is reported in
/// `session_status_source` and named in `reason` while the verdict stays
/// answerable. Escalating a coord blip to UNKNOWN would be no safer than the
/// fail-closed count already is, and would make the endpoint unreadable
/// precisely when an operator is trying to use it.
#[allow(clippy::too_many_arguments)]
pub fn build_verdict(
    terminal: Option<TerminalPlane>,
    headless: Option<HeadlessPlane>,
    ai: Option<AiPlane>,
    totals: Option<LiveClaudeTotals>,
    unknowns: Vec<String>,
    drain: DrainInfo,
    census: CensusInfo,
    status_source: SessionStatusSource,
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

    let wind_down_candidates = terminal.as_ref().map(wind_down_candidates_in);

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
            session_status_source: status_source,
            wind_down_candidates,
            boundary: BOUNDARY,
            safe_with_resume: false,
            safe_with_resume_reason: RESUME_NOT_COMPUTED.to_string(),
            resume: None,
        };
    }

    // Every plane resolved.
    let t = terminal.expect("checked above");
    let h = headless.expect("checked above");
    let a = ai.expect("checked above");
    let totals = totals.expect("checked above");

    let mut parts: Vec<String> = Vec::new();
    if t.blocking_count > 0 {
        // The ACTIVITY axis leads, and both numbers are always present: an
        // operator must be able to see what was discounted and why, never a
        // single count that hides the reasoning (D6).
        parts.push(format!(
            "{} of {} live terminal-hosted agent `claude` process{} {} still working{} ({} top-level live); no graceful stop path exists for {}, so {} in-flight work will be lost",
            t.blocking_count,
            t.count,
            if t.count == 1 { "" } else { "es" },
            if t.blocking_count == 1 { "is" } else { "are" },
            if t.finished_count > 0 {
                format!(
                    " ({} marked finished on {} coord work axis, discounted — but still RUNNING, and a restart still kills {})",
                    t.finished_count,
                    if t.finished_count == 1 { "its" } else { "their" },
                    if t.finished_count == 1 { "it" } else { "them" },
                )
            } else {
                String::new()
            },
            t.root_count,
            if t.blocking_count == 1 { "it" } else { "them" },
            if t.blocking_count == 1 { "its" } else { "their" },
        ));
    } else if t.finished_count > 0 {
        // Every terminal-hosted process is discounted. Say plainly that they
        // are STILL THERE — `safe_to_restart: true` means "no work worth
        // protecting", never "nothing is running".
        parts.push(format!(
            "0 of {} live terminal-hosted agent `claude` process{} {} still working (all {} marked finished on their coord work axis); {} process{} {} still running and a restart will still kill {}",
            t.count,
            if t.count == 1 { "" } else { "es" },
            if t.count == 1 { "is" } else { "are" },
            t.finished_count,
            t.finished_count,
            if t.finished_count == 1 { "" } else { "es" },
            if t.finished_count == 1 { "is" } else { "are" },
            if t.finished_count == 1 { "it" } else { "them" },
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

    if status_source.degraded && totals.total > 0 {
        // NAME the degradation in the reason, not only in the block below it:
        // a reader who sees `blocking == total` must be able to tell "coord
        // said nothing is finished" from "coord could not be asked".
        parts.push(format!(
            "the coord work axis could NOT be read ({}), so every live process is counted as work in flight — fail-closed: absence is never \"finished\"",
            if status_source.note.is_empty() {
                "no cause reported".to_string()
            } else {
                status_source.note.clone()
            },
        ));
    }

    // The verdict now reads `totals.blocking`, which is `totals.total` minus
    // ONLY the processes whose coord session row resolved to an explicit
    // `finished`. Every other class — every non-terminal status, an absent or
    // unset axis, an unrecognised word, an ambiguous attribution, a coord
    // outage, and every non-terminal-hosted plane — is still in `blocking`, so
    // the change moves the verdict in the finished-discounting direction and
    // in no other. `ai_sessions.count` is untouched: that plane is keyed by
    // `task_run_id`, not by `claude_code_session_id`, so the work axis says
    // nothing about it.
    let safe = totals.blocking == 0 && a.count == 0;
    let reason = if safe && totals.total == 0 {
        "no live agent sessions in any plane".to_string()
    } else if safe && parts.is_empty() {
        // Unreachable (a discounted process always emits a clause), but a safe
        // verdict must never imply the box is empty when it is not.
        format!(
            "no live agent session is still working ({} live `claude` process(es) are marked finished and remain RUNNING — a restart still kills them)",
            totals.finished_discounted
        )
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
        session_status_source: status_source,
        wind_down_candidates,
        boundary: BOUNDARY,
        safe_with_resume: false,
        safe_with_resume_reason: RESUME_NOT_COMPUTED.to_string(),
        resume: None,
    }
}

/// `safe_with_resume_reason` before [`attach_resume`] runs.
pub const RESUME_NOT_COMPUTED: &str = "not computed";

/// PURE: attach the quiet-barrier resume verdict to a built readiness body.
///
/// `barrier` is the live runner-restart barrier state; `block` the resume
/// block the handler gathered under it (`None` when the barrier is not open —
/// nothing is gathered then). The existing `safe_to_restart` verdict is never
/// touched.
pub fn attach_resume(
    verdict: &mut RestartReadiness,
    barrier: &crate::quiet_barrier::BarrierState,
    block: Option<crate::quiet_barrier::resume::ResumeBlock>,
) {
    use crate::quiet_barrier::BarrierState;
    match (barrier, block) {
        (BarrierState::Absent, _) => {
            verdict.safe_with_resume = false;
            verdict.safe_with_resume_reason = "no runner-restart quiet barrier is open, so idle \
                sessions can still be woken and none is counted as resumable (open one with \
                `quiet-barrier.sh open runner-restart`)"
                .to_string();
            verdict.resume = None;
        }
        (BarrierState::Unknown(why), _) => {
            verdict.safe_with_resume = false;
            verdict.safe_with_resume_reason = format!(
                "UNKNOWN: the quiet-barrier store could not be read ({why}); autonomous wakes \
                 are held fail-closed, but no resume verdict is computed on an unknown barrier"
            );
            verdict.resume = None;
        }
        (BarrierState::Open(b), None) => {
            verdict.safe_with_resume = false;
            verdict.safe_with_resume_reason = format!(
                "UNKNOWN: runner-restart barrier {} is open but the resume classification could \
                 not be gathered",
                b.id
            );
            verdict.resume = None;
        }
        (BarrierState::Open(_), Some(block)) => {
            verdict.safe_with_resume = block.blocking_count == 0;
            verdict.safe_with_resume_reason = if block.blocking_count == 0 {
                format!(
                    "every live agent session is at a resumable safe point under barrier {} \
                     ({} resumable, {} finished); the boot restore is expected to bring back {} \
                     session(s)",
                    block.barrier_id,
                    block.resumable_count,
                    block.finished_count,
                    block.expected_restore_set.len()
                )
            } else {
                format!(
                    "{} straggler(s) not at a resumable safe point under barrier {} ({} \
                     resumable) — see `resume.stragglers`",
                    block.blocking_count, block.barrier_id, block.resumable_count
                )
            };
            verdict.resume = Some(block);
        }
    }
}

/// `windDownCandidates` for a resolved terminal plane: top-level processes
/// whose wind-down verdict is `eligible`. Computed whether or not the runner is
/// drained, and THIS endpoint closes none of them — but since Phase 4 the
/// wind-down executor acts on the same verdict while drained, so this is a
/// count of candidates, not of sessions that will certainly be closed.
pub fn wind_down_candidates_in(plane: &TerminalPlane) -> usize {
    plane
        .processes
        .iter()
        .filter(|p| p.wind_down.as_ref().is_some_and(WindDownView::is_eligible))
        .count()
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

    // ── ONE fresh, observed census pass ──────────────────────────────────
    //
    // `wind_down_observer::fresh_pass` owns the whole sequence: the bulk coord
    // WORK-axis read, `tracking_health::compute` against that FRESH map (never
    // the background census's empty one), and the wind-down observation. The
    // Phase 4 wind-down tick calls the same function, so the two can never
    // build two differently-shaped censuses (D1).
    //
    // The fetch NEVER fails (see `session_work_status`): a coord outage yields
    // an empty map, every process blocks, and the verdict is bit-for-bit the
    // pre-work-axis one — with the degradation stated in the response.
    //
    // This HANDLER acts on nothing: it reports the verdicts and closes no
    // session. That is a property of the endpoint, not of the verdict — since
    // Phase 4 the wind-down executor reads the same one and does act on it,
    // while drained. Do not restore the old "DRY RUN" wording: it described the
    // verdict, and stopped being true of it.
    let fresh = wind_down_observer::fresh_pass(app, wind_down::grace_from_env()).await;
    let wind_down_observer::FreshPass {
        pass,
        status_fetch,
        observed,
        unknowns: pass_unknowns,
    } = fresh;
    unknowns.extend(pass_unknowns);
    let status_source = SessionStatusSource::from(&status_fetch);

    // ── The ACTIVITY axis (`safe_to_restart` never reads it; the quiet-barrier
    //    `resume` block below reads it per process) ──────────────────────────
    //
    // The pane half is the observation `fresh_pass` already took for the
    // wind-down verdicts — no second look at any pane. The record half reads
    // Claude Code's `sessions/<pid>.json` only for processes the pane did not
    // decide, off the executor and under `RECORD_READ_TIMEOUT`.
    //
    // Under an open runner-restart barrier the record is read for EVERY
    // process, pane-decided or not: the `resume` block needs its `waiting`
    // status (a permission prompt or question, lost on resume — plan D2).
    let barrier = crate::quiet_barrier::runner_restart_barrier_open();
    let barrier_open = matches!(barrier, crate::quiet_barrier::BarrierState::Open(_));
    let activity_evidence = match pass.as_ref() {
        Some(p) => {
            gather_activity_evidence(
                &p.report,
                &observed,
                crate::terminal::transcript::find_claude_config_dirs(),
                claude_activity::proc_start_ticks,
                RECORD_READ_TIMEOUT,
                &RECORD_READ_IN_FLIGHT,
                barrier_open,
            )
            .await
        }
        None => ActivityEvidence::default(),
    };

    let terminal = pass
        .as_ref()
        .map(|p| terminal_plane_from(&p.report, &p.open_records, now_ms));
    let headless = pass.as_ref().map(|p| headless_plane_from(&p.report));
    let totals = pass
        .as_ref()
        .map(|p| live_claude_totals_observed(&p.report, &activity_evidence));
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
        coord_drain: crate::coord_drain_state::snapshot(),
    };

    let census = census_info(tracking_health::latest().as_ref(), now_ms);

    // ── Quiet-barrier resume classification (plan quiet-on-demand, D5) ────
    // Gathered ONLY while a runner-restart barrier is open: without one, an
    // idle session can be woken, so nothing is resumable by construction and
    // none of this is paid for. `barrier` was read once, above, before the
    // activity evidence, so both see the same barrier state.
    let resume_block = match &barrier {
        crate::quiet_barrier::BarrierState::Open(b) => Some(
            gather_resume(
                app,
                b,
                crate::quiet_barrier::autonomous_wakes_deferred_under(&barrier),
                pass.as_ref(),
                &observed,
                &activity_evidence,
                ai.as_ref(),
                &unknowns,
                now_ms,
            )
            .await,
        ),
        _ => None,
    };

    let mut verdict = build_verdict(
        terminal,
        headless,
        ai,
        totals,
        unknowns,
        drain,
        census,
        status_source,
    );
    attach_resume(&mut verdict, &barrier, resume_block);
    Json(verdict)
}

/// What [`resume_block_from`] reads about one top-level terminal-hosted
/// process from the live box — the parts that need the process table, the
/// terminal manager or the pending-prompt registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResumeProbe {
    pub terminal_id: Option<String>,
    pub sideband: crate::quiet_barrier::resume::SidebandRead,
    pub descendants: crate::quiet_barrier::resume::Descendants,
    pub pending_autonomous: Vec<String>,
}

/// Gather the D2 inputs for every live agent session from the live box, and
/// fold them into the `resume` block through [`resume_block_from`]. Every read
/// that fails becomes an `unknown` straggler, never a resumable session.
#[allow(clippy::too_many_arguments)]
async fn gather_resume(
    app: &tauri::AppHandle,
    barrier: &crate::quiet_barrier::Barrier,
    wake_paths_gated: bool,
    pass: Option<&tracking_health::TrackingHealthPass>,
    observed: &wind_down_observer::ObservedInputs,
    activity: &ActivityEvidence,
    ai: Option<&AiPlane>,
    unknowns: &[String],
    now_ms: i64,
) -> crate::quiet_barrier::resume::ResumeBlock {
    use crate::quiet_barrier::resume::{
        classify_descendants, resolve_mcp_servers, Descendants, SidebandRead,
    };
    use tauri::Manager;

    // The restore side: what the NEXT boot would select.
    //
    // ASSUMPTION — the restart happens NOW. `restorable_records(now, Some(now),
    // …)` anchors the selection at this instant, as if the shutdown marker
    // were written now. If the actual shutdown comes minutes later and a
    // session's `last_seen_at` is not heartbeated meanwhile, it can age out of
    // the 600 s anchor grace and the real expected set is SMALLER than this
    // one. Phase 5's post-restart `restore-census` is what catches that: it
    // compares the restored set against the snapshot taken from this field and
    // reports `partial`/`mismatch`, never a silent pass.
    let restorable: Result<HashMap<String, bool>, String> = match app
        .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
    {
        Some(store) => {
            let records = store.restorable_records(now_ms, Some(now_ms), true);
            let probe = crate::session::reconcile::DiskTranscriptIndex::discover();
            Ok(
                crate::session::restore_census::expected_rows(records, &probe)
                    .into_iter()
                    .map(|row| (row.claude_session_id, row.restorable))
                    .collect(),
            )
        }
        None => Err("the lifecycle store did not resolve".to_string()),
    };

    let top: Vec<&LiveClaudeProcess> = pass
        .map(|p| {
            p.report
                .terminal_hosted
                .iter()
                .filter(|p| !p.nested_under_claude)
                .collect()
        })
        .unwrap_or_default();
    let snapshot = if top.is_empty() {
        crate::process_capture::process_tree::ProcessSnapshot::default()
    } else {
        crate::process_capture::process_tree::snapshot_process_table_public().await
    };
    let mut pids: Vec<u32> = top.iter().map(|p| p.pid).collect();
    for p in &top {
        if let Some(children) = snapshot.parent_map.get(&p.pid) {
            pids.extend(children.iter().copied());
        }
    }
    let cmdlines = if pids.is_empty() {
        HashMap::new()
    } else {
        crate::process_capture::process_tree::command_lines_for_pids(&pids).await
    };
    let manager = app
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .map(|m| m.inner().clone());

    let probe = |p: &LiveClaudeProcess| -> ResumeProbe {
        let terminal_id = p
            .session_id
            .as_deref()
            .and_then(|sid| observed.terminal_for(sid))
            .map(str::to_string);
        let sideband = match (manager.as_ref(), terminal_id.as_deref()) {
            (Some(m), Some(t)) => match m.get(t).map(|s| s.last_agent_status()) {
                Some(Ok(Some(state))) => SidebandRead::Reported(state.state),
                Some(Ok(None)) => SidebandRead::NeverReported,
                Some(Err(_)) | None => SidebandRead::Unreadable,
            },
            _ => SidebandRead::Unreadable,
        };
        let descendants = if snapshot.parent_map.is_empty() {
            Descendants::Unknown("the process table is unreadable".to_string())
        } else {
            let sigs =
                resolve_mcp_servers(cmdlines.get(&p.pid).map(String::as_str), p.cwd.as_deref());
            classify_descendants(p.pid, &snapshot.parent_map, &cmdlines, sigs.as_deref())
        };
        let pending_autonomous = terminal_id
            .as_deref()
            .map(crate::quiet_barrier::pending::for_session)
            .unwrap_or_default();
        ResumeProbe {
            terminal_id,
            sideband,
            descendants,
            pending_autonomous,
        }
    };

    // An AI session's queued autonomous SDK messages are registered under its
    // INSTANCE key (session id + instance number), not the manager key the AI
    // plane reports — resolve it through the manager.
    let sessions = app
        .try_state::<Arc<crate::claude_session::SessionManager>>()
        .map(|m| m.inner().clone());
    // A lookup miss is UNKNOWN (an `unknown` straggler), never a silent "no
    // pending": entries are keyed by instance, so no bare-key fallback could
    // ever match.
    let ai_pending = |key: &str| -> Option<Vec<String>> {
        let session = sessions.as_ref()?.get(key)?;
        Some(crate::quiet_barrier::pending::for_session(
            session.pending_registry_key(),
        ))
    };

    resume_block_from(
        barrier,
        wake_paths_gated,
        pass,
        activity,
        ai,
        unknowns,
        &restorable,
        &probe,
        &ai_pending,
    )
}

/// PURE: the `resume` block from everything [`gather_resume`] read. Every
/// D2 condition is decided here, so it is unit-testable without a box:
///
/// - a census pass that did not resolve is an `unknown` straggler — never an
///   empty, vacuously "all resumable" plane;
/// - a session whose coord work axis reads `finished` (`!blocks_restart`) is
///   counted `finished`, not resumable and not a straggler;
/// - the barrier's own requester is a `requester` straggler;
/// - the rest are classified by [`crate::quiet_barrier::resume::classify`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn resume_block_from(
    barrier: &crate::quiet_barrier::Barrier,
    wake_paths_gated: bool,
    pass: Option<&tracking_health::TrackingHealthPass>,
    activity: &ActivityEvidence,
    ai: Option<&AiPlane>,
    unknowns: &[String],
    restorable: &Result<HashMap<String, bool>, String>,
    probe: &dyn Fn(&LiveClaudeProcess) -> ResumeProbe,
    ai_pending: &dyn Fn(&str) -> Option<Vec<String>>,
) -> crate::quiet_barrier::resume::ResumeBlock {
    use crate::quiet_barrier::resume::{build_block, RestoreSelection, SessionInput, Straggler};

    let mut others: Vec<Straggler> = Vec::new();
    if !unknowns.is_empty() {
        others.push(Straggler::other(
            None,
            "unknown",
            format!("unknown: {}", unknowns.join("; ")),
        ));
    }
    match ai {
        None => others.push(Straggler::other(
            None,
            "unknown",
            "unknown: the AI/task-run plane could not be determined",
        )),
        Some(a) => {
            for s in &a.sessions {
                // A queued autonomous SDK message lives only in memory and
                // dies with the restart: name it, so the operator sees WHAT
                // would be lost, not just that the session is live.
                let Some(pending) = ai_pending(&s.id) else {
                    others.push(Straggler::other(
                        None,
                        "unknown",
                        format!(
                            "unknown: AI/task-run session {} did not resolve through the session \
                             manager, so whether it holds a queued autonomous prompt is unknown",
                            s.id
                        ),
                    ));
                    continue;
                };
                others.push(if pending.is_empty() {
                    Straggler::other(
                        None,
                        "ai_session",
                        format!(
                            "AI/task-run session {} is live; the terminal restore path does not \
                             resume it",
                            s.id
                        ),
                    )
                } else {
                    Straggler::other(
                        None,
                        "pending_autonomous_prompt",
                        format!(
                            "AI/task-run session {} holds a deferred autonomous prompt in memory, \
                             which the restart would drop: {}",
                            s.id,
                            pending.join(", ")
                        ),
                    )
                });
            }
        }
    }

    let mut expected_restore_set: Vec<String> = restorable
        .as_ref()
        .map(|m| {
            m.iter()
                .filter(|(_, ok)| **ok)
                .map(|(id, _)| id.clone())
                .collect()
        })
        .unwrap_or_default();
    expected_restore_set.sort();

    let Some(pass) = pass else {
        others.push(Straggler::other(
            None,
            "unknown",
            "unknown: the process census did not resolve, so no live session could be \
             classified",
        ));
        return build_block(
            &barrier.id,
            &[],
            others,
            expected_restore_set,
            wake_paths_gated,
        );
    };
    for p in &pass.report.headless_exempt {
        others.push(Straggler::other(
            Some(p.pid),
            "headless",
            "headless agent-runtime `claude` — not restored by the terminal restore path",
        ));
    }
    for p in &pass.report.live_untracked {
        others.push(Straggler::other(
            Some(p.pid),
            "unknown",
            "unknown: a live `claude` no terminal, AI session or headless registration accounts for",
        ));
    }
    if ai.is_some_and(|a| a.count == 0) {
        for p in &pass.report.ai_plane {
            others.push(Straggler::other(
                Some(p.pid),
                "ai_session",
                "a live `claude` under the AI plane's roots while no AI session is open",
            ));
        }
    }

    let sessions: Vec<SessionInput> = pass
        .report
        .terminal_hosted
        .iter()
        .filter(|p| !p.nested_under_claude)
        .map(|p| {
            let ResumeProbe {
                terminal_id,
                sideband,
                descendants,
                pending_autonomous,
            } = probe(p);
            let restore = match (restorable, p.session_id.as_deref()) {
                (Err(why), _) => RestoreSelection::Unknown(why.clone()),
                (Ok(_), None) => RestoreSelection::Unknown("no session id".to_string()),
                (Ok(m), Some(sid)) => match m.get(sid) {
                    Some(true) => RestoreSelection::Restorable,
                    Some(false) => RestoreSelection::NotRestorable,
                    None => RestoreSelection::NotSelected,
                },
            };
            SessionInput {
                session_id: p.session_id.clone(),
                terminal_id,
                pid: p.pid,
                finished: !p.blocks_restart,
                // The same per-process class `live_claude.by_activity` counts:
                // one census, never a parallel idle heuristic.
                activity: process_activity(p, activity),
                record: record_waiting_read(p, activity),
                is_requester: p
                    .session_id
                    .as_deref()
                    .is_some_and(|sid| barrier.exempts(sid)),
                pending_autonomous,
                sideband,
                descendants,
                restore,
            }
        })
        .collect();

    build_block(
        &barrier.id,
        &sessions,
        others,
        expected_restore_set,
        wake_paths_gated,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tracking_health::{
        evaluate, LiveClaudeProcess, SessionWorkStatus, TrackedDeadRecord, TrackingHealthReport,
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
            finished_at: None,
            wind_down_outcome: None,
            wind_down_at: None,
            finish_reason: None,
            finish_synced: false,
            spawn_device_default: None,
        }
    }

    fn idle_drain() -> DrainInfo {
        DrainInfo {
            already_drained: false,
            is_draining: false,
            would_be_noop: true,
            covers: DRAIN_COVERS,
            call: "POST http://127.0.0.1:9876/drain".to_string(),
            coord_drain: crate::coord_drain_state::snapshot_fixture(
                &crate::coord_drain_state::CoordDrainState::Clear,
            ),
        }
    }

    /// Plan `2026-09-13-drained-runner-never-reaches-idle`, Phase 3: coord's
    /// device drain is reported beside the local drain, under its own key.
    #[test]
    fn drain_reports_coord_drain_beside_the_local_drain() {
        let json = serde_json::to_value(idle_drain()).unwrap();
        assert_eq!(json["is_draining"], false);
        assert_eq!(json["coordDrain"]["state"], "clear");
        assert_eq!(json["coordDrain"]["autonomousSpawnsAllowed"], true);
        assert!(json.get("coord_drain").is_none(), "{json}");

        let drained = DrainInfo {
            coord_drain: crate::coord_drain_state::snapshot_fixture(
                &crate::coord_drain_state::CoordDrainState::Drained {
                    until: None,
                    reason: Some("rebuild".into()),
                },
            ),
            ..idle_drain()
        };
        let json = serde_json::to_value(drained).unwrap();
        assert_eq!(json["is_draining"], false, "the local drain is untouched");
        assert_eq!(json["coordDrain"]["state"], "drained");
        assert_eq!(json["coordDrain"]["reason"], "rebuild");
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
            clean_status_source(),
        )
    }

    /// A CLEAN work-axis read: coord answered. Tests that want the degraded
    /// posture ask for it explicitly — never by omission.
    fn clean_status_source() -> SessionStatusSource {
        SessionStatusSource {
            source: "coord",
            door: session_work_status::DOOR,
            requested: 0,
            resolved: 0,
            degraded: false,
            note: String::new(),
        }
    }

    /// The coord read FAILED. Every process blocks, and the response says so.
    fn degraded_status_source(requested: usize, note: &str) -> SessionStatusSource {
        SessionStatusSource {
            source: "unavailable",
            door: session_work_status::DOOR,
            requested,
            resolved: 0,
            degraded: true,
            note: note.to_string(),
        }
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
            clean_status_source(),
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
                has_live_children: Some(false),
                nested_under_claude: false,
                session_id: None,
                session_status: None,
                blocks_restart: true,
                wind_down: None,
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
        // 2026-09-10: the clause now LEADS with the activity split
        // (`<blocking> of <live>`), so the count and the label are no longer
        // adjacent. The label itself is unchanged and still names the plane.
        assert!(
            v.reason.contains("1 of 1 live terminal-hosted agent"),
            "{}",
            v.reason
        );
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
            &HashMap::new(),
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
        assert_eq!(
            first.has_live_children,
            Some(true),
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
            clean_status_source(),
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
            "nestedUnderClaude",
            "root_count",
            "cwd",
            "null on Windows",
            "hasLiveChildren",
            "never a verdict",
        ] {
            assert!(v.boundary.contains(claim), "boundary lost `{claim}`");
        }
    }

    /// No per-process field may appear in BOUNDARY under a spelling the
    /// payload does not use.
    ///
    /// BOUNDARY is emitted verbatim as the endpoint's own self-description, so
    /// a key named there that is not in the JSON sends a reader grepping for
    /// something that is not in the response. `LiveClaudeProcess` is
    /// `#[serde(rename_all = "camelCase")]` while `TerminalPlane` is NOT, and
    /// that mixed provenance is what drifted the prose: the sentence carried
    /// `has_live_children` / `session_status` / `nested_under_claude` against a
    /// camelCase payload, beside a correct `root_count` from the plane and a
    /// correct `windDown` added later in the same string.
    ///
    /// Derived from the SERIALIZED object rather than a literal list, in both
    /// directions: every camelCase key BOUNDARY mentions must appear
    /// backticked, and no key's snake_case twin may appear backticked at all.
    /// A fourth field added to the sentence later is therefore covered too.
    #[test]
    fn boundary_names_process_fields_by_their_serialized_keys() {
        fn to_snake(camel: &str) -> String {
            let mut out = String::new();
            for c in camel.chars() {
                if c.is_ascii_uppercase() {
                    out.push('_');
                    out.push(c.to_ascii_lowercase());
                } else {
                    out.push(c);
                }
            }
            out
        }

        let now_ms = chrono::Utc::now().timestamp_millis();
        let json = serde_json::to_value(wind_down_proc(
            1,
            Some("sess"),
            Some("finished"),
            false,
            false,
        ))
        .unwrap();
        let keys: Vec<String> = json.as_object().unwrap().keys().cloned().collect();
        assert!(
            keys.iter().any(|k| k == "hasLiveChildren"),
            "fixture did not serialize the field this test is about: {keys:?}"
        );

        let mut named = 0usize;
        for key in &keys {
            let snake = to_snake(key);
            if snake == *key {
                continue; // single-word key: no twin to confuse it with
            }
            // `session_status` is the ONE legitimate snake spelling in the
            // sentence — but as `coord.sessions.session_status`, the coord
            // COLUMN, so the backticked bare token must still be absent.
            assert!(
                !BOUNDARY.contains(&format!("`{snake}`")),
                "BOUNDARY carries `{snake}`, which the payload spells `{key}`"
            );
            if BOUNDARY.contains(&format!("`{key}`")) {
                named += 1;
            }
        }
        assert!(
            named >= 3,
            "BOUNDARY should still name the per-process keys it describes; named {named}"
        );

        // `root_count` IS correct: it lives on TerminalPlane, which has no
        // `rename_all`.
        let plane =
            serde_json::to_value(terminal_plane_from(&empty_report(now_ms), &[], now_ms)).unwrap();
        assert!(plane.get("root_count").is_some(), "{plane}");
        assert!(BOUNDARY.contains("`root_count`"));

        // `windDownCandidates` is camelCase only by an explicit
        // `#[serde(rename)]` on a struct with no `rename_all`, so it is the
        // most fragile spelling in the sentence: dropping that attribute
        // renames the field and nothing else would fail.
        let verdict = serde_json::to_value(verdict_from(
            &empty_report(now_ms),
            &[],
            Some(ai_plane_from(&[], &[], now_ms)),
            vec![],
            idle_drain(),
            fresh_census(now_ms),
            now_ms,
        ))
        .unwrap();
        assert!(
            verdict.get("windDownCandidates").is_some(),
            "the #[serde(rename)] on wind_down_candidates is gone: {verdict}"
        );
        assert!(BOUNDARY.contains("`windDownCandidates`"));
    }

    #[test]
    fn rfc3339_parsing() {
        assert_eq!(rfc3339_to_millis("1970-01-01T00:00:01Z"), Some(1000),);
        assert_eq!(rfc3339_to_millis("not-a-timestamp"), None);
    }

    // =======================================================================
    // The WORK axis — plan
    // `2026-09-10-restart-readiness-counts-open-sessions-not-active-ones`
    //
    // Every one of these drives the REAL `evaluate` over a synthetic snapshot,
    // with the coord status map INJECTED exactly as `cwd_by_pid` is. No
    // processes, no coord, no clock.
    // =======================================================================

    /// Three terminal-hosted sessions, one live `claude` each, claimed by
    /// their own live terminal.
    fn three_terminal_sessions(
        now_ms: i64,
    ) -> (
        crate::process_capture::process_tree::ProcessSnapshot,
        Vec<TerminalSessionRecord>,
        HashMap<String, u32>,
    ) {
        let snap = snap_with(
            &[(1, &[10, 20, 30]), (10, &[11]), (20, &[21]), (30, &[31])],
            &[
                (11, now_ms / 1000 - 600),
                (21, now_ms / 1000 - 600),
                (31, now_ms / 1000 - 600),
            ],
            &[(11, "claude"), (21, "claude"), (31, "claude")],
        );
        let records = vec![
            record("sess-0", "t-0", now_ms - 600_000),
            record("sess-1", "t-1", now_ms - 600_000),
            record("sess-2", "t-2", now_ms - 600_000),
        ];
        let terminal_pids: HashMap<String, u32> = [
            ("t-0".to_string(), 10u32),
            ("t-1".to_string(), 20u32),
            ("t-2".to_string(), 30u32),
        ]
        .into_iter()
        .collect();
        (snap, records, terminal_pids)
    }

    fn evaluate_with_status(
        snap: &crate::process_capture::process_tree::ProcessSnapshot,
        records: &[TerminalSessionRecord],
        terminal_pids: &HashMap<String, u32>,
        statuses: &HashMap<String, SessionWorkStatus>,
        now_ms: i64,
    ) -> TrackingHealthReport {
        evaluate(
            snap,
            1,
            records,
            terminal_pids,
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
            statuses,
            now_ms - 3_600_000,
            now_ms,
        )
    }

    fn statuses(pairs: &[(&str, &str)]) -> HashMap<String, SessionWorkStatus> {
        pairs
            .iter()
            .map(|(id, raw)| (id.to_string(), SessionWorkStatus::parse(raw)))
            .collect()
    }

    /// **The headline.** Every live session declared finished ⇒ nothing blocks
    /// ⇒ the restart is safe — and the reason says the processes are STILL
    /// RUNNING, because finishing never terminates anything.
    #[test]
    fn finished_sessions_do_not_block_the_restart() {
        let now_ms = 1_800_000_000_000;
        let (snap, records, tpids) = three_terminal_sessions(now_ms);
        let report = evaluate_with_status(
            &snap,
            &records,
            &tpids,
            &statuses(&[
                ("sess-0", "finished"),
                ("sess-1", "finished"),
                ("sess-2", "finished"),
            ]),
            now_ms,
        );
        assert_eq!(report.live_claude_total, 3);
        assert_eq!(report.terminal_hosted.len(), 3);

        let v = verdict_from(
            &report,
            &records,
            Some(ai_plane_from(&[], &[], now_ms)),
            vec![],
            idle_drain(),
            fresh_census(now_ms),
            now_ms,
        );
        let totals = v.live_claude.as_ref().expect("totals");
        assert_eq!(totals.total, 3, "the live count is UNCHANGED");
        assert_eq!(totals.blocking, 0);
        assert_eq!(totals.finished_discounted, 3);
        assert!(v.safe_to_restart, "reason was: {}", v.reason);
        assert!(
            v.reason.contains("still running"),
            "a safe verdict must never imply the box is empty: {}",
            v.reason
        );
        assert!(v.reason.contains("0 of 3"), "{}", v.reason);
    }

    /// **The direction-of-change pin.** One session still working ⇒ it still
    /// blocks, and BOTH numbers are reported (D6).
    #[test]
    fn a_working_session_still_blocks_and_both_numbers_are_reported() {
        let now_ms = 1_800_000_000_000;
        let (snap, records, tpids) = three_terminal_sessions(now_ms);
        let report = evaluate_with_status(
            &snap,
            &records,
            &tpids,
            &statuses(&[
                ("sess-0", "finished"),
                ("sess-1", "finished"),
                ("sess-2", "working"),
            ]),
            now_ms,
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
        let totals = v.live_claude.as_ref().expect("totals");
        assert_eq!(
            (totals.total, totals.blocking, totals.finished_discounted),
            (3, 1, 2)
        );
        assert!(!v.safe_to_restart);
        let t = v.terminal_sessions.as_ref().expect("terminal plane");
        assert_eq!((t.count, t.blocking_count, t.finished_count), (3, 1, 2));
        assert!(v.reason.contains("1 of 3"), "{}", v.reason);
        assert!(v.reason.contains("still working"), "{}", v.reason);
        assert!(v.reason.contains("2 marked finished"), "{}", v.reason);
        assert!(
            v.reason.contains("still RUNNING"),
            "the discount must never read as termination: {}",
            v.reason
        );
    }

    /// Every NON-terminal status blocks. `finished` is the only word that
    /// discounts anything.
    #[test]
    fn every_non_finished_status_still_blocks() {
        let now_ms = 1_800_000_000_000;
        for word in ["working", "blocked", "stalled", "waiting_human"] {
            let (snap, records, tpids) = three_terminal_sessions(now_ms);
            let report = evaluate_with_status(
                &snap,
                &records,
                &tpids,
                &statuses(&[("sess-0", word), ("sess-1", word), ("sess-2", word)]),
                now_ms,
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
            let totals = v.live_claude.as_ref().expect("totals");
            assert_eq!(totals.blocking, 3, "status `{word}` must block");
            assert_eq!(totals.finished_discounted, 0, "status `{word}`");
            assert!(
                !v.safe_to_restart,
                "status `{word}` must never read as safe"
            );
        }
    }

    /// An ABSENT status is UNKNOWN and blocks. Absence is never "finished".
    #[test]
    fn an_absent_status_blocks() {
        let now_ms = 1_800_000_000_000;
        let (snap, records, tpids) = three_terminal_sessions(now_ms);
        let report = evaluate_with_status(
            &snap,
            &records,
            &tpids,
            &statuses(&[("sess-0", "finished")]),
            now_ms,
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
        let totals = v.live_claude.as_ref().expect("totals");
        assert_eq!(
            (totals.total, totals.blocking, totals.finished_discounted),
            (3, 2, 1)
        );
        assert!(!v.safe_to_restart);
        let t = v.terminal_sessions.as_ref().expect("terminal plane");
        let without: Vec<&LiveClaudeProcess> = t
            .processes
            .iter()
            .filter(|p| p.session_status.is_none())
            .collect();
        assert_eq!(without.len(), 2);
        assert!(
            without.iter().all(|p| p.blocks_restart),
            "a process with no status must block"
        );
    }

    /// A status word this build does not know reaches the operator VERBATIM
    /// and BLOCKS. A growing vocabulary must never grow a new way to say safe.
    #[test]
    fn an_unrecognised_status_blocks_and_is_carried_verbatim() {
        let now_ms = 1_800_000_000_000;
        let (snap, records, tpids) = three_terminal_sessions(now_ms);
        let report = evaluate_with_status(
            &snap,
            &records,
            &tpids,
            &statuses(&[
                ("sess-0", "finished"),
                ("sess-1", "finished"),
                ("sess-2", "vacationing"),
            ]),
            now_ms,
        );
        let odd = report
            .terminal_hosted
            .iter()
            .find(|p| p.session_id.as_deref() == Some("sess-2"))
            .expect("sess-2 process");
        assert_eq!(odd.session_status.as_deref(), Some("vacationing"));
        assert!(odd.blocks_restart);

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
        assert_eq!(v.live_claude.as_ref().unwrap().blocking, 1);
    }

    /// coord's legacy `"done"` wire word parses to `Finished` in coord itself,
    /// so it must mean the same here — otherwise an old writer's finish
    /// silently stops counting.
    #[test]
    fn the_legacy_done_alias_counts_as_finished() {
        let now_ms = 1_800_000_000_000;
        let (snap, records, tpids) = three_terminal_sessions(now_ms);
        let report = evaluate_with_status(
            &snap,
            &records,
            &tpids,
            &statuses(&[("sess-0", "done"), ("sess-1", "done"), ("sess-2", "DONE")]),
            now_ms,
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
        assert_eq!(v.live_claude.as_ref().unwrap().finished_discounted, 3);
        assert!(v.safe_to_restart, "{}", v.reason);
    }

    /// **AMBIGUOUS attribution blocks.** Two open records whose terminals
    /// share a subtree both claim the same live `claude`; the runner cannot
    /// say whose it is, so it reports no session id and counts it as work —
    /// even when BOTH candidate sessions are marked finished.
    #[test]
    fn an_ambiguous_process_to_session_mapping_blocks() {
        let now_ms = 1_800_000_000_000;
        let snap = snap_with(
            &[(1, &[10]), (10, &[20]), (20, &[11])],
            &[(11, now_ms / 1000 - 600)],
            &[(11, "claude")],
        );
        let records = vec![
            record("sess-a", "t-a", now_ms - 600_000),
            record("sess-b", "t-b", now_ms - 600_000),
        ];
        let tpids: HashMap<String, u32> = [("t-a".to_string(), 10u32), ("t-b".to_string(), 20u32)]
            .into_iter()
            .collect();
        let report = evaluate_with_status(
            &snap,
            &records,
            &tpids,
            &statuses(&[("sess-a", "finished"), ("sess-b", "finished")]),
            now_ms,
        );
        assert_eq!(report.terminal_hosted.len(), 1);
        let p = &report.terminal_hosted[0];
        assert_eq!(p.session_id, None, "an ambiguous claim names no session");
        assert_eq!(p.session_status, None);
        assert!(
            p.blocks_restart,
            "ambiguity is an unknown, and unknowns block"
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
        assert_eq!(v.live_claude.as_ref().unwrap().blocking, 1);
    }

    /// The work axis is TERMINAL-HOSTED ONLY. An AI-plane, headless-exempt or
    /// unclassified process has no `claude_session_id` coord can be asked
    /// about, so it blocks regardless of what the status map contains.
    #[test]
    fn non_terminal_planes_are_never_discounted() {
        let now_ms = 1_800_000_000_000;
        let snap = snap_with(
            &[
                (1, &[10, 20, 30, 41]),
                (10, &[11]),
                (20, &[21]),
                (30, &[31]),
            ],
            &[],
            &[
                (11, "claude"),
                (21, "claude"),
                (31, "claude"),
                (41, "claude"),
            ],
        );
        let records = vec![record("sess-0", "t-0", now_ms - 600_000)];
        let tpids: HashMap<String, u32> = [("t-0".to_string(), 10u32)].into_iter().collect();
        let map = statuses(&[
            ("sess-0", "finished"),
            ("sess-ai", "finished"),
            ("sess-headless", "finished"),
        ]);
        let report = evaluate(
            &snap,
            1,
            &records,
            &tpids,
            &[30u32].into_iter().collect(),
            &[20u32].into_iter().collect(),
            &HashMap::new(),
            &map,
            now_ms - 3_600_000,
            now_ms,
        );
        assert_eq!(report.live_claude_total, 4);
        assert_eq!(report.terminal_hosted.len(), 1);
        assert_eq!(report.ai_plane.len(), 1);
        assert_eq!(report.headless_exempt.len(), 1);
        assert_eq!(report.live_untracked.len(), 1);
        for p in report
            .ai_plane
            .iter()
            .chain(&report.headless_exempt)
            .chain(&report.live_untracked)
        {
            assert_eq!(p.session_id, None);
            assert_eq!(p.session_status, None);
            assert!(p.blocks_restart, "pid {} must block", p.pid);
        }
        let totals = live_claude_totals_from(&report);
        assert_eq!(
            (totals.total, totals.blocking, totals.finished_discounted),
            (4, 3, 1)
        );
        assert!(report.partition_covers_total());
    }

    /// **The regression pin.** With an EMPTY status map — which is exactly
    /// what a coord outage produces — the verdict is the pre-2026-09-10 one:
    /// `blocking == total`, nothing discounted, unsafe.
    #[test]
    fn an_empty_status_map_reproduces_the_previous_verdict() {
        let now_ms = 1_800_000_000_000;
        let (snap, records, tpids) = three_terminal_sessions(now_ms);
        let report = evaluate_with_status(&snap, &records, &tpids, &HashMap::new(), now_ms);
        let totals = live_claude_totals_from(&report);
        assert_eq!(totals.total, 3);
        assert_eq!(
            totals.blocking, totals.total,
            "with no status source, EVERY live process blocks"
        );
        assert_eq!(totals.finished_discounted, 0);
        assert!(report.terminal_hosted.iter().all(|p| p.blocks_restart));

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
        assert!(v.reason.contains("3 of 3"), "{}", v.reason);
        assert!(
            !v.reason.contains("marked finished"),
            "nothing was discounted: {}",
            v.reason
        );
    }

    /// A coord outage is DEGRADED, not UNKNOWN: the endpoint still answers,
    /// every process blocks, `unknowns` stays empty, and the response says
    /// which of the two worlds produced the number.
    #[test]
    fn an_unreadable_work_axis_is_degraded_not_unknown() {
        let now_ms = 1_800_000_000_000;
        let (snap, records, tpids) = three_terminal_sessions(now_ms);
        let report = evaluate_with_status(&snap, &records, &tpids, &HashMap::new(), now_ms);
        let v = build_verdict(
            Some(terminal_plane_from(&report, &records, now_ms)),
            Some(headless_plane_from(&report)),
            Some(ai_plane_from(&[], &[], now_ms)),
            Some(live_claude_totals_from(&report)),
            vec![],
            idle_drain(),
            fresh_census(now_ms),
            degraded_status_source(3, "coord work-status: request timed out after 2s"),
        );
        assert!(!v.safe_to_restart);
        assert!(
            !v.reason.starts_with("UNKNOWN"),
            "a coord blip must not make the endpoint unreadable: {}",
            v.reason
        );
        assert!(v.reason.contains("could NOT be read"), "{}", v.reason);
        assert!(v.reason.contains("timed out"), "{}", v.reason);
        assert!(v.reason.contains("fail-closed"), "{}", v.reason);
        assert!(v.session_status_source.degraded);
        assert_eq!(v.session_status_source.source, "unavailable");
        assert_eq!(v.live_claude.as_ref().unwrap().blocking, 3);
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(
            json["session_status_source"]["degraded"],
            serde_json::json!(true)
        );
        assert_eq!(json["live_claude"]["blocking"], serde_json::json!(3));
        assert_eq!(
            json["live_claude"]["finished_discounted"],
            serde_json::json!(0)
        );
    }

    /// The per-process detail carries the join, so an operator triaging
    /// remotely can see WHICH sessions are still working before deciding what
    /// to close (initiative scope 1).
    #[test]
    fn per_process_detail_names_the_session_and_its_status() {
        let now_ms = 1_800_000_000_000;
        let (snap, records, tpids) = three_terminal_sessions(now_ms);
        let report = evaluate_with_status(
            &snap,
            &records,
            &tpids,
            &statuses(&[
                ("sess-0", "finished"),
                ("sess-1", "working"),
                ("sess-2", "waiting_human"),
            ]),
            now_ms,
        );
        let t = terminal_plane_from(&report, &records, now_ms);
        let json = serde_json::to_value(&t).unwrap();
        let procs = json["processes"].as_array().expect("processes");
        assert_eq!(procs.len(), 3);
        let mut seen: Vec<(String, String, bool)> = procs
            .iter()
            .map(|p| {
                (
                    p["sessionId"].as_str().unwrap_or("<none>").to_string(),
                    p["sessionStatus"].as_str().unwrap_or("<none>").to_string(),
                    p["blocksRestart"].as_bool().expect("blocksRestart"),
                )
            })
            .collect();
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ("sess-0".to_string(), "finished".to_string(), false),
                ("sess-1".to_string(), "working".to_string(), true),
                ("sess-2".to_string(), "waiting_human".to_string(), true),
            ]
        );
    }

    /// The BOUNDARY says, verbatim on every response, that a finished session
    /// is still running — the honest limitation this change must not hide.
    #[test]
    fn boundary_states_that_finishing_does_not_terminate_the_process() {
        assert!(BOUNDARY.contains("sessionStatus"));
        assert!(BOUNDARY.contains("PROCESS IS STILL RUNNING"));
        assert!(BOUNDARY.contains("NEVER \"not running\""));
        assert!(BOUNDARY.contains("ambiguous"));
    }

    // ---- wind-down (reported here; ACTED ON by the executor) -------------

    use crate::session::wind_down_observer::{
        apply_wind_down, terminal_ids_by_session, TerminalObservation,
    };
    use qontinui_runner_lib::wind_down::{GridIdle, SessionKind, Sideband};

    fn wind_down_proc(
        pid: u32,
        session_id: Option<&str>,
        status: Option<&str>,
        nested: bool,
        children: bool,
    ) -> LiveClaudeProcess {
        LiveClaudeProcess {
            pid,
            parent_pid: None,
            image: Some("claude".to_string()),
            age_s: Some(3_600),
            cwd: None,
            has_live_children: Some(children),
            nested_under_claude: nested,
            session_id: session_id.map(str::to_string),
            session_status: status.map(str::to_string),
            blocks_restart: status != Some("finished"),
            wind_down: None,
        }
    }

    const IDLE_LONG_AGO: TerminalObservation = TerminalObservation {
        sideband: Sideband::NeverReported,
        grid: GridIdle::Idle { since_ms: 1_000 },
    };

    #[test]
    fn wind_down_is_attached_per_top_level_terminal_process_and_counted() {
        let now_ms = 1_000 + 3_600_000;
        let grace = std::time::Duration::from_secs(600);
        let open = vec![
            record("sess-done", "t-done", 1),
            record("sess-busy", "t-busy", 1),
            record("sess-gone", "t-gone", 1),
            record("sess-loop", "t-loop", 1),
        ];
        let mut processes = vec![
            // finished + idle for an hour → eligible
            wind_down_proc(10, Some("sess-done"), Some("finished"), false, false),
            // its nested subagent → no windDown at all
            wind_down_proc(11, Some("sess-done"), None, true, false),
            // still working on the coord axis → ineligible, never eligible
            wind_down_proc(20, Some("sess-busy"), Some("working"), false, false),
            // pane not observable → unknown
            wind_down_proc(30, Some("sess-gone"), Some("finished"), false, false),
            // ambiguous attribution (no session id) → unknown
            wind_down_proc(40, None, None, false, false),
            // a looping agent needs no finished declaration
            wind_down_proc(50, Some("sess-loop"), None, false, false),
        ];
        let observations: HashMap<String, TerminalObservation> = [
            ("t-done".to_string(), IDLE_LONG_AGO),
            ("t-busy".to_string(), IDLE_LONG_AGO),
            ("t-loop".to_string(), IDLE_LONG_AGO),
        ]
        .into();
        let loops: HashSet<String> = ["t-loop".to_string()].into();

        apply_wind_down(
            &mut processes,
            &terminal_ids_by_session(&open),
            &observations,
            &HashMap::new(),
            &HashSet::new(),
            &loops,
            grace,
            now_ms,
        );

        let view = |i: usize| processes[i].wind_down.clone();
        let done = view(0).expect("top-level process gets a view");
        assert_eq!((done.eligibility, done.since), ("eligible", Some(1_000)));
        assert_eq!(view(1), None, "nested subagent");
        let busy = view(2).unwrap();
        assert_eq!(
            (busy.eligibility, busy.reason),
            ("ineligible", Some("not_finished"))
        );
        let gone = view(3).unwrap();
        assert_eq!(gone.eligibility, "unknown");
        let ambiguous = view(4).unwrap();
        assert_eq!(
            (ambiguous.eligibility, ambiguous.reason),
            ("unknown", Some("work_status_unknown"))
        );
        let looping = view(5).unwrap();
        assert_eq!(
            (looping.eligibility, looping.kind),
            ("eligible", SessionKind::Looping)
        );

        let report = TrackingHealthReport {
            live_claude_total: processes.len(),
            tracked_open_total: open.len(),
            terminal_hosted: processes,
            ..empty_report(now_ms)
        };
        let v = verdict_from(
            &report,
            &open,
            Some(ai_plane_from(&[], &[], now_ms)),
            vec![],
            idle_drain(),
            fresh_census(now_ms),
            now_ms,
        );
        assert_eq!(v.wind_down_candidates, Some(2));

        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["windDownCandidates"], 2);
        let procs = json["terminal_sessions"]["processes"].as_array().unwrap();
        assert_eq!(procs[0]["windDown"]["eligibility"], "eligible");
        assert_eq!(procs[0]["windDown"]["since"], 1_000);
        assert!(procs[1].get("windDown").is_none(), "nested omits windDown");
    }

    #[test]
    fn wind_down_does_not_change_the_restart_verdict() {
        // An eligible finished session is already discounted by the work
        // axis; wind-down eligibility is a report beside the verdict and moves
        // nothing in it.
        let now_ms = 1_000 + 3_600_000;
        let open = vec![record("sess-done", "t-done", 1)];
        let mut with = vec![wind_down_proc(
            10,
            Some("sess-done"),
            Some("working"),
            false,
            false,
        )];
        let without = with.clone();
        apply_wind_down(
            &mut with,
            &terminal_ids_by_session(&open),
            &[("t-done".to_string(), IDLE_LONG_AGO)].into(),
            &HashMap::new(),
            &HashSet::new(),
            &HashSet::new(),
            std::time::Duration::from_secs(600),
            now_ms,
        );
        let verdict = |procs: Vec<LiveClaudeProcess>| {
            let report = TrackingHealthReport {
                live_claude_total: procs.len(),
                tracked_open_total: 1,
                terminal_hosted: procs,
                ..empty_report(now_ms)
            };
            verdict_from(
                &report,
                &open,
                Some(ai_plane_from(&[], &[], now_ms)),
                vec![],
                idle_drain(),
                fresh_census(now_ms),
                now_ms,
            )
        };
        let (a, b) = (verdict(with), verdict(without));
        assert_eq!(
            (a.safe_to_restart, &a.reason),
            (b.safe_to_restart, &b.reason)
        );
        assert_eq!(a.wind_down_candidates, Some(0));
        assert_eq!(b.wind_down_candidates, Some(0));
    }

    #[test]
    fn wind_down_candidates_are_null_when_the_terminal_plane_is_unknown() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let v = build_verdict(
            None,
            None,
            Some(ai_plane_from(&[], &[], now_ms)),
            None,
            vec!["the terminal-session plane could not be determined: test".to_string()],
            idle_drain(),
            fresh_census(now_ms),
            clean_status_source(),
        );
        assert_eq!(v.wind_down_candidates, None);
        assert!(serde_json::to_value(&v).unwrap()["windDownCandidates"].is_null());
    }

    #[test]
    /// Phase 4 made the verdict ACTIONABLE, so the boundary must no longer
    /// call it a dry run — but it must still say that THIS endpoint closes
    /// nothing, and that an `eligible` is a candidacy rather than a prediction.
    fn boundary_states_what_wind_down_is_and_is_not() {
        assert!(
            !BOUNDARY.contains("DRY-RUN"),
            "the wind-down executor acts on this verdict while drained — the \
             boundary must not still call it a dry run"
        );
        assert!(BOUNDARY.contains("THIS ENDPOINT closes nothing"));
        assert!(BOUNDARY.contains("WHILE COORD HOLDS THIS DEVICE DRAINED"));
        assert!(BOUNDARY.contains("a candidacy and never a prediction"));
        assert!(BOUNDARY.contains("windDownCandidates"));
        assert!(BOUNDARY.contains("whether or not the runner is drained"));
    }

    // ---- the ACTIVITY axis: `live_claude.by_activity` (report-only) -------
    //
    // Plan `2026-09-29-quiet-is-measured-by-session-existence-and-machine-wide-so-a-24x7-box-never-gets-one`,
    // Phase 6. The classification table itself is pinned in
    // `session::claude_activity::tests`; these pin the AGGREGATION — every
    // census class is classified, the cells sum to `total`, the pane is
    // consulted only for the process it hosts, and the verdict never moves.

    const ACT_NOW: i64 = 1_790_000_000_000;

    fn act_record(status: &str, msg_age_min: Option<i64>) -> RecordReading {
        RecordReading::Parsed(claude_activity::RecordEvidence {
            status: status.to_string(),
            status_updated_at_ms: None,
            last_message_ms: msg_age_min.map(|m| ACT_NOW - m * 60_000),
        })
    }

    /// One process per census class plus extras, so every class and every
    /// record shape is represented.
    fn activity_report() -> TrackingHealthReport {
        TrackingHealthReport {
            checked_at_ms: ACT_NOW,
            live_claude_total: 9,
            tracked_open_total: 3,
            terminal_hosted: vec![
                wind_down_proc(1, Some("s1"), Some("working"), false, false), // pane: working
                wind_down_proc(2, Some("s2"), Some("finished"), false, false), // pane: idle
                wind_down_proc(3, Some("s1"), None, true, false),             // nested: record
                // An idle pane, but a live child (a days-old MCP shim) vetoes
                // pane-idle WITHOUT counting as work: the record decides.
                wind_down_proc(4, Some("s4"), Some("working"), false, true),
            ],
            ai_plane: vec![wind_down_proc(5, None, None, false, false)],
            headless_exempt: vec![
                wind_down_proc(6, None, None, false, false),
                wind_down_proc(7, None, None, false, false),
            ],
            live_untracked: vec![
                wind_down_proc(8, None, None, false, false),
                wind_down_proc(9, None, None, false, false),
            ],
            tracked_dead: vec![],
        }
    }

    fn activity_evidence() -> ActivityEvidence {
        ActivityEvidence {
            pane_by_pid: [
                (
                    1,
                    TerminalObservation {
                        sideband: Sideband::NeverReported,
                        grid: GridIdle::Busy,
                    },
                ),
                (
                    2,
                    TerminalObservation {
                        sideband: Sideband::Reported {
                            state: qontinui_runner_lib::wind_down::SidebandState::NotWorking,
                            set_at_ms: 1,
                        },
                        grid: GridIdle::Idle { since_ms: 1 },
                    },
                ),
                (
                    4,
                    TerminalObservation {
                        sideband: Sideband::Reported {
                            state: qontinui_runner_lib::wind_down::SidebandState::NotWorking,
                            set_at_ms: 1,
                        },
                        grid: GridIdle::Idle { since_ms: 1 },
                    },
                ),
            ]
            .into(),
            records: [
                // pid 1 and 2: the pane decides, whatever the record says.
                (1, act_record("idle", None)),
                (2, act_record("busy", Some(1))),
                (3, act_record("busy", Some(2))),       // working
                (4, act_record("waiting", None)),       // idle
                (5, act_record("shell", Some(600))),    // stale
                (6, RecordReading::Unparseable),        // unknown
                (7, act_record("compacting", Some(1))), // unknown status
                (8, RecordReading::Ambiguous),          // unknown
                                                        // pid 9: no record at all — unknown.
            ]
            .into(),
            now_ms: ACT_NOW,
        }
    }

    /// `process_activity` is the per-process twin of `by_activity`: the
    /// quiet-barrier `resume` block reads it, so the two can never disagree
    /// about which process is idle. Pid 4 is the case that matters to resume —
    /// an idle pane whose MCP-shim child vetoes pane-idle, decided `idle` by
    /// Claude Code's record.
    #[test]
    fn process_activity_is_the_per_process_twin_of_by_activity() {
        use claude_activity::Activity;
        let report = activity_report();
        let evidence = activity_evidence();
        let mut summed = ActivityCounts::default();
        for list in [
            &report.terminal_hosted,
            &report.ai_plane,
            &report.headless_exempt,
            &report.live_untracked,
        ] {
            for p in list {
                summed.add(process_activity(p, &evidence));
            }
        }
        assert_eq!(summed, activity_counts(&report, &evidence));
        let by_pid = |pid: u32| {
            report
                .terminal_hosted
                .iter()
                .find(|p| p.pid == pid)
                .map(|p| process_activity(p, &evidence))
        };
        assert_eq!(by_pid(1), Some(Activity::Working));
        assert_eq!(by_pid(2), Some(Activity::Idle));
        assert_eq!(by_pid(4), Some(Activity::Idle));
        // No evidence at all is `unknown`, never `idle` — resume blocks on it.
        assert_eq!(
            process_activity(&report.terminal_hosted[1], &ActivityEvidence::default()),
            Activity::Unknown
        );

        // Pid 4's record reads `waiting`: the activity axis says `idle`, the
        // resume-only rule says a prompt is pending (plan D2).
        let p4 = &report.terminal_hosted[3];
        assert_eq!(p4.pid, 4);
        assert_eq!(process_activity(p4, &evidence), Activity::Idle);
        use crate::quiet_barrier::resume::RecordWaitingRead;
        assert_eq!(
            record_waiting_read(p4, &evidence),
            RecordWaitingRead::Waiting
        );
        assert_eq!(
            record_waiting_read(&report.terminal_hosted[1], &evidence),
            RecordWaitingRead::NotWaiting
        );
    }

    /// Under a barrier every live process's record is read, pane-decided or
    /// not — and `by_activity` does not move, because the pane still decides
    /// first.
    #[test]
    fn reading_pane_decided_records_never_moves_by_activity() {
        let report = activity_report();
        let evidence = activity_evidence();
        let undecided = pids_needing_a_record(&report, &evidence.pane_by_pid, ACT_NOW, false);
        let all = pids_needing_a_record(&report, &evidence.pane_by_pid, ACT_NOW, true);
        assert_eq!(all.len(), report.live_claude_total);
        assert!(undecided.len() < all.len());
        // Pid 2's pane decides `idle`; a `waiting` record for it changes
        // nothing on the activity axis.
        let mut with_pane_decided = evidence.clone();
        with_pane_decided
            .records
            .insert(2, act_record("waiting", None));
        assert_eq!(
            activity_counts(&report, &with_pane_decided),
            activity_counts(&report, &evidence)
        );
        assert_eq!(
            record_waiting_read(&report.terminal_hosted[1], &with_pane_decided),
            crate::quiet_barrier::resume::RecordWaitingRead::Waiting
        );
    }

    #[test]
    fn by_activity_classifies_every_census_class_and_sums_to_total() {
        let report = activity_report();
        let totals = live_claude_totals_observed(&report, &activity_evidence());
        assert_eq!(
            totals.by_activity,
            ActivityCounts {
                working: 2, // pid 1 (pane grid busy), pid 3 (busy + recent message)
                idle: 2, // pid 2 (pane idle beats a busy record), pid 4 (child-vetoed pane, waiting record)
                stale: 1, // pid 5 (shell, silent 10 h)
                unknown: 4, // pids 6-9: unparseable, unknown status, ambiguous, missing
            }
        );
        assert_eq!(totals.by_activity.sum(), totals.total);
        assert_eq!(totals.total, 9);
    }

    #[test]
    fn by_activity_with_no_evidence_is_all_unknown_never_idle() {
        let report = activity_report();
        let totals = live_claude_totals_from(&report);
        assert_eq!(
            totals.by_activity,
            ActivityCounts {
                unknown: 9,
                ..ActivityCounts::default()
            }
        );
        assert_eq!(totals.by_activity.sum(), totals.total);
        // And on an empty box every cell is zero, still summing to total.
        let empty = live_claude_totals_from(&empty_report(ACT_NOW));
        assert_eq!(empty.by_activity, ActivityCounts::default());
        assert_eq!(empty.by_activity.sum(), empty.total);
    }

    #[test]
    fn by_activity_does_not_move_any_other_count_or_the_verdict() {
        let report = activity_report();
        let plain = live_claude_totals_from(&report);
        let observed = live_claude_totals_observed(&report, &activity_evidence());
        assert_eq!(
            LiveClaudeTotals {
                by_activity: plain.by_activity,
                ..observed.clone()
            },
            plain,
            "only by_activity may differ"
        );

        // An all-idle box that is still blocking stays unsafe: the verdict
        // reads `blocking`, never `by_activity`.
        let one = TrackingHealthReport {
            live_claude_total: 1,
            terminal_hosted: vec![wind_down_proc(1, Some("s1"), Some("working"), false, false)],
            ..empty_report(ACT_NOW)
        };
        let idle_everywhere = ActivityEvidence {
            records: [(1, act_record("idle", None))].into(),
            now_ms: ACT_NOW,
            ..ActivityEvidence::default()
        };
        let totals = live_claude_totals_observed(&one, &idle_everywhere);
        assert_eq!(totals.by_activity.idle, 1);
        let v = build_verdict(
            Some(terminal_plane_from(&one, &[], ACT_NOW)),
            Some(headless_plane_from(&one)),
            Some(ai_plane_from(&[], &[], ACT_NOW)),
            Some(totals),
            vec![],
            idle_drain(),
            fresh_census(ACT_NOW),
            clean_status_source(),
        );
        assert!(!v.safe_to_restart, "an idle session still dies on restart");
    }

    #[test]
    fn by_activity_serializes_under_live_claude_in_the_modules_snake_case() {
        let report = activity_report();
        let v = build_verdict(
            Some(terminal_plane_from(&report, &[], ACT_NOW)),
            Some(headless_plane_from(&report)),
            Some(ai_plane_from(&[], &report.ai_plane, ACT_NOW)),
            Some(live_claude_totals_observed(&report, &activity_evidence())),
            vec![],
            idle_drain(),
            fresh_census(ACT_NOW),
            clean_status_source(),
        );
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(
            json["live_claude"]["by_activity"],
            serde_json::json!({"working": 2, "idle": 2, "stale": 1, "unknown": 4})
        );
        assert!(json["live_claude"].get("byActivity").is_none(), "{json}");
    }

    #[test]
    fn pane_observations_cover_only_observed_top_level_terminal_processes() {
        let report = activity_report();
        let observed = ObservedInputs {
            terminal_by_session: [
                ("s1".to_string(), "t1".to_string()),
                ("s2".to_string(), "t2".to_string()),
                ("s4".to_string(), "t4".to_string()),
            ]
            .into(),
            // t4 was not observed this pass.
            by_terminal: [
                ("t1".to_string(), IDLE_LONG_AGO),
                ("t2".to_string(), TerminalObservation::UNOBSERVABLE),
            ]
            .into(),
            ..ObservedInputs::default()
        };
        let by_pid = pane_observations_by_pid(&report, &observed);
        // pid 3 shares s1's pane but is nested: the pane is not ITS
        // observation. pid 4's pane was never observed: no stand-in entry.
        assert_eq!(
            by_pid,
            [(1, IDLE_LONG_AGO), (2, TerminalObservation::UNOBSERVABLE)].into()
        );
    }

    /// Only processes the pane did NOT decide get a record read (L2): pid 1
    /// (busy grid) and pid 2 (idle pane) are decided; pid 4's idle pane is
    /// vetoed by its child, so it still needs one.
    #[test]
    fn records_are_read_only_for_processes_the_pane_did_not_decide() {
        let report = activity_report();
        let evidence = activity_evidence();
        let pids: Vec<u32> = pids_needing_a_record(&report, &evidence.pane_by_pid, ACT_NOW, false)
            .into_iter()
            .map(|p| p.pid)
            .collect();
        assert_eq!(pids, vec![3, 4, 5, 6, 7, 8, 9]);
    }

    fn write_record(dir: &std::path::Path, pid: u32, status: &str) {
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        std::fs::write(
            dir.join(format!("sessions/{pid}.json")),
            format!(r#"{{"pid":{pid},"sessionId":"s{pid}","status":"{status}","procStart":"77"}}"#),
        )
        .unwrap();
    }

    fn kernel_agrees(_: u32) -> Option<String> {
        Some("77".to_string())
    }

    /// L1: the handler's evidence assembly, end to end against a temp config
    /// dir — pane-decided processes, record-decided ones, and a missing
    /// record, summing to `total`.
    #[tokio::test]
    async fn gather_activity_evidence_reads_records_from_the_given_config_dirs() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(".claude-x");
        for pid in [3, 4, 6, 7, 8] {
            write_record(&dir, pid, "idle");
        }
        write_record(&dir, 5, "waiting");
        // pid 9: no record at all.
        let report = activity_report();
        let observed = ObservedInputs {
            terminal_by_session: [
                ("s1".to_string(), "t1".to_string()),
                ("s2".to_string(), "t2".to_string()),
            ]
            .into(),
            by_terminal: [
                (
                    "t1".to_string(),
                    TerminalObservation {
                        sideband: Sideband::NeverReported,
                        grid: GridIdle::Busy,
                    },
                ),
                ("t2".to_string(), IDLE_LONG_AGO),
            ]
            .into(),
            ..ObservedInputs::default()
        };
        static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
        let evidence = gather_activity_evidence(
            &report,
            &observed,
            vec![dir],
            kernel_agrees,
            RECORD_READ_TIMEOUT,
            &IN_FLIGHT,
            false,
        )
        .await;
        assert!(
            !evidence.records.contains_key(&1),
            "a pane-decided process is never read"
        );
        let totals = live_claude_totals_observed(&report, &evidence);
        // pid 1 working (busy grid); pid 2's pane never reported a sideband so
        // it falls to the record — none written for it, so unknown; 3-8 idle
        // from their records; 9 missing → unknown.
        assert_eq!(
            totals.by_activity,
            ActivityCounts {
                working: 1,
                idle: 6,
                stale: 0,
                unknown: 2,
            }
        );
        assert_eq!(totals.by_activity.sum(), totals.total);
    }

    /// L2: a record read that overruns its budget costs the records — every
    /// undecided process reads `unknown`, never `idle` — and the sum holds.
    /// The kernel read sleeps ONCE, so the abandoned thread ends promptly.
    #[tokio::test]
    async fn a_record_read_that_times_out_leaves_every_undecided_process_unknown() {
        static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
        static SLEPT: AtomicBool = AtomicBool::new(false);
        fn slow_once(_: u32) -> Option<String> {
            if !SLEPT.swap(true, Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            Some("77".to_string())
        }
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(".claude-x");
        for pid in 1..=9 {
            write_record(&dir, pid, "idle");
        }
        let report = activity_report();
        let evidence = gather_activity_evidence(
            &report,
            &ObservedInputs::default(),
            vec![dir],
            slow_once,
            std::time::Duration::from_millis(20),
            &IN_FLIGHT,
            false,
        )
        .await;
        assert!(evidence.records.is_empty());
        let totals = live_claude_totals_observed(&report, &evidence);
        assert_eq!(
            totals.by_activity,
            ActivityCounts {
                unknown: 9,
                ..ActivityCounts::default()
            }
        );
    }

    /// N2: while an abandoned read is still running, a new call does NOT
    /// start another (that would leak a blocking thread per poll on a hung
    /// filesystem) — it reports `unknown` — and once the read ends, reads
    /// resume.
    #[tokio::test]
    async fn only_one_record_read_is_in_flight_at_a_time() {
        static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
        static SLEPT: AtomicBool = AtomicBool::new(false);
        fn slow_once(_: u32) -> Option<String> {
            if !SLEPT.swap(true, Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            Some("77".to_string())
        }
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(".claude-x");
        for pid in 1..=9 {
            write_record(&dir, pid, "idle");
        }
        let report = activity_report();
        let observed = ObservedInputs::default();
        async fn gather(
            report: &TrackingHealthReport,
            observed: &ObservedInputs,
            dir: &std::path::Path,
            kernel: fn(u32) -> Option<String>,
            timeout_ms: u64,
        ) -> ActivityEvidence {
            gather_activity_evidence(
                report,
                observed,
                vec![dir.to_path_buf()],
                kernel,
                std::time::Duration::from_millis(timeout_ms),
                &IN_FLIGHT,
                false,
            )
            .await
        }

        // 1. Times out; its blocking read is still running.
        assert!(gather(&report, &observed, &dir, slow_once, 20)
            .await
            .records
            .is_empty());
        assert!(
            IN_FLIGHT.load(Ordering::SeqCst),
            "the abandoned read holds the slot"
        );

        // 2. Skipped outright, even with a fast kernel and a generous budget.
        let skipped = gather(&report, &observed, &dir, kernel_agrees, 3_000).await;
        assert!(skipped.records.is_empty());
        assert_eq!(
            live_claude_totals_observed(&report, &skipped)
                .by_activity
                .unknown,
            9
        );

        // 3. Once the abandoned read finishes, the slot is free again.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while IN_FLIGHT.load(Ordering::SeqCst) {
            assert!(std::time::Instant::now() < deadline, "the read never ended");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let resumed = gather(&report, &observed, &dir, kernel_agrees, 3_000).await;
        assert_eq!(resumed.records.len(), 9);
        assert_eq!(
            live_claude_totals_observed(&report, &resumed)
                .by_activity
                .idle,
            9
        );
    }

    // ── Quiet-barrier resume fields (plan 2026-09-29-quiet-on-demand, D5) ──

    fn idle_verdict() -> RestartReadiness {
        let now_ms = chrono::Utc::now().timestamp_millis();
        verdict_from(
            &empty_report(now_ms),
            &[],
            Some(ai_plane_from(&[], &[], now_ms)),
            vec![],
            idle_drain(),
            fresh_census(now_ms),
            now_ms,
        )
    }

    fn open_barrier() -> crate::quiet_barrier::BarrierState {
        let crate::quiet_barrier::FileRecord::Recorded(b) = crate::quiet_barrier::parse_record(
            include_bytes!("../quiet_barrier/fixtures/open-runner-restart.json"),
            None,
        ) else {
            panic!("fixture must parse");
        };
        crate::quiet_barrier::BarrierState::Open(b)
    }

    /// Without a barrier the new fields are present, `false` / `null`, with a
    /// reason — and the existing verdict is untouched.
    #[test]
    fn resume_fields_serialize_false_and_null_without_a_barrier() {
        let mut v = idle_verdict();
        assert!(v.safe_to_restart);
        attach_resume(&mut v, &crate::quiet_barrier::BarrierState::Absent, None);
        assert!(v.safe_to_restart, "safe_to_restart must not move");
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["safe_with_resume"], false);
        assert!(json["resume"].is_null());
        assert!(json["safe_with_resume_reason"]
            .as_str()
            .unwrap()
            .contains("no runner-restart quiet barrier is open"));

        let mut v = idle_verdict();
        attach_resume(
            &mut v,
            &crate::quiet_barrier::BarrierState::Unknown("corrupt".into()),
            None,
        );
        assert!(!v.safe_with_resume);
        assert!(v.safe_with_resume_reason.starts_with("UNKNOWN"));
        assert!(v.resume.is_none());
    }

    /// Note 13: `resume_block_from` — the pure half of `gather_resume` — maps
    /// a `finished` session to `finished_count`, the barrier's requester to a
    /// `requester` straggler, an in-memory pending prompt to a
    /// `pending_autonomous_prompt` straggler, a session with no record read to
    /// `unknown`, and resumes the rest; a census pass that did not resolve is
    /// an `unknown` straggler, never a vacuous "all resumable".
    #[test]
    fn resume_block_from_maps_finished_requester_pending_and_a_missing_pass() {
        use crate::quiet_barrier::resume::{Descendants, SidebandRead};
        let now_ms = ACT_NOW;
        let crate::quiet_barrier::BarrierState::Open(mut barrier) = open_barrier() else {
            unreachable!()
        };
        barrier.requester_session_id = Some("s-req".to_string());

        let mut report = empty_report(now_ms);
        report.terminal_hosted = vec![
            wind_down_proc(1, Some("s-ok"), Some("working"), false, true),
            wind_down_proc(2, Some("s-fin"), Some("finished"), false, true),
            wind_down_proc(3, Some("s-req"), Some("working"), false, true),
            wind_down_proc(4, Some("s-pend"), Some("working"), false, true),
            wind_down_proc(5, Some("s-norec"), Some("working"), false, true),
        ];
        report.live_claude_total = 5;
        let pass = tracking_health::TrackingHealthPass {
            report,
            open_records: vec![],
        };
        let evidence = ActivityEvidence {
            pane_by_pid: HashMap::new(),
            records: [1u32, 2, 3, 4]
                .into_iter()
                .map(|pid| (pid, act_record("idle", None)))
                .collect(),
            now_ms,
        };
        let restorable: Result<HashMap<String, bool>, String> =
            Ok(["s-ok", "s-fin", "s-req", "s-pend", "s-norec"]
                .into_iter()
                .map(|s| (s.to_string(), true))
                .collect());
        let probe = |p: &LiveClaudeProcess| ResumeProbe {
            terminal_id: Some(format!("term-{}", p.pid)),
            sideband: SidebandRead::Reported("finished".to_string()),
            descendants: Descendants::McpOnly,
            pending_autonomous: if p.pid == 4 {
                vec!["auto_response:r (scheduled)".to_string()]
            } else {
                vec![]
            },
        };
        let ai = ai_plane_from(&[], &[], now_ms);
        let block = resume_block_from(
            &barrier,
            true,
            Some(&pass),
            &evidence,
            Some(&ai),
            &[],
            &restorable,
            &probe,
            &|_| Some(vec![]),
        );
        assert_eq!(block.resumable_count, 1, "{block:?}");
        assert_eq!(block.finished_count, 1);
        let class_of = |sid: &str| {
            block
                .stragglers
                .iter()
                .find(|s| s.session_id.as_deref() == Some(sid))
                .map(|s| s.class)
        };
        assert_eq!(class_of("s-req"), Some("requester"));
        assert_eq!(class_of("s-pend"), Some("pending_autonomous_prompt"));
        assert_eq!(class_of("s-norec"), Some("unknown"));
        assert_eq!(class_of("s-ok"), None);
        assert_eq!(class_of("s-fin"), None);
        assert_eq!(block.blocking_count, 3);
        assert_eq!(block.expected_restore_set.len(), 5);

        // An AI session holding a queued autonomous message is a
        // `pending_autonomous_prompt` straggler; one without is `ai_session`.
        let ai_two = ai_plane_from(
            &[
                AiSessionInput {
                    id: "tr-queued".to_string(),
                    state: "processing".to_string(),
                    has_worktree: false,
                    created_at_ms: None,
                },
                AiSessionInput {
                    id: "tr-plain".to_string(),
                    state: "ready".to_string(),
                    has_worktree: false,
                    created_at_ms: None,
                },
            ],
            &[],
            now_ms,
        );
        let with_ai = resume_block_from(
            &barrier,
            true,
            Some(&pass),
            &evidence,
            Some(&ai_two),
            &[],
            &restorable,
            &probe,
            &|id| match id {
                "tr-queued" => Some(vec![
                    "sdk_queue (1 autonomous SDK message(s) queued in memory)".to_string(),
                ]),
                "tr-plain" => Some(vec![]),
                _ => None,
            },
        );
        let classes: Vec<&str> = with_ai
            .stragglers
            .iter()
            .filter(|s| s.session_id.is_none() && s.pid.is_none())
            .map(|s| s.class)
            .collect();
        assert!(
            classes.contains(&"pending_autonomous_prompt"),
            "{classes:?}"
        );
        assert!(classes.contains(&"ai_session"), "{classes:?}");

        // A session the manager cannot resolve is `unknown`, never "no pending".
        let ai_lost = ai_plane_from(
            &[AiSessionInput {
                id: "tr-lost".to_string(),
                state: "ready".to_string(),
                has_worktree: false,
                created_at_ms: None,
            }],
            &[],
            now_ms,
        );
        let lost = resume_block_from(
            &barrier,
            true,
            Some(&pass),
            &evidence,
            Some(&ai_lost),
            &[],
            &restorable,
            &probe,
            &|_| None,
        );
        let unknown_ai = lost
            .stragglers
            .iter()
            .find(|s| s.reason.contains("tr-lost"))
            .expect("the unresolved AI session is reported");
        assert_eq!(unknown_ai.class, "unknown");

        // The census pass did not resolve: an explicit unknown straggler.
        let missing = resume_block_from(
            &barrier,
            true,
            None,
            &evidence,
            Some(&ai),
            &[],
            &restorable,
            &probe,
            &|_| Some(vec![]),
        );
        assert_eq!(missing.resumable_count, 0);
        assert_eq!(missing.blocking_count, 1);
        assert_eq!(missing.stragglers[0].class, "unknown");
        assert!(
            missing.stragglers[0].reason.contains("census"),
            "{missing:?}"
        );
    }

    /// Under an open barrier the `resume` block serializes with its exact wire
    /// keys (Phase 5's script reads them), and `safe_with_resume` follows
    /// `blocking_count == 0`.
    #[test]
    fn resume_block_serializes_its_wire_keys_under_an_open_barrier() {
        use crate::quiet_barrier::resume::{build_block, Straggler};
        let mut v = idle_verdict();
        let block = build_block(
            "rr-20260929T101500Z-a1b2c3",
            &[],
            vec![],
            vec!["sess-a".to_string()],
            true,
        );
        attach_resume(&mut v, &open_barrier(), Some(block));
        assert!(v.safe_with_resume);
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["safe_with_resume"], true);
        let resume = &json["resume"];
        let mut keys: Vec<&str> = resume
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "barrier_id",
                "blocking_count",
                "expected_restore_set",
                "finished_count",
                "resumable_count",
                "stragglers",
                "wake_paths_gated",
            ]
        );
        assert_eq!(resume["barrier_id"], "rr-20260929T101500Z-a1b2c3");
        assert_eq!(
            resume["expected_restore_set"],
            serde_json::json!(["sess-a"])
        );

        let mut v = idle_verdict();
        let block = build_block(
            "rr-20260929T101500Z-a1b2c3",
            &[],
            vec![Straggler::other(Some(42), "headless", "headless")],
            vec![],
            true,
        );
        attach_resume(&mut v, &open_barrier(), Some(block));
        assert!(!v.safe_with_resume);
        let json = serde_json::to_value(&v).unwrap();
        let straggler = &json["resume"]["stragglers"][0];
        let mut keys: Vec<&str> = straggler
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["class", "pid", "reason", "session_id", "terminal_id"]
        );
        assert_eq!(straggler["pid"], 42);
        assert_eq!(json["resume"]["blocking_count"], 1);
    }
}
