//! HTTP control surface for the runner's **steward** sessions.
//!
//! Modeled on `mcp/terminals.rs`'s `create_terminal_handler` /
//! `list_terminals_handler` (the closest template — NOT `mcp/worktrees.rs`,
//! which is cited only for its `ApiResponse` envelope shape): a titled PTY
//! terminal session is a steward's entire lifecycle. Opening the tab starts
//! it; closing the tab (killing the PTY child) stops it. There is at most one
//! session **per steward kind**, tracked by terminal **id** in
//! [`steward_meta_store`] (see [`find_running_steward`] for why title cannot
//! serve as the marker — discovered in manual testing, corrects the plan's
//! original §6 Q2 resolution).
//!
//! `POST /steward/{kind}/start` spawns the PTY **server-side** (mirroring
//! `create_terminal_handler`, `mcp/terminals.rs:137`) so the identical code
//! path serves both a UI button (which just calls this endpoint instead of
//! hand-rolling open-tab + type-command) and an agent driving it via `curl`
//! (which has no frontend to type into — server-side spawn is necessary for
//! parity, not just simpler; see plan §6 Q3).
//!
//! # Why this module is a roster and not three constants
//!
//! It launched exactly one steward (`merge-train-steward`) from three
//! module-level constants until 2026-08-28. Every other steward skill the
//! fleet has — `dev-ops-steward`, `cleanup-steward` — therefore had **no
//! supported launch path at all**, and in particular no path that sets the
//! enablement env var its own kill-switch reads. A `/dev-ops-steward` session
//! started by hand ran for 50 iterations with `COORD_DEVOPS_STEWARD_ENABLED`
//! unset: nothing on the machine wrote it, so the documented way to stop that
//! steward ("unset the flag") was inert, because the flag was never set in the
//! first place. Adding a steward is now one [`STEWARDS`] row.

use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::Manager;
use tracing::{error, info, warn};

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::terminal::types::TerminalInfo;
use crate::terminal::TerminalManager;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

// ============================================================================
// The steward roster
// ============================================================================

/// One steward the runner knows how to launch.
///
/// Every field here was a module-level constant before the roster existed, and
/// **none of them can be derived from the others** — which is why this is a
/// table rather than a naming convention:
///
/// * `enable_env` alternates prefix between skills (`COORD_MERGE_STEWARD_ENABLED`,
///   `COORD_DEVOPS_STEWARD_ENABLED`, but `QONTINUI_CLEANUP_STEWARD_ENABLED`).
/// * `default_mode` is drawn from a **different vocabulary** per skill —
///   `observe`/`autonomous` for two of them, `report`/`reap` for cleanup — so
///   this module deliberately does not validate mode against an enum.
/// * `extra_args` exists because one skill arms its own `/loop` and the others
///   do not; see the field's own doc comment.
pub struct StewardSpec {
    /// Stable identifier and URL path segment (`merge-train`, `dev-ops`, …).
    pub kind: &'static str,
    /// Human-readable label; the UI renders this verbatim.
    pub label: &'static str,
    /// The slash-command this launches, **without** the leading slash. Also
    /// used as the terminal's initial cosmetic title.
    pub skill: &'static str,
    /// The environment variable the skill's own enablement gate reads. The
    /// launch command sets it to `1`; a steward started any other way will
    /// find it unset and refuse to do anything, which is the whole point.
    pub enable_env: &'static str,
    /// Default `--mode=`, matching the skill document's own stated default.
    pub default_mode: &'static str,
    /// Default `/loop` interval, matching the skill document's own stated
    /// default rather than a separately-guessed value.
    pub default_interval: &'static str,
    /// Extra arguments appended after `--mode=`.
    ///
    /// `dev-ops-steward` **arms its own `/loop`** unless told not to, so
    /// wrapping it in `/loop` without `--no-loop` would leave two loops
    /// driving one session — the accumulation hazard that skill's own Step 0
    /// warns about. `merge-train-steward` and `cleanup-steward` do not
    /// self-arm and do not recognise the flag, so it must not be passed to
    /// them: this is per-steward data, not a global.
    pub extra_args: &'static [&'static str],
}

/// Every steward the runner can launch. Adding one is a row here; nothing
/// else in this module is per-steward.
pub const STEWARDS: &[StewardSpec] = &[
    StewardSpec {
        kind: "merge-train",
        label: "Merge-train steward",
        skill: "merge-train-steward",
        enable_env: "COORD_MERGE_STEWARD_ENABLED",
        // CORRECTED 2026-08-28. This was `observe`, with a comment claiming it
        // matched "`merge-train-steward.md`'s own stated default". It has not
        // matched since 2026-07-22, when that skill moved its default to
        // `autonomous` after completing its observe soak; the launcher was
        // never updated and the comment silently became false. Tracking the
        // skill is the point of this table — a launcher that disagrees with
        // the skill it launches is the defect this module was rewritten to
        // end. An observe re-soak — which that skill asks for after a major
        // change — is still reachable, but only over the API
        // (`POST /steward/merge-train/start {"mode":"observe"}`): the UI
        // button sends an empty body and so can only ever launch this default.
        default_mode: "autonomous",
        default_interval: "5m",
        extra_args: &[],
    },
    StewardSpec {
        kind: "dev-ops",
        label: "Dev-ops steward",
        skill: "dev-ops-steward",
        enable_env: "COORD_DEVOPS_STEWARD_ENABLED",
        default_mode: "autonomous",
        default_interval: "10m",
        extra_args: &["--no-loop"],
    },
    StewardSpec {
        kind: "cleanup",
        label: "Cleanup steward",
        skill: "cleanup-steward",
        enable_env: "QONTINUI_CLEANUP_STEWARD_ENABLED",
        // `report` (detect + print only) is this skill's own documented
        // default; `reap` is the mutating mode and is opt-in.
        default_mode: "report",
        default_interval: "15m",
        extra_args: &[],
    },
];

/// Look up a steward by its `kind` path segment.
pub fn steward_spec(kind: &str) -> Option<&'static StewardSpec> {
    STEWARDS.iter().find(|s| s.kind == kind)
}

/// The 404 body for an unrecognised kind — names what IS valid, so a caller
/// that guessed the segment is told the roster rather than just refused.
fn unknown_kind_error(kind: &str) -> (StatusCode, Json<ApiResponse<()>>) {
    let valid: Vec<&str> = STEWARDS.iter().map(|s| s.kind).collect();
    (
        StatusCode::NOT_FOUND,
        Json(api_error(format!(
            "unknown steward kind '{}' (valid: {})",
            kind,
            valid.join(", ")
        ))),
    )
}

// ============================================================================
// In-process steward metadata (kind/mode/interval used at launch)
// ============================================================================

/// What a steward terminal was started as. Keyed by terminal id so
/// `GET /steward/{kind}/status` can echo back what `start` was called with.
/// Runner-local and in-memory only: a runner restart kills the PTY (and thus
/// the steward) anyway, so there's nothing durable to lose.
struct StewardMeta {
    kind: String,
    mode: String,
    interval: String,
    /// Has a `claude` EVER been observed in this pane?
    ///
    /// The latch that separates "the steward has not arrived yet" from "the
    /// steward left" — two states a single process-table look cannot tell
    /// apart, because both read as no `claude` in a perfectly readable table.
    ///
    /// `start_steward` inserts this row and returns 200 the moment
    /// `TerminalManager::create` succeeds, but the launch command is typed by
    /// a DETACHED task after a 300 ms sleep, and `claude` then has to start —
    /// realistically 1-3 s, longer on a loaded box. The pane has a real root
    /// pid from PTY spawn, so throughout that window the probe correctly
    /// answers `Some(false)`. Without this latch the roster reported
    /// `running: false` for a steward that had just started, the
    /// single-instance guard (which is `steward_status().running` once
    /// `StartClaim` drops) stood open for the whole window, and the UI — which
    /// polls `/stewards` every 2 s and re-enables Start whenever `running` is
    /// false — invited a second click. Two `merge-train` stewards driving the
    /// merge train at once is the exact outcome the fail direction exists to
    /// prevent, and the first version of this predicate shipped it
    /// deterministically at every start.
    ///
    /// Latched (never cleared) the first time a probe sees a `claude`. A
    /// restart of the runner drops the whole store with the PTYs, so there is
    /// nothing stale to inherit.
    claude_seen: bool,
}

/// Global registry of steward metadata, keyed by terminal id. Holds at most
/// one live entry per `kind` — enforced by the single-instance guard in
/// [`steward_start_handler`] — but is a flat map rather than a map-of-kind so
/// that stale-entry pruning stays a single `retain` over live terminal ids.
fn steward_meta_store() -> &'static Mutex<HashMap<String, StewardMeta>> {
    static STORE: OnceLock<Mutex<HashMap<String, StewardMeta>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Terminal ids of every steward session this runner started and still
/// tracks — the steward half of wind-down's session-kind resolution
/// (`mcp/restart_readiness.rs`). May name a terminal that has since exited;
/// callers join it against live terminals. A poisoned store yields an empty
/// set, which classifies every session as an ordinary terminal session — the
/// strictest kind, since it alone must be declared `finished`.
pub(crate) fn steward_terminal_ids() -> std::collections::HashSet<String> {
    steward_meta_store()
        .lock()
        .map(|guard| guard.keys().cloned().collect())
        .unwrap_or_default()
}

/// The steward KIND a terminal hosts, when this runner started one there.
///
/// The wind-down executor needs this to record a `stopped_by_drain` kind: the
/// census knows a pane is a steward (its terminal id is in
/// [`steward_terminal_ids`]) but not WHICH one, and the undrain restart is
/// per-kind. A poisoned store answers `None` — the close still happens, the
/// restart is then owed to an operator, which is the honest degradation.
pub(crate) fn steward_kind_for_terminal(terminal_id: &str) -> Option<String> {
    steward_meta_store()
        .lock()
        .ok()
        .and_then(|guard| guard.get(terminal_id).map(|meta| meta.kind.clone()))
}

/// Kinds whose `start` is currently in flight.
///
/// The single-instance guard cannot be enforced by the metadata store alone:
/// that store is keyed by *terminal id*, and there is no id to record until
/// `TerminalManager::create` has already spawned the PTY. Two concurrent
/// `POST /steward/{kind}/start` calls — the UI button and an agent driving
/// the same endpoint by `curl`, say — would therefore both read
/// `running: false`, both spawn, and both insert. The store would then hold
/// two live rows for one kind, and `stop` would kill whichever `list()`
/// yielded first — leaving the survivor invisible to `status` until a prune
/// exposes it, so it takes a SECOND `stop` to end. Recoverable, not
/// unrecoverable: an earlier version of this comment said "unstoppable from
/// any surface", which overstates it. What is not recoverable is the interval
/// — for a merge-train steward it is spent with two of them driving the merge
/// train. This set closes the window by claiming the kind *before* the check.
fn starting_kinds() -> &'static Mutex<std::collections::HashSet<String>> {
    static STARTING: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    STARTING.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// An RAII claim on one kind's start slot, released on drop.
///
/// Drop rather than explicit removal because `steward_start_handler` has
/// several early-return paths (the 409, the spawn failure) and a missed
/// removal on any one of them would wedge that kind's start forever — a
/// worse failure than the race it guards.
struct StartClaim(String);

impl StartClaim {
    /// Claim the slot, or `None` if a start of this kind is already in
    /// flight. A poisoned lock also yields `None`: refusing a concurrent
    /// start is the safe direction.
    fn acquire(kind: &str) -> Option<Self> {
        let mut guard = starting_kinds().lock().ok()?;
        if !guard.insert(kind.to_string()) {
            return None;
        }
        Some(StartClaim(kind.to_string()))
    }
}

impl Drop for StartClaim {
    fn drop(&mut self) {
        if let Ok(mut guard) = starting_kinds().lock() {
            guard.remove(&self.0);
        }
    }
}

fn prune_stale_meta(terminal_manager: &TerminalManager) {
    let alive_ids: std::collections::HashSet<String> = terminal_manager
        .list()
        .into_iter()
        .filter(|info| info.is_alive)
        .map(|info| info.id)
        .collect();
    if let Ok(mut guard) = steward_meta_store().lock() {
        guard.retain(|id, _| alive_ids.contains(id));
    }
}

/// The terminal ids recorded for one steward kind.
///
/// Split out of [`find_running_steward`] so the kind-filtering — the part
/// that decides whether one steward's session can be mistaken for another's
/// — is testable without a Tauri `AppHandle` or a live PTY.
fn tracked_ids_for_kind(
    store: &HashMap<String, StewardMeta>,
    kind: &str,
) -> std::collections::HashSet<String> {
    store
        .iter()
        .filter(|(_, meta)| meta.kind == kind)
        .map(|(id, _)| id.clone())
        .collect()
}

/// PURE: is this pane still running the steward?
///
/// ## Why the shell's liveness is not the question
///
/// A steward IS a `claude`. The pane's shell outliving it is not the steward
/// running — it is a bare prompt. This mattered from the moment wind-down
/// started closing stewards: `graceful_exit` reaches its close callback only
/// after `claude` is proven gone, so a `CloseRefused` (ordinary — a single
/// `ClaudeProbe::Unreadable` on a contended box produces one) leaves a pane
/// that is REGISTERED and whose SHELL is alive, with no `claude` in it. Keyed
/// on `info.is_alive` alone, that pane answered `running: true` forever: the
/// undrain restart hit the single-instance guard, took a 409 (whose body
/// carries no `DeferClass` code, so it read as the benign "already running"
/// family), dropped the kind from the stopped-by-drain set, and logged
/// "no restart needed — already running". For a merge-train steward that is
/// PRs silently ceasing to land.
///
/// `hosts_claude` is the pane's own process answer: `Some(true)` a `claude`
/// lives there, `Some(false)` none right now, `None` the process table could
/// not be read. `claude_seen` is [`StewardMeta::claude_seen`] — whether one was
/// EVER seen here.
///
/// **`Some(false)` is ambiguous on its own**, and that is what `claude_seen`
/// resolves: before the latch it means "not started yet", after it "left". A
/// steward is running until a `claude` has been seen and then is not.
///
/// **Fail direction: a pane counts as running unless we can prove it was
/// occupied and is now empty.** `None` keeps the old answer, and so does a
/// `Some(false)` on a pane that has never been occupied.
///
/// The asymmetry is deliberate. A false "running" costs a refused start an
/// operator can see and retry. A false "not running" costs two stewards of one
/// kind, and — measured against the code rather than asserted — that is
/// recoverable but not free: `stop` kills whichever `list()` yields first,
/// `close` drops it from the manager map, `prune_stale_meta` drops its row, and
/// the NEXT `status` then finds the survivor, so a second `stop` ends it. Two
/// stop calls, not zero. Temporarily invisible, not permanently ungovernable —
/// an earlier version of this comment claimed the latter, and the overstatement
/// is worth correcting because it is exactly the cost model that should have
/// caught the start-window hole above.
pub(crate) fn steward_pane_is_running(
    is_alive: bool,
    hosts_claude: Option<bool>,
    claude_seen: bool,
) -> bool {
    if !is_alive {
        return false;
    }
    match hosts_claude {
        Some(true) => true,
        // The only arm that says "not running", and only once we know the pane
        // was occupied at some point.
        Some(false) => !claude_seen,
        None => true,
    }
}

/// PURE: is this pane a tracked steward that has been EMPTIED — occupied once,
/// provably empty now, and so neither the running steward nor something that
/// will become one?
///
/// These are what a refused graceful close leaves behind (`CloseRefused` /
/// `CloseOutcomeUnknown` are reached only after `claude` is gone, and neither
/// closes the tab). Before the liveness fix above they were mistaken for the
/// running steward, which broke the undrain restart; now that they are not, the
/// undrain starts a SECOND pane and the emptied one would be retained forever —
/// live shell so `prune_stale_meta` never evicts it, no top-level `claude` so
/// the wind-down census never sees it again, and nothing else closes it. One
/// leaked pane per drain/undrain cycle. See [`reap_emptied_panes`].
pub(crate) fn steward_pane_is_emptied(
    is_alive: bool,
    hosts_claude: Option<bool>,
    claude_seen: bool,
) -> bool {
    is_alive && hosts_claude == Some(false) && claude_seen
}

/// Does a `claude` live in the pane rooted at `root_pid`, as one already-taken
/// process snapshot sees it? `None` when the question cannot be answered — a
/// remote pane with no local pid, or an unreadable table.
pub(crate) fn pane_hosts_claude(
    snapshot: &crate::process_capture::process_tree::ProcessSnapshot,
    root_pid: Option<u32>,
) -> Option<bool> {
    let root = root_pid?;
    match crate::terminal::graceful_exit::probe_from_snapshot(root, snapshot, &[]) {
        crate::terminal::graceful_exit::ClaudeProbe::Readable(view) => {
            Some(!view.subtree_claude.is_empty())
        }
        crate::terminal::graceful_exit::ClaudeProbe::Unreadable(_) => None,
    }
}

/// One tracked pane of a kind, with everything the two predicates above need.
struct TrackedPane {
    info: TerminalInfo,
    hosts_claude: Option<bool>,
    claude_seen: bool,
}

/// The tracked panes of one kind, judged against an already-taken snapshot —
/// **and the `claude_seen` latch advanced where this look supplies the
/// evidence.**
///
/// The latch is set here rather than at start because start has nothing to
/// latch on: the pane is empty then, by construction. Every path that asks
/// whether a steward is running goes through this function, and the roster is
/// polled every 2 s, so the first probe after `claude` appears carries it.
///
/// A poisoned store yields no latch, so every pane reads as `claude_seen:
/// false` — running, and never reaped. That is the safe direction for both
/// predicates.
fn tracked_panes(
    terminal_manager: &TerminalManager,
    kind: &str,
    snapshot: &crate::process_capture::process_tree::ProcessSnapshot,
) -> Vec<TrackedPane> {
    let tracked_ids = steward_meta_store()
        .lock()
        .map(|guard| tracked_ids_for_kind(&guard, kind))
        .unwrap_or_default();
    if tracked_ids.is_empty() {
        return Vec::new();
    }
    let observed: Vec<(TerminalInfo, Option<bool>)> = terminal_manager
        .list()
        .into_iter()
        .filter(|info| tracked_ids.contains(&info.id))
        .map(|info| {
            let hosts = pane_hosts_claude(snapshot, info.pid);
            (info, hosts)
        })
        .collect();

    let mut latched: HashMap<String, bool> = HashMap::new();
    if let Ok(mut guard) = steward_meta_store().lock() {
        for (info, hosts) in &observed {
            if let Some(meta) = guard.get_mut(&info.id) {
                if *hosts == Some(true) {
                    meta.claude_seen = true;
                }
                latched.insert(info.id.clone(), meta.claude_seen);
            }
        }
    }

    observed
        .into_iter()
        .map(|(info, hosts_claude)| {
            let claude_seen = latched.get(&info.id).copied().unwrap_or(false);
            TrackedPane {
                info,
                hosts_claude,
                claude_seen,
            }
        })
        .collect()
}

/// [`find_running_steward`] against an already-taken process snapshot.
///
/// The snapshot is required rather than optional. A caller with nothing to
/// judge passes `ProcessSnapshot::default()`, whose empty parent map makes
/// [`pane_hosts_claude`] answer `None` — the same fall-back-to-shell-liveness
/// behaviour an `Option::None` used to encode, without a second way to spell
/// it or an arm to keep in sync.
fn find_running_steward_in(
    terminal_manager: &TerminalManager,
    kind: &str,
    snapshot: &crate::process_capture::process_tree::ProcessSnapshot,
) -> Option<TerminalInfo> {
    tracked_panes(terminal_manager, kind, snapshot)
        .into_iter()
        .find(|pane| {
            steward_pane_is_running(pane.info.is_alive, pane.hosts_claude, pane.claude_seen)
        })
        .map(|pane| pane.info)
}

/// Close the tracked panes of this kind that have been EMPTIED — see
/// [`steward_pane_is_emptied`] for what leaves them behind and why nothing
/// else ever removes them.
///
/// **Closing is safe precisely because of the predicate**: the snapshot proves
/// there is no `claude` in the subtree, so this cannot kill a live agent
/// session, which is the act D5 and served policy `production-and-cost`
/// `runner-lifecycle` forbid. It is a bare shell being closed.
///
/// Returns how many it closed. Called from [`start_steward`] BEFORE the
/// single-instance guard, which is the moment the leak would otherwise
/// double: the undrain restart is about to open a second pane for a kind
/// whose first one is an empty husk.
async fn reap_emptied_panes(
    terminal_manager: &Arc<TerminalManager>,
    kind: &str,
    snapshot: &crate::process_capture::process_tree::ProcessSnapshot,
) -> usize {
    let emptied: Vec<String> = tracked_panes(terminal_manager, kind, snapshot)
        .into_iter()
        .filter(|pane| {
            steward_pane_is_emptied(pane.info.is_alive, pane.hosts_claude, pane.claude_seen)
        })
        .map(|pane| pane.info.id)
        .collect();

    let mut closed = 0usize;
    for id in emptied {
        let mgr = terminal_manager.clone();
        let target = id.clone();
        match spawn_blocking_tracked(move || mgr.close(&target)).await {
            Ok(Ok(())) => {
                info!(
                    "steward: reaped emptied {} pane {} — shell alive, no claude in it",
                    kind, id
                );
                closed += 1;
                if let Ok(mut guard) = steward_meta_store().lock() {
                    guard.remove(&id);
                }
            }
            Ok(Err(e)) => warn!(
                "steward: could not close emptied {} pane {}: {}",
                kind, id, e
            ),
            Err(e) => warn!(
                "steward: reaper task for {} pane {} failed to join: {}",
                kind, id, e
            ),
        }
    }
    closed
}

/// Find the live terminal session tracked as the steward **of this kind**, if
/// any.
///
/// Keyed on terminal **id** via [`steward_meta_store`], NOT on `title`.
/// Manual testing (2026-07-19, temp-runner UI Bridge verification) found
/// that `title` cannot serve as a durable single-instance marker on this
/// runner: PowerShell (and other shells) emit OSC 0/2 title-change escape
/// sequences, and xterm.js relays those back to the runner via
/// `TerminalSession::set_title` — "Phase 2 of bi-directional title sync"
/// (`terminal/session.rs:1541-1555`) — silently overwriting our title
/// sentinel moments after the shell starts. The skill name is still set at
/// creation as the terminal's initial cosmetic label, but only the
/// metadata-store id membership is authoritative for "running".
///
/// Takes a process snapshot ONLY when this kind has a tracked pane to judge,
/// so the common "not running" answer stays free.
async fn find_running_steward(
    terminal_manager: &TerminalManager,
    kind: &str,
) -> Option<TerminalInfo> {
    let snapshot = snapshot_for_kind(kind).await;
    find_running_steward_in(terminal_manager, kind, &snapshot)
}

/// A process snapshot for judging one kind's panes — taken only when that kind
/// has a tracked pane, and otherwise the empty default (see
/// [`find_running_steward_in`]).
async fn snapshot_for_kind(kind: &str) -> crate::process_capture::process_tree::ProcessSnapshot {
    let has_tracked = steward_meta_store()
        .lock()
        .map(|guard| !tracked_ids_for_kind(&guard, kind).is_empty())
        .unwrap_or(false);
    if has_tracked {
        crate::process_capture::process_tree::snapshot_process_table_public().await
    } else {
        Default::default()
    }
}

// ============================================================================
// Request / Response Types
// ============================================================================

/// Request body for `POST /steward/{kind}/start`.
#[derive(Debug, Deserialize)]
pub struct StewardStartRequest {
    /// Skill mode. The valid values are the launched skill's own, not a
    /// vocabulary this module owns — see [`StewardSpec::default_mode`].
    #[serde(default)]
    pub mode: Option<String>,
    /// `/loop` polling interval (e.g. `"5m"`).
    #[serde(default)]
    pub interval: Option<String>,
}

/// Response for `GET /steward/{kind}/status`, and one element of
/// `GET /stewards`.
#[derive(Debug, Serialize)]
pub struct StewardStatusResponse {
    /// Which steward this row describes.
    pub kind: String,
    pub label: String,
    pub skill: String,
    /// What `start` would use when the caller supplies nothing — surfaced so
    /// a UI can show the cadence it is about to launch without duplicating
    /// this table.
    pub default_mode: String,
    pub default_interval: String,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    /// Unix timestamp in milliseconds the steward's terminal was created at
    /// (the terminal's own `created_at`, not a separately-tracked value).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
}

/// Compute one steward's status — shared by `GET /steward/{kind}/status`, the
/// `GET /stewards` roster, and the single-instance guard in
/// `POST /steward/{kind}/start`.
fn steward_status_in(
    terminal_manager: &TerminalManager,
    spec: &'static StewardSpec,
    snapshot: &crate::process_capture::process_tree::ProcessSnapshot,
) -> StewardStatusResponse {
    let running = find_running_steward_in(terminal_manager, spec.kind, snapshot);
    prune_stale_meta(terminal_manager);

    let base = StewardStatusResponse {
        kind: spec.kind.to_string(),
        label: spec.label.to_string(),
        skill: spec.skill.to_string(),
        default_mode: spec.default_mode.to_string(),
        default_interval: spec.default_interval.to_string(),
        running: false,
        session_id: None,
        mode: None,
        interval: None,
        started_at: None,
    };

    match running {
        Some(info) => {
            let (mode, interval) = steward_meta_store()
                .lock()
                .ok()
                .and_then(|guard| {
                    guard
                        .get(&info.id)
                        .map(|meta| (Some(meta.mode.clone()), Some(meta.interval.clone())))
                })
                .unwrap_or((None, None));

            StewardStatusResponse {
                running: true,
                session_id: Some(info.id.clone()),
                mode,
                interval,
                started_at: Some(info.created_at),
                ..base
            }
        }
        None => base,
    }
}

/// The longest `mode` or `interval` this endpoint will accept.
///
/// Every legitimate value is a short word (`autonomous`) or a duration
/// (`10m`); the cap exists so a pasted blob cannot become a multi-kilobyte
/// line typed into a live shell.
const MAX_LAUNCH_TOKEN_LEN: usize = 32;

/// Reject a `mode`/`interval` that would not survive being typed into a shell
/// as a bare word.
///
/// **This is the boundary that makes the endpoint a steward launcher rather
/// than an arbitrary local-command executor.** Both values are interpolated
/// into [`build_launch_command`] and the result is written straight into a PTY
/// (`session.write`, in [`steward_start_handler`]) — so every byte reaches a
/// live shell. Unvalidated, `{"mode":"autonomous; <anything>"}` runs
/// `<anything>` on the box, and a value containing `\r` or `\n` injects an
/// entire second command line, because the writer terminates the command with
/// `\r\n`. The runner's HTTP API is loopback-only, but "loopback" includes
/// every local process and every agent that can reach port 9876.
///
/// This deliberately validates **shape, not vocabulary**. The three skills
/// draw modes from different vocabularies (`observe`/`autonomous` versus
/// `report`/`reap`) and this module owns none of them — see
/// [`StewardSpec::default_mode`] — so an unrecognised-but-well-formed mode is
/// still forwarded, and the skill reports it itself. Quoting the value instead
/// of refusing it was rejected for the same reason `build_launch_command`
/// branches on the shell: the correct quoting differs between PowerShell and
/// POSIX, and a quoting bug here fails open.
fn validate_launch_token(value: &str, field: &str) -> Result<(), String> {
    if value.len() > MAX_LAUNCH_TOKEN_LEN {
        return Err(format!(
            "{} is too long ({} chars, max {})",
            field,
            value.len(),
            MAX_LAUNCH_TOKEN_LEN
        ));
    }
    // Alphanumerics plus `-`, `_` and `.`: enough for every mode the three
    // skills define and every `/loop` interval spelling (`5m`, `10m`, `1h30m`),
    // with no character a shell treats as syntax.
    if let Some(bad) = value
        .chars()
        .find(|&c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Err(format!(
            "{} contains an unsupported character {:?} \
             (allowed: letters, digits, '-', '_', '.')",
            field, bad
        ));
    }
    Ok(())
}

/// Build the launch command typed into the PTY, platform-appropriate for the
/// env-var prefix. `TerminalSession::build_shell_command` (`terminal/session.rs`)
/// spawns `powershell.exe` on Windows and `$SHELL` (bash by default) on
/// other platforms, so the env-var syntax must match — the same convention
/// already used by the frontend's `buildAiLaunchCommand`
/// (`aiLaunchCommand.ts`): `$env:VAR="value"; cmd` on Windows,
/// `VAR=value cmd` (POSIX prefix form) elsewhere. The plan's literal
/// invocation is POSIX-shaped; this reproduces its effect on both shells
/// rather than typing bash syntax into a PowerShell prompt where it would not
/// parse.
///
/// The `enable_env` assignment is the load-bearing half: every steward skill
/// gates itself on that variable and does nothing at all when it is unset, so
/// a launch command that omitted it would start a session that reports itself
/// disabled on every iteration.
fn build_launch_command(spec: &StewardSpec, mode: &str, interval: &str) -> String {
    let mut base = format!("claude /loop {} /{} --mode={}", interval, spec.skill, mode);
    for arg in spec.extra_args {
        base.push(' ');
        base.push_str(arg);
    }
    if cfg!(target_os = "windows") {
        format!("$env:{}=\"1\"; {}", spec.enable_env, base)
    } else {
        format!("{}=1 {}", spec.enable_env, base)
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Get the TerminalManager from Tauri managed state (same helper pattern as
/// `mcp/terminals.rs::get_terminal_manager`).
fn get_terminal_manager(state: &ApiState) -> Arc<TerminalManager> {
    state
        .app_handle
        .state::<Arc<TerminalManager>>()
        .inner()
        .clone()
}

// ============================================================================
// Handlers
// ============================================================================

/// [`steward_status_in`] taking its own process snapshot. One snapshot serves
/// every kind, and it is skipped entirely when nothing is tracked.
async fn steward_status_all(terminal_manager: &TerminalManager) -> Vec<StewardStatusResponse> {
    let any_tracked = steward_meta_store()
        .lock()
        .map(|guard| !guard.is_empty())
        .unwrap_or(false);
    let snapshot = if any_tracked {
        crate::process_capture::process_tree::snapshot_process_table_public().await
    } else {
        Default::default()
    };
    STEWARDS
        .iter()
        .map(|spec| steward_status_in(terminal_manager, spec, &snapshot))
        .collect()
}

/// [`steward_status_in`] for ONE kind, taking its own snapshot when that kind
/// has a tracked pane to judge.
async fn steward_status(
    terminal_manager: &TerminalManager,
    spec: &'static StewardSpec,
) -> StewardStatusResponse {
    let snapshot = snapshot_for_kind(spec.kind).await;
    steward_status_in(terminal_manager, spec, &snapshot)
}

/// `GET /stewards` — the whole roster with each steward's live status.
///
/// One request answers "what can this runner launch, and what is up right
/// now?", so a polling UI does not have to know the roster in advance or fan
/// out one request per kind.
pub async fn stewards_list_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<StewardStatusResponse>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);
    let rows = steward_status_all(&terminal_manager).await;
    Ok(Json(ApiResponse::success(rows)))
}

/// `GET /steward/{kind}/status` — is this steward running, and if so, which
/// terminal/mode/interval?
pub async fn steward_status_handler(
    State(state): State<Arc<ApiState>>,
    Path(kind): Path<String>,
) -> Result<Json<ApiResponse<StewardStatusResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let spec = steward_spec(&kind).ok_or_else(|| unknown_kind_error(&kind))?;
    let terminal_manager = get_terminal_manager(&state);
    Ok(Json(ApiResponse::success(
        steward_status(&terminal_manager, spec).await,
    )))
}

/// `POST /steward/{kind}/start` — spawn this steward's PTY terminal session
/// server-side, mirroring `create_terminal_handler`
/// (`mcp/terminals.rs:137`). Refuses with 409 if a steward **of this kind** is
/// already running; different kinds coexist by design.
///
/// An HTTP caller is AUTONOMOUS under coord's device drain (plan
/// `2026-09-13-drained-runner-never-reaches-idle`, D3): while the device is
/// drained, or its drain state is unknown, this answers 409 and starts nothing.
/// The runner UI starts stewards through the [`steward_start`] Tauri command
/// instead, which is an operator action and is never deferred.
pub async fn steward_start_handler(
    State(state): State<Arc<ApiState>>,
    Path(kind): Path<String>,
    Json(request): Json<StewardStartRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    start_steward(
        state.app_handle.clone(),
        kind,
        request,
        crate::coord_drain_state::SpawnOrigin::Steward,
    )
    .await
}

/// `steward_start` — the runner UI's own start button. An operator sitting at
/// this runner (D3), so the coord device drain never defers it; the UI shows the
/// draining banner instead. Same validation, single-instance guard and launch as
/// `POST /steward/{kind}/start`. Returns that route's `data` object, or the
/// refusal text (prefixed with its HTTP status) as the error.
#[tauri::command]
pub async fn steward_start(
    app_handle: tauri::AppHandle,
    kind: String,
    mode: Option<String>,
    interval: Option<String>,
) -> Result<serde_json::Value, String> {
    match start_steward(
        app_handle,
        kind,
        StewardStartRequest { mode, interval },
        crate::coord_drain_state::SpawnOrigin::OperatorTerminal,
    )
    .await
    {
        Ok(Json(resp)) => Ok(resp.data.unwrap_or(serde_json::Value::Null)),
        Err((status, Json(err))) => Err(format!(
            "{}: {}",
            status.as_u16(),
            err.error
                .unwrap_or_else(|| "steward start refused".to_string())
        )),
    }
}

/// Restart a steward the drained-runner wind-down stopped (plan
/// `2026-09-13-drained-runner-never-reaches-idle`, Phase 4 / D6).
///
/// Its own defaults are used, exactly as `POST /steward/{kind}/start` with an
/// empty body would: the runner does not persist the mode and interval a
/// stopped steward was launched with — the metadata store is in-memory and the
/// close removed the row — so reconstructing them would be invention. *"undrain
/// returns the runner to what it was running"* is satisfied at the roster's
/// granularity, and an operator who launched a non-default mode over the API
/// relaunches it the same way.
///
/// Carries [`SpawnOrigin::Steward`] — the AUTONOMOUS origin — deliberately: the
/// restart is the runner's own act, not an operator's, so it passes the same
/// gate every other autonomous steward start passes. A drain that re-armed
/// between the undrain decision and this call therefore defers it rather than
/// spawning into a drained device, and [`RestartOutcome::Deferred`] is what
/// says so — the caller KEEPS the kind owed, so the next undrain restarts it.
///
/// That sentence used to read *"the kind is already out of the stopped-by-drain
/// set, so nothing retries forever"*, which was the pre-S-2 behaviour: the
/// whole set was cleared before the restarts ran, so a deferral silently lost
/// the steward. Settlement is now per-kind and keyed on this outcome, and the
/// no-retry-forever property comes from the undrain edge being the only
/// trigger, not from the kind having already been dropped.
pub(crate) async fn restart_after_drain(
    app_handle: tauri::AppHandle,
    kind: &str,
) -> Result<RestartOutcome, String> {
    match start_steward(
        app_handle,
        kind.to_string(),
        StewardStartRequest {
            mode: None,
            interval: None,
        },
        crate::coord_drain_state::SpawnOrigin::Steward,
    )
    .await
    {
        Ok(_) => Ok(RestartOutcome::Started),
        // 409 covers TWO families, and collapsing them loses a steward.
        //
        // * A DRAIN DEFERRAL — the drain re-armed between the undrain decision
        //   and this call. Nothing started, and nothing else will re-record the
        //   kind: there is no tab left to close, so the wind-down tick can never
        //   put it back in the stopped-by-drain set. The caller must keep it
        //   owed. `code` is exactly what says so — `api_refusal` stamps
        //   `DeferClass`'s `device_drained` / `drain_unreadable`.
        // * ANYTHING ELSE — already running (an operator restarted it by hand
        //   during the drain), or a start already in flight. Benign; nothing is
        //   owed, and the caller settles the kind out of its set.
        //
        // The "already running" arm is only benign because
        // `steward_pane_is_running` now asks whether a `claude` lives in the
        // pane. While it keyed on the SHELL, a pane wind-down had emptied
        // answered `running: true` forever and landed HERE, which dropped the
        // kind from the stopped-by-drain set and left the steward down.
        Err((StatusCode::CONFLICT, Json(err))) => {
            let deferred = err.code.as_deref().is_some_and(|code| {
                code == crate::coord_drain_state::DeferClass::Drained.code()
                    || code == crate::coord_drain_state::DeferClass::Unknown.code()
            });
            let reason = err
                .error
                .unwrap_or_else(|| "already running or deferred".to_string());
            Ok(if deferred {
                RestartOutcome::Deferred(reason)
            } else {
                RestartOutcome::NotNeeded(reason)
            })
        }
        Err((status, Json(err))) => Err(format!(
            "{}: {}",
            status.as_u16(),
            err.error
                .unwrap_or_else(|| "steward restart refused".to_string())
        )),
    }
}

/// What [`restart_after_drain`] achieved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RestartOutcome {
    /// A fresh steward session was started.
    Started,
    /// Nothing was started, and nothing is wrong: the reason is carried so the
    /// caller can log WHICH benign case it was.
    NotNeeded(String),
    /// The coord device drain deferred the restart — it re-armed between the
    /// undrain decision and this call. Nothing was started and the kind is
    /// STILL OWED: no tab is left to close, so nothing else will re-record it.
    Deferred(String),
}

/// The shared start path behind the HTTP route and the Tauri command. `origin`
/// decides how the coord device drain applies: `Steward` (autonomous) is
/// deferred while the drain holds, `OperatorTerminal` is not.
async fn start_steward(
    app_handle: tauri::AppHandle,
    kind: String,
    request: StewardStartRequest,
    origin: crate::coord_drain_state::SpawnOrigin,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let spec = steward_spec(&kind).ok_or_else(|| unknown_kind_error(&kind))?;
    let terminal_manager: Arc<TerminalManager> =
        app_handle.state::<Arc<TerminalManager>>().inner().clone();

    // Resolve and validate BEFORE taking the start claim or spawning anything,
    // so a malformed body is a clean 400 with no side effects — it neither
    // occupies the single-instance slot nor leaves a PTY behind.
    let mode = request
        .mode
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| spec.default_mode.to_string());
    let interval = request
        .interval
        .filter(|i| !i.is_empty())
        .unwrap_or_else(|| spec.default_interval.to_string());

    for (value, field) in [(&mode, "mode"), (&interval, "interval")] {
        if let Err(detail) = validate_launch_token(value, field) {
            warn!(
                "HTTP: Refusing to start {} — {} (value rejected before reaching the shell)",
                spec.skill, detail
            );
            return Err((StatusCode::BAD_REQUEST, Json(api_error(detail))));
        }
    }

    // Coord device drain (D3). After validation, so a malformed body is still a
    // clean 400; before the start claim, so a deferral occupies nothing. The
    // deferral is counted on the draining banner, keyed by steward kind.
    if let crate::coord_drain_state::DrainGate::Defer { reason, class } =
        crate::coord_drain_state::drain_gate_for_work(origin, &format!("steward:{}", spec.kind))
    {
        warn!("HTTP: Not starting {} — {reason}", spec.skill);
        return Err((
            StatusCode::CONFLICT,
            Json(crate::coord_drain_state::api_refusal(&reason, class)),
        ));
    }

    // Claim the start slot BEFORE the running-check, and hold it for the rest
    // of this handler (released on drop). Checking first and claiming later
    // would leave exactly the window this claim exists to close — see
    // [`starting_kinds`].
    let _claim = StartClaim::acquire(spec.kind).ok_or_else(|| {
        warn!(
            "HTTP: Refusing to start {} — another start of this kind is already in flight",
            spec.skill
        );
        (
            StatusCode::CONFLICT,
            Json(api_error(format!("{} is already starting", spec.skill))),
        )
    })?;

    // Reap this kind's emptied panes BEFORE the guard reads them. A refused
    // graceful close leaves a live shell with no `claude` in it, and nothing
    // else ever removes one: `prune_stale_meta` keeps it (the shell is alive)
    // and the wind-down census skips it (no top-level `claude`). Without this
    // the undrain restart below opens a second pane beside the husk and the
    // runner accumulates one per drain/undrain cycle.
    //
    // Its snapshot is NOT reused for the guard. The guard takes its own,
    // deliberately: it must see the state AFTER these closes, and it is the
    // one read where a stale answer re-opens the double-start window.
    let reaper_snapshot = snapshot_for_kind(spec.kind).await;
    reap_emptied_panes(&terminal_manager, spec.kind, &reaper_snapshot).await;

    // Single-instance guard, PER KIND: refuse if `GET /steward/{kind}/status`
    // would report running: true. A merge-train steward must not block a
    // dev-ops one.
    let current = steward_status(&terminal_manager, spec).await;
    if current.running {
        let session_id = current.session_id.unwrap_or_default();
        warn!(
            "HTTP: Refusing to start {} — already running as terminal {}",
            spec.skill, session_id
        );
        return Err((
            StatusCode::CONFLICT,
            Json(api_error(format!(
                "{} is already running (terminal {})",
                spec.skill, session_id
            ))),
        ));
    }

    let launch_command = build_launch_command(spec, &mode, &interval);

    info!(
        "HTTP: Starting {} (mode={}, interval={})",
        spec.skill, mode, interval
    );

    match terminal_manager.create(
        Some(spec.skill.to_string()),
        None, // working_dir — default to workspace root
        None, // page_id — default "default"
        None, // cols — default
        None, // rows — default
        app_handle,
        None, // command override — interactive shell, we type the command in
        // The shared session-env contribution. No isolated edit context here
        // (a steward runs in the shared checkout), so `QONTINUI_SESSION_WORKTREES`
        // is omitted exactly as before — but a steward is an agent session and
        // must still learn where the plans live. See `agent_worktree::session_env`.
        crate::agent_worktree::session_env::session_extra_env(None),
        // UNATTENDED spawn — respect the critical floor. A steward is a
        // long-running autonomous agent session; starting one on a box
        // that is already out of commit is how the incident's `claude`-inside-a-
        // terminal deaths happened. The refusal returns as this endpoint's error
        // body (with lane/headroom/floor), and stewards are explicitly
        // restartable, so a refusal defers the steward rather than losing it.
        false,
        // Interactive shell; the skill command is typed in afterwards and the
        // account is chosen then, so the every-account mint applies.
        crate::terminal::TrustArm::AccountChosenLater,
        // A steward session runs under the machine default tenant.
        None,
    ) {
        Ok(info) => {
            info!("HTTP: Created {} terminal: {}", spec.skill, info.id);

            // Recording the terminal id is what makes this steward reachable
            // by `status` and `stop`. If it fails we must NOT leave the PTY
            // running: an unrecorded steward reports as absent and answers
            // `stop` with a 404, which is precisely the ungovernable-steward
            // failure this module exists to prevent. Close what we just
            // opened and report the failure instead of leaking it.
            // Reduce the lock result to a plain `Result<(), String>` in its own
            // statement. A `PoisonError` carries the `MutexGuard`, which is not
            // `Send`, so holding it across the `.await` below would make this
            // handler's future non-`Send` and it would stop satisfying axum's
            // `Handler` bound — a compile error whose message names the route,
            // not the guard. Scoping the lock here drops it before any await.
            let recorded: Result<(), String> = match steward_meta_store().lock() {
                Ok(mut guard) => {
                    guard.insert(
                        info.id.clone(),
                        StewardMeta {
                            kind: spec.kind.to_string(),
                            mode: mode.clone(),
                            interval: interval.clone(),
                            // Nothing to latch yet — the pane is empty by
                            // construction at this instant, and stays so until
                            // the detached launch task has typed the command
                            // and `claude` has started.
                            claude_seen: false,
                        },
                    );
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            };

            if let Err(detail) = recorded {
                error!(
                    "HTTP: steward metadata store poisoned while registering {} \
                     (terminal {}): {} — closing the terminal rather than leaking \
                     an unstoppable steward",
                    spec.skill, info.id, detail
                );
                let mgr = terminal_manager.clone();
                let orphan = info.id.clone();
                // Not `let _`: a `JoinError` here means the PTY this handler
                // just refused to leak WAS leaked, and it is now an
                // unrecorded, unstoppable steward — exactly the failure the
                // comment above invokes. Say so; there is nothing left to
                // retry with.
                match spawn_blocking_tracked(move || mgr.close(&orphan)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => error!(
                        "HTTP: failed to close the orphaned {} terminal {}: {} — \
                         it is now an unrecorded steward",
                        spec.skill, info.id, e
                    ),
                    Err(e) => error!(
                        "HTTP: the orphan-close task for {} terminal {} did not \
                         join: {} — the PTY is LEAKED and unreachable by stop",
                        spec.skill, info.id, e
                    ),
                }
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!(
                        "Failed to register {}: metadata store unavailable",
                        spec.skill
                    ))),
                ));
            }

            // Type the launch command in after a short delay for shell
            // initialization — identical pattern to `create_terminal_handler`'s
            // `initial_command` handling (`mcp/terminals.rs:194-205`).
            //
            // Manual testing (2026-07-19, temp-runner UI Bridge verification)
            // initially misread this as a `$` → `$$` corruption bug: reading
            // the buffer via `?format=text` (ANSI-stripped) visually mangles
            // PSReadLine's per-token syntax-highlighting colors into what
            // looks like a doubled leading character. The raw (base64,
            // un-stripped) buffer confirmed the actual bytes PowerShell
            // receives are byte-for-byte correct, and PowerShell's own
            // `\x1b]633;E;...` shell-integration readback echoes the exact,
            // uncorrupted command line back — this write path has no bug.
            // The "Syntaxfehler" seen in that same manual test traced to an
            // unrelated, pre-existing `claude` PowerShell **function**
            // already defined in the test machine's own `$PROFILE`
            // (confirmed via `Get-Command claude` → `Function`, not the CLI
            // binary) whose own body has a syntax error — an environment
            // issue on that machine, out of scope for this plan.
            let mgr = terminal_manager.clone();
            let tid = info.id.clone();
            let cmd = launch_command.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                if let Some(session) = mgr.get(&tid) {
                    let cmd = format!("{}\r\n", cmd);
                    let _ = session.write(
                        cmd.as_bytes(),
                        crate::terminal::session::PtyWriteCaller::StewardLaunchCommand,
                    );
                }
            });

            Ok(Json(ApiResponse::success(serde_json::json!({
                "id": info.id,
                "kind": spec.kind,
                "title": spec.skill,
                "mode": mode,
                "interval": interval,
            }))))
        }
        Err(e) => {
            error!("HTTP: Failed to start {}: {}", spec.skill, e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to start {}: {}", spec.skill, e))),
            ))
        }
    }
}

/// Query for `POST /steward/{kind}/stop`.
#[derive(Debug, Default, Deserialize)]
pub struct StewardStopQuery {
    /// `boundary` for the GRACEFUL variant (plan
    /// `2026-09-13-drained-runner-never-reaches-idle`, D6). Anything else — or
    /// absent — keeps the shipped kill.
    pub at: Option<String>,
}

/// The `at=` value that selects the graceful stop.
pub const STOP_AT_BOUNDARY: &str = "boundary";

/// PURE: does this `at=` select the graceful variant? Exact match, so a
/// mistyped value takes the DOCUMENTED default (the kill) rather than silently
/// becoming the other one.
pub fn stops_at_boundary(at: Option<&str>) -> bool {
    at.map(str::trim) == Some(STOP_AT_BOUNDARY)
}

/// `POST /steward/{kind}/stop` — stop this steward's tracked terminal session.
///
/// Two variants, selected by `?at=`:
///
/// * **default** — the shipped `TerminalManager::close` kill path
///   (`terminal/manager.rs:221`, same as `close_terminal_handler`,
///   `mcp/terminals.rs:387`). Immediate, and it kills the `claude` in the pane.
/// * **`?at=boundary`** — `TerminalManager::graceful_exit`: types `/exit` at
///   the steward's idle window (its `/loop` iteration boundary), waits for
///   `claude` to leave, and only then closes the tab. Never kills a live
///   `claude` (D5), so it can answer `exit_stuck` and leave the steward
///   RUNNING — the response says which, and the caller must read it rather
///   than assume a 200 means stopped.
///
/// This is the same graceful path the wind-down executor uses; the difference
/// is only that this one is asked for, so it does not wait for the idle window
/// to have lasted a grace period — `graceful_exit` refuses on its own if the
/// pane is not at an empty prompt.
pub async fn steward_stop_handler(
    State(state): State<Arc<ApiState>>,
    Path(kind): Path<String>,
    axum::extract::Query(query): axum::extract::Query<StewardStopQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let spec = steward_spec(&kind).ok_or_else(|| unknown_kind_error(&kind))?;
    let terminal_manager = get_terminal_manager(&state);

    let info = find_running_steward(&terminal_manager, spec.kind)
        .await
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("{} is not running", spec.skill))),
            )
        })?;

    let manager = terminal_manager.clone();
    let terminal_id = info.id.clone();

    if stops_at_boundary(query.at.as_deref()) {
        info!(
            "HTTP: Stopping {} at its iteration boundary (terminal {})",
            spec.skill, terminal_id
        );
        let outcome = manager
            .graceful_exit(
                &terminal_id,
                crate::terminal::graceful_exit::DEFAULT_DEADLINE,
            )
            .await
            .map_err(|e| {
                error!("HTTP: Failed to stop {} gracefully: {}", spec.skill, e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to stop {}: {}", spec.skill, e))),
                )
            })?;
        let stopped = matches!(
            outcome,
            crate::terminal::graceful_exit::GracefulExitOutcome::Exited { .. }
        );
        // Only a real exit retires the metadata row. A steward left running
        // must stay reachable by `status` and `stop`.
        if stopped {
            if let Ok(mut guard) = steward_meta_store().lock() {
                guard.remove(&info.id);
            }
        } else {
            warn!(
                "HTTP: {} was NOT stopped at its boundary — left running: {:?}",
                spec.skill, outcome
            );
        }
        return Ok(Json(ApiResponse::success(serde_json::json!({
            "stopped": stopped,
            "kind": spec.kind,
            "session_id": info.id,
            "at": STOP_AT_BOUNDARY,
            "outcome": outcome,
        }))));
    }

    info!("HTTP: Stopping {} (terminal {})", spec.skill, terminal_id);

    spawn_blocking_tracked(move || manager.close(&terminal_id))
        .await
        .map_err(|e| {
            error!(
                "HTTP: spawn_blocking error closing {} terminal: {}",
                spec.skill, e
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Internal error: {}", e))),
            )
        })?
        .map_err(|e| {
            error!("HTTP: Failed to stop {}: {}", spec.skill, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to stop {}: {}", spec.skill, e))),
            )
        })?;

    if let Ok(mut guard) = steward_meta_store().lock() {
        guard.remove(&info.id);
    }

    Ok(Json(ApiResponse::success(serde_json::json!({
        "stopped": true,
        "kind": spec.kind,
        "session_id": info.id,
    }))))
}

// ============================================================================
// Routes
// ============================================================================

/// Create routes for this module.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};

    axum::Router::new()
        .route("/stewards", get(stewards_list_handler))
        .route("/steward/{kind}/status", get(steward_status_handler))
        .route("/steward/{kind}/start", post(steward_start_handler))
        .route("/steward/{kind}/stop", post(steward_stop_handler))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert that `values` has no duplicates, naming the offender.
    fn assert_unique(values: impl Iterator<Item = &'static str>, field: &str) {
        let mut seen: Vec<&str> = values.collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            before,
            seen.len(),
            "duplicate {} in the steward roster — remaining: {:?}",
            field,
            seen
        );
    }

    /// B-1: a steward is a `claude`, not a shell. The pane wind-down emptied
    /// has a LIVE shell and no `claude`, and keying on the shell made it answer
    /// `running: true` forever — which swallowed the undrain restart as a
    /// benign "already running" 409 and left the steward down.
    #[test]
    fn a_pane_whose_claude_left_is_not_a_running_steward() {
        // The regression, stated as the predicate sees it. `claude_seen` is
        // what makes this pane "left" rather than "not arrived yet".
        assert!(
            !steward_pane_is_running(true, Some(false), true),
            "a live shell that HAD a claude and no longer does is a bare prompt, \
             not a running steward"
        );
        // ...and the ordinary running case still is.
        assert!(steward_pane_is_running(true, Some(true), true));
        assert!(steward_pane_is_running(true, Some(true), false));
        // A dead pane is not running whatever the process table says.
        for seen in [true, false] {
            assert!(!steward_pane_is_running(false, Some(true), seen));
            assert!(!steward_pane_is_running(false, Some(false), seen));
            assert!(!steward_pane_is_running(false, None, seen));
        }
    }

    /// B3-1: the START WINDOW. Between `TerminalManager::create` returning and
    /// `claude` appearing — a detached task, a 300 ms sleep, then a program
    /// start, so 1-3 s and longer on a loaded box — the pane is alive, has a
    /// real root pid, and provably hosts no `claude`. That is byte-for-byte the
    /// same observation as an emptied pane.
    ///
    /// Reading it as "not running" is not a cosmetic wrong answer: the roster
    /// is the single-instance guard, so for that whole window `start` would
    /// have admitted a SECOND steward of the kind, and the UI re-enables its
    /// Start button on exactly that field. Two merge-train stewards driving
    /// the merge train is the outcome the fail direction exists to prevent, and
    /// the first version of this predicate produced it at every single start.
    #[test]
    fn a_steward_that_has_not_arrived_yet_is_still_running() {
        assert!(
            steward_pane_is_running(true, Some(false), false),
            "an empty pane that has NEVER hosted a claude is starting, not stopped"
        );
        // And the latch is the only difference between the two readings.
        assert!(!steward_pane_is_running(true, Some(false), true));
    }

    /// FAIL DIRECTION. An unanswerable process table must keep the old
    /// answer, because a false "not running" admits a second steward of the
    /// kind. Recoverable — `stop` takes the one `list()` yields first, pruning
    /// then exposes the survivor to the NEXT `status`, so two stop calls end
    /// it — but two is not zero, and for a merge-train steward the interval
    /// between them is spent with two of them driving the train.
    #[test]
    fn an_unreadable_process_table_keeps_the_pane_running() {
        for seen in [true, false] {
            assert!(
                steward_pane_is_running(true, None, seen),
                "a pane counts as running unless it is PROVEN to have emptied"
            );
        }
    }

    /// The reaper's predicate is strictly narrower than "not running": it fires
    /// only on the one state that leaks, and never on an unreadable table or a
    /// pane that has yet to start.
    #[test]
    fn only_a_pane_that_was_occupied_and_is_now_empty_is_reaped() {
        assert!(steward_pane_is_emptied(true, Some(false), true));

        // Never on a starting pane — closing one of those races the launch.
        assert!(!steward_pane_is_emptied(true, Some(false), false));
        // Never on an occupied pane. This is the load-bearing one: the reaper
        // CLOSES, and closing a pane that hosts a live `claude` is the act D5
        // and `runner-lifecycle` forbid.
        assert!(!steward_pane_is_emptied(true, Some(true), true));
        // Never on an unreadable table — "could not look" is not evidence.
        assert!(!steward_pane_is_emptied(true, None, true));
        // A dead pane needs no reaping; `prune_stale_meta` already has it.
        assert!(!steward_pane_is_emptied(false, Some(false), true));
    }

    /// The two predicates never both hold: nothing is reaped while it counts
    /// as the running steward, whatever the inputs.
    #[test]
    fn a_reaped_pane_is_never_a_running_one() {
        for alive in [true, false] {
            for hosts in [Some(true), Some(false), None] {
                for seen in [true, false] {
                    assert!(
                        !(steward_pane_is_running(alive, hosts, seen)
                            && steward_pane_is_emptied(alive, hosts, seen)),
                        "alive={alive} hosts={hosts:?} seen={seen}"
                    );
                }
            }
        }
    }

    /// `?at=boundary` selects the GRACEFUL stop; everything else keeps the
    /// shipped kill. Exact match, so a typo takes the documented default
    /// rather than silently becoming the other variant.
    #[test]
    fn only_an_exact_at_boundary_selects_the_graceful_stop() {
        assert!(stops_at_boundary(Some("boundary")));
        assert!(stops_at_boundary(Some("  boundary  ")));
        assert!(!stops_at_boundary(None));
        for other in [
            "",
            "Boundary",
            "boundry",
            "iteration-boundary",
            "kill",
            "now",
        ] {
            assert!(!stops_at_boundary(Some(other)), "{other:?}");
        }
    }

    #[test]
    fn roster_kinds_are_unique() {
        // `kind` is both the routing key and the single-instance key, so a
        // duplicate silently makes one steward unreachable.
        assert_unique(STEWARDS.iter().map(|s| s.kind), "kind");
    }

    #[test]
    fn roster_enablement_variables_are_unique() {
        // The highest-consequence data error in this table is a copy-paste
        // that gives two stewards the SAME enablement variable: the launch
        // command would then set a flag the started skill does not read, so
        // that steward runs and refuses to act on every iteration — which is
        // indistinguishable from a healthy idle watch, and is the exact bug
        // this roster was introduced to fix. Uniqueness is checked separately
        // from `kind` because the leak-check in
        // `launch_command_sets_the_skills_own_enablement_variable` skips any
        // pair whose variables are equal, so without this assertion that
        // error would pass every other test in this file.
        assert_unique(STEWARDS.iter().map(|s| s.enable_env), "enable_env");
        assert_unique(STEWARDS.iter().map(|s| s.skill), "skill");
    }

    #[test]
    fn roster_matches_the_skill_documents() {
        // A SECOND, INDEPENDENT copy of the three variable names, transcribed
        // from the skill documents that own them:
        //   merge-train-steward.md:84   COORD_MERGE_STEWARD_ENABLED
        //   dev-ops-steward.md:157      COORD_DEVOPS_STEWARD_ENABLED
        //   cleanup-steward.md:63       QONTINUI_CLEANUP_STEWARD_ENABLED
        // Everything else in this file derives its expectations from STEWARDS
        // itself and so cannot fail on a wrong name. This can.
        let expected: &[(&str, &str, &str, &str)] = &[
            (
                "merge-train",
                "COORD_MERGE_STEWARD_ENABLED",
                "autonomous",
                "5m",
            ),
            (
                "dev-ops",
                "COORD_DEVOPS_STEWARD_ENABLED",
                "autonomous",
                "10m",
            ),
            (
                "cleanup",
                "QONTINUI_CLEANUP_STEWARD_ENABLED",
                "report",
                "15m",
            ),
        ];
        assert_eq!(
            STEWARDS.len(),
            expected.len(),
            "roster changed size — update this transcription from the skill docs"
        );
        for (kind, env, mode, interval) in expected {
            let spec = steward_spec(kind).unwrap_or_else(|| panic!("{} missing from roster", kind));
            assert_eq!(spec.enable_env, *env, "{} enablement variable", kind);
            assert_eq!(spec.default_mode, *mode, "{} default mode", kind);
            assert_eq!(
                spec.default_interval, *interval,
                "{} default interval",
                kind
            );
        }
    }

    #[test]
    fn tracked_ids_for_kind_does_not_mix_kinds() {
        // The per-kind single-instance guard is only correct if this filter
        // is: a merge-train session leaking into dev-ops' id set would let
        // `stop` kill the wrong steward.
        let mut store: HashMap<String, StewardMeta> = HashMap::new();
        for (id, kind) in [
            ("term-a", "merge-train"),
            ("term-b", "dev-ops"),
            ("term-c", "dev-ops"),
        ] {
            store.insert(
                id.to_string(),
                StewardMeta {
                    kind: kind.to_string(),
                    mode: "autonomous".to_string(),
                    interval: "5m".to_string(),
                    claude_seen: false,
                },
            );
        }

        let merge = tracked_ids_for_kind(&store, "merge-train");
        assert_eq!(merge.len(), 1);
        assert!(merge.contains("term-a"));

        let dev_ops = tracked_ids_for_kind(&store, "dev-ops");
        assert_eq!(dev_ops.len(), 2);
        assert!(dev_ops.contains("term-b") && dev_ops.contains("term-c"));
        assert!(
            !dev_ops.contains("term-a"),
            "dev-ops picked up a merge-train terminal"
        );

        // A kind with no rows must be empty, not everything.
        assert!(tracked_ids_for_kind(&store, "cleanup").is_empty());
    }

    #[test]
    fn start_claim_excludes_a_concurrent_start_of_the_same_kind_only() {
        let first = StartClaim::acquire("merge-train").expect("first claim");
        assert!(
            StartClaim::acquire("merge-train").is_none(),
            "a second concurrent start of the same kind must be refused"
        );
        // A different kind is unaffected — stewards of different kinds are
        // meant to coexist.
        let other = StartClaim::acquire("dev-ops").expect("a different kind is not blocked");
        drop(other);

        drop(first);
        // Released on drop, so the kind can start again afterwards.
        assert!(
            StartClaim::acquire("merge-train").is_some(),
            "the claim must be released on drop, or that kind can never start again"
        );
    }

    #[test]
    fn spec_lookup_matches_by_kind_and_rejects_unknown() {
        assert_eq!(
            steward_spec("dev-ops").map(|s| s.skill),
            Some("dev-ops-steward")
        );
        assert_eq!(
            steward_spec("merge-train").map(|s| s.skill),
            Some("merge-train-steward")
        );
        // The skill name is NOT the kind; looking one up by skill must miss.
        assert!(steward_spec("dev-ops-steward").is_none());
        assert!(steward_spec("nope").is_none());
    }

    #[test]
    fn launch_command_sets_the_skills_own_enablement_variable() {
        // The discriminating assertion: each steward must get ITS variable,
        // not a shared one. `COORD_MERGE_STEWARD_ENABLED=1 claude
        // /dev-ops-steward` would launch a session whose gate never opens.
        for spec in STEWARDS {
            let cmd = build_launch_command(spec, spec.default_mode, spec.default_interval);
            assert!(
                cmd.contains(spec.enable_env),
                "{} launch command omits {}: {}",
                spec.kind,
                spec.enable_env,
                cmd
            );
            assert!(
                cmd.contains(&format!("/{} ", spec.skill)),
                "{} launch command does not invoke /{}: {}",
                spec.kind,
                spec.skill,
                cmd
            );
            for other in STEWARDS {
                if other.enable_env != spec.enable_env {
                    assert!(
                        !cmd.contains(other.enable_env),
                        "{} launch command leaks {}'s variable: {}",
                        spec.kind,
                        other.kind,
                        cmd
                    );
                }
            }
        }
    }

    #[test]
    fn build_launch_command_is_shell_appropriate() {
        let spec = steward_spec("merge-train").expect("merge-train is in the roster");
        let cmd = build_launch_command(spec, "observe", "5m");
        if cfg!(target_os = "windows") {
            assert_eq!(
                cmd,
                "$env:COORD_MERGE_STEWARD_ENABLED=\"1\"; claude /loop 5m /merge-train-steward --mode=observe"
            );
        } else {
            assert_eq!(
                cmd,
                "COORD_MERGE_STEWARD_ENABLED=1 claude /loop 5m /merge-train-steward --mode=observe"
            );
        }
    }

    #[test]
    fn build_launch_command_honors_custom_mode_and_interval() {
        let spec = steward_spec("merge-train").expect("merge-train is in the roster");
        let cmd = build_launch_command(spec, "autonomous", "10m");
        assert!(cmd.contains("--mode=autonomous"));
        assert!(cmd.contains("/loop 10m"));
    }

    #[test]
    fn only_the_self_arming_steward_is_told_not_to_loop() {
        // `dev-ops-steward` arms its own `/loop`; wrapping it in one without
        // `--no-loop` leaves two loops driving a single session. The other two
        // skills do not recognise the flag at all, so passing it would be an
        // unknown argument rather than a harmless extra.
        let dev_ops = build_launch_command(
            steward_spec("dev-ops").expect("dev-ops is in the roster"),
            "autonomous",
            "10m",
        );
        assert!(dev_ops.ends_with("--no-loop"), "got: {}", dev_ops);

        for kind in ["merge-train", "cleanup"] {
            let spec = steward_spec(kind).expect("in the roster");
            let cmd = build_launch_command(spec, spec.default_mode, spec.default_interval);
            assert!(
                !cmd.contains("--no-loop"),
                "{} got --no-loop: {}",
                kind,
                cmd
            );
        }
    }

    #[test]
    fn every_roster_default_survives_validation() {
        // The gate runs on the RESOLVED values, so a roster default that the
        // validator rejects would make that steward permanently unlaunchable
        // from the UI button (which sends an empty body and therefore always
        // takes the defaults). This is the assertion that couples the two.
        for spec in STEWARDS {
            assert!(
                validate_launch_token(spec.default_mode, "mode").is_ok(),
                "{} default mode {:?} is rejected by its own launcher",
                spec.kind,
                spec.default_mode
            );
            assert!(
                validate_launch_token(spec.default_interval, "interval").is_ok(),
                "{} default interval {:?} is rejected by its own launcher",
                spec.kind,
                spec.default_interval
            );
        }
    }

    #[test]
    fn validation_accepts_the_modes_the_skills_actually_define() {
        // Shape, not vocabulary: this module does not own any skill's mode
        // list, so every well-formed word from every skill must pass —
        // including the ones no roster row uses as a default (`observe` after
        // a major change, `reap` for a mutating cleanup run).
        for value in ["observe", "autonomous", "report", "reap"] {
            assert!(
                validate_launch_token(value, "mode").is_ok(),
                "{} must be forwardable",
                value
            );
        }
        for value in ["5m", "10m", "15m", "1h30m", "90s"] {
            assert!(
                validate_launch_token(value, "interval").is_ok(),
                "{} must be forwardable",
                value
            );
        }
    }

    #[test]
    fn validation_rejects_values_that_would_reach_the_shell_as_syntax() {
        // Each of these is a working local-command execution against the
        // launcher if the value is interpolated unchecked: the resulting line
        // is typed verbatim into a PTY. `\r` matters as much as `;` — the
        // writer appends `\r\n`, so an embedded carriage return submits an
        // entire second command line.
        for payload in [
            "autonomous; calc",
            "autonomous && whoami",
            "autonomous | whoami",
            "autonomous\rcalc",
            "autonomous\ncalc",
            "$(whoami)",
            "`whoami`",
            "autonomous ; rm -rf /",
            "--mode=x --dangerously-skip-permissions",
        ] {
            let err = match validate_launch_token(payload, "mode") {
                Ok(()) => panic!("{:?} must be refused, not typed into a shell", payload),
                Err(e) => e,
            };
            assert!(
                err.contains("mode"),
                "the refusal must name the offending field: {}",
                err
            );
        }

        // Length is capped independently of the character set — an
        // all-alphanumeric blob is well-formed but still has no business
        // being typed into a shell.
        let long = "a".repeat(MAX_LAUNCH_TOKEN_LEN + 1);
        assert!(validate_launch_token(&long, "interval").is_err());
        assert!(validate_launch_token(&"a".repeat(MAX_LAUNCH_TOKEN_LEN), "interval").is_ok());
    }

    #[test]
    fn a_rejected_value_never_reaches_the_built_command() {
        // Belt-and-braces: prove the payload the validator refuses WOULD have
        // been interpolated verbatim, so this test fails if a future refactor
        // starts sanitising in `build_launch_command` and drops the gate.
        let spec = steward_spec("merge-train").expect("merge-train is in the roster");
        let payload = "autonomous; calc";
        assert!(validate_launch_token(payload, "mode").is_err());
        assert!(
            build_launch_command(spec, payload, "5m").contains("; calc"),
            "build_launch_command does not quote — the validator is the only guard"
        );
    }

    #[test]
    fn unknown_kind_error_names_the_valid_kinds() {
        let (status, Json(body)) = unknown_kind_error("bogus");
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(!body.success);

        // Read the MESSAGE, not just the status. An earlier version of this
        // test destructured the body away and so passed against an empty
        // error string — it asserted a property of `STEWARDS` rather than of
        // the function under test.
        let message = body.error.expect("error envelope carries a message");
        assert!(
            message.contains("bogus"),
            "message should quote the offending kind: {}",
            message
        );
        for spec in STEWARDS {
            assert!(
                message.contains(spec.kind),
                "message should name the valid kind '{}' so a caller can \
                 self-correct: {}",
                spec.kind,
                message
            );
        }
    }
}
