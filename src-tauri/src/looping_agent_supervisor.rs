//! Tier-0 looping-agent supervisor — the impure glue (plan
//! `merge-shepherd-fixer-PLAN.md`, Phase 1).
//!
//! Composes the PURE cores in `qontinui_runner_lib::looping_agent` (registry,
//! idle/context-low predicates, per-tick decision policy, bundled playbook +
//! prompts) over the runner's existing substrate:
//!
//! - **Spawn** reuses the `run_continuation_terminal` recipe verbatim
//!   ([`crate::agent_runtime`]): docked visible tab whose PTY child IS an
//!   interactive `claude` (never `--print`), account pinned via
//!   `pick_best_account`, pre-pinned `--session-id`, `.mcp.json` + fleet
//!   slash-commands provisioned into the agent's home dir, durable lifecycle
//!   capture via `create_tracked_terminal_session_backend`.
//! - **Idle detection** reads each tab's rendered VT grid (the fleet's proven
//!   grid-scan approach — Ink's synchronized-output batching hides raw-byte
//!   windows) and applies the conservative lib predicate with a short
//!   two-read quiescence debounce.
//! - **Self-heal** follows `mcp/task_supervisor.rs`: the supervisor loop
//!   itself runs under `spawn_supervised`, so a panic in a tick respawns the
//!   loop with backoff instead of silently killing the feature.
//!
//! Never-expire mechanics (all decided by the pure policy core):
//! (a) cadence + idle-nudge — when the tab is idle and the cadence elapsed,
//!     submit "run your next cycle" via `TerminalSession::submit_prompt`;
//! (b) context-low / every-K-cycles relaunch — close the tab and respawn a
//!     FRESH `--session-id` claude pointed at the same playbook + journal
//!     (NO `--resume`, NO summarization — the journal is the memory);
//! (c) death-respawn — a dead tab whose `desired_state=running` is respawned
//!     (with escalating backoff on spawn failures);
//! (d) account exhaustion — left entirely to the existing
//!     `terminal/usage_limit.rs` + `account_migration.rs` machinery.
//!
//! Restart safety: runtime bookkeeping (terminal id + pinned claude session
//! id) is persisted in the registry, so after a runner restart the supervisor
//! RE-ATTACHES to a boot-restored tab (via the session lifecycle store)
//! instead of spawning a duplicate shepherd; while the lifecycle record is
//! still open but unmatched (a restore may be mid-flight) it WAITS — the
//! lifecycle poll closes truly-dead records within a few ticks, which then
//! triggers a clean fresh respawn.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tauri::Manager;
use tracing::{info, warn};

use qontinui_runner_lib::looping_agent::idle::{snapshot_context_low, snapshot_looks_idle};
use qontinui_runner_lib::looping_agent::playbook;
use qontinui_runner_lib::looping_agent::policy::{self, Action, Liveness, SpawnReason, TickInput};
use qontinui_runner_lib::looping_agent::registry::{
    DesiredState, LoopingAgentDef, LoopingAgentRecord, LoopingAgentRegistry, MERGE_SHEPHERD_ID,
};

/// Supervisor tick interval. Overridable via
/// `QONTINUI_LOOPING_AGENT_TICK_MS` (floored at 500ms). 5s is well under any
/// sane cadence while costing one grid snapshot per enabled agent per tick.
fn tick_interval() -> Duration {
    let ms = std::env::var("QONTINUI_LOOPING_AGENT_TICK_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n >= 500)
        .unwrap_or(5_000);
    Duration::from_millis(ms)
}

/// Delay before the FIRST tick after boot, so the boot-restore machinery
/// (durable-session resurrection + the reconcile backstop) has settled before
/// the supervisor judges liveness — prevents a restart from double-spawning a
/// shepherd whose old tab is being restored.
const BOOT_SETTLE_DELAY: Duration = Duration::from_secs(45);

/// Two-read idle quiescence debounce: both reads must look idle AND render
/// identical text (nothing streaming between them). Mirrors the PTY idle gate
/// in `mcp/session_message_poller.rs`.
const IDLE_QUIESCENCE_DEBOUNCE: Duration = Duration::from_millis(1500);

/// Holds the supervisor's shutdown sender for the process lifetime (never
/// signalled today; exists so a future orderly-shutdown path can stop the
/// loop cleanly).
static SHUTDOWN_TX: OnceLock<tokio::sync::watch::Sender<bool>> = OnceLock::new();

/// Per-agent spawn-failure streaks: `agent_id -> (consecutive_failures,
/// next_attempt_unix_ms)`. In-memory only (a restart retries immediately,
/// which is correct — the failure cause may have been the dying process).
/// Option-wrapped so the static is const-initializable (house style).
static SPAWN_FAILURES: Mutex<Option<HashMap<String, (u32, i64)>>> = Mutex::new(None);

/// Registry file path, port-namespaced like the session lifecycle store so a
/// temp/secondary runner (9877+) never adopts — or double-spawns — the
/// primary's agents.
fn registry_path() -> PathBuf {
    let api_port = crate::mcp::types::get_mcp_api_port();
    let file_name = if api_port == 9876 {
        "looping-agents.json".to_string()
    } else {
        format!("looping-agents-{api_port}.json")
    };
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".qontinui")
        .join("runner")
        .join(file_name)
}

/// A looping agent's home dir: its cwd, `.mcp.json` + fleet-command
/// provisioning target, and default journal location. Deliberately NOT a repo
/// checkout — the shepherd drives `gh`/coord tools and spawns its own
/// isolated worktrees for fixes, and provisioning into a private dir never
/// clobbers operator files in a shared checkout.
fn agent_home_dir(agent_id: &str) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".qontinui")
        .join("runner")
        .join("looping-agents")
        .join(agent_id)
}

/// Default journal path for a seeded built-in agent.
fn default_journal_path(agent_id: &str) -> String {
    agent_home_dir(agent_id)
        .join("journal.jsonl")
        .to_string_lossy()
        .to_string()
}

/// Open (or fall back for) the registry, seed the built-in Merge Shepherd
/// (DISABLED — the operator flips it on explicitly), manage the registry in
/// Tauri state for the control-surface commands, and start the supervised
/// tick loop. Call once from `main.rs` setup.
pub(crate) fn start(app: &tauri::AppHandle) {
    let path = registry_path();
    let registry = match LoopingAgentRegistry::open(&path) {
        Ok(r) => Arc::new(r),
        Err(e) => {
            warn!(
                error = %e,
                path = %path.display(),
                "looping_agent_supervisor: registry open failed — using ephemeral fallback"
            );
            let fallback = std::env::temp_dir().join("qontinui-runner-looping-agents.json");
            match LoopingAgentRegistry::open(&fallback) {
                Ok(r) => Arc::new(r),
                Err(e) => {
                    warn!(
                        error = %e,
                        "looping_agent_supervisor: ephemeral registry open failed — \
                         looping agents disabled this run"
                    );
                    return;
                }
            }
        }
    };
    if registry.seed_merge_shepherd(default_journal_path(MERGE_SHEPHERD_ID)) {
        info!(
            "looping_agent_supervisor: seeded built-in '{MERGE_SHEPHERD_ID}' \
             (disabled — enable via looping_agent_set_enabled)"
        );
    }
    app.manage(registry);

    let (tx, rx) = tokio::sync::watch::channel(false);
    let _ = SHUTDOWN_TX.set(tx);
    // Self-heal posture: the loop runs under the shared task supervisor so a
    // panicking tick respawns the loop (with backoff) instead of killing the
    // feature until the next runner restart. `spawn_supervised` calls
    // `tokio::spawn`, which needs a live runtime context — enter one via
    // `tauri::async_runtime::spawn` (setup runs on the main thread, outside
    // any reactor).
    tauri::async_runtime::spawn(async move {
        crate::mcp::task_supervisor::spawn_supervised("looping-agent-supervisor", rx, run_loop);
    });
    info!("looping_agent_supervisor: started");
}

/// The supervisor loop body: boot-settle delay, then tick forever. Runs under
/// `spawn_supervised`, so returning/panicking respawns it.
async fn run_loop() {
    tokio::time::sleep(BOOT_SETTLE_DELAY).await;
    let mut ticker = tokio::time::interval(tick_interval());
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        tick_once().await;
    }
}

/// One supervisor pass over every enabled+running agent.
async fn tick_once() {
    let Some(app) = crate::tauri_app_handle::current() else {
        return; // headless/unit-test context — nothing to supervise
    };
    let Some(registry) = app
        .try_state::<Arc<LoopingAgentRegistry>>()
        .map(|s| s.inner().clone())
    else {
        return;
    };
    for rec in registry.list() {
        if !rec.def.enabled || rec.def.desired_state != DesiredState::Running {
            continue;
        }
        supervise_one(&app, &registry, &rec).await;
    }
}

/// Rendered-grid read for one session: `(lines, cursor_row)`. Lock-poison
/// tolerant; the guard never crosses an await.
fn read_grid(session: &crate::terminal::session::TerminalSession) -> (Vec<String>, u16) {
    let grid = session.grid();
    let guard = grid.lock().unwrap_or_else(|e| e.into_inner());
    let snap = guard.text_snapshot();
    (snap.lines, snap.cursor_row)
}

/// Resolve the agent's live tab: direct terminal-id hit first, then re-attach
/// through the durable lifecycle record (whose terminal id a boot-restore
/// rewrites). Returns the pure [`Liveness`] plus the live session handle.
fn resolve_live_session(
    app: &tauri::AppHandle,
    rec: &LoopingAgentRecord,
) -> (
    Liveness,
    Option<(String, Arc<crate::terminal::session::TerminalSession>)>,
) {
    let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
        // No terminal manager managed (shouldn't happen in a real runner):
        // treat as pending — NEVER spawn into a half-initialized app.
        return (Liveness::PendingRestore, None);
    };
    let tm = tm.inner();

    let mut live: Option<(String, Arc<crate::terminal::session::TerminalSession>)> = None;
    let mut terminal_registered = false;
    if let Some(tid) = rec.runtime.terminal_id.as_deref() {
        if let Some(s) = tm.get(tid) {
            terminal_registered = true;
            live = Some((tid.to_string(), s));
        }
    }

    let mut lifecycle_open = false;
    let mut lifecycle_terminal_registered = false;
    if !terminal_registered {
        if let (Some(store), Some(csid)) = (
            app.try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>(),
            rec.runtime.claude_session_id.as_deref(),
        ) {
            if let Some(lrec) = store.inner().get(csid) {
                if lrec.state == "open" {
                    lifecycle_open = true;
                    if let Some(s) = tm.get(&lrec.terminal_id) {
                        lifecycle_terminal_registered = true;
                        live = Some((lrec.terminal_id.clone(), s));
                    }
                }
            }
        }
    }

    (
        policy::resolve_liveness(
            terminal_registered,
            lifecycle_open,
            lifecycle_terminal_registered,
        ),
        live,
    )
}

/// Supervise a single agent for one tick: observe, decide (pure), act.
async fn supervise_one(
    app: &tauri::AppHandle,
    registry: &Arc<LoopingAgentRegistry>,
    rec: &LoopingAgentRecord,
) {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let (liveness, live) = resolve_live_session(app, rec);

    // Re-attach bookkeeping: adopt a lifecycle-resolved terminal id so the
    // next tick (and the status command) hit it directly.
    if let Some((tid, _)) = &live {
        if rec.runtime.terminal_id.as_deref() != Some(tid.as_str()) {
            let tid = tid.clone();
            info!(
                agent = %rec.def.id,
                terminal = %tid,
                "looping_agent_supervisor: re-attached to boot-restored tab"
            );
            registry.update_runtime(&rec.def.id, |rt| rt.terminal_id = Some(tid));
        }
    }

    // Observe idle/context-low on the live grid (two-read quiescence debounce
    // — never nudge into a frame that is still streaming).
    let (mut idle, mut context_low) = (false, false);
    if let Some((_, session)) = &live {
        let (lines_a, cursor_a) = read_grid(session);
        if snapshot_looks_idle(&lines_a, cursor_a) {
            tokio::time::sleep(IDLE_QUIESCENCE_DEBOUNCE).await;
            let (lines_b, cursor_b) = read_grid(session);
            idle = snapshot_looks_idle(&lines_b, cursor_b)
                && lines_a == lines_b
                && cursor_a == cursor_b;
            context_low =
                snapshot_context_low(&lines_b, rec.def.lifecycle_policy.context_low_threshold_pct);
        }
    }

    let input = TickInput {
        enabled: rec.def.enabled,
        desired_running: rec.def.desired_state == DesiredState::Running,
        tab_alive: liveness != Liveness::Dead,
        ever_spawned: rec.runtime.last_spawn_at_ms.is_some(),
        spawn_backoff_elapsed: spawn_backoff_elapsed(&rec.def.id, now_ms),
        spawn_grace_elapsed: policy::spawn_grace_elapsed(
            now_ms,
            rec.runtime.last_spawn_at_ms,
            rec.def.lifecycle_policy.spawn_grace_secs,
        ),
        idle,
        context_low,
        cadence_elapsed: policy::cadence_elapsed(
            now_ms,
            rec.runtime.last_cycle_started_at_ms,
            rec.def.cadence_secs,
        ),
        cycles_since_relaunch: rec.runtime.cycles_since_relaunch,
        relaunch_every_cycles: rec.def.lifecycle_policy.relaunch_every_cycles,
    };

    match policy::decide(&input) {
        Action::None => {}
        Action::Spawn(reason) => {
            let prompt = spawn_prompt(&rec.def, reason != SpawnReason::FirstSpawn);
            let focus = reason == SpawnReason::FirstSpawn;
            do_spawn(app, registry, &rec.def, prompt, focus, false).await;
        }
        Action::Nudge => {
            let Some((tid, session)) = &live else { return };
            match session.submit_prompt(&playbook::nudge_prompt(&rec.def.journal_path)) {
                Ok(()) => {
                    info!(
                        agent = %rec.def.id,
                        terminal = %tid,
                        cycle = rec.runtime.cycles_since_relaunch + 1,
                        "looping_agent_supervisor: nudged next cycle"
                    );
                    registry.update_runtime(&rec.def.id, |rt| {
                        rt.cycles_since_relaunch = rt.cycles_since_relaunch.saturating_add(1);
                        rt.last_cycle_started_at_ms = Some(now_ms);
                    });
                }
                Err(e) => warn!(
                    agent = %rec.def.id,
                    terminal = %tid,
                    error = %e,
                    "looping_agent_supervisor: nudge submit failed (will retry next tick)"
                ),
            }
        }
        Action::Relaunch(reason) => {
            let Some((tid, _)) = &live else { return };
            info!(
                agent = %rec.def.id,
                terminal = %tid,
                ?reason,
                cycles = rec.runtime.cycles_since_relaunch,
                "looping_agent_supervisor: relaunching fresh (kill + fresh --session-id + \
                 re-read journal)"
            );
            if let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() {
                if let Err(e) = tm.inner().close(tid) {
                    warn!(
                        agent = %rec.def.id,
                        terminal = %tid,
                        error = %e,
                        "looping_agent_supervisor: closing old tab failed — proceeding to respawn"
                    );
                }
            }
            let prompt = spawn_prompt(&rec.def, true);
            do_spawn(app, registry, &rec.def, prompt, false, true).await;
        }
    }
}

/// Build the spawn prompt: the bundled playbook + config + the one-cycle
/// contract; relaunches add the "read your journal and continue" framing.
fn spawn_prompt(def: &LoopingAgentDef, is_relaunch: bool) -> String {
    let pb = playbook::playbook_for(&def.playbook_ref).unwrap_or_else(|| {
        warn!(
            agent = %def.id,
            playbook_ref = %def.playbook_ref,
            "looping_agent_supervisor: unknown playbook_ref — falling back to the bundled \
             merge-shepherd playbook"
        );
        playbook::MERGE_SHEPHERD_PLAYBOOK
    });
    if is_relaunch {
        playbook::relaunch_prompt(pb, &def.journal_path, &def.repos)
    } else {
        playbook::initial_prompt(pb, &def.journal_path, &def.repos)
    }
}

/// Whether the agent's spawn-failure backoff window has elapsed.
fn spawn_backoff_elapsed(agent_id: &str, now_ms: i64) -> bool {
    let guard = SPAWN_FAILURES.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref().and_then(|m| m.get(agent_id)) {
        Some((_, next_attempt_ms)) => now_ms >= *next_attempt_ms,
        None => true,
    }
}

/// Record a spawn failure: bump the streak and schedule the next attempt via
/// the pure escalating backoff.
fn note_spawn_failure(agent_id: &str, now_ms: i64) {
    let mut guard = SPAWN_FAILURES.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    let failures = map
        .get(agent_id)
        .map(|(f, _)| *f)
        .unwrap_or(0)
        .saturating_add(1);
    let delay_ms = (policy::spawn_backoff_secs(failures) as i64).saturating_mul(1000);
    map.insert(
        agent_id.to_string(),
        (failures, now_ms.saturating_add(delay_ms)),
    );
}

/// Clear an agent's spawn-failure streak (a spawn succeeded).
fn clear_spawn_failures(agent_id: &str) {
    let mut guard = SPAWN_FAILURES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(map) = guard.as_mut() {
        map.remove(agent_id);
    }
}

/// Spawn the agent's visible docked tab — a thin composition of the
/// `run_continuation_terminal` recipe (see the module doc). On success,
/// records the fresh terminal + pinned claude-session ids and resets the
/// cycle counter (the spawn prompt runs cycle 1); on failure, notes the
/// failure for the escalating backoff.
async fn do_spawn(
    app: &tauri::AppHandle,
    registry: &Arc<LoopingAgentRegistry>,
    def: &LoopingAgentDef,
    prompt: String,
    focus: bool,
    is_relaunch: bool,
) {
    let now_ms = chrono::Utc::now().timestamp_millis();
    match spawn_looping_agent_terminal(app, def, prompt).await {
        Ok((terminal_id, claude_session_id)) => {
            info!(
                agent = %def.id,
                terminal = %terminal_id,
                claude_session = %claude_session_id,
                "looping_agent_supervisor: spawned visible looping-agent tab"
            );
            clear_spawn_failures(&def.id);
            registry.update_runtime(&def.id, |rt| {
                rt.terminal_id = Some(terminal_id.clone());
                rt.claude_session_id = Some(claude_session_id.clone());
                // The spawn prompt itself starts cycle 1.
                rt.cycles_since_relaunch = 1;
                rt.last_spawn_at_ms = Some(now_ms);
                rt.last_cycle_started_at_ms = Some(now_ms);
                if is_relaunch {
                    rt.relaunch_count = rt.relaunch_count.saturating_add(1);
                }
            });
            if focus {
                // First-ever spawn (operator just enabled it): surface the tab.
                // Relaunches/respawns deliberately do NOT yank focus.
                crate::agent_runtime::emit_terminal_focus_request(app, &terminal_id);
            }
        }
        Err(e) => {
            warn!(
                agent = %def.id,
                error = %e,
                "looping_agent_supervisor: spawn failed — backing off"
            );
            note_spawn_failure(&def.id, now_ms);
        }
    }
}

/// The spawn recipe itself (mirrors `run_condition_check_terminal`, which is
/// the minimal sibling of `run_continuation_terminal`): visible docked tab,
/// interactive `claude` as the PTY child with the prompt as the trailing
/// positional argv, pinned `--session-id`, best-account pinning with the
/// fail-loud no-credential guard, page spreading, durable lifecycle capture,
/// coord-MCP + fleet slash-command provisioning into the agent's home dir.
async fn spawn_looping_agent_terminal(
    app: &tauri::AppHandle,
    def: &LoopingAgentDef,
    prompt: String,
) -> Result<(String, String), String> {
    let terminal_manager = app
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| "TerminalManager state not managed".to_string())?;
    let session_registry = app
        .try_state::<Arc<crate::session::SessionRegistry>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| "SessionRegistry state not managed".to_string())?;

    // The agent's private home dir is its cwd + provisioning target + journal
    // home (see `agent_home_dir`).
    let home = agent_home_dir(&def.id);
    std::fs::create_dir_all(&home)
        .map_err(|e| format!("failed to create agent home {}: {e}", home.display()))?;
    let workdir = home.to_string_lossy().to_string();

    // Coord MCP + fleet slash commands, same as gate continuations. The
    // actually-bound API port comes from AppState (fail-closed `None` writes
    // a degraded breadcrumb instead of a dead proxy config — the F1 lesson).
    let bound_port = app
        .try_state::<Arc<crate::commands::AppState>>()
        .map(|s| crate::mcp::types::runner_api_port(s.inner()));
    crate::coord_mcp::provision_coord_mcp_for_session(&workdir, bound_port);
    crate::fleet_commands::provision_fleet_commands_for_session(&workdir);

    // Interactive `claude` argv with the prompt as the trailing positional
    // arg (the proven continuation recipe — flags before prompt, `--`
    // terminator, fresh pinned session id per spawn attempt). The canonical
    // runner-context briefing rides `--append-system-prompt` (same as gate
    // continuations); the playbook itself goes in the visible initial prompt
    // so it lives in scrollback and is re-sent verbatim on every fresh
    // relaunch.
    let claude_bin = crate::agent_runtime::claude_bin_path();
    let pinned_session_id = uuid::Uuid::new_v4().to_string();
    let command = Some(crate::agent_runtime::build_continuation_claude_command(
        claude_bin,
        &pinned_session_id,
        Vec::new(),
        prompt,
        Some(crate::terminal::runner_context(
            crate::mcp::types::get_mcp_api_port(),
        )),
    ));

    // Account selection (fail-loud): never spawn a `claude` that dies
    // instantly with a 401 under a quota-exhausted/unauthenticated default.
    let _ = tokio::task::spawn_blocking(crate::ai_provider::pick_best_account).await;
    let selected_config_dir = {
        let ai = crate::settings::get_ai_settings();
        crate::ai_provider::get_effective_config_dir(&ai.claude_cli)
    };
    if selected_config_dir.is_none()
        && !crate::ai_provider::oauth_refresh::default_location_has_valid_credentials()
    {
        let instance = crate::instance::instance_name().unwrap_or_else(|| "primary".to_string());
        return Err(format!(
            "no authenticated Claude account on this runner — run /login (instance={instance})"
        ));
    }

    // Spread across non-full pages (same picker + ceiling as continuations).
    let counts: Vec<(String, usize)> = {
        let mut per_page: HashMap<String, usize> = HashMap::new();
        for info in terminal_manager.list() {
            *per_page.entry(info.page_id).or_insert(0) += 1;
        }
        per_page.into_iter().collect()
    };
    let target_page = crate::agent_runtime::pick_continuation_page(
        &counts,
        crate::agent_runtime::CONTINUATION_PAGE_ZONE_CEILING,
        || uuid::Uuid::new_v4().to_string(),
    );

    let capture_hint = crate::commands::terminal::SessionCaptureHint {
        config_dir: selected_config_dir,
        working_dir: workdir.clone(),
        title: def.name.clone(),
        page_id: Some(target_page.clone()),
        claude_session_id: Some(pinned_session_id.clone()),
        zone_index: None,
        // Autonomous agent → pin the agent git identity on the PTY.
        inject_agent_git_identity: true,
    };

    let (terminal_id, _coord_session) =
        crate::commands::terminal::create_tracked_terminal_session_backend(
            &terminal_manager,
            &session_registry,
            app.clone(),
            def.name.clone(),
            workdir,
            None,
            Some(format!("looping-agent:{}", def.id)),
            def.repos.first().cloned(),
            command,
            None,
            capture_hint,
            Some(target_page),
        )?;
    Ok((terminal_id, pinned_session_id))
}

// ---------------------------------------------------------------------------
// Status snapshot (consumed by the control-surface commands)
// ---------------------------------------------------------------------------

/// Wire status for one looping agent: the durable record plus live
/// observations.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LoopingAgentStatus {
    #[serde(flatten)]
    pub record: LoopingAgentRecord,
    /// A live tab is currently hosting (or being boot-restored for) this
    /// agent.
    pub tab_alive: bool,
    /// Rendered-grid idle read (`None` when there is no live tab). Single
    /// read, no quiescence debounce — status is observational.
    pub idle: Option<bool>,
}

/// Assemble the live status for one record.
pub(crate) fn status_snapshot(
    app: &tauri::AppHandle,
    rec: &LoopingAgentRecord,
) -> LoopingAgentStatus {
    let (liveness, live) = resolve_live_session(app, rec);
    let idle = live.as_ref().map(|(_, session)| {
        let (lines, cursor_row) = read_grid(session);
        snapshot_looks_idle(&lines, cursor_row)
    });
    LoopingAgentStatus {
        record: rec.clone(),
        tab_alive: liveness != Liveness::Dead,
        idle,
    }
}
