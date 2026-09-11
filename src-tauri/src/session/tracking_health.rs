//! Session-tracking health check + untracked-backend-spawn counter (plan
//! `2026-07-03-runner-session-tracking-drift-and-guardrails`, Phase 3 items
//! 1–2).
//!
//! ## Why
//!
//! Three prior "spawned but never durably recorded" fixes each patched one
//! call site; nothing detects the NEXT gap. The 2026-07-03 live audit found
//! 9 of 11 live Claude sessions untracked/mis-tracked — discovered only by a
//! manual multi-hour process-tree cross-reference. This module performs that
//! exact cross-reference automatically on a slow interval:
//!
//! - **live-but-untracked** — a live `claude` process in the runner's own
//!   process subtree that no open [`SessionLifecycleStore`] record accounts
//!   for (a restart would silently drop it), and
//! - **tracked-open-but-dead** — an open record whose terminal is gone or
//!   whose subtree no longer contains a live `claude` (a stale row the
//!   liveness poll should have flipped).
//!
//! ## Legitimate headless planes are EXEMPT
//!
//! Two claude planes legitimately run outside the terminal lifecycle store
//! and must not trip the WARN (false-positive alert fatigue is the exact
//! failure mode this plan fights):
//!
//! 1. **WS `LaunchPayload` agent spawns** (`agent_runtime::run_agent_subprocess`
//!    → `spawn_claude_child`, plus the headless gate-continuation arm) —
//!    direct tokio children with no PTY and no `capture_hint` BY DESIGN.
//!    Their child PIDs are registered here via
//!    [`register_headless_claude_pid`] for the child's lifetime.
//! 2. **Headless AI-session / task-run plane** (`ClaudeSession::spawn`,
//!    inline PIDs, pty workers) — restore rides the task-run DB plane via a
//!    pinned `--session-id <task_run_id>`, not the terminal store. Their PIDs
//!    come from `SessionManager::list_all_with_state()` at check time.
//!
//! Both exemptions are by tracked child PID (each PID's inclusive subtree is
//! subtracted), never by name/cmdline heuristics.
//!
//! **They are exempt from the WARN, not invisible.** [`evaluate`] keeps the two
//! planes APART and reports each by name, alongside the terminal-hosted plane
//! and the unclassified residue — see [`TrackingHealthReport`]. Folding them
//! into one `accounted` set is what let `/restart-readiness` describe nine
//! headless agent children as *"9 terminal-hosted agent sessions"* on a box
//! with zero terminal records (measured 2026-09-07). Exempting a population
//! from an alert must not also erase it from the census a destructive
//! operation is gated on.
//!
//! The decision core ([`evaluate`]) is a pure function over a
//! [`ProcessSnapshot`] + the open records, so it is unit-testable with a
//! synthetic snapshot and a tempdir store — no real processes needed. Process
//! enumeration REUSES `process_capture::process_tree` (one snapshot per tick,
//! same as the liveness poll); no new enumerator.
//!
//! The latest result is held in a module-global (same idiom as the
//! `auth.rs` data-plane counters) and surfaced on `/health` as the
//! `sessionTracking` object via [`health_json`].

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tracing::{debug, warn};

use crate::process_capture::process_tree::{
    claude_pids_in_inclusive_subtree, claude_present_in_inclusive_subtree, ProcessSnapshot,
};
use crate::session::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};
use crate::terminal::TerminalManager;

/// How often the periodic cross-reference runs.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(600);
/// Boot delay before the first check — lets boot-restore re-open its records
/// and the restored PTYs register, so the first tick doesn't report the
/// restore window itself as drift.
const INITIAL_DELAY: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// Untracked-backend-spawn counter (Phase 3 item 1)
// ---------------------------------------------------------------------------

/// `session_lifecycle_untracked_backend_spawn_total` — total backend-flavored
/// `create_terminal_session_backend` calls that arrived with
/// `capture_hint: None` (i.e. spawns that will NOT be durably recorded and
/// would be silently lost on restart). Same static-`AtomicU64` idiom as
/// `auth.rs::DATA_PLANE_TOTAL`; surfaced on `/health` via [`health_json`].
static UNTRACKED_BACKEND_SPAWN_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Increment the untracked-backend-spawn counter, returning the new total.
/// The caller owns the accompanying `warn!` (it has the spawn context).
pub fn note_untracked_backend_spawn() -> u64 {
    UNTRACKED_BACKEND_SPAWN_TOTAL.fetch_add(1, Ordering::Relaxed) + 1
}

/// Current value of `session_lifecycle_untracked_backend_spawn_total`.
pub fn untracked_backend_spawn_total() -> u64 {
    UNTRACKED_BACKEND_SPAWN_TOTAL.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Headless-plane exemption registry (agent-runtime children)
// ---------------------------------------------------------------------------

/// Live PIDs of headless `claude` children the agent runtime owns directly
/// (WS `LaunchPayload` spawns + the headless gate-continuation arm). These
/// legitimately have no lifecycle record — the spawn loop registers each
/// child for its lifetime so the health check never flags a normal agent run.
static HEADLESS_CLAUDE_PIDS: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();

fn headless_pids_cell() -> &'static Mutex<HashSet<u32>> {
    HEADLESS_CLAUDE_PIDS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Mark a headless agent-runtime `claude` child as exempt from the
/// live-but-untracked diff. Pair with [`unregister_headless_claude_pid`] when
/// the child exits (a leaked entry is self-limiting — the PID stops matching
/// a live claude once the process dies).
pub fn register_headless_claude_pid(pid: u32) {
    if pid == 0 {
        return;
    }
    if let Ok(mut g) = headless_pids_cell().lock() {
        g.insert(pid);
    }
}

/// Remove a headless child from the exemption set (its process exited).
pub fn unregister_headless_claude_pid(pid: u32) {
    if let Ok(mut g) = headless_pids_cell().lock() {
        g.remove(&pid);
    }
}

/// Snapshot of the currently-registered headless agent-runtime child PIDs.
pub fn headless_claude_pids() -> HashSet<u32> {
    headless_pids_cell()
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Health-check report
// ---------------------------------------------------------------------------

/// The coord WORK axis for one session — `coord.sessions.session_status`,
/// mirrored runner-side.
///
/// **Orthogonal to liveness by construction.** coord's own definition
/// (`qontinui-coord` `crates/coord/src/sessions.rs`) calls it *"a SECOND,
/// orthogonal axis ALONGSIDE"* [`crate::session::session_lifecycle_store`]'s
/// `state`, noting that a row can be `state=active` AND
/// `session_status=stalled`. The same holds here: a `Finished` session's
/// `claude` PROCESS IS STILL RUNNING — finishing is metadata and never
/// terminates anything (`/finish-session`: *"This never touches the
/// process"*).
///
/// [`Unrecognised`](Self::Unrecognised) exists so a status coord starts
/// serving tomorrow reaches an operator VERBATIM instead of being silently
/// dropped — and, crucially, still BLOCKS. A vocabulary that grows must never
/// be able to grow a new way to say "safe to restart".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionWorkStatus {
    Working,
    Blocked,
    Stalled,
    WaitingHuman,
    /// The agent declared its work complete. **The only value that discounts
    /// a live process from the restart verdict.**
    Finished,
    /// A value coord served that this build does not know. Carried onto the
    /// wire as-is; treated as blocking.
    Unrecognised(String),
}

impl SessionWorkStatus {
    /// Total — never fails. Mirrors coord's `SessionStatus::parse`, including
    /// its legacy `"done"` alias for `Finished` (parse-only there too);
    /// anything else becomes [`Self::Unrecognised`] rather than `None`, so the
    /// raw word survives to the response.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "working" => Self::Working,
            "blocked" => Self::Blocked,
            "stalled" => Self::Stalled,
            "waiting_human" => Self::WaitingHuman,
            "finished" => Self::Finished,
            // Legacy alias, accepted by coord's own parser.
            "done" => Self::Finished,
            _ => Self::Unrecognised(raw.trim().to_string()),
        }
    }

    /// The wire word, for the response body.
    pub fn as_wire(&self) -> String {
        match self {
            Self::Working => "working".to_string(),
            Self::Blocked => "blocked".to_string(),
            Self::Stalled => "stalled".to_string(),
            Self::WaitingHuman => "waiting_human".to_string(),
            Self::Finished => "finished".to_string(),
            Self::Unrecognised(raw) => raw.clone(),
        }
    }
}

/// **The whole safety argument, in one total function. FAIL CLOSED.**
///
/// The ONLY input that yields `false` is a status that resolved to
/// [`SessionWorkStatus::Finished`]. Absent (no coord row, coord unreachable,
/// the work axis unset), unrecognised, unattributed (nothing claims the
/// process), ambiguous (two records claim it) and every non-terminal status
/// all yield `true`.
///
/// Absence is never "finished". A permissive default here would let a restart
/// destroy live work while the endpoint said it was safe — which is the exact
/// failure `GET /restart-readiness` exists to prevent.
pub fn blocks_restart(status: Option<&SessionWorkStatus>) -> bool {
    !matches!(status, Some(SessionWorkStatus::Finished))
}

/// One live `claude` process in the runner's inclusive subtree, with every
/// fact about it that the SNAPSHOT already carries.
///
/// Used for all four classes [`evaluate`] partitions the live set into
/// (terminal-hosted, AI plane, headless-exempt, unclassified), so an operator
/// on a HEADLESS box — where there are no terminal panes to look at and
/// `/restart-readiness` is the only window onto the work — can see what is
/// actually running instead of a bare number.
///
/// Every field is derived from the one [`ProcessSnapshot`] the pass already
/// took, plus the injected `cwd_by_pid` map. **Nothing here samples anything
/// over an interval**: this type feeds a single-snapshot endpoint that gates a
/// destructive operation, and a sampler would make it slow without making it
/// more true.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LiveClaudeProcess {
    pub pid: u32,
    /// Parent pid from the snapshot's `parent_map`, when the process appears
    /// as somebody's child. `None` for the subtree root itself, or where the
    /// snapshot could not resolve a parent.
    pub parent_pid: Option<u32>,
    /// Image name from the snapshot (e.g. `claude.exe` on Windows, `comm` on
    /// Unix), when resolved.
    pub image: Option<String>,
    /// Seconds since the process started, from the snapshot's
    /// `creation_times` (epoch SECONDS — normalized here). `None` where the
    /// platform helper could not resolve a creation time (`0`, a `/proc` or
    /// WMI miss) or where the value is in the future (a clock artifact).
    /// NEVER a fabricated age.
    pub age_s: Option<i64>,
    /// Working directory, from `process_tree::working_directories_for_pids`.
    /// On this fleet that is the agent worktree, which is the single most
    /// useful identifier for a headless session. `None` on Windows (a
    /// process's cwd is not exposed by `Win32_Process`) and for any pid whose
    /// `/proc/<pid>/cwd` could not be read.
    pub cwd: Option<String>,
    /// **HINT, NOT A VERDICT.** True iff this pid has at least one child in
    /// the same snapshot. A `claude` mid-tool-call has children (a `cargo`, a
    /// `git`); a `claude` between turns has none and is NOT therefore idle,
    /// abandoned, or safe to kill. Read it as "there is visibly a child
    /// process attached right now", never as "this session is busy" — and
    /// never let it weaken the restart verdict, which counts every live
    /// process regardless.
    pub has_live_children: bool,
    /// True iff this process's parent is ITSELF a counted `claude` in the same
    /// live set — i.e. it is a nested subagent rather than a top-level agent
    /// session. Load-bearing for honest prose: `live_claude_total` is a count
    /// of PROCESSES, and on this fleet one agent session routinely fans out
    /// into several, so "N sessions" over the raw total is wrong by a factor.
    pub nested_under_claude: bool,
    /// The `claude_session_id` this process is attributed to — the key coord
    /// addresses a session by (`claude_code_session_id`). `None` for every
    /// non-terminal-hosted class (nothing attributes them), and `None` where
    /// two different open records claim the same pid: an AMBIGUOUS attribution
    /// is an unknown, and unknowns block. Never a guess.
    pub session_id: Option<String>,
    /// The coord WORK axis for [`Self::session_id`], verbatim as coord served
    /// it. `None` = not resolved — which is UNKNOWN, not "not finished".
    ///
    /// **Always `None` for a nested subagent** ([`Self::nested_under_claude`]),
    /// even when its ancestor's session reads `finished`: nobody declared THIS
    /// process complete, and an inherited declaration is not an observation
    /// about it.
    pub session_status: Option<String>,
    /// Does this process count as work in flight? `true` for every process
    /// except one whose [`Self::session_status`] resolved to exactly
    /// `finished`. See [`blocks_restart`] — this field IS that function's
    /// answer, recorded per process so an operator can see which sessions were
    /// discounted and why.
    ///
    /// ⚠ `false` does NOT mean the process is gone. It is still running, still
    /// holding memory, and a restart will still kill it. It means only that
    /// its session declared its work complete.
    pub blocks_restart: bool,
}

/// An open lifecycle record whose terminal is gone or whose subtree contains
/// no live `claude` — a stale row masquerading as a restorable session.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrackedDeadRecord {
    pub claude_session_id: String,
    pub terminal_id: String,
    pub title: Option<String>,
}

/// Result of one cross-reference pass.
///
/// ## The live `claude` set is PARTITIONED, not merely filtered
///
/// [`evaluate`] assigns every live `claude` pid in the runner's inclusive
/// subtree to exactly one of four disjoint classes:
///
/// | field | population |
/// |---|---|
/// | [`Self::terminal_hosted`] | claimed by a LIVE tracked terminal's subtree — the terminal-hosted plane |
/// | [`Self::ai_plane`] | claimed by a `SessionManager::list_all_with_state()` root — the AI / task-run plane |
/// | [`Self::headless_exempt`] | claimed by a [`register_headless_claude_pid`] root — agent-runtime headless children |
/// | [`Self::live_untracked`] | claimed by NOTHING — drift, and a restart would drop it silently |
///
/// so `terminal_hosted + ai_plane + headless_exempt + live_untracked ==
/// live_claude_total` ([`Self::partition_covers_total`]).
///
/// **Why the split exists.** These were previously merged into one
/// `accounted` set, leaving the report with only a total. `/restart-readiness`
/// then surfaced that total as `terminal_sessions.count` and its reason string
/// called it *"N terminal-hosted agent sessions"* — on a headless box, where
/// `tracked_open_total` is `0` and EVERY live claude is headless-exempt, that
/// is a description of a population that does not exist there, attached to a
/// count of one that does. Measured 2026-09-07: 9 live, 0 tracked records,
/// reason `"9 terminal-hosted agent sessions are live"`, all detail arrays
/// empty. The exemption machinery existed precisely to tell these apart; the
/// report just wasn't carrying the answer forward. Plan
/// `2026-09-07-restart-readiness-counts-headless-exempt-sessions-as-terminal-hosted`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackingHealthReport {
    /// Unix millis when this pass ran.
    pub checked_at_ms: i64,
    /// Total live `claude` PROCESSES found in the runner's inclusive subtree —
    /// the sum of the four classes below, and the number every prior consumer
    /// of this report read. Unchanged in meaning.
    pub live_claude_total: usize,
    /// Total open lifecycle records at check time.
    pub tracked_open_total: usize,
    /// Live claude claimed by a live tracked terminal.
    pub terminal_hosted: Vec<LiveClaudeProcess>,
    /// Live claude claimed by the AI / task-run plane.
    pub ai_plane: Vec<LiveClaudeProcess>,
    /// Live claude claimed by the agent-runtime headless registry.
    pub headless_exempt: Vec<LiveClaudeProcess>,
    /// Live claude nothing claims. **Definition unchanged** by the split:
    /// `live − (terminal ∪ AI ∪ agent-runtime)` is exactly the old
    /// `live − accounted`, because `exempt_root_pids` was precisely the union
    /// of the latter two.
    pub live_untracked: Vec<LiveClaudeProcess>,
    pub tracked_dead: Vec<TrackedDeadRecord>,
}

impl TrackingHealthReport {
    pub fn is_clean(&self) -> bool {
        self.live_untracked.is_empty() && self.tracked_dead.is_empty()
    }

    /// The four classes account for every live claude, with no pid counted
    /// twice and none dropped. Asserted by the unit tests; a false here would
    /// mean the readiness endpoint is under- or over-reporting live work.
    pub fn partition_covers_total(&self) -> bool {
        self.terminal_hosted.len()
            + self.ai_plane.len()
            + self.headless_exempt.len()
            + self.live_untracked.len()
            == self.live_claude_total
    }

    /// Count of entries in `list` that are NOT nested subagents — i.e. the
    /// top-level agent processes, which is the closest honest analogue of
    /// "how many sessions".
    pub fn root_count(list: &[LiveClaudeProcess]) -> usize {
        list.iter().filter(|p| !p.nested_under_claude).count()
    }

    /// Count of entries in `list` that count as WORK IN FLIGHT — everything
    /// except a process whose coord session row reads `finished`.
    pub fn blocking_count(list: &[LiveClaudeProcess]) -> usize {
        list.iter().filter(|p| p.blocks_restart).count()
    }

    /// Count of entries in `list` DISCOUNTED because their session declared
    /// its work finished. The complement of [`Self::blocking_count`], reported
    /// separately so no consumer ever sees one number that hides the other.
    pub fn finished_count(list: &[LiveClaudeProcess]) -> usize {
        list.iter().filter(|p| !p.blocks_restart).count()
    }
}

/// One completed cross-reference pass, plus the open records it was computed
/// against.
///
/// [`compute`] returns the records so an on-demand caller (the
/// `/restart-readiness` verdict, plan
/// `2026-08-29-no-single-answer-to-is-it-safe-to-restart-the-runner` Phase 1)
/// can render a per-session list without re-reading the store — the report
/// alone carries only counts plus the two drift lists.
#[derive(Debug, Clone)]
pub struct TrackingHealthPass {
    pub report: TrackingHealthReport,
    /// The open lifecycle records `report` was cross-referenced against.
    pub open_records: Vec<TerminalSessionRecord>,
}

/// Reference instant for the PID-reuse guard: the runner primary's OWN boot
/// time, captured once and shared by EVERY caller of [`evaluate`].
///
/// ⚠ Read the doc comment on `claude_present_in_inclusive_subtree` before
/// touching this. It is deliberately NOT `now()` and deliberately NOT a
/// per-record `opened_at`: a later record can legitimately reuse an
/// already-running terminal whose PID predates that record, and an
/// `opened_at`-keyed guard falsely flips such live idle sessions to
/// tracked-dead (measured live 2026-07-03: all 9 `trackedDead` were this
/// artifact). Substituting `now()` in an on-demand caller would reintroduce
/// exactly that defect for every session on the box.
///
/// Hoisted out of `run_periodic`'s local so the on-demand `/restart-readiness`
/// pass computes against the identical reference — two passes keyed on
/// different instants could disagree, which is the D1 "second census" failure
/// this plan forbids.
static PRIMARY_BOOT_UNIX_MILLIS: OnceLock<i64> = OnceLock::new();

/// Set the primary-boot reference instant (first writer wins), returning the
/// effective value. Called once from startup (`main.rs` setup) before the API
/// server can serve a readiness request, and again — harmlessly — by
/// [`run_periodic`] so the periodic task still works if startup never set it.
pub fn set_primary_boot_unix_millis(ms: i64) -> i64 {
    *PRIMARY_BOOT_UNIX_MILLIS.get_or_init(|| ms)
}

/// The primary-boot reference instant, or `None` if startup has not set it
/// yet. An on-demand caller MUST treat `None` as UNKNOWN (fail closed) rather
/// than substituting `now()` — see [`PRIMARY_BOOT_UNIX_MILLIS`].
pub fn primary_boot_unix_millis() -> Option<i64> {
    PRIMARY_BOOT_UNIX_MILLIS.get().copied()
}

/// Latest completed report — `/health` reads this; the periodic task is the
/// sole writer. `None` until the first pass completes.
static LATEST: OnceLock<Mutex<Option<TrackingHealthReport>>> = OnceLock::new();

fn latest_cell() -> &'static Mutex<Option<TrackingHealthReport>> {
    LATEST.get_or_init(|| Mutex::new(None))
}

/// Clone of the most recent report, if any pass has completed.
pub fn latest() -> Option<TrackingHealthReport> {
    latest_cell().lock().ok().and_then(|g| g.clone())
}

fn store_latest(report: TrackingHealthReport) {
    if let Ok(mut g) = latest_cell().lock() {
        *g = Some(report);
    }
}

/// The `/health` `sessionTracking` object: last run timestamp + drift counts
/// (fields null before the first pass), plus the untracked-backend-spawn
/// counter (always live — it counts spawns, not checks).
pub fn health_json() -> serde_json::Value {
    let counter = untracked_backend_spawn_total();
    match latest() {
        Some(r) => serde_json::json!({
            "lastCheckAt": r.checked_at_ms,
            "liveClaudeTotal": r.live_claude_total,
            "trackedOpenTotal": r.tracked_open_total,
            "liveUntracked": r.live_untracked.len(),
            "trackedDead": r.tracked_dead.len(),
            "liveUntrackedDetail": r.live_untracked,
            "trackedDeadDetail": r.tracked_dead,
            // The population split (plan
            // `2026-09-07-restart-readiness-counts-headless-exempt-sessions-as-terminal-hosted`).
            // `liveClaudeTotal` above is still the TOTAL; these say what it is
            // made of, so `/health` and `/restart-readiness` agree on shape.
            "terminalHostedTotal": r.terminal_hosted.len(),
            "aiPlaneTotal": r.ai_plane.len(),
            "headlessExemptTotal": r.headless_exempt.len(),
            "headlessExemptDetail": r.headless_exempt,
            "untrackedBackendSpawnsTotal": counter,
        }),
        None => serde_json::json!({
            "lastCheckAt": serde_json::Value::Null,
            "liveUntracked": serde_json::Value::Null,
            "trackedDead": serde_json::Value::Null,
            "untrackedBackendSpawnsTotal": counter,
        }),
    }
}

// ---------------------------------------------------------------------------
// Pure decision core
// ---------------------------------------------------------------------------

/// Cross-reference one process snapshot against the open lifecycle records,
/// PARTITIONING every live `claude` into the four classes
/// [`TrackingHealthReport`] documents.
///
/// Pure over its inputs (testable with a synthetic snapshot — no real
/// processes, no filesystem, no clock):
///
/// - `runner_pid` roots the "live claude" universe: every claude-image PID in
///   its inclusive subtree.
/// - `terminal_pids` maps each LIVE terminal id to its PTY child PID (from
///   `TerminalManager::list()`); a record whose terminal id is absent has no
///   live PTY.
/// - A record is **accounted** when its terminal's subtree has a live claude
///   (per `claude_present_in_inclusive_subtree`, including its PID-reuse
///   guard against `primary_boot_unix_millis` — the runner primary's OWN boot
///   time, NEVER a per-record `opened_at`: a later record can legitimately
///   reuse an already-running terminal, whose PID then predates that record's
///   `opened_at` by design, and an `opened_at`-keyed guard falsely flips such
///   live idle sessions to tracked-dead — see the guard fn's doc comment);
///   those claude PIDs become **terminal-hosted**. Every open record that is
///   not accounted is **tracked-open-but-dead**.
/// - `ai_plane_root_pids` and `agent_runtime_root_pids` are the two legitimate
///   headless planes the module docs name. They were previously ONE
///   `exempt_root_pids` set, and merging them is what left the readiness
///   endpoint unable to say which population was blocking a restart. Each
///   root's inclusive-subtree claude PIDs are claimed by its own class, so a
///   normal agent run still reports no drift AND is now visible by name.
/// - `cwd_by_pid` is injected (resolved by [`compute`] via
///   `process_tree::working_directories_for_pids`) so this stays a pure
///   function — it performs no I/O of its own. An absent pid yields
///   `cwd: None`, never a guess.
/// - `session_status_by_id` is injected the SAME way and for the SAME reason:
///   the coord WORK axis keyed by `claude_session_id`, resolved by the caller
///   (`GET /restart-readiness` → `crate::mcp::session_work_status::fetch`)
///   over a bounded network read this function must never perform. Pass an
///   EMPTY map wherever no status source was consulted — the periodic census
///   does exactly that — and every process then blocks, which is today's
///   behaviour bit-for-bit.
///
/// **The work axis is TERMINAL-HOSTED ONLY.** Only a terminal-hosted process
/// has an open lifecycle record, so only it has a `claude_session_id` coord can
/// be asked about. AI-plane, headless-exempt and unclassified processes get
/// `session_id: None` and therefore block regardless of what the map contains.
/// A pid claimed by two DIFFERENT records is ambiguous and also gets `None` —
/// fail-closed, per [`blocks_restart`].
///
/// **Precedence is fixed and total**: terminal → AI plane → agent-runtime
/// headless → unclassified. A pid claimed by more than one root lands in the
/// first matching class and is never double-counted, so the four vectors sum
/// to `live_claude_total`. Terminal wins because a durable lifecycle record is
/// the strongest claim on the box; the residue is deliberately the LAST class,
/// so anything the runner cannot explain is still reported as live work and
/// still blocks a restart.
#[allow(clippy::too_many_arguments)]
pub fn evaluate(
    snapshot: &ProcessSnapshot,
    runner_pid: u32,
    open_records: &[TerminalSessionRecord],
    terminal_pids: &HashMap<String, u32>,
    agent_runtime_root_pids: &HashSet<u32>,
    ai_plane_root_pids: &HashSet<u32>,
    cwd_by_pid: &HashMap<u32, String>,
    session_status_by_id: &HashMap<String, SessionWorkStatus>,
    primary_boot_unix_millis: i64,
    now_ms: i64,
) -> TrackingHealthReport {
    let live_claude: Vec<u32> = claude_pids_in_inclusive_subtree(runner_pid, snapshot);
    let live_set: HashSet<u32> = live_claude.iter().copied().collect();

    // Child -> parent, inverted once from the snapshot's parent -> children
    // index. Used only for `parent_pid` / `nested_under_claude` reporting.
    let mut parent_of: HashMap<u32, u32> = HashMap::new();
    for (&parent, kids) in &snapshot.parent_map {
        for &kid in kids {
            parent_of.entry(kid).or_insert(parent);
        }
    }

    let mut ai_claimed: HashSet<u32> = HashSet::new();
    for &root in ai_plane_root_pids {
        ai_claimed.extend(claude_pids_in_inclusive_subtree(root, snapshot));
    }
    let mut agent_runtime_claimed: HashSet<u32> = HashSet::new();
    for &root in agent_runtime_root_pids {
        agent_runtime_claimed.extend(claude_pids_in_inclusive_subtree(root, snapshot));
    }

    let mut terminal_claimed: HashSet<u32> = HashSet::new();
    // Which open record claims each terminal-hosted pid — the join key for the
    // coord work axis. A pid claimed by a SECOND, DIFFERENT record is recorded
    // as ambiguous and reports no session id at all: two records disagreeing
    // about who owns a process is an unknown, and unknowns block.
    let mut claim_by_pid: HashMap<u32, String> = HashMap::new();
    let mut ambiguous_pids: HashSet<u32> = HashSet::new();
    let mut tracked_dead: Vec<TrackedDeadRecord> = Vec::new();

    for rec in open_records {
        let alive = match terminal_pids.get(&rec.terminal_id) {
            Some(&pid) => {
                let present =
                    claude_present_in_inclusive_subtree(pid, snapshot, primary_boot_unix_millis);
                // ⚠ The CLAIM is registered whether or not the record is
                // `present`, while `terminal_claimed` (the classification set)
                // stays gated on `present` exactly as before.
                //
                // Why: `present` is `false` for a record whose terminal PID
                // fails the PID-reuse guard, yet that terminal's subtree can
                // still hold a live `claude`. If that pid ALSO sits inside a
                // present record's subtree, gating the claim on `present`
                // would attribute it solely to the present record — and if
                // THAT record reads `finished`, a process belonging to a
                // different session would be silently discounted. Registering
                // the claim unconditionally makes the pid AMBIGUOUS instead,
                // which blocks. Fail-closed beats tidy.
                for claimed in claude_pids_in_inclusive_subtree(pid, snapshot) {
                    if present {
                        terminal_claimed.insert(claimed);
                    }
                    match claim_by_pid.get(&claimed) {
                        Some(existing) if existing != &rec.claude_session_id => {
                            ambiguous_pids.insert(claimed);
                        }
                        Some(_) => {}
                        None => {
                            claim_by_pid.insert(claimed, rec.claude_session_id.clone());
                        }
                    }
                }
                present
            }
            // No live terminal hosts this record at all. Nothing to walk, so
            // no claim can be registered — and a live `claude` belonging to
            // such a record is, by construction, invisible to this join. See
            // `BOUNDARY`.
            None => false,
        };
        if !alive {
            tracked_dead.push(TrackedDeadRecord {
                claude_session_id: rec.claude_session_id.clone(),
                terminal_id: rec.terminal_id.clone(),
                title: rec.title.clone(),
            });
        }
    }

    // `session_id` is `None` for every class but terminal-hosted, and `None`
    // for an ambiguously-claimed pid — so `status` is `None` there too and
    // `blocks_restart` is `true`. Fail-closed falls out of the types.
    let describe = |pid: u32, terminal_hosted: bool| -> LiveClaudeProcess {
        let parent_pid = parent_of.get(&pid).copied();
        let nested_under_claude = parent_pid.map(|p| live_set.contains(&p)).unwrap_or(false);
        let session_id = if terminal_hosted && !ambiguous_pids.contains(&pid) {
            claim_by_pid.get(&pid).cloned()
        } else {
            None
        };
        // ⚠ **A NESTED process is never discounted by its ancestor's
        // declaration.** The claim walk is subtree-wide, so every `claude`
        // under a terminal inherits that terminal's record id — but a nested
        // subagent has no coord row of its own and nobody declared IT
        // finished. `/finish-session` explicitly does not touch the process,
        // so a parent marked finished while a nested `claude` is mid-write
        // must NOT flip the verdict to safe. The attribution is still
        // reported (`session_id` says which session it belongs to); only the
        // DISCOUNT is withheld, so an inherited status reads as UNKNOWN and
        // blocks. Fail-closed: absence of a declaration about THIS process is
        // never "finished".
        let status = if nested_under_claude {
            None
        } else {
            session_id
                .as_deref()
                .and_then(|id| session_status_by_id.get(id))
        };
        LiveClaudeProcess {
            pid,
            parent_pid,
            image: snapshot.names.get(&pid).cloned(),
            age_s: age_s_from_creation(snapshot.creation_times.get(&pid).copied(), now_ms),
            cwd: cwd_by_pid.get(&pid).cloned(),
            has_live_children: snapshot
                .parent_map
                .get(&pid)
                .map(|kids| !kids.is_empty())
                .unwrap_or(false),
            nested_under_claude,
            session_status: status.map(|s| s.as_wire()),
            session_id,
            blocks_restart: blocks_restart(status),
        }
    };

    let mut terminal_hosted: Vec<LiveClaudeProcess> = Vec::new();
    let mut ai_plane: Vec<LiveClaudeProcess> = Vec::new();
    let mut headless_exempt: Vec<LiveClaudeProcess> = Vec::new();
    let mut live_untracked: Vec<LiveClaudeProcess> = Vec::new();

    // Iterate `live_claude` (deterministic BFS order) so the emitted arrays are
    // stable across passes with an unchanged process table.
    for &pid in &live_claude {
        let is_terminal_hosted = terminal_claimed.contains(&pid);
        let entry = describe(pid, is_terminal_hosted);
        if is_terminal_hosted {
            terminal_hosted.push(entry);
        } else if ai_claimed.contains(&pid) {
            ai_plane.push(entry);
        } else if agent_runtime_claimed.contains(&pid) {
            headless_exempt.push(entry);
        } else {
            live_untracked.push(entry);
        }
    }

    TrackingHealthReport {
        checked_at_ms: now_ms,
        live_claude_total: live_claude.len(),
        tracked_open_total: open_records.len(),
        terminal_hosted,
        ai_plane,
        headless_exempt,
        live_untracked,
        tracked_dead,
    }
}

/// Seconds between a snapshot creation time (epoch SECONDS, `0`/absent when
/// the platform helper could not resolve one) and `now_ms` (epoch MILLIS).
///
/// `None` for an unknown creation time and for a negative result (a process
/// stamped in the future is a clock artifact, not an age). Never fabricates.
fn age_s_from_creation(created_secs: Option<i64>, now_ms: i64) -> Option<i64> {
    let created = created_secs?;
    if created <= 0 {
        return None;
    }
    let age = now_ms / 1000 - created;
    if age < 0 {
        None
    } else {
        Some(age)
    }
}

// ---------------------------------------------------------------------------
// Periodic task
// ---------------------------------------------------------------------------

/// One live cross-reference pass — snapshot the process table, resolve the
/// exempt planes, run [`evaluate`]. **Computes only**: it neither logs a
/// verdict nor publishes to the `latest()` cache.
///
/// This is the single shared body behind both consumers:
///
/// - [`run_periodic`] → [`run_once`], which logs + `store_latest`s the result;
/// - `GET /restart-readiness` (`mcp/restart_readiness.rs`), which calls this
///   directly so its verdict is FRESH rather than up to `CHECK_INTERVAL`
///   (600 s) stale.
///
/// Splitting rather than copying is load-bearing: two counting bodies could
/// disagree, and a readiness verdict that disagrees with `/health` is worse
/// than the buried-but-consistent number it replaces (plan
/// `2026-08-29-no-single-answer-…` D1).
///
/// Returns `None` when the process snapshot helper failed — an empty parent
/// map means "couldn't see the process table", **not** "no drift". Each caller
/// owns the posture for that: the periodic task skips the pass (fail-open,
/// keeping the previous result — same as the liveness poll's
/// `tick_snapshot_ok`), while the readiness endpoint MUST fail closed.
///
/// `session_status_by_id` is passed straight through to [`evaluate`] and is
/// the ONLY way the coord work axis enters a pass. **[`run_periodic`] passes
/// an empty map deliberately**: the background census runs every
/// [`CHECK_INTERVAL`] whether anyone is asking or not, and it must never do
/// network I/O. Only the on-demand `/restart-readiness` path resolves statuses,
/// and it resolves them before calling this.
pub async fn compute(
    terminal_manager: &Arc<TerminalManager>,
    store: &Arc<SessionLifecycleStore>,
    session_manager: &Arc<crate::claude_session::SessionManager>,
    primary_boot_unix_millis: i64,
    session_status_by_id: &HashMap<String, SessionWorkStatus>,
) -> Option<TrackingHealthPass> {
    let snap = crate::process_capture::process_tree::snapshot_process_table_public().await;
    if snap.parent_map.is_empty() {
        return None;
    }

    let open = store.open_records();
    let terminal_pids: HashMap<String, u32> = terminal_manager
        .list()
        .into_iter()
        .filter_map(|i| i.pid.filter(|&p| p > 0).map(|p| (i.id, p)))
        .collect();

    // The two exempt planes (module docs), kept SEPARATE rather than unioned:
    // agent-runtime headless children (registry) and the AI-session/task-run
    // plane (ClaudeSessions, inline PIDs, workers). Merging them is what left
    // `/restart-readiness` unable to name the population blocking a restart.
    let agent_runtime_roots: HashSet<u32> = headless_claude_pids();
    let ai_plane_roots: HashSet<u32> = session_manager
        .list_all_with_state()
        .into_iter()
        .map(|(_, _, pid)| pid)
        .filter(|&p| p > 0)
        .collect();

    let runner_pid = std::process::id();

    // Working directories for exactly the live claude pids — the agent
    // worktree, which on a headless box is the only identifier an operator
    // has. TARGETED (≤ tens of pids), fail-open, no subprocess.
    //
    // D1: this is NOT a second census. It is the SAME pure function
    // (`claude_pids_in_inclusive_subtree`) over the SAME `snap` that
    // `evaluate` is handed one line later, so the two walks cannot disagree —
    // they are the same computation, evaluated twice over one snapshot.
    let live_pids =
        crate::process_capture::process_tree::claude_pids_in_inclusive_subtree(runner_pid, &snap);
    let cwd_by_pid =
        crate::process_capture::process_tree::working_directories_for_pids(&live_pids).await;

    let report = evaluate(
        &snap,
        runner_pid,
        &open,
        &terminal_pids,
        &agent_runtime_roots,
        &ai_plane_roots,
        &cwd_by_pid,
        session_status_by_id,
        primary_boot_unix_millis,
        chrono::Utc::now().timestamp_millis(),
    );

    Some(TrackingHealthPass {
        report,
        open_records: open,
    })
}

/// One live pass for the PERIODIC task: [`compute`], then log + publish.
/// Skips (keeping the previous result) when the snapshot helper failed — an
/// empty parent map means "couldn't see the process table", not "no drift"
/// (same posture as the liveness poll's `tick_snapshot_ok`).
async fn run_once(
    terminal_manager: &Arc<TerminalManager>,
    store: &Arc<SessionLifecycleStore>,
    session_manager: &Arc<crate::claude_session::SessionManager>,
    primary_boot_unix_millis: i64,
) {
    let Some(pass) = compute(
        terminal_manager,
        store,
        session_manager,
        primary_boot_unix_millis,
        // The periodic census never consults coord: an EMPTY map means every
        // live process blocks, which is exactly what this task reported before
        // the work axis existed. `/health`'s numbers are unchanged, and this
        // 600 s task does no network I/O.
        &HashMap::new(),
    )
    .await
    else {
        debug!("session tracking health: process snapshot unavailable — skipping this pass");
        return;
    };
    let report = pass.report;

    if report.is_clean() {
        debug!(
            live_claude = report.live_claude_total,
            tracked_open = report.tracked_open_total,
            "session tracking health: clean"
        );
    } else {
        let untracked_pids: Vec<u32> = report.live_untracked.iter().map(|p| p.pid).collect();
        let dead_sessions: Vec<&str> = report
            .tracked_dead
            .iter()
            .map(|d| d.claude_session_id.as_str())
            .collect();
        warn!(
            live_untracked = report.live_untracked.len(),
            tracked_dead = report.tracked_dead.len(),
            live_claude_total = report.live_claude_total,
            tracked_open_total = report.tracked_open_total,
            untracked_pids = ?untracked_pids,
            dead_sessions = ?dead_sessions,
            "session tracking health: DRIFT — live claude sessions not durably tracked \
             (would be lost on restart) and/or stale open records"
        );
    }

    store_latest(report);
}

/// Detached periodic health check — spawned once at startup alongside the
/// liveness poll (main.rs setup). Runs forever; every pass is fail-open.
pub async fn run_periodic(
    terminal_manager: Arc<TerminalManager>,
    store: Arc<SessionLifecycleStore>,
    session_manager: Arc<crate::claude_session::SessionManager>,
) {
    // Reference instant for the PID-reuse guard: this primary's own boot time,
    // NOT a per-record `opened_at`. Same idiom as the liveness poll in main.rs;
    // see the doc comment on `claude_present_in_inclusive_subtree` for the
    // incident writeup (2026-07-03).
    //
    // Now shared via the process-global `OnceLock` so the on-demand
    // `/restart-readiness` pass keys on the IDENTICAL instant. Startup sets it
    // before the API server binds; this `get_or_init` is the fallback for a
    // build/path where it didn't, and is a no-op when it did.
    let primary_boot_unix_millis =
        set_primary_boot_unix_millis(chrono::Utc::now().timestamp_millis());
    tokio::time::sleep(INITIAL_DELAY).await;
    loop {
        run_once(
            &terminal_manager,
            &store,
            &session_manager,
            primary_boot_unix_millis,
        )
        .await;
        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn snap_with(
        parent_map: &[(u32, &[u32])],
        creation: &[(u32, i64)],
        names: &[(u32, &str)],
    ) -> ProcessSnapshot {
        let mut s = ProcessSnapshot::default();
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
            finish_reason: None,
            finish_synced: false,
        }
    }

    /// A minimal [`LiveClaudeProcess`] for tests that only care about the pid.
    fn proc_entry(pid: u32) -> LiveClaudeProcess {
        LiveClaudeProcess {
            pid,
            parent_pid: None,
            image: Some("claude".to_string()),
            age_s: None,
            cwd: None,
            has_live_children: false,
            nested_under_claude: false,
            session_id: None,
            session_status: None,
            blocks_restart: true,
        }
    }

    /// Reusable tempdir-based store harness: open records flow through a real
    /// `SessionLifecycleStore` so the test exercises the same read path
    /// (`open_records`) the live task uses.
    fn store_with(
        records: &[TerminalSessionRecord],
    ) -> (Arc<SessionLifecycleStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap(),
        );
        for rec in records {
            store.record_open(rec.clone());
        }
        (store, dir)
    }

    /// Fixture: runner (pid 1) → shell 5 → claude 10 (tracked, "t-5"), and a
    /// stray claude 20 directly under the runner with no record. Creation
    /// times sit at `now` so the PID-reuse guard is a no-op.
    #[test]
    fn detects_live_untracked_claude() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        let snap = snap_with(
            &[(1, &[5, 20]), (5, &[10])],
            &[(5, now_s), (10, now_s), (20, now_s)],
            &[
                (5, "powershell.exe"),
                (10, "claude.exe"),
                (20, "claude.exe"),
            ],
        );
        let (store, _dir) = store_with(&[record("sess-a", "t-5", now_ms)]);
        let terminal_pids: HashMap<String, u32> = [("t-5".to_string(), 5)].into_iter().collect();

        let report = evaluate(
            &snap,
            1,
            &store.open_records(),
            &terminal_pids,
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            now_ms,
            now_ms,
        );

        assert_eq!(report.live_claude_total, 2);
        assert_eq!(report.tracked_open_total, 1);
        assert!(report.tracked_dead.is_empty());
        let untracked: Vec<u32> = report.live_untracked.iter().map(|p| p.pid).collect();
        assert_eq!(untracked, vec![20]);
        assert_eq!(
            report.live_untracked[0].image,
            Some("claude.exe".to_string())
        );
        assert!(report.partition_covers_total());
        assert!(!report.is_clean());
    }

    /// An open record whose terminal id maps to no live PTY, and one whose
    /// PTY subtree has no claude, are both tracked-open-but-dead.
    #[test]
    fn detects_tracked_open_but_dead() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        // Runner (1) → bare shell 5 (no claude child anywhere).
        let snap = snap_with(&[(1, &[5])], &[(5, now_s)], &[(5, "powershell.exe")]);
        let (store, _dir) = store_with(&[
            record("sess-gone-terminal", "t-missing", now_ms),
            record("sess-no-claude", "t-5", now_ms),
        ]);
        let terminal_pids: HashMap<String, u32> = [("t-5".to_string(), 5)].into_iter().collect();

        let report = evaluate(
            &snap,
            1,
            &store.open_records(),
            &terminal_pids,
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            now_ms,
            now_ms,
        );

        assert!(report.live_untracked.is_empty());
        let mut dead: Vec<&str> = report
            .tracked_dead
            .iter()
            .map(|d| d.claude_session_id.as_str())
            .collect();
        dead.sort();
        assert_eq!(dead, vec!["sess-gone-terminal", "sess-no-claude"]);
        assert!(!report.is_clean());
    }

    /// Fully-consistent state: every live claude is accounted for by an open
    /// record, and every open record is alive → clean report. Covers both
    /// session shapes (claude-as-child under a shell, tracked PID *is*
    /// claude).
    #[test]
    fn clean_when_fully_tracked() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        let snap = snap_with(
            &[(1, &[5, 30]), (5, &[10])],
            &[(5, now_s), (10, now_s), (30, now_s)],
            &[(5, "pwsh.exe"), (10, "claude.exe"), (30, "claude.exe")],
        );
        let (store, _dir) = store_with(&[
            record("sess-shell", "t-5", now_ms),
            // Agent shape: the tracked PID itself is claude.
            record("sess-agent", "t-30", now_ms),
        ]);
        let terminal_pids: HashMap<String, u32> =
            [("t-5".to_string(), 5u32), ("t-30".to_string(), 30u32)]
                .into_iter()
                .collect();

        let report = evaluate(
            &snap,
            1,
            &store.open_records(),
            &terminal_pids,
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            now_ms,
            now_ms,
        );

        assert!(report.is_clean(), "unexpected drift: {report:?}");
        assert_eq!(report.live_claude_total, 2);
        assert_eq!(report.tracked_open_total, 2);
    }

    /// Headless-plane exemption: a claude with no lifecycle record whose PID
    /// (or subtree root) is exempt — an agent-runtime child or an
    /// AI-session/task-run child — must NOT be reported as live-untracked. A
    /// second genuinely-untracked claude in the same pass still is.
    #[test]
    fn exempt_headless_planes_do_not_report_drift() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        // Runner (1) → agent claude 40 (exempt, with a nested claude 41 in its
        // subtree), and a stray claude 50 that is NOT exempt.
        let snap = snap_with(
            &[(1, &[40, 50]), (40, &[41])],
            &[(40, now_s), (41, now_s), (50, now_s)],
            &[(40, "claude.exe"), (41, "claude.exe"), (50, "claude.exe")],
        );
        let (store, _dir) = store_with(&[]);
        let exempt: HashSet<u32> = [40].into_iter().collect();

        let report = evaluate(
            &snap,
            1,
            &store.open_records(),
            &HashMap::new(),
            &exempt,
            &HashSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            now_ms,
            now_ms,
        );

        // 40 and its subtree member 41 are exempt; only 50 remains.
        let untracked: Vec<u32> = report.live_untracked.iter().map(|p| p.pid).collect();
        assert_eq!(untracked, vec![50]);
        assert!(report.tracked_dead.is_empty());
    }

    /// P3 regression (defect 1): a session whose claude process PREDATES its
    /// record's `opened_at` — a later record opened against an already-running
    /// terminal (reconnect into an existing pane) — must NOT be classified
    /// tracked-dead. The PID-reuse reference is the runner primary's own boot
    /// time; keyed on `opened_at` (the old bug) the guard falsely read the
    /// older PID as recycled and flipped the live idle session to trackedDead
    /// (measured live: all 9 trackedDead were this artifact).
    #[test]
    fn claude_predating_opened_at_is_not_tracked_dead() {
        // Boot at 1_000s (1_000_000ms). Shell 5 + claude 10 created at boot
        // (1_000s). The record against t-5 was opened MUCH later (2_000_000ms).
        let primary_boot_ms = 1_000_000;
        let now_ms = 3_000_000;
        let snap = snap_with(
            &[(1, &[5]), (5, &[10])],
            &[(5, 1_000), (10, 1_000)],
            &[(5, "powershell.exe"), (10, "claude.exe")],
        );
        let (store, _dir) = store_with(&[record("sess-reconnect", "t-5", 2_000_000)]);
        let terminal_pids: HashMap<String, u32> = [("t-5".to_string(), 5)].into_iter().collect();

        let report = evaluate(
            &snap,
            1,
            &store.open_records(),
            &terminal_pids,
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            primary_boot_ms,
            now_ms,
        );

        assert!(
            report.tracked_dead.is_empty(),
            "live session falsely tracked-dead: {report:?}"
        );
        assert!(report.live_untracked.is_empty());
        assert!(report.is_clean());
    }

    /// P3 regression (defect 2): a qontinui path-identity shim — image name
    /// `claude.exe`, exe path inside a shim dir — must NOT count toward
    /// `live_claude_total` nor surface as live-untracked, while a real
    /// `claude.exe` at a non-shim path still does.
    #[test]
    fn shim_process_is_not_counted_as_live_claude() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        // Runner (1) → shim claude 20 (shim dir) and real stray claude 30.
        let mut snap = snap_with(
            &[(1, &[20, 30])],
            &[(20, now_s), (30, now_s)],
            &[(20, "claude.exe"), (30, "claude.exe")],
        );
        snap.exe_paths.insert(
            20,
            r"C:\Users\x\AppData\Local\Temp\qontinui-identity-t9\claude.exe".to_string(),
        );
        snap.exe_paths.insert(
            30,
            r"C:\Users\x\AppData\Local\Programs\claude\claude.exe".to_string(),
        );
        let (store, _dir) = store_with(&[]);

        let report = evaluate(
            &snap,
            1,
            &store.open_records(),
            &HashMap::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            now_ms,
            now_ms,
        );

        assert_eq!(report.live_claude_total, 1, "shim excluded from the count");
        let untracked: Vec<u32> = report.live_untracked.iter().map(|p| p.pid).collect();
        assert_eq!(untracked, vec![30], "only the REAL claude reports drift");
    }

    #[test]
    fn headless_pid_registry_round_trip() {
        register_headless_claude_pid(0); // ignored — 0 means "unknown"
        register_headless_claude_pid(9999);
        assert!(headless_claude_pids().contains(&9999));
        assert!(!headless_claude_pids().contains(&0));
        unregister_headless_claude_pid(9999);
        assert!(!headless_claude_pids().contains(&9999));
    }

    #[test]
    fn untracked_backend_spawn_counter_increments() {
        let before = untracked_backend_spawn_total();
        let after = note_untracked_backend_spawn();
        assert!(after > before);
        assert!(untracked_backend_spawn_total() >= after);
    }

    // ------------------------------------------------------------------
    // The population split (plan
    // `2026-09-07-restart-readiness-counts-headless-exempt-sessions-as-terminal-hosted`)
    // ------------------------------------------------------------------

    /// Ids of a class, in emitted order.
    fn pids(list: &[LiveClaudeProcess]) -> Vec<u32> {
        list.iter().map(|p| p.pid).collect()
    }

    /// **THE REGRESSION TEST — this is the shape that shipped broken.**
    ///
    /// A headless box: zero terminals, zero lifecycle records, zero AI
    /// sessions, and every live `claude` a direct agent-runtime child of the
    /// runner (two of them having spawned a nested subagent `claude`).
    /// Measured live 2026-09-07 as `terminal_sessions.count: 9`,
    /// `tracked_open_total: 0`, `sessions: []`, reason
    /// `"9 terminal-hosted agent sessions are live"`.
    ///
    /// Before the split the report carried only `live_claude_total: 6` and the
    /// readiness endpoint had no way to say these were not terminal-hosted.
    #[test]
    fn all_live_claude_headless_exempt_is_not_terminal_hosted() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        // Runner (1) → four headless claude children (10..13); 10 and 11 each
        // spawned a nested subagent claude (100, 110).
        let snap = snap_with(
            &[(1, &[10, 11, 12, 13]), (10, &[100]), (11, &[110])],
            &[
                (10, now_s - 4_000),
                (11, now_s - 4_000),
                (12, now_s - 4_000),
                (13, now_s - 4_000),
                (100, now_s - 100),
                (110, now_s - 100),
            ],
            &[
                (10, "claude"),
                (11, "claude"),
                (12, "claude"),
                (13, "claude"),
                (100, "claude"),
                (110, "claude"),
            ],
        );
        let agent_runtime: HashSet<u32> = [10u32, 11, 12, 13].into_iter().collect();

        let report = evaluate(
            &snap,
            1,
            &[], // no lifecycle records exist on a headless box
            &HashMap::new(),
            &agent_runtime,
            &HashSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            now_ms,
            now_ms,
        );

        assert_eq!(report.live_claude_total, 6);
        assert_eq!(report.tracked_open_total, 0);
        assert!(
            report.terminal_hosted.is_empty(),
            "NOTHING here is terminal-hosted: {:?}",
            report.terminal_hosted
        );
        assert!(report.ai_plane.is_empty());
        assert_eq!(report.headless_exempt.len(), 6);
        assert!(
            report.live_untracked.is_empty(),
            "the exemption still suppresses the drift WARN"
        );
        assert!(report.is_clean());
        assert!(report.partition_covers_total());

        // Processes are not sessions: 4 top-level agents, 6 processes.
        assert_eq!(TrackingHealthReport::root_count(&report.headless_exempt), 4);
        let nested: Vec<u32> = report
            .headless_exempt
            .iter()
            .filter(|p| p.nested_under_claude)
            .map(|p| p.pid)
            .collect();
        assert_eq!(nested, vec![100, 110]);

        // The activity HINT: 10 and 11 have a child attached; 12/13/100/110 do
        // not — which does not make them idle, only childless.
        let with_children: Vec<u32> = report
            .headless_exempt
            .iter()
            .filter(|p| p.has_live_children)
            .map(|p| p.pid)
            .collect();
        assert_eq!(with_children, vec![10, 11]);
    }

    /// Terminal-hosted, AI-plane, headless and unclassified live claude in one
    /// snapshot land in four disjoint classes that sum to the total.
    #[test]
    fn mixed_terminal_and_headless_are_classified_separately() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        // Runner (1) → shell 5 → claude 10 (tracked "t-5")
        //            → shell 6 → claude 11 (tracked "t-6")
        //            → headless claude 20, 21
        //            → AI-plane worker 30 → claude 31
        //            → stray claude 40 (nothing claims it)
        let snap = snap_with(
            &[
                (1, &[5, 6, 20, 21, 30, 40]),
                (5, &[10]),
                (6, &[11]),
                (30, &[31]),
            ],
            &[
                (5, now_s),
                (6, now_s),
                (10, now_s),
                (11, now_s),
                (20, now_s),
                (21, now_s),
                (30, now_s),
                (31, now_s),
                (40, now_s),
            ],
            &[
                (5, "bash"),
                (6, "bash"),
                (10, "claude"),
                (11, "claude"),
                (20, "claude"),
                (21, "claude"),
                (30, "node"),
                (31, "claude"),
                (40, "claude"),
            ],
        );
        let records = vec![
            record("sess-a", "t-5", now_ms - 60_000),
            record("sess-b", "t-6", now_ms - 60_000),
        ];
        let terminal_pids: HashMap<String, u32> =
            [("t-5".to_string(), 5u32), ("t-6".to_string(), 6u32)]
                .into_iter()
                .collect();
        let agent_runtime: HashSet<u32> = [20u32, 21].into_iter().collect();
        let ai_roots: HashSet<u32> = [30u32].into_iter().collect();

        let report = evaluate(
            &snap,
            1,
            &records,
            &terminal_pids,
            &agent_runtime,
            &ai_roots,
            &HashMap::new(),
            &HashMap::new(),
            now_ms,
            now_ms,
        );

        assert_eq!(report.live_claude_total, 6);
        assert_eq!(pids(&report.terminal_hosted), vec![10, 11]);
        assert_eq!(pids(&report.ai_plane), vec![31]);
        assert_eq!(pids(&report.headless_exempt), vec![20, 21]);
        assert_eq!(pids(&report.live_untracked), vec![40]);
        assert!(report.partition_covers_total());
        assert!(report.tracked_dead.is_empty());
        // None of these is nested under another claude.
        assert_eq!(TrackingHealthReport::root_count(&report.headless_exempt), 2);
    }

    /// No claude anywhere: every class empty, total zero, clean, and the
    /// partition still holds (0 == 0). The empty case must not special-case.
    #[test]
    fn empty_subtree_reports_every_class_empty() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        let snap = snap_with(
            &[(1, &[5]), (5, &[9])],
            &[(5, now_s), (9, now_s)],
            &[(5, "bash"), (9, "cargo")],
        );

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
        assert!(report.terminal_hosted.is_empty());
        assert!(report.ai_plane.is_empty());
        assert!(report.headless_exempt.is_empty());
        assert!(report.live_untracked.is_empty());
        assert!(report.tracked_dead.is_empty());
        assert!(report.is_clean());
        assert!(report.partition_covers_total());
    }

    /// Every detail field comes from the snapshot (or the injected cwd map),
    /// and an unresolvable one is `None` — never a fabricated value.
    #[test]
    fn detail_fields_come_from_the_snapshot_and_never_fabricate() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        // 10: known creation time, a cwd, and a live child.
        // 11: creation time UNKNOWN (0), no cwd entry, no children.
        let snap = snap_with(
            &[(1, &[10, 11]), (10, &[99])],
            &[(10, now_s - 273), (11, 0), (99, now_s)],
            &[(10, "claude"), (11, "claude"), (99, "cargo")],
        );
        let cwds: HashMap<u32, String> = [(
            10u32,
            "/home/x/qontinui-worktrees/01a07bad/qontinui-coord".to_string(),
        )]
        .into_iter()
        .collect();
        let agent_runtime: HashSet<u32> = [10u32, 11].into_iter().collect();

        let report = evaluate(
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
        );

        let a = &report.headless_exempt[0];
        assert_eq!(a.pid, 10);
        assert_eq!(a.parent_pid, Some(1));
        assert_eq!(a.image, Some("claude".to_string()));
        assert_eq!(a.age_s, Some(273));
        assert_eq!(
            a.cwd.as_deref(),
            Some("/home/x/qontinui-worktrees/01a07bad/qontinui-coord")
        );
        assert!(a.has_live_children, "pid 99 is attached to it");
        assert!(!a.nested_under_claude);

        let b = &report.headless_exempt[1];
        assert_eq!(b.pid, 11);
        assert_eq!(b.age_s, None, "an unknown creation time is null, not 0");
        assert_eq!(b.cwd, None, "an unresolvable cwd is null, not a guess");
        assert!(!b.has_live_children);
    }

    /// A clock-skewed process (created in the "future") reports `age_s: None`
    /// rather than a negative age.
    #[test]
    fn future_creation_time_is_unknown_not_negative() {
        assert_eq!(age_s_from_creation(Some(0), 1_000_000), None);
        assert_eq!(age_s_from_creation(None, 1_000_000), None);
        assert_eq!(age_s_from_creation(Some(1_100), 1_000_000), None);
        assert_eq!(age_s_from_creation(Some(900), 1_000_000), Some(100));
    }

    /// The split does not move `live_untracked`: the new classification's
    /// residue equals the OLD `live − (agent_runtime ∪ ai)` expression, since
    /// `exempt_root_pids` was precisely that union.
    #[test]
    fn live_untracked_definition_is_unchanged_by_the_split() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        let snap = snap_with(
            &[(1, &[5, 20, 30, 40]), (5, &[10]), (30, &[31])],
            &[
                (5, now_s),
                (10, now_s),
                (20, now_s),
                (30, now_s),
                (31, now_s),
                (40, now_s),
            ],
            &[
                (5, "bash"),
                (10, "claude"),
                (20, "claude"),
                (30, "node"),
                (31, "claude"),
                (40, "claude"),
            ],
        );
        let records = vec![record("sess-a", "t-5", now_ms - 60_000)];
        let terminal_pids: HashMap<String, u32> = [("t-5".to_string(), 5u32)].into_iter().collect();
        let agent_runtime: HashSet<u32> = [20u32].into_iter().collect();
        let ai_roots: HashSet<u32> = [30u32].into_iter().collect();

        let report = evaluate(
            &snap,
            1,
            &records,
            &terminal_pids,
            &agent_runtime,
            &ai_roots,
            &HashMap::new(),
            &HashMap::new(),
            now_ms,
            now_ms,
        );

        // Recompute the PRE-SPLIT expression by hand from the same snapshot.
        let mut exempt_union: HashSet<u32> = HashSet::new();
        for root in agent_runtime.iter().chain(ai_roots.iter()) {
            exempt_union.extend(claude_pids_in_inclusive_subtree(*root, &snap));
        }
        exempt_union.extend(claude_pids_in_inclusive_subtree(5, &snap));
        let expected_untracked: Vec<u32> = claude_pids_in_inclusive_subtree(1, &snap)
            .into_iter()
            .filter(|p| !exempt_union.contains(p))
            .collect();

        assert_eq!(pids(&report.live_untracked), expected_untracked);
        assert_eq!(expected_untracked, vec![40]);
    }

    /// `health_json` always carries the counter; report fields flip from null
    /// to concrete after a pass is stored.
    #[test]
    fn health_json_shape() {
        let v = health_json();
        assert!(v.get("untrackedBackendSpawnsTotal").is_some());

        store_latest(TrackingHealthReport {
            checked_at_ms: 123,
            live_claude_total: 1,
            tracked_open_total: 1,
            terminal_hosted: vec![],
            ai_plane: vec![],
            headless_exempt: vec![proc_entry(77)],
            live_untracked: vec![],
            tracked_dead: vec![],
        });
        let v = health_json();
        assert_eq!(v["lastCheckAt"], 123);
        assert_eq!(v["liveUntracked"], 0);
        assert_eq!(v["trackedDead"], 0);
        // The split is surfaced on /health too, so the two doors agree.
        assert_eq!(v["liveClaudeTotal"], 1);
        assert_eq!(v["terminalHostedTotal"], 0);
        assert_eq!(v["aiPlaneTotal"], 0);
        assert_eq!(v["headlessExemptTotal"], 1);
        assert_eq!(v["headlessExemptDetail"][0]["pid"], 77);
    }

    // ── The WORK axis (plan 2026-09-10) ─────────────────────────────────────

    /// [`SessionWorkStatus::parse`] is TOTAL and mirrors coord's own parser,
    /// including its legacy `"done"` alias. Nothing round-trips to `None`.
    #[test]
    fn work_status_parse_mirrors_coords_vocabulary() {
        use SessionWorkStatus::*;
        assert_eq!(SessionWorkStatus::parse("working"), Working);
        assert_eq!(SessionWorkStatus::parse("blocked"), Blocked);
        assert_eq!(SessionWorkStatus::parse("stalled"), Stalled);
        assert_eq!(SessionWorkStatus::parse("waiting_human"), WaitingHuman);
        assert_eq!(SessionWorkStatus::parse("finished"), Finished);
        // Legacy wire word — coord still PARSES it, so this mirror must too.
        assert_eq!(SessionWorkStatus::parse("done"), Finished);
        // Case and surrounding whitespace are normalized.
        assert_eq!(SessionWorkStatus::parse("  FINISHED "), Finished);
        // Anything else survives verbatim rather than vanishing.
        assert_eq!(
            SessionWorkStatus::parse("vacationing"),
            Unrecognised("vacationing".to_string())
        );
        assert_eq!(
            SessionWorkStatus::parse("vacationing").as_wire(),
            "vacationing"
        );
    }

    /// **The fail-closed core.** `finished` is the ONLY input that stops a
    /// process counting as work in flight.
    #[test]
    fn only_finished_stops_a_process_blocking() {
        use SessionWorkStatus::*;
        assert!(!blocks_restart(Some(&Finished)));
        for s in [
            Working,
            Blocked,
            Stalled,
            WaitingHuman,
            Unrecognised("something-new".to_string()),
        ] {
            assert!(blocks_restart(Some(&s)), "{s:?} must block");
        }
        // Absence is never "finished".
        assert!(blocks_restart(None));
    }

    /// A terminal-hosted process is joined to the record that claims it, and
    /// its status comes from the injected map — the same injection posture as
    /// `cwd_by_pid`.
    #[test]
    fn terminal_hosted_processes_carry_their_session_id_and_status() {
        let now_ms = 1_800_000_000_000;
        let snap = snap_with(
            &[(1, &[5]), (5, &[6]), (6, &[7])],
            &[],
            &[(6, "claude"), (7, "claude")],
        );
        let records = vec![record("sess-x", "t-5", now_ms - 60_000)];
        let terminal_pids: HashMap<String, u32> = [("t-5".to_string(), 5u32)].into_iter().collect();
        let statuses: HashMap<String, SessionWorkStatus> =
            [("sess-x".to_string(), SessionWorkStatus::Finished)]
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
            &statuses,
            now_ms - 3_600_000,
            now_ms,
        );
        assert_eq!(
            report.terminal_hosted.len(),
            2,
            "the nested subagent counts too"
        );
        // Both processes are ATTRIBUTED to the record that claims them...
        for p in &report.terminal_hosted {
            assert_eq!(p.session_id.as_deref(), Some("sess-x"));
        }
        // ...but only the ROOT is discounted. The nested subagent inherits the
        // attribution, NOT the declaration: nobody declared it finished, and
        // `/finish-session` does not touch the process, so a parent marked
        // finished while a nested `claude` is mid-write must not read as safe.
        let root = report
            .terminal_hosted
            .iter()
            .find(|p| !p.nested_under_claude)
            .expect("root process");
        assert_eq!(root.session_status.as_deref(), Some("finished"));
        assert!(!root.blocks_restart);

        let nested = report
            .terminal_hosted
            .iter()
            .find(|p| p.nested_under_claude)
            .expect("nested process");
        assert_eq!(
            nested.session_status, None,
            "an INHERITED status is not an observation about this process"
        );
        assert!(
            nested.blocks_restart,
            "a nested subagent is never discounted by its ancestor's declaration"
        );

        assert_eq!(
            TrackingHealthReport::blocking_count(&report.terminal_hosted),
            1
        );
        assert_eq!(
            TrackingHealthReport::finished_count(&report.terminal_hosted),
            1
        );
        assert!(report.partition_covers_total());
    }

    /// **F2 regression.** A record whose terminal fails the PID-reuse guard
    /// (`present == false`, so it is `tracked_dead`) still REGISTERS its claim,
    /// so a live `claude` that also sits inside a present record's subtree
    /// becomes AMBIGUOUS and blocks — instead of being silently discounted on
    /// the present record's `finished`.
    #[test]
    fn a_dead_records_claim_still_makes_a_shared_pid_ambiguous() {
        let now_ms = 1_800_000_000_000;
        // 1 -> 5 (terminal A) -> 20 (terminal B) -> 21 (claude).
        // Terminal B's pid predates the primary-boot reference, so the
        // PID-reuse guard reports B as not present.
        let snap = snap_with(
            &[(1, &[5]), (5, &[20]), (20, &[21])],
            &[(20, 1_000), (21, now_ms / 1000 - 600)],
            &[(21, "claude")],
        );
        let records = vec![
            record("sess-a", "t-a", now_ms - 60_000),
            record("sess-b", "t-b", now_ms - 60_000),
        ];
        let terminal_pids: HashMap<String, u32> =
            [("t-a".to_string(), 5u32), ("t-b".to_string(), 20u32)]
                .into_iter()
                .collect();
        let statuses: HashMap<String, SessionWorkStatus> =
            [("sess-a".to_string(), SessionWorkStatus::Finished)]
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
            &statuses,
            // Primary boot AFTER terminal B's creation time, so B fails the
            // PID-reuse guard while A (no creation time recorded) does not.
            2_000_000,
            now_ms,
        );
        assert_eq!(report.terminal_hosted.len(), 1);
        let p = &report.terminal_hosted[0];
        assert_eq!(
            p.session_id, None,
            "two records claim this pid — the attribution is ambiguous"
        );
        assert_eq!(p.session_status, None);
        assert!(
            p.blocks_restart,
            "an ambiguous claim must never be discounted on one claimant's `finished`"
        );
    }

    /// With no status source at all, every live process blocks — the
    /// pre-2026-09-10 behaviour, which is what a coord outage must reproduce.
    #[test]
    fn an_empty_status_map_leaves_every_process_blocking() {
        let now_ms = 1_800_000_000_000;
        let snap = snap_with(&[(1, &[5]), (5, &[6])], &[], &[(6, "claude")]);
        let records = vec![record("sess-x", "t-5", now_ms - 60_000)];
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
            now_ms - 3_600_000,
            now_ms,
        );
        assert_eq!(report.terminal_hosted.len(), 1);
        let p = &report.terminal_hosted[0];
        assert_eq!(
            p.session_id.as_deref(),
            Some("sess-x"),
            "the JOIN still happens"
        );
        assert_eq!(p.session_status, None, "but no status was resolved");
        assert!(p.blocks_restart);
        assert_eq!(
            TrackingHealthReport::blocking_count(&report.terminal_hosted),
            1
        );
    }
}
