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
use tracing::{debug, info, warn};

use qontinui_runner_lib::looping_agent::desired_state::{
    self, CoordDesiredState, DesiredSource, EffectiveDesired,
};
use qontinui_runner_lib::looping_agent::idle::{snapshot_context_low, snapshot_looks_idle};
use qontinui_runner_lib::looping_agent::lease::{self, HeldSlot};
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

/// Slot leases this supervisor currently holds: `agent_id -> HeldSlot`
/// (Phase 8).
///
/// In-memory ONLY, and that is correct rather than a gap: coord's Redis is the
/// authority on who holds a slot, and this map is just a local memo of what we
/// believe. A runner restart forgets it, re-acquires on the next tick, and gets
/// `renewed` back for its own still-live lease (the owner token is
/// machine-scoped — see `looping_agent_coord::claim_body`), so the slot is
/// re-adopted rather than stranded. Persisting it would create a second,
/// disagreeing source of truth — the exact tier-0-CHECK-vs-tier-1-RECORD
/// mismatch that produced the #1025 spawn storm.
static HELD_SLOTS: Mutex<Option<HashMap<String, HeldSlot>>> = Mutex::new(None);

fn held_slot_for(agent_id: &str) -> Option<HeldSlot> {
    let guard = HELD_SLOTS.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().and_then(|m| m.get(agent_id)).cloned()
}

fn set_held_slot(agent_id: &str, slot: HeldSlot) {
    let mut guard = HELD_SLOTS.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(HashMap::new)
        .insert(agent_id.to_string(), slot);
}

fn clear_held_slot(agent_id: &str) -> Option<HeldSlot> {
    let mut guard = HELD_SLOTS.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_mut().and_then(|m| m.remove(agent_id))
}

/// Last-logged effective posture per agent, so the posture line is
/// edge-triggered rather than emitted every 5s tick (the same
/// log-only-on-transition discipline `fleet_policy_poller` uses).
#[allow(clippy::type_complexity)]
static LAST_POSTURE: Mutex<Option<HashMap<String, (bool, u32, DesiredSource)>>> = Mutex::new(None);

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

/// One supervisor pass over every agent.
///
/// Phase 8: the pass no longer short-circuits on the local registry flag. COORD
/// is now the declared source of truth ("this tenant wants N merge shepherds,
/// armed"), so an agent whose LOCAL flag is off may still be running per coord
/// — and, just as importantly, an agent that is locally enabled but holds a
/// slot lease it must release when coord disarms still needs a visit. The pure
/// [`desired_state::resolve`] does the fold per agent; the fail-safe direction
/// (disarmed/absent/unreachable → the local posture, whose seed is
/// `enabled=false`) lives there and is unit-tested there.
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

    // One poll per tick, shared by every agent (cached ~45s inside).
    let coord_rules = crate::looping_agent_coord::desired_state_rules().await;

    for rec in registry.list() {
        supervise_one(&app, &registry, &rec, coord_rules.as_deref()).await;
    }
}

/// Map a registry `playbook_ref` to its coord `agent_playbook` document name
/// and its `coord.sessions.role` wire value.
///
/// Phase 1 knows exactly one agent, so this is a match rather than a registry
/// column; adding a role means adding an arm here and a variant to coord's
/// `SessionRole`. An unknown ref yields `None` and the agent is supervised
/// purely from its local registry posture — it can never chase a coord slot,
/// because a lease key needs a role to be built from.
fn role_for_agent(playbook_ref: &str) -> Option<&'static str> {
    match playbook_ref {
        MERGE_SHEPHERD_ID => Some("merge_shepherd"),
        _ => None,
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
///
/// Phase 8 threads two new things through the same observe→decide→act shape:
/// the coord-declared desired state (which replaces the local flag as the
/// posture input) and the claim-first slot lease (which gates every
/// session-creating action).
async fn supervise_one(
    app: &tauri::AppHandle,
    registry: &Arc<LoopingAgentRegistry>,
    rec: &LoopingAgentRecord,
    coord_rules: Option<&[CoordDesiredState]>,
) {
    let now_ms = chrono::Utc::now().timestamp_millis();

    // -- Posture: coord's declaration folded over the local registry (pure). --
    let role = role_for_agent(&rec.def.playbook_ref);
    let coord_rule = match (coord_rules, role) {
        (Some(rules), Some(role)) => desired_state::rule_for_role(rules, role),
        // No coord answer (unreachable / never polled) or an agent with no
        // fleet role → the resolver's local-registry fallback.
        _ => None,
    };
    let effective = desired_state::resolve(
        rec.def.enabled,
        rec.def.desired_state == DesiredState::Running,
        coord_rule,
    );
    log_posture_transition(&rec.def.id, &effective);

    // Agent-registry spawn authorization (plan
    // `2026-07-28-migrate-claude-md-into-qontinui.md` Phase 4c, served clause
    // `agent-spawn-authorization`). A looping agent is the archetypal spawn
    // that OUTLIVES the request: it relaunches itself indefinitely on the
    // user's own AI quota. Standing per-path opt-in, default OFF for a fresh
    // user.
    //
    // Resolved here, applied in TWO precise places below — deliberately NOT by
    // zeroing `effective`:
    //   * Zeroing the posture would send `reconcile_slot_lease` down its
    //     "clean stop" branch, RELEASING the fleet slot lease of an agent whose
    //     tab is still alive. Another runner on a stale-but-valid registry
    //     snapshot would then acquire the freed slot and spawn a second copy —
    //     the #1025 double-spawn this lease exists to prevent, re-opened by a
    //     fleet-wide decision that reaches runners at different times.
    //   * So instead: `may_acquire` stops us taking a NEW lease, while an
    //     existing hold keeps heart-beating for as long as the agent is
    //     DESIRED RUNNING (the heartbeat branch is gated on `effective`, not
    //     on tab liveness — which is resolved later in this fn — so a refused
    //     agent whose tab has already exited also keeps its slot warm; that is
    //     the safe direction during rollout skew, since a peer runner reading
    //     the same tenant verdict is refused too and starves on nothing); and
    //     the action gate below rewrites Spawn/Relaunch to None.
    //
    // A refusal therefore stops NEW spawns and relaunches without killing or
    // orphaning a live session. That is the correct reading: the clause governs
    // whether a spawn HAPPENS, and the runner-lifecycle clause forbids killing
    // a live session to enforce it. `Action::Nudge` is left alone — continuing
    // an existing conversation is not a spawn.
    //
    // Only asked when the agent is actually wanted running: an agent the
    // operator has switched off can never spawn, so asking every tick would be
    // a pointless coord read and pointless log volume.
    let authz = if effective.running {
        crate::agent_authorization::authorize_spawn(
            Some(&rec.def.name),
            crate::agent_authorization::SpawnPath::StandingContinuation,
        )
        .await
    } else {
        crate::agent_authorization::SpawnDecision::Allow
    };
    let authz_permits_spawn = authz.allows_spawn();

    // -- Slot lease reconciliation (claim-first — D3 / the #1025 lesson). --
    //
    // MUST happen BEFORE `policy::decide` is acted on: `gate_action` rewrites
    // any session-creating action to `None` unless we hold a lease, so the
    // lease state is an INPUT to acting, never a check performed afterwards.
    let holds_lease = reconcile_slot_lease(app, rec, &effective, authz_permits_spawn).await;

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

    // SHADOW MODE (plan 2026-08-11-agent-bailout-detector-for-finish-to-zero,
    // Phase 2): a confirmed-idle tick is the moment a turn just ENDED, so it is
    // the moment to classify how it ended. Classify and record only — the
    // verdict feeds NOTHING below, and `TickInput` deliberately does not carry
    // it. The false-positive rate on real fleet traffic is unknown; Phase 3
    // reviews the recorded corpus by hand before Phase 4 enables any action.
    //
    // Note this reads the TRANSCRIPT, not the grid `idle` was computed from —
    // the grid's normalizer collapses newlines, so the "last non-empty
    // paragraph" rule cannot be evaluated against it. See `turn_ending_shadow`.
    if idle {
        if let Some(csid) = rec.runtime.claude_session_id.as_deref() {
            let agent_id = rec.def.id.clone();
            let journal_path = rec.def.journal_path.clone();
            let csid = csid.to_string();
            // Off the tick's critical path: the read touches disk and sweeps
            // every Claude config dir on the box, and shadow mode must never
            // slow or perturb the loop it observes.
            // Returns None when this tick saw the SAME ending already
            // journalled — the common case at a 5s tick.
            tokio::task::spawn_blocking(move || {
                if let Some(ending) = crate::turn_ending_shadow::observe_turn_ending(
                    &agent_id,
                    &journal_path,
                    &csid,
                    now_ms,
                ) {
                    debug!(
                        agent = %agent_id,
                        ending = %ending.kind_label(),
                        "looping_agent_supervisor: turn ending recorded (SHADOW MODE)"
                    );
                }
            });
        }
    }

    let input = TickInput {
        // Phase 8: BOTH posture inputs now come from the resolved effective
        // state, not from the local registry row. `resolve` has already folded
        // `def.enabled` / `def.desired_state` in as the fallback arm, so an
        // armed coord rule can run an agent whose local flag is off, and an
        // armed `count=0` can stop one whose local flag is on.
        enabled: effective.running,
        desired_running: effective.running,
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

    // CLAIM-FIRST: the gate makes a lease-less spawn/relaunch structurally
    // impossible. This is the whole point of the phase — not "check whether a
    // shepherd exists and spawn if not" (a TOCTOU race every runner in the
    // fleet loses simultaneously — #1025), but "you already won the atomic
    // slot, therefore you may spawn".
    let action = lease::gate_action(policy::decide(&input), holds_lease);

    // Phase 4c action gate. Applied AFTER `policy::decide` so a refusal cannot
    // reach `Action::Relaunch`, which CLOSES the live tab before respawning —
    // a refusal discovered inside `do_spawn` would kill a running agent and
    // never restart it. `Nudge` and `None` pass through untouched.
    let action = if authz_permits_spawn {
        action
    } else {
        match action {
            Action::Spawn(reason) => {
                warn!(
                    agent = %rec.def.id,
                    ?reason,
                    decision = authz.label(),
                    "looping_agent_supervisor: spawn suppressed by the agent registry: {}",
                    authz.reason().unwrap_or("no reason recorded")
                );
                Action::None
            }
            Action::Relaunch(reason) => {
                warn!(
                    agent = %rec.def.id,
                    ?reason,
                    decision = authz.label(),
                    "looping_agent_supervisor: relaunch suppressed by the agent registry — \
                     the existing tab is left running untouched: {}",
                    authz.reason().unwrap_or("no reason recorded")
                );
                Action::None
            }
            other => other,
        }
    };

    match action {
        Action::None => {}
        Action::Spawn(reason) => {
            let prompt = spawn_prompt(&rec.def, reason != SpawnReason::FirstSpawn).await;
            let focus = reason == SpawnReason::FirstSpawn;
            do_spawn(app, registry, &rec.def, prompt, focus, false).await;
        }
        Action::Nudge => {
            let Some((tid, session)) = &live else { return };
            match session.submit_prompt(&playbook::nudge_prompt(&rec.def.journal_path)) {
                Ok(_) => {
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
            let prompt = spawn_prompt(&rec.def, true).await;
            do_spawn(app, registry, &rec.def, prompt, false, true).await;
        }
    }
}

/// Build the spawn prompt: the playbook + config + the one-cycle contract;
/// relaunches add the "read your journal and continue" framing.
///
/// Phase 8: the playbook is FETCHED FROM COORD at spawn
/// (`GET /coord/agent-playbook/:name`) so the versioned, web-edited document is
/// the source of truth. The runner-bundled `include_str!` copy remains the seed
/// and the offline fallback — resolved by the pure
/// [`playbook::resolve_playbook`], which is where the "blank body = absent"
/// and "unknown ref still yields a usable playbook" rules are pinned.
///
/// The fetch happens per spawn rather than per tick: spawns are rare (first
/// enable, relaunch, death-respawn) and each one is the exact moment the
/// freshest instructions matter, so there is nothing to cache.
async fn spawn_prompt(def: &LoopingAgentDef, is_relaunch: bool) -> String {
    let coord_body = crate::looping_agent_coord::fetch_playbook(&def.playbook_ref).await;
    if coord_body.is_none() {
        debug!(
            agent = %def.id,
            playbook_ref = %def.playbook_ref,
            "looping_agent_supervisor: no coord-served playbook — using the bundled fallback"
        );
    }
    let pb = playbook::resolve_playbook(coord_body.as_deref(), &def.playbook_ref);
    if is_relaunch {
        playbook::relaunch_prompt(pb, &def.journal_path, &def.repos)
    } else {
        playbook::initial_prompt(pb, &def.journal_path, &def.repos)
    }
}

/// Reconcile this agent's slot lease for one tick and report whether we hold
/// one. **The claim-first heart of Phase 8** (D3).
///
/// Lifecycle:
/// * Not running (disarmed / locally stopped) + we hold a lease → RELEASE it,
///   so a peer can fill the slot immediately instead of waiting out the TTL.
/// * Running + we hold a lease → HEARTBEAT. `stolen` drops it (coord's
///   authoritative answer); a transport error keeps it (see
///   `lease::still_held_after_heartbeat`).
/// * Running + no lease → ACQUIRE, walking the candidate slots in order. The
///   FIRST `claimed`/`renewed` is ours and is the permission to spawn. All
///   slots `held` → other live shepherds own them → we do nothing at all.
///
/// Returns `true` only when this supervisor genuinely holds a slot. Every
/// "can't tell" path returns `false`, because the one thing that must never
/// happen is spawning on an assumption.
async fn reconcile_slot_lease(
    app: &tauri::AppHandle,
    rec: &LoopingAgentRecord,
    effective: &EffectiveDesired,
    may_acquire: bool,
) -> bool {
    let agent_id = &rec.def.id;

    // Identity needed to build the lease key. Missing either one means we
    // cannot name the slot — so we cannot hold it, so we must not spawn.
    // (A tenant-less runner is unpaired; a role-less agent has no fleet slot.)
    let Some(role) = role_for_agent(&rec.def.playbook_ref) else {
        return false;
    };
    let Some(tenant_id) = crate::fleet::resolve_tenant_id() else {
        debug!(
            agent = %agent_id,
            "looping_agent_supervisor: no tenant binding — cannot claim a slot lease, not spawning"
        );
        return false;
    };
    let Some(machine_id) = app
        .try_state::<Arc<crate::session::SessionRegistry>>()
        .map(|s| s.inner().machine_id())
    else {
        return false;
    };

    let held = held_slot_for(agent_id);
    let claude_session_id = rec.runtime.claude_session_id.clone();

    // -- Clean stop: release what we hold. --
    if !effective.running {
        if let Some(slot) = clear_held_slot(agent_id) {
            info!(
                agent = %agent_id,
                resource_key = %slot.resource_key,
                "looping_agent_supervisor: agent is no longer desired — releasing its slot lease"
            );
            crate::looping_agent_coord::release_slot(&slot.resource_key, &machine_id, agent_id)
                .await;
        }
        return false;
    }

    // -- Hold: heartbeat it. --
    if let Some(slot) = &held {
        let outcome = crate::looping_agent_coord::heartbeat_slot(
            &slot.resource_key,
            &machine_id,
            agent_id,
            claude_session_id.as_deref(),
        )
        .await;
        if lease::still_held_after_heartbeat(&outcome) {
            return true;
        }
        // Stolen: another supervisor owns this slot now. Drop our memo and fall
        // through to the acquire path — if a slot is genuinely free we take it,
        // otherwise we do nothing this tick.
        warn!(
            agent = %agent_id,
            resource_key = %slot.resource_key,
            "looping_agent_supervisor: slot lease lost (stolen/expired) — another runner owns \
             this slot now"
        );
        clear_held_slot(agent_id);
    }

    // -- Acquire: claim-first. The FIRST win is the permission to spawn. --
    //
    // Phase 4c: an agent-registry refusal stops us taking a NEW slot (it would
    // starve the rest of the fleet of a slot nothing can use) but deliberately
    // does NOT reach the release branch above — an agent whose tab is still
    // alive keeps heart-beating the slot it already holds, so no peer runner
    // can acquire it and spawn a second copy while this one lives.
    if !may_acquire {
        debug!(
            agent = %agent_id,
            "looping_agent_supervisor: agent-registry refusal — not acquiring a new slot lease"
        );
        return false;
    }
    for slot in lease::candidate_slots(effective.slots, None) {
        let resource_key = lease::slot_resource_key(&tenant_id.to_string(), role, slot);
        let outcome = crate::looping_agent_coord::acquire_slot(
            &resource_key,
            &machine_id,
            agent_id,
            claude_session_id.as_deref(),
        )
        .await;
        match &outcome {
            _ if lease::spawn_permitted(&outcome) => {
                info!(
                    agent = %agent_id,
                    %resource_key,
                    "looping_agent_supervisor: acquired slot lease — spawn permitted"
                );
                set_held_slot(agent_id, HeldSlot { slot, resource_key });
                return true;
            }
            lease::AcquireOutcome::Held { current_holder } => {
                debug!(
                    agent = %agent_id,
                    %resource_key,
                    holder = current_holder.as_deref().unwrap_or("unknown"),
                    "looping_agent_supervisor: slot held by another runner — trying the next"
                );
            }
            lease::AcquireOutcome::Unavailable(e) => {
                // Coord unreachable is NOT a free slot. Stop walking (the rest
                // will fail identically) and spawn nothing.
                debug!(
                    agent = %agent_id,
                    %resource_key,
                    error = %e,
                    "looping_agent_supervisor: slot acquire unavailable — not spawning"
                );
                return false;
            }
            // `spawn_permitted` already covered Acquired.
            lease::AcquireOutcome::Acquired => unreachable!(),
        }
    }
    false
}

/// Edge-triggered posture logging: emit a line only when an agent's effective
/// posture CHANGES, never once per 5s tick.
fn log_posture_transition(agent_id: &str, effective: &EffectiveDesired) {
    let mut guard = LAST_POSTURE.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    let now = (effective.running, effective.slots, effective.source);
    if map.get(agent_id) == Some(&now) {
        return;
    }
    map.insert(agent_id.to_string(), now);
    info!(
        agent = %agent_id,
        running = effective.running,
        slots = effective.slots,
        source = ?effective.source,
        "looping_agent_supervisor: effective desired state changed"
    );
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

    // Agent-registry backstop. `supervise_one` folds the decision into the
    // posture so a refusal never reaches here in the normal path; this covers
    // any other caller. A refusal is deliberately NOT recorded as a spawn
    // failure — authorization is a standing decision, not a transient fault,
    // and feeding it into the escalating backoff would misreport it as a
    // broken agent.
    let authz = crate::agent_authorization::authorize_spawn(
        Some(&def.name),
        crate::agent_authorization::SpawnPath::StandingContinuation,
    )
    .await;
    if !authz.allows_spawn() {
        warn!(
            agent = %def.id,
            decision = authz.label(),
            "looping_agent_supervisor: spawn refused by the agent registry: {}",
            authz.reason().unwrap_or("no reason recorded")
        );
        return;
    }
    if let crate::agent_authorization::SpawnDecision::Warn { reason } = &authz {
        warn!(
            agent = %def.id,
            "looping_agent_supervisor: spawning under a warn_proceed disposition: {reason}"
        );
    }

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
        Err(e) if e.starts_with(crate::resource_guard::CRITICAL_REFUSAL_PREFIX) => {
            // The spawn-time resource guard refused (plan
            // `2026-08-07-runner-resource-guard-and-session-protection` §Part D).
            // Deliberately NOT recorded as a spawn failure, for the same reason
            // the registry-authorization refusal above is not: this is a POLICY
            // VERDICT on the machine's headroom, not evidence of a broken agent.
            // Feeding it into the escalating backoff (30 s doubling to a 900 s
            // cap) would misreport it and, worse, would keep the agent down for
            // up to 15 minutes AFTER the box recovered — the supervisor
            // re-evaluates every tick, so leaving the streak untouched is what
            // makes "as soon as there is commit to spare" true. Nothing hammers
            // the machine either: the next tick re-probes and gets refused again
            // for as long as the box is still under the critical floor.
            warn!(
                agent = %def.id,
                error = %e,
                "looping_agent_supervisor: spawn refused by the resource guard — \
                 retrying on the next tick, no backoff"
            );
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
    // Last-mile agent-registry gate (Phase 4c). `do_spawn` already checked,
    // but this fn IS the spawn recipe — any future caller reaching it directly
    // must not bypass the standing opt-in. The decision is served from the
    // module's TTL cache, so the second check costs no extra coord round-trip.
    let authz = crate::agent_authorization::authorize_spawn(
        Some(&def.name),
        crate::agent_authorization::SpawnPath::StandingContinuation,
    )
    .await;
    if let Some(refusal) = authz.refusal() {
        return Err(refusal);
    }

    // Spawn-time resource gate, EARLY-OUT arm — the same pre-check
    // `commands::terminal::terminal_create` runs, for the same reason and one
    // extra one. Below the critical floor the PTY seam will refuse this spawn
    // anyway; without this, every refused attempt would first create the agent's
    // home dir, rewrite its coord-MCP + fleet-command provisioning and probe the
    // Claude accounts. That matters here specifically because `do_spawn` does
    // NOT put a resource refusal into the escalating backoff (a policy verdict is
    // not a broken agent), so the supervisor re-attempts on EVERY tick while the
    // box is starved. Retrying every tick is what makes the agent restart the
    // moment there is commit to spare; this is what keeps that retry cheap.
    crate::resource_guard::precheck_spawn("looping-agent session", false)?;

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
    crate::fleet_skills::provision_fleet_skills_for_session(&workdir);

    // Interactive `claude` argv with the prompt as the trailing positional
    // arg (the proven continuation recipe — flags before prompt, `--`
    // terminator, fresh pinned session id per spawn attempt). The canonical
    // runner-context briefing rides `--append-system-prompt` (same as gate
    // continuations); the playbook itself goes in the visible initial prompt
    // so it lives in scrollback and is re-sent verbatim on every fresh
    // relaunch.
    let claude_bin = crate::agent_runtime::claude_bin_path();
    let pinned_session_id = uuid::Uuid::new_v4().to_string();

    // Account selection (fail-loud): never spawn a `claude` that dies
    // instantly with a 401 under a quota-exhausted/unauthenticated default.
    // Resolved BEFORE the argv build so its per-account launch command can layer
    // into the shared launch seam.
    let _ = tokio::task::spawn_blocking(crate::ai_provider::pick_best_account).await;
    let (selected_config_dir, _config_dir_source) = {
        let ai = crate::settings::get_ai_settings();
        crate::ai_provider::get_effective_config_dir(&ai.claude_cli)
    };
    let launch_cfg = crate::claude_session::launch_spec::LaunchConfig::from_settings(
        selected_config_dir.as_deref(),
    );
    let command = Some(crate::agent_runtime::build_continuation_claude_command(
        claude_bin,
        &pinned_session_id,
        Vec::new(),
        prompt,
        Some(crate::terminal::runner_context(
            crate::terminal::spawn_seam_api_port(),
        )),
        &launch_cfg,
    ));

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
            // UNATTENDED spawn — respect the critical floor. A looping agent is
            // by construction the one caller that will ask again: the supervisor
            // re-evaluates its definitions on every tick, so a refusal now is a
            // deferral, not a cancellation. `do_spawn` classifies the refusal by
            // its `resource_guard:critical:` prefix and does NOT feed it to
            // `note_spawn_failure`, so the agent restarts on the first tick after
            // the box has commit to spare rather than sitting out an escalating
            // backoff it did nothing to earn — a resource verdict is not a broken
            // agent. (Without that arm the refusal would land in the generic
            // `Err` arm and the fifth one would delay the restart by up to
            // 15 minutes past recovery.) Overriding here would instead let a loop
            // hammer a starved machine with new `claude` processes indefinitely —
            // the exact shape of the 2026-08-06→07 incident.
            false,
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
