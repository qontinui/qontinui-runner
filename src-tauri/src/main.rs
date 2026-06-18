// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// Increase recursion limit for large serde_json::json! macros in database export
#![recursion_limit = "256"]
// Allow dead code: many modules are in active development with planned integrations
#![allow(dead_code)]
// Allow complex types: API response types are intentionally detailed
#![allow(clippy::type_complexity)]
// Allow many arguments: refactoring to structs is tracked separately
#![allow(clippy::too_many_arguments)]

mod action_service;
mod agent_claims;
mod agent_daemons;
mod agent_pusher;
// Plan `2026-05-19-coordinator-production-readiness.md` Phase 4 —
// coord-driven Claude Code subprocess runtime. Subscribes to spawn-
// requests on coord WS, materializes worktrees, spawns the `claude`
// CLI, heartbeats the claim, forwards stdout/stderr to coord logs.
mod agent_runtime;
mod agent_token;
mod agent_worktree;
mod agentic_verification;
mod ai_pricing;
mod ai_provider;
mod ai_router;
mod ai_workflows;
pub mod api_config;
mod api_request;
mod asset_headers;
mod auth;
mod auto_commit;
mod backup;
mod check_executor;
mod check_generation;
mod claude_accounts;
mod claude_protocol;
mod claude_session;
mod click_overlay;
mod commands;
mod comparison;
mod config;
mod config_facade;
mod config_storage;
mod constraint_engine;
mod container;
mod context;
mod coord_doctor_cmd;
mod coord_http;
mod coord_mcp;
mod coord_questions;
mod coordinator;
mod cost_management;
mod crash_dumps;
mod credential_helper;
mod database;
mod debug_lifecycle;
mod demo_workflows;
mod dev_services;
mod dirty_poller;
mod discoveries;
mod display;
mod doctor;
mod dom_capture;
mod drain;
mod error;
mod error_monitor; // Must be declared before error (error re-exports ErrorSeverity from error_monitor)
mod event_system;
mod execution_context;
mod execution_core;
mod executor;
mod exploration;
mod findings;
mod fixer;
// Row 2 Phase 1 (fleet topology + per-device budget). Detects local
// resources on startup and POSTs role + budget to `coord.devices`
// (was `coord.machines` pre-Phase-3-Unified-Devices-Registry) so
// `GET /coord/fleet` can answer "where do I have agent capacity?".
mod fleet;
mod fleet_commands;
mod flow_control;
mod follow_up;
mod fs_atomic;
mod git_status_subset;
// D5 Phase 1 — Git Supervision Channel. Consumes git/spec events from the
// existing `trigger_system` (via the `SupervisionProposal` action variant)
// and routes them into a bounded in-process ring buffer + Tauri event
// channel for the frontend supervision hook.
mod git_supervision;
// D4+D6 Blind-Spot Recommender (Phase 2): proactive enumeration of regions
// no live observer's scope covers, ranked by information value.
mod blind_spots;
mod graphql;
mod health_monitor;
mod heartbeat;
mod install_effects_producer;
mod instance;
mod instance_health;
mod instance_manager;
mod iteration_bundle;
#[cfg(windows)]
mod job_object;
mod knowledge_acquisition;
mod known_issues;
mod launch_env;
mod log_consolidation;
mod logging;
mod macros;
mod mcp;
mod mcp_api;
mod mcp_client;
mod mcp_embedded;
mod memory;
mod meta_optimizer;
mod middleware;
mod observer_registry;
mod online_learning;
mod orchestration_loop;
mod orchestration_loop_configs;
mod orchestrator;
mod otel;
mod paths;
mod planning_bridge;
mod playwright;
mod pm_detect;
mod process_capture;
mod process_helpers;
mod productivity;
mod prompt_snippets;
mod prompts;
mod rag;
mod recording;
mod reflection;
mod regression_api;
mod repo_detection;
mod restate;
mod rework;
mod routing;
mod runtime_env;
mod safe_lock;
mod saved_api_requests;
mod scenarios;
mod scheduler;
mod scheduler_service;
mod schema_registry;
mod screen;
mod sdk_features;
mod secure_storage;
mod security;
mod semantic_conventions;
mod server_mode;
mod session; // Plan 2026-05-22-coord-native-session-coordination Phase 2 — unified Session primitive
             // Hook-free, runner-side WIP-attribution capture (mirrors fleet::tree_publisher).
             // Reads each hosted session's transcript and POSTs file-edit attribution to
             // coord. Gated OFF by default (COORD_SESSION_ATTRIBUTION_ENABLED).
mod session_attribution;
mod settings;
// `startup_panic` is a minimal, dep-free panic-hook installer called from
// the very top of `main()` so early-init crashes (DB connect, Tauri builder,
// axum router construction) write a `runner-panic.log` the supervisor can
// pick up. Must come before any module that panics during static init.
mod skills;
mod slash_commands;
mod spawn_placement;
mod spec_api;
mod spec_experimentation;
mod spec_utils;
mod startup_panic;
mod state_discovery;
mod state_explorer;
mod state_machine_configs;
mod stats;
mod step_event_builder;
mod step_executor;
mod step_injection;
mod step_metadata;
mod step_output;
mod step_registry;
mod step_types;
mod steps;
mod storage;
pub(crate) mod str_utils;
mod summary_generator;
mod tauri_app_handle;
mod tauri_command_audit;
mod terminal;
mod test_executor;
mod test_orchestrator;
mod ticket_system;
mod tiered_info;
mod timeout_config;
mod trace_api;
mod tracing_layers;
mod trigger_system;
mod tunnel;
mod ui_bridge_evaluate;
mod ui_bridge_invoke;
mod ui_bridge_invoke_probe;
mod ui_bridge_plugin;
mod ui_error;
mod unified_ai_session;
mod unified_workflow_executor;
mod unified_workflows;
mod util;
mod validation;
mod verification;
mod vga;
mod video_recorder;
mod vision;
mod wake_handler; // Phase F.1 — qontinui:// custom-URL deep-link wake handler
mod win32_compat;
mod window_assignments;
mod window_manager;
mod window_placement;
mod workflow;
mod workflow_event_bus;
mod workflow_generation;
mod workflow_queue;
mod workflow_state;

// Stream E (Flywheel) Step 11 — end-to-end integration test module. The
// file-level `#![cfg(all(test, feature = "spec-authoring"))]` ensures it
// only compiles when running tests with the feature enabled.
#[cfg(test)]
mod flywheel_e2e_tests;

// Phase 9 of the runner tier-decoupling rollout — calibration matrix
// covering Tier 0 / 1 / 2 boundary invariants.
// See plans/2026-05-20-runner-tier-decoupling.md.
#[cfg(test)]
mod tier_matrix_tests;
mod worktree;
mod wrappers;
mod zombie_sweep;

use commands::AppState;
use display::profiles::ActionLogProfile;
use display::DisplayProcessor;
use doctor::{start_doctor_async, DoctorConfig};
use error_monitor::{start_error_monitor_async, ErrorMonitorConfig};
use logging::{init_logging, setup_panic_handler, LoggingConfig};
use std::sync::atomic::{AtomicBool, AtomicU16};
use std::sync::{Arc, Mutex};
use storage::LocalStorage;
use tauri::Manager;
use tiered_info::RunRecordingHandler;
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};
use video_recorder::VideoRecordingService;

fn main() {
    // Enable backtraces in crash dumps for better diagnostics
    std::env::set_var("RUST_BACKTRACE", "1");

    // Install the startup-panic hook FIRST, before any other setup. Panics
    // during early init (database connection, Tauri builder, axum router
    // construction) would otherwise vanish — the process exits with code 1
    // and the supervisor sees only that. This hook writes a one-file-per-boot
    // `runner-panic.log` the supervisor reads after a non-zero exit.
    startup_panic::install_startup_panic_hook();

    // Initialize lifecycle debugging BEFORE anything else
    debug_lifecycle::init_lifecycle_debug();

    // Per-monitor DPI awareness must be set before any screen capture
    // (Windows: PROCESS_PER_MONITOR_DPI_AWARE_V2). No-op on macOS/Linux.
    screen::ensure_dpi_awareness();

    let result = std::panic::catch_unwind(run_app);

    match result {
        Ok(Ok(())) => {
            info!("Application exited successfully");
            debug_lifecycle::log_exit("Normal exit", 0);
        }
        Ok(Err(e)) => {
            error!("Application error: {}", e);
            debug_lifecycle::log_exit(&format!("Application error: {}", e), 1);
            std::process::exit(1);
        }
        Err(panic) => {
            error!("Application panicked: {:?}", panic);
            debug_lifecycle::log_exit(&format!("Panic in main: {:?}", panic), 2);
            std::process::exit(2);
        }
    }
}

fn run_app() -> Result<(), Box<dyn std::error::Error>> {
    // Read every QONTINUI_* / WEBVIEW2_* env var that influences startup
    // exactly once. All downstream code reads from this snapshot via the
    // Tauri-managed `Arc<RunnerLaunchEnv>` instead of re-parsing env vars.
    // See `launch_env.rs` for the full list and rationale.
    let launch_env: launch_env::SharedLaunchEnv = Arc::new(launch_env::RunnerLaunchEnv::read());

    // Read persisted OTel settings so the tracing pipeline uses saved config
    let otel_config = crate::settings::get_otel_settings();
    let logging_result = init_logging(LoggingConfig {
        otel: otel_config,
        ..LoggingConfig::default()
    })?;
    setup_panic_handler();

    // Start health monitoring to detect resource exhaustion
    health_monitor::start_health_monitor();

    // Initialize Windows Job Object so spawned AI processes are auto-killed
    // if the runner crashes (safety net for the explicit taskkill in shutdown).
    #[cfg(windows)]
    job_object::init_job_object();

    info!("Starting Qontinui Runner v{}", env!("CARGO_PKG_VERSION"));

    // Initialize Sentry for crash reporting (release builds only).
    // The guard must live for the entire application lifetime — when it drops, Sentry shuts down.
    #[cfg(not(debug_assertions))]
    let _sentry_guard = std::env::var("SENTRY_DSN").ok().map(|dsn| {
        let guard = sentry::init((
            dsn,
            sentry::ClientOptions {
                release: sentry::release_name!(),
                environment: Some("beta".into()),
                before_send: Some(std::sync::Arc::new(|event| {
                    info!("Sending error to Sentry: {:?}", event);
                    Some(event)
                })),
                ..Default::default()
            },
        ));
        info!("Sentry crash reporting initialized");
        guard
    });

    // Initialize DisplayProcessor with ActionLogProfile
    let mut display_processor = DisplayProcessor::new();
    display_processor.register_profile(ActionLogProfile::with_default_config());
    let display_processor = Arc::new(TokioMutex::new(display_processor));

    // Initialize LocalStorage
    let local_storage = Arc::new(Mutex::new(
        LocalStorage::with_default_config().expect("Failed to initialize local storage"),
    ));

    // Initialize VideoRecordingService
    let video_recorder = Arc::new(Mutex::new(VideoRecordingService::new()));

    // Initialize PostgreSQL connection.
    //
    // Connection settings come from `~/.qontinui/profiles.json` per the
    // canonical-DB topology (see `tmp_canonical_db_topology_plan.md` §3 and
    // `crate::profiles`). Selection order: `QONTINUI_ENV` env var → file's
    // `active` field → `dev`. Falls back to legacy `RUNNER_DATABASE_URL`
    // env var (and ultimately a localhost default) when profiles.json is
    // missing, so unmigrated machines remain bootable.
    //
    // Uses a dedicated tokio runtime for the one-shot async connection —
    // cannot use tauri::async_runtime::block_on here because the Tauri
    // runtime hasn't started yet. PG pool connections are tied to their
    // creating runtime, so this same runtime stays alive for the rest of
    // the bootstrap path.
    let pg_db: Arc<crate::database::pg::PgDb> = {
        let profile = qontinui_runner_lib::profiles::load();
        info!(
            "Connecting to canonical PG via profile '{}'",
            profile.source
        );
        let pg_url = profile.database_url;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime for PG initialization");

        match rt.block_on(crate::database::pg::PgDb::new(&pg_url)) {
            Ok(pg) => {
                info!("PostgreSQL connected successfully");
                let pg = Arc::new(pg);
                crate::database::pg::PgDb::set_global(pg.clone());
                crate::database::pg::set_pg_available(true);

                // Phase 5 startup sweep (productivity-coordinator-completion-reports
                // §9 "Memory pressure audit"): clear any stale
                // `assignment_brief_extras` rows on tasks past `pending`.
                // Rule E populates the field for pending tasks; once the
                // task is past pending, the AssignTask hook should have
                // cleared it. Anything still set is a leak from a prior
                // crash or a path that bypassed the clear. One-shot,
                // idempotent — safe to run unconditionally.
                match rt.block_on(pg.clear_stale_assignment_brief_extras()) {
                    Ok(0) => {
                        // No leaks — common case after a clean shutdown.
                    }
                    Ok(n) => {
                        warn!(
                            "PG bootstrap: cleared {} stale assignment_brief_extras row(s) \
                             on non-pending tasks",
                            n
                        );
                    }
                    Err(e) => {
                        // Non-fatal — the sweep is a hardening backstop, not
                        // a load-bearing invariant. Log and continue.
                        warn!(
                            "PG bootstrap: clear_stale_assignment_brief_extras failed \
                             (non-fatal): {}",
                            e
                        );
                    }
                }

                // spec-multi-app Stream F.1: register dev apps (runner, web,
                // supervisor) on startup so the multi-tenant Spec API has
                // entries to serve. Gated on `QONTINUI_DEV_BOOTSTRAP=1` so
                // production runners do not register the developer's
                // hard-coded sibling-repo layout. Idempotent — failures
                // (including AlreadyRegistered) are swallowed.
                if let Err(e) = rt.block_on(crate::database::pg::apps::bootstrap_dev_apps(&pg)) {
                    warn!(
                        "PG bootstrap: bootstrap_dev_apps failed (non-fatal): {:?}",
                        e
                    );
                }
                pg
            }
            Err(e) => {
                // Degraded boot (QONTINUI_ALLOW_NO_DB): the runner normally
                // HARD-PANICS here so a misconfigured production runner surfaces
                // the broken DB immediately. When degraded boot is explicitly
                // enabled (CI contract smoke, offline demos, fast local UI
                // iteration, or riding out a transient PG blip), construct an
                // UNVERIFIED PgDb whose deadpool pool reconnects lazily, mark PG
                // unavailable so DB-backed routes return a clean 503, and
                // continue booting the Tauri/axum stack. Default (flag unset) =
                // the current fail-fast invariant — prod is unchanged.
                let degraded = std::env::var("QONTINUI_ALLOW_NO_DB")
                    .map(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes"))
                    .unwrap_or(false);
                if degraded {
                    warn!(
                        "PostgreSQL connection failed: {}. QONTINUI_ALLOW_NO_DB is set — \
                         booting DEGRADED: DB-backed routes return 503 until PG is reachable. \
                         DO NOT use this mode in production.",
                        e
                    );
                    match crate::database::pg::PgDb::new_degraded(&pg_url) {
                        Ok(pg) => {
                            let pg = Arc::new(pg);
                            crate::database::pg::PgDb::set_global(pg.clone());
                            crate::database::pg::set_pg_available(false);
                            pg
                        }
                        Err(build_err) => {
                            // Even building the pool failed — a genuine config
                            // error (unparseable URL), not mere unreachability.
                            // Fail fast regardless of the degraded flag.
                            error!(
                                "Degraded boot requested but PG pool could not be built: {}",
                                build_err
                            );
                            panic!("PostgreSQL pool construction failed — {}", build_err);
                        }
                    }
                } else {
                    error!(
                        "PostgreSQL connection failed: {}. Ensure docker-compose PG is running \
                         (or set QONTINUI_ALLOW_NO_DB=1 to boot DB-less in degraded mode).",
                        e
                    );
                    panic!("PostgreSQL connection required — {}", e);
                }
            }
        }
    };

    // SQLite span layer, queue recovery, JSON migration, seed rules, and
    // check-group repair removed — all persistence now via PgDb.

    // Initialize online learning singletons (model router bandit + drift monitor)
    {
        let ol_pg = pg_db.clone();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime for online learning init");
        rt.block_on(online_learning::initialize(&ol_pg));
    }

    // Row 2 Phase 1: publish this runner's DeviceBudget to coord.devices
    // (role = agent, max_concurrent_agents derived from RAM via §3.2 policy).
    // Best-effort — failures log a warning and the runner still boots.
    // Phase 3 (Unified Devices Registry): now POSTs to coord HTTP rather
    // than direct-PG UPSERT, with exponential-backoff retry preserving the
    // runner-bootable-when-coord-down property. See
    // `plans/2026-05-14-fleet-topology-and-build-pool-design.md` §3.2
    // and `fleet.rs::publish_on_startup`.
    //
    // STARTUP-RESILIENCE (§8): this call is `await`-ed NOT here on the
    // critical boot path, but inside the dedicated "fleet-publishers" thread
    // below (after the heartbeat block). `publish_on_startup` is already
    // fail-open at the value level, but on a coord 503 its exponential-backoff
    // retry ([2,4,8,16,32,60]s + 10s timeouts) blocks ~60-120s. Running it
    // synchronously here delayed `run_app()` from reaching the MCP `/health`
    // bind (downstream in `.setup()` → `mcp_api::start_server`), so the
    // supervisor health-probe killed the runner before it could come up
    // (the 2026-06 startup outage). Detaching it onto the publishers runtime
    // keeps coord-base latency entirely OFF the `/health`-bind critical path.
    // The clone of `pg_db` for that thread is taken here so the borrow is
    // resolved before `pg_db` is moved into later state.
    let fleet_publish_pg = pg_db.clone();

    // fleet heartbeat — see plan §5 and fleet.rs::spawn_heartbeat.
    //
    // Periodic HTTP POST `{device_id, hostname}` to coord's
    // `/coord/devices/register` (Phase 3 Unified Devices Registry; was
    // `/coord/machine/register`), refreshing `coord.devices.last_seen_at`
    // so coord's push-aware liveness ladder (plan 2026-05-18-push-aware-
    // fleet-liveness §4) recognizes this runner as alive even when the
    // inbound probe can't reach us (NAT/firewall asymmetry).
    //
    // We need a long-lived runtime here, but there is no ambient tokio
    // runtime at this point in `run_app` (Tauri's runtime is constructed
    // later in `tauri::Builder::run`). So park a dedicated OS thread
    // hosting a multi-thread runtime, kept alive forever via a `pending`
    // future. Same posture as the supervisor's interval-style background
    // tasks — a one-shot `block_on` is the wrong shape because the
    // heartbeat task lives for the runner's entire lifetime.
    // The heartbeat gets its OWN OS thread + single-thread runtime,
    // isolated from the publisher/census/reclaim/agent-runtime tasks
    // below. Those siblings walk dozens of git worktrees with
    // synchronous `git` subprocess calls and real-file stat sweeps,
    // which block their runtime's only worker for minutes at a time on
    // a busy machine. When all five tasks shared one worker thread, the
    // heartbeat starved into silent multi-minute stalls — the
    // 2026-06-03 fleet-liveness outage: the device aged out of
    // `/coord/fleet/state` (120s TTL vs 30s cadence) while the loop
    // neither ticked nor warned, on a freshly-built binary. A
    // current_thread runtime hosting ONLY the 30s heartbeat guarantees
    // fleet liveness can't be starved by sibling blocking work.
    std::thread::Builder::new()
        .name("fleet-heartbeat".to_string())
        .spawn(|| {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    warn!(
                        "fleet::heartbeat: failed to build dedicated tokio runtime ({e}). \
                         Skipping periodic heartbeat — runner still boots."
                    );
                    return;
                }
            };
            rt.block_on(async {
                fleet::spawn_heartbeat();
                // Park this thread's runtime forever so the spawned
                // interval task keeps ticking for the runner's lifetime.
                std::future::pending::<()>().await;
            });
        })
        .map(|_| ())
        .unwrap_or_else(|e| {
            warn!(
                "fleet::heartbeat: failed to spawn dedicated OS thread ({e}). \
                 Skipping periodic heartbeat — runner still boots."
            );
        });

    std::thread::Builder::new()
        .name("fleet-publishers".to_string())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .thread_name("fleet-pub-rt")
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    warn!(
                        "fleet::publishers: failed to build dedicated tokio runtime ({e}). \
                         Skipping tree publisher / census / reclaim / agent runtime — \
                         runner still boots."
                    );
                    return;
                }
            };
            rt.block_on(async {
                // STARTUP-RESILIENCE (§8): the one-shot DeviceBudget publish,
                // moved OFF the boot critical path (see the `fleet_publish_pg`
                // comment up by the PG bootstrap). It's `await`-ed here on the
                // publishers runtime so a coord 503 (its ~60-120s backoff
                // ladder) can no longer delay `run_app()` reaching the MCP
                // `/health` bind. Args are unchanged from the original
                // synchronous call (`&fleet_pg`, `MachineRole::Agent`).
                // Nothing downstream depends on its completion — it's a
                // best-effort registry UPSERT; the periodic publishers /
                // heartbeat below re-assert this device's presence regardless.
                // It runs FIRST in this block so the initial registration
                // still happens promptly once coord is reachable, before the
                // periodic publishers begin their cadences.
                fleet::publish_on_startup(&fleet_publish_pg, fleet::MachineRole::Agent).await;

                // Fleet auto-response rules: arm from the on-disk cache first
                // (so a session that hit the transient rate-limit message during
                // a restart is recovered before the first network fetch), then
                // start the periodic fetch loop that refreshes the rule set from
                // the qontinui-web backend. Best-effort: a coord/web outage
                // leaves the cached rules in place. See
                // `terminal::auto_response_fleet`.
                terminal::auto_response_fleet::reload_from_cache_at_boot();
                terminal::auto_response_fleet::spawn_fetch_loop();

                // Fleet auto-response matcher: a debounced poll of every live
                // terminal's RENDERED VT grid. Scanning the rendered screen
                // (not the raw byte stream) is what lets the engine see the
                // rate-limit error inside a full-screen TUI like Claude Code,
                // which paints each whole frame as one synchronized-output
                // update; on a fresh on-screen match it submits the rule's
                // prompt into that session after a per-rule backoff. See
                // `terminal::auto_response`.
                terminal::auto_response::spawn_grid_scan_loop();

                // Usage-limit watcher: the same grid-scan approach for the
                // Claude CLI's token-exhaustion messages. A debounced poll of
                // every live terminal's rendered screen hands a (probe-guarded)
                // hint to `account_migration` when a session's account is
                // exhausted. Scanning the grid (not the raw byte stream) is what
                // lets it see the limit message inside a full-screen TUI. See
                // `terminal::usage_limit`.
                terminal::usage_limit::spawn_grid_scan_loop();

                // Plan 2026-05-19-coordinator-production-readiness.md
                // Phase 1 — periodic primary-tree state publisher. These
                // four tasks share one worker thread; they're all
                // best-effort publishers/pollers that tolerate delaying
                // each other (unlike the heartbeat above, which feeds
                // coord's 120s liveness TTL and must never be starved).
                fleet::spawn_tree_publisher();
                // Hook-free, runner-side WIP-attribution capture (sibling of
                // the tree publisher above). Reads each hosted session's Claude
                // transcript forward-only and POSTs file-edit attribution to
                // coord's /coord/wip-attribution. Gated OFF by default
                // (COORD_SESSION_ATTRIBUTION_ENABLED) — no live consumer yet, so
                // it's a no-op until armed. See `session_attribution`.
                session_attribution::spawn_session_attribution();
                // Ξ_Worktree census (Phase 1) — periodic disk-footprint
                // + junction-status + volume-free-space census of every
                // on-disk git worktree, POSTed to
                // `/agents/<device_id>/worktree-census`. coord can't see
                // the operator's Windows disk; the runner is the only
                // vantage point. Default 300s cadence (env override
                // QONTINUI_WORKTREE_CENSUS_INTERVAL_SECS) — an order of
                // magnitude slower than the heartbeat/tree-publisher
                // because each tick stats real files under
                // node_modules/target (junctions are skipped, so a
                // junctioned 165GB target costs ~0).
                agent_worktree::census::spawn_census();
                // Ξ_Worktree reclaim (Phase 4) — periodically pull
                // coord's pending per-device reclaim instructions and
                // execute the INV-W4 safe path (unlink junctions FIRST,
                // then remove the worktree; or recreate a drifted
                // junction). Arming is per-action: rejunction_armed /
                // remove_armed both default OFF (the poller only LOGS what
                // it would do); coord arms remove via
                // COORD_WORKTREE_RECLAIM_ENABLED and graduates rejunction
                // to default-on once the G6 build-guard is proven.
                // Default 300s cadence (env
                // QONTINUI_WORKTREE_RECLAIM_INTERVAL_SECS). Same machine-
                // wide, anonymous, device-keyed posture as the census.
                agent_worktree::reclaim::spawn_reclaim();
                // Scheduled-maintenance executor (Phase 1, plan
                // 2026-06-08-coord-scheduled-maintenance-subsystem) — a
                // sibling of the reclaim pull-loop. Periodically pulls
                // coord's pending per-device maintenance instructions
                // (GET /coord/maintenance/instructions/:device_id) and
                // executes the `checkout_main_ff_pull` branch-reset action,
                // re-checking the safety floor (clean + same branch +
                // ancestor-of-origin-default) on the live disk first.
                // Per-instruction `armed` defaults OFF → log-only dry-run
                // ("[maintenance dry-run] would reset …"); coord arms it
                // under COORD_MAINTENANCE_ENABLED. A real reset reports via
                // the git_ops record path; ack is the next census. Default
                // 300s cadence (env QONTINUI_MAINTENANCE_INTERVAL_SECS).
                // Same machine-wide, anonymous, device-keyed identity as
                // the reclaim poller (census::{load_device_id,coord_http_base}).
                agent_worktree::maintenance_executor::spawn_maintenance();
                // Ξ_FS backstop (Phase 5) — a defense-in-depth DETECTOR for
                // edits that leaked OUTSIDE any session worktree. Periodically
                // scans the SHARED canonical checkouts; alarms (POST
                // /coord/fs/observations with source=canonical_drift) on a
                // checkout that is dirty AND not covered by any live
                // kind=worktree claim. Tier-1 surface/attribute only — never
                // mutates the working tree. Gated OFF by default
                // (QONTINUI_FS_BACKSTOP_ENABLED); best-effort, never blocking.
                // Default 300s cadence (QONTINUI_FS_BACKSTOP_INTERVAL_SECS).
                agent_worktree::fs_backstop::spawn_backstop();
                // Phase 4 — coord-driven Claude Code subprocess runtime.
                // Subscribes to `events.agent.spawn_requested.<device_id>`
                // on coord WS and supervises the resulting `claude` CLI
                // child processes. No-op when no profile is configured.
                agent_runtime::spawn_runtime();
                // Park this thread's runtime forever so the spawned
                // interval task keeps ticking for the runner's lifetime.
                std::future::pending::<()>().await;
            });
        })
        .map(|_| ())
        .unwrap_or_else(|e| {
            warn!(
                "fleet::heartbeat: failed to spawn dedicated OS thread ({e}). \
                 Skipping periodic heartbeat — runner still boots."
            );
        });

    // Migrate plaintext API keys to secure keychain storage
    if let Err(e) = config_facade::migrate_api_keys_to_keychain() {
        warn!("API key migration to keychain failed (non-fatal): {}", e);
    }

    // Architecture spec caching removed — all persistence now via PgDb.
    // Initialize RAGState (graceful degradation if dependencies missing)
    let rag_state = match commands::rag::RAGState::new() {
        Ok(state) => {
            info!("RAG state initialized successfully");
            Arc::new(state)
        }
        Err(e) => {
            warn!(
                "RAG initialization failed (non-fatal): {}. RAG features will be disabled.",
                e
            );
            // Create a degraded RAGState that will return errors on use
            Arc::new(commands::rag::RAGState::new_degraded())
        }
    };

    // Create broadcast channel for WebSocket event streaming
    // Capacity of 256 allows for burst events without dropping
    let (event_broadcast, _) = tokio::sync::broadcast::channel::<serde_json::Value>(256);

    // Touch-events broadcast channel for the Rust deconflicter loop
    // (§4.1 of plans/2026-05-13-coord-as-deconflicter-plan.md). The sender
    // is stashed in AppState so `claude_session::dispatcher::auto_register_file`
    // can fire on every Edit/Write; the receiver is consumed by
    // `DeconflicterLoop::start` after the Tauri app handle is available.
    // Capacity 256 mirrors the WebSocket channel — drop-on-full is fine
    // because the deconflicter is a soft advisor (missed touches degrade
    // gracefully: the next touch on the same path re-triggers).
    let (touch_events_tx, touch_events_rx) =
        tokio::sync::broadcast::channel::<crate::coordinator::deconflicter::TouchEvent>(256);

    // Create run recording handler for automatic workflow execution recording
    let run_recording_handler = Arc::new(RunRecordingHandler::new());

    // Create MCP client manager for calling external MCP servers
    let mcp_client_manager = mcp_client::McpClientManager::new();

    // Create instance manager for multi-instance dev workflows
    let instance_manager = Arc::new(instance_manager::InstanceManager::new(pg_db.clone()));

    // Create session manager for interactive Claude CLI sessions
    let session_manager = Arc::new(claude_session::SessionManager::new());

    // Create terminal manager for embedded PTY terminals.
    // Plan 2026-05-22-coord-native-session-coordination Phase 2 — the unified
    // Session primitive's PTY/Claude-CLI transports reach this manager via
    // `app.state::<Arc<TerminalManager>>()` inside `.setup(...)` so it stays
    // shared with the legacy `terminal_*` Tauri commands until Phase 9
    // collapses both paths.
    let terminal_manager = Arc::new(terminal::TerminalManager::new());

    // Create shared AppState for both Tauri and MCP API
    // Create shared SDK connection for UI Bridge (shared between AppState and ApiState)
    let shared_sdk_connection = Arc::new(TokioMutex::new(
        crate::mcp::sdk_client::SdkConnectionManager::new(),
    ));

    // Headless window control — `QONTINUI_SERVER_MODE=1` now ONLY governs
    // whether the main window is created and whether Restate is auto-enabled.
    // Web-backend integration (Phase 3G) is driven entirely by the
    // persisted `WebIntegrationSettings` (with env-var overlay support in
    // `settings::load_settings`). A desktop runner can register with web
    // via the Settings UI; a headless deploy can set
    // `QONTINUI_WEB_BACKEND_URL` + `QONTINUI_RUNNER_TOKEN` and get the same
    // behavior without touching settings.
    let server_mode_is_on = launch_env.server_mode;
    let web_integration_settings = crate::settings::load_settings().web_integration.clone();
    let initial_server_mode_state: Option<crate::server_mode::ServerModeState> =
        crate::server_mode::ServerModeConfig::from_settings(&web_integration_settings).map(|cfg| {
            info!(
                "Web-backend integration enabled (backend={})",
                cfg.web_backend_url
            );
            crate::server_mode::ServerModeState::new(cfg)
        });
    if initial_server_mode_state.is_none() && !web_integration_settings.backend_url.is_empty() {
        warn!(
            "Web-integration not active (enabled={}, backend_url_empty={}) — phase events and heartbeats will NOT be reported until it is enabled with a backend URL. (A runner_token is no longer required; the relay authenticates with the device JWT.)",
            web_integration_settings.enabled,
            web_integration_settings.backend_url.is_empty(),
        );
    }
    let server_mode_state: Arc<tokio::sync::RwLock<Option<crate::server_mode::ServerModeState>>> =
        Arc::new(tokio::sync::RwLock::new(initial_server_mode_state));

    let shared_app_state = Arc::new(AppState {
        bridge_manager: TokioMutex::new(None), // Initialized in setup() when app_handle is available
        extraction_executor: Mutex::new(None), // Initialized on-demand
        sdk_connection: shared_sdk_connection.clone(),
        exploration_cancel: Arc::new(TokioMutex::new(None)),
        current_config: Mutex::new(None),
        display_processor,
        local_storage,
        video_recorder,
        event_broadcast,
        pg_db,
        run_recording_handler,
        mcp_client_manager: tokio::sync::Mutex::new(mcp_client_manager),
        error_monitor_handle: TokioMutex::new(None), // Initialized in setup()
        doctor_handle: TokioMutex::new(None),        // Initialized in setup()
        url_lock_manager: Arc::new(crate::executor::UrlLockManager::new()),
        file_registry_manager: Arc::new(crate::executor::FileRegistryManager::new()),
        upcoming_file_registry: Arc::new(
            crate::executor::upcoming_file_registry::UpcomingFileRegistry::new(),
        ),
        file_lock_manager: Arc::new(crate::executor::FileLockManager::new()),
        touch_events_tx: touch_events_tx.clone(),
        ui_bridge_failure_tracker:
            crate::step_executor::handlers::ui_bridge::UiBridgeFailureTracker::new(),
        process_capture_manager: TokioMutex::new(None), // Initialized in setup()
        api_ready: AtomicBool::new(false),              // Set when MCP API server binds
        frontend_ready: AtomicBool::new(false), // Set on first successful UI Bridge IPC response
        api_port: AtomicU16::new(crate::mcp::types::get_mcp_api_port()), // Updated when server binds
        api_lan_bound: AtomicBool::new(false), // Derived from the actual bound address when server binds
        ai_pid_tracker: Arc::new(std::sync::Mutex::new(Vec::new())),
        canvas_state: Arc::new(tokio::sync::RwLock::new(
            crate::mcp::canvas::CanvasState::new(),
        )),
        orchestration_loops: std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::orchestration_loop::loop_engine::MultiLoopManager::new(),
        )),
        container_executor: TokioMutex::new(None), // Initialized via container settings when enabled
        run_cost_trackers: TokioMutex::new(std::collections::HashMap::new()),
        working_representation_cache: {
            let cache =
                Arc::new(crate::memory::working_representation::WorkingRepresentationCache::new());
            crate::memory::working_representation::WorkingRepresentationCache::set_global(
                cache.clone(),
            );
            cache
        },
        server_mode: server_mode_state.clone(),
        ui_error: Arc::new(ui_error::UiErrorState::new()),
        crash_dumps: Arc::new(crash_dumps::CrashDumpState::new()),
        usb_transport: Arc::new(tokio::sync::OnceCell::new()),
        app_registry: Arc::new(tokio::sync::OnceCell::new()),
        app_dispatcher: Arc::new(tokio::sync::OnceCell::new()),
        wrapper_state: Arc::new(tokio::sync::OnceCell::new()),
        prehydration_cache: Arc::new(
            crate::unified_workflow_executor::prehydration::PrehydrationCache::new(),
        ),
    });
    let mcp_app_state = shared_app_state.clone();
    let mcp_rag_state = rag_state.clone();
    let heartbeat_app_state = shared_app_state.clone();
    let crash_dump_app_state = shared_app_state.clone();

    // Workstream C (Move 2B): compartment wrappers around Arc<AppState> so
    // per-module plugins can migrate from `State<Arc<AppState>>` to a scoped
    // `State<<Compartment>>`. Both forms coexist during the gradual migration.
    let bridge_compartment = commands::compartments::BridgeCompartment(shared_app_state.clone());
    let execution_compartment =
        commands::compartments::ExecutionCompartment(shared_app_state.clone());
    let integration_compartment =
        commands::compartments::IntegrationCompartment(shared_app_state.clone());
    let health_compartment = commands::compartments::HealthCompartment(shared_app_state.clone());
    let storage_compartment = commands::compartments::StorageCompartment(shared_app_state.clone());

    // Frontend log-sync store. Holds the most recent batches of logs the React
    // UI mirrors back into Rust state for HTTP API consumption. Created here
    // so it can be both `.manage()`d for the Tauri commands in
    // `commands::log_api` and exposed to the MCP HTTP server later.
    let log_api_store = commands::log_api::create_log_store();

    // Create error monitor config for later initialization
    let error_monitor_pg = shared_app_state.pg_db.clone();

    // Secondary instances (spawned by InstanceManager) must NOT use single-instance
    // plugin — it would prevent them from starting since they share the same binary.
    let is_secondary_instance = instance::is_secondary();

    let mut builder = tauri::Builder::default();

    if !is_secondary_instance {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // When a second instance is launched, focus the existing window
            if let Some(window) =
                app.get_webview_window(qontinui_runner_lib::get_main_window_label())
            {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
            // Phase F.1 — single-instance plugin's `deep-link` feature flag
            // forwards `qontinui://...` URLs from the secondary launch's argv
            // to the running primary so the wake handler runs in the live
            // process instead of starting a duplicate runner.
            wake_handler::handle_args_from_secondary_instance(app.clone(), &args);
        }));
    }

    // Skip the window-state plugin for secondary instances. The plugin
    // persists `window-state.json` to a path derived from the bundle
    // identifier, which is shared by every spawned instance — so a saved
    // off-screen / minimized geometry from any prior session would be
    // restored for fresh test runners. WebView2 throttles JS execution for
    // off-screen / minimized windows, which is one of the failure modes
    // that made `spawn-test` produce runners whose UI Bridge snapshot
    // hung at "Frontend did not become ready within 10s".
    if !is_secondary_instance {
        builder = builder.plugin(tauri_plugin_window_state::Builder::default().build());
    }

    let app = builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        // Phase F.1 — Wake-from-web custom URL scheme (`qontinui://wake?...`)
        // and launch-on-system-startup toggle. Deep-link plugin must come
        // before `.setup(...)` so `app.deep_link().on_open_url(...)` is
        // available in the setup closure. Autostart's MacosLauncher kind is
        // platform-conditional but the API is identical on Windows/Linux.
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(ui_bridge_plugin::init())
        // All other in-app Tauri commands are registered via the central
        // `generate_handler!` below rather than per-module plugins.
        //
        // Rationale: the 90-module plugin split (Workstream B of
        // REFACTOR_WAVE_3_PLAN) compiled clean but broke runtime IPC. Tauri 2
        // plugin commands are invokable only as `plugin:<name>|<cmd>` — the
        // frontend's hundreds of bare `invoke("<cmd>")` calls therefore failed
        // with "Command not found", stranding the app on "Starting API
        // server...". Tauri 2 identifier rules also reject the `qontinui_*`
        // plugin names outright (underscores disallowed in capability
        // prefixes), so the plugin shape can't be salvaged without a
        // cross-cutting rename.
        //
        // Keeping everything app-level lets the existing frontend surface work
        // unchanged, and Tauri skips the ACL check on bare commands when no
        // AppManifest is defined (`tauri-2.10.3/src/webview/mod.rs:1801-1806`).
        //
        // `ui_bridge_plugin` remains the one exception — its handler fns are
        // non-`pub` and the frontend talks to them only via HTTP, not IPC.
        //
        // The per-module `plugin()` fns are left in place for future reuse
        // even though nothing invokes them now.
        //
        // 2026-05-21 audit-trail shim — every IPC dispatch is observed once
        // before being forwarded to the macro-generated handler. We record
        // *only* the command name and a unix-millis timestamp into the bounded
        // ring buffer at `crate::tauri_command_audit`. No args, no return
        // values; the security argument for keeping credential-returning
        // commands (e.g. `get_test_auto_login`) off the UI Bridge invoke
        // allowlist is preserved — operators only learn that a command with
        // that name fired at some moment, surfaced via the runner-only
        // `GET /ui-bridge/control/tauri-command-history` endpoint
        // (see `mcp/ui_bridge/tauri_audit.rs`).
        //
        // The `generate_handler!` macro expands to `move |invoke| { … }` of
        // exact type `Fn(tauri::Invoke<tauri::Wry>) -> bool` (see
        // `tauri-macros-2.6.0/src/command/handler.rs::From<Handler>`), which
        // matches `Builder::invoke_handler`'s `F: Fn(Invoke<R>) -> bool + Send
        // + Sync + 'static` bound. Binding it to a local lets us wrap with a
        // pre-hook without forking the macro or losing per-command ACL
        // matching.
        .invoke_handler({
            // Type-annotate the binding so Rust can resolve the macro's
            // untyped closure parameter (`move |invoke| { ... }`) — without
            // the explicit `impl Fn(...) -> bool` ascription the call to
            // `invoke.message.command()` is ambiguous.
            let inner: Box<dyn Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync> =
                Box::new(tauri::generate_handler![
            commands::accessibility::a11y_ai_context,
            commands::accessibility::a11y_capture,
            commands::accessibility::a11y_click,
            commands::accessibility::a11y_connect,
            commands::accessibility::a11y_disconnect,
            commands::accessibility::a11y_focus,
            commands::accessibility::a11y_query,
            commands::accessibility::a11y_type_text,
            commands::accessibility::check_chrome_available,
            commands::accessibility::get_accessibility_settings,
            commands::accessibility::launch_chrome_debug,
            commands::accessibility::save_accessibility_settings,
            commands::activity_timeline::delete_activity_timeline_entry,
            commands::activity_timeline::get_activity_timeline_entry,
            commands::activity_timeline::get_activity_timeline_for_task_run,
            commands::activity_timeline::get_activity_timeline_range,
            commands::activity_timeline::get_activity_timeline_stats,
            commands::activity_timeline::get_scripted_output_stats,
            commands::activity_timeline::get_oneshot_stats,
            commands::activity_timeline::insert_activity_entry,
            commands::activity_timeline::search_activity_timeline,
            commands::activity_timeline::search_activity_timeline_filtered,
            commands::adaptive_learning::delete_curated_example,
            commands::adaptive_learning::delete_playbook_entry,
            commands::adaptive_learning::get_adaptive_learning_stats,
            commands::adaptive_learning::get_curated_examples,
            commands::adaptive_learning::get_gepa_run_detail,
            commands::adaptive_learning::get_gepa_runs,
            commands::adaptive_learning::get_learning_trends,
            commands::adaptive_learning::get_playbook_entries,
            commands::adaptive_learning::get_playbook_entry_detail,
            commands::adaptive_learning::get_template_lifecycle_history,
            commands::adaptive_learning::get_template_performance,
            commands::adaptive_learning::update_playbook_entry_status,
            commands::agentic_metrics::get_agentic_metric_aggregates,
            commands::agentic_metrics::get_agentic_scores,
            commands::agentic_metrics::get_composite_score_trend,
            commands::agentic_metrics::push_agentic_scores_to_backend,
            commands::agentic_metrics::push_latest_agentic_scores,
            commands::agentic_metrics::recompute_agentic_baselines,
            commands::ai_data::get_ai_prompts_for_viewer,
            commands::ai_data::get_consolidated_ai_output,
            commands::ai_data::get_contexts_for_viewer,
            commands::ai_data::get_jsonl_logs_summary,
            commands::ai_data::get_loaded_config_for_viewer,
            commands::ai_data::get_screenshots_for_viewer,
            commands::ai_data::get_task_run_api_requests_from_db,
            commands::ai_data::get_task_run_awas_steps_from_db,
            commands::ai_data::get_task_run_context,
            commands::ai_data::get_task_run_events_from_db,
            commands::ai_data::get_task_run_for_viewer,
            commands::ai_data::get_task_run_migrated_logs_summary,
            commands::ai_data::get_task_run_playwright_results_from_db,
            commands::ai_data::get_task_run_screenshots_from_db,
            commands::ai_data::get_task_run_verification_results_from_db,
            commands::ai_data::get_task_runs_for_viewer,
            commands::ai_data::get_text_logs_summary,
            commands::ai_data::read_jsonl_logs_for_task_run,
            commands::ai_data::read_jsonl_logs_for_viewer,
            commands::ai_data::read_text_logs_for_viewer,
            commands::ai_data::reopen_task_run,
            commands::ai_generation::explore_flow_step,
            commands::ai_generation::generate_api_request_with_ai,
            commands::ai_generation::generate_context_with_ai,
            commands::ai_generation::generate_element_ai_description,
            commands::ai_generation::generate_task_prompt_with_ai,
            commands::ai_generation::generate_test_and_agentic_step,
            commands::ai_generation::suggest_exploration_strategy_with_ai,
            commands::ai_session::close_ai_session,
            commands::ai_session::commit_session_progress,
            commands::ai_session::create_ai_session,
            commands::ai_session::generate_workflow_from_session,
            commands::ai_session::get_ai_output,
            commands::ai_session::get_ai_session_state,
            commands::ai_session::get_session_commit_state,
            commands::ai_session::interrupt_ai_session,
            commands::ai_session::list_ai_sessions,
            commands::ai_session::promote_session_to_worktree,
            commands::ai_session::recent_session_touched_files,
            commands::ai_session::rename_ai_session,
            commands::ai_session::send_user_message,
            commands::ai_settings::check_accounts_usage,
            commands::ai_settings::check_claude_cli_auth,
            commands::ai_settings::delete_ai_api_key_command,
            commands::ai_settings::get_agentic_settings,
            commands::ai_settings::get_ai_settings,
            commands::ai_settings::get_claude_accounts,
            commands::ai_settings::get_provider_circuit_states,
            commands::ai_settings::get_wsv_settings,
            commands::ai_settings::has_ai_api_key,
            commands::ai_settings::list_wsv_disagreements,
            commands::ai_settings::refresh_claude_cli_auth,
            commands::ai_settings::reset_provider_circuit,
            commands::ai_settings::save_agentic_settings,
            commands::ai_settings::save_ai_api_key_command,
            commands::ai_settings::save_ai_settings,
            commands::ai_settings::save_gemini_settings,
            commands::ai_settings::save_wsv_settings,
            commands::ai_settings::switch_claude_account,
            commands::ai_settings::test_ai_connection,
            commands::ai_settings::test_wsv_connection,
            commands::auth::check_auth_status,
            commands::auth::device_jwt_present,
            commands::auth::get_access_token_for_websocket,
            commands::auth::get_coord_device_token,
            commands::auth::get_api_port,
            commands::auth::get_device_info,
            commands::auth::get_user_projects,
            commands::auth::is_api_ready,
            commands::auth::get_runner_tier,
            commands::auth::kick_device_jwt_refresher_cmd,
            commands::auth::logout,
            commands::auth::qontinui_sign_out,
            commands::auth::set_runner_tier,
            commands::auth::cognito_sign_in,
            commands::auth::cognito_sign_in_password,
            commands::autostart::get_autostart_enabled,
            commands::autostart::set_autostart_enabled,
            commands::backup::export_all_data,
            commands::backup::get_export_summary,
            commands::backup::get_import_preview,
            commands::backup::import_all_data,
            commands::build_id::get_build_id,
            commands::checkpoint_browser::add_sample_checkpoints,
            commands::checkpoint_browser::clear_all_checkpoints,
            commands::checkpoint_browser::compare_orchestrator_checkpoints,
            commands::checkpoint_browser::complete_replay_session,
            commands::checkpoint_browser::create_orchestrator_checkpoint,
            commands::checkpoint_browser::delete_orchestrator_checkpoint,
            commands::checkpoint_browser::fail_replay_session,
            commands::checkpoint_browser::find_checkpoints_by_tag,
            commands::checkpoint_browser::get_checkpoint_count,
            commands::checkpoint_browser::get_checkpoint_stats,
            commands::checkpoint_browser::get_checkpoint_task_ids,
            commands::checkpoint_browser::get_checkpoints_count,
            commands::checkpoint_browser::get_checkpoints_filtered,
            commands::checkpoint_browser::get_checkpoints_paginated,
            commands::checkpoint_browser::get_latest_checkpoint,
            commands::checkpoint_browser::get_orchestrator_checkpoint,
            commands::checkpoint_browser::get_replay_lineage,
            commands::checkpoint_browser::get_task_lineage_info,
            commands::checkpoint_browser::list_active_replay_sessions,
            commands::checkpoint_browser::list_orchestrator_checkpoints,
            commands::checkpoint_browser::register_task_for_lineage,
            commands::checkpoint_browser::replay_from_checkpoint,
            commands::checkpoint_browser::start_replay_session,
            commands::checkpoints::checkpoint_delete,
            commands::checkpoints::checkpoint_get,
            commands::checkpoints::checkpoint_history,
            commands::checkpoints::checkpoint_list_active,
            commands::checkpoints::checkpoint_save,
            commands::checkpoints::checkpoint_status,
            commands::checkpoints::session_create,
            commands::checkpoints::session_update_status,
            commands::checkpoints::setting_get,
            commands::checkpoints::setting_set,
            commands::checkpoints::settings_get_all,
            commands::checks::create_check,
            commands::checks::create_check_group,
            commands::checks::delete_check,
            commands::checks::delete_check_group,
            commands::checks::detect_project_check_suggestions,
            commands::checks::execute_check_by_id,
            commands::checks::execute_check_group,
            commands::checks::execute_code_check,
            commands::checks::execute_code_check_suite,
            commands::checks::get_check,
            commands::checks::get_check_group,
            commands::checks::get_check_results,
            commands::checks::get_check_tool_info,
            commands::checks::get_checks_in_group,
            commands::checks::list_check_groups,
            commands::checks::list_checks,
            commands::checks::repair_check_group_associations,
            commands::checks::set_checks_in_group,
            commands::checks::update_check,
            commands::checks::update_check_group,
            commands::chunk_labels::delete_chunk_label,
            commands::chunk_labels::list_chunk_labels,
            commands::chunk_labels::upsert_chunk_label,
            commands::claims::claims_acquire,
            commands::claims::claims_release,
            commands::claims::claims_steal,
            commands::clipboard::share_file_to_mobile,
            commands::clipboard::share_to_mobile,
            commands::command_interpreter::command_interpret,
            commands::comparison::get_comparison_status,
            commands::comparison::list_comparisons,
            commands::comparison::start_comparison,
            commands::config::get_auto_load_last_config,
            commands::config::get_claude_account_launch_commands,
            commands::config::get_claude_config_dirs,
            commands::config::get_current_configuration,
            commands::config::get_include_summary_step_by_default,
            commands::config::get_last_config_path,
            commands::config::get_workspace_paths,
            commands::config::load_configuration,
            commands::config::save_auto_load_last_config,
            commands::config::save_claude_account_launch_commands,
            commands::config::save_claude_config_dirs,
            commands::config::save_include_summary_step_by_default,
            commands::config::save_last_monitor_index,
            commands::config::save_last_monitor_indices,
            commands::config::save_last_workflow_id,
            commands::container_settings::check_docker_status,
            commands::container_settings::get_container_settings,
            commands::container_settings::update_container_settings,
            commands::context::create_context,
            commands::context::delete_context,
            commands::context::evaluate_auto_include,
            commands::context::get_all_contexts,
            commands::context::get_builtin_contexts_cmd,
            commands::context::get_context,
            commands::context::get_context_categories,
            commands::context::record_context_usage,
            commands::context::search_contexts,
            commands::context::set_context_enabled,
            commands::context::update_context,
            commands::cost_dashboard::get_active_budget_status,
            commands::cost_dashboard::get_cost_dashboard,
            crash_dumps::dismiss_recent_crash,
            coord_doctor_cmd::coord_doctor_run,
            coord_doctor_cmd::coord_doctor_text,
            commands::dag_workflows::export_dag_workflow,
            commands::dag_workflows::import_dag_workflow,
            commands::dag_workflows::import_dag_workflows_from_project,
            commands::dag_workflows::respond_dag_approval,
            commands::dag_workflows::validate_dag_workflow,
            commands::database::explain_query_plan,
            commands::database::get_database_stats,
            commands::database::optimize_database,
            commands::dataset::package_dataset,
            commands::dataset::scan_local_images,
            commands::debug::get_debug_settings,
            commands::debug::set_debug_settings,
            commands::dev_findings::dev_seed_finding,
            commands::dev_findings::is_dev_endpoints_enabled,
            commands::discoveries::clear_discovery,
            commands::discoveries::clear_failed_discoveries,
            commands::discoveries::get_discovery_summary,
            commands::discoveries::get_discovery_sync_status,
            commands::discoveries::get_pending_discoveries_cmd,
            commands::discoveries::sync_discoveries,
            doctor::commands::doctor_get_status,
            doctor::commands::stop_process_by_pid,
            commands::durable_execution::get_iteration_commits,
            commands::durable_execution::get_iteration_diffs,
            commands::durable_execution::get_phase_results,
            commands::durable_execution::list_replay_points,
            commands::durable_execution::replay_workflow,
            commands::durable_execution::rollback_workflow_to_iteration,
            error_monitor::commands::acknowledge_all_errors,
            error_monitor::commands::acknowledge_error,
            error_monitor::commands::get_debug_context,
            error_monitor::commands::get_debug_context_for_ai,
            error_monitor::commands::get_error_event,
            error_monitor::commands::get_error_recurrence_history,
            error_monitor::commands::get_error_summary,
            error_monitor::commands::get_recent_errors,
            error_monitor::commands::get_unresolved_errors,
            error_monitor::commands::has_actionable_errors,
            error_monitor::commands::ignore_error,
            error_monitor::commands::link_error_to_finding,
            error_monitor::commands::open_error_in_editor,
            error_monitor::commands::query_error_events,
            error_monitor::commands::resolve_error,
            error_monitor::commands::search_errors,
            error_monitor::commands::update_error_status,
            error_monitor::workflow::check_fixable_errors,
            commands::event_search::search_events,
            commands::execution::bridge_execution::get_bridge_info,
            commands::execution::bridge_execution::list_bridges,
            commands::execution::bridge_execution::run_workflow_on_bridge,
            commands::execution::bridge_execution::transfer_gui_lock,
            commands::execution::executor_status::get_executor_status,
            commands::execution::executor_status::get_input_validation_status,
            commands::execution::executor_status::get_monitors,
            commands::execution::executor_status::set_input_capture_enabled,
            commands::execution::python_executor::start_python_executor,
            commands::execution::python_executor::stop_python_executor,
            commands::execution::python_executor::update_capture_settings,
            commands::execution::system_ops::check_for_updates,
            commands::execution::system_ops::handle_error,
            commands::execution::system_ops::install_update,
            commands::execution::system_ops::open_folder,
            commands::execution::workflow_execution::get_resolved_initial_states,
            commands::execution::workflow_execution::get_workflow_required_screens,
            commands::execution::workflow_execution::pause_execution,
            commands::execution::workflow_execution::resume_execution,
            commands::execution::workflow_execution::start_execution,
            commands::execution::workflow_execution::stop_execution,
            commands::execution_reporting::complete_execution_run,
            commands::execution_reporting::create_execution_run,
            commands::execution_reporting::report_action_executions,
            commands::execution_reporting::report_execution_issues,
            commands::execution_reporting::upload_execution_screenshot,
            commands::execution_variables::get_execution_variables_settings,
            commands::execution_variables::get_resolved_execution_context,
            commands::execution_variables::save_execution_variables_settings,
            commands::execution_variables::test_env_var,
            commands::federation::get_federation_reports,
            commands::extraction::create_extraction_session,
            commands::extraction::export_state_structure,
            commands::extraction::export_training_data,
            commands::extraction::get_extraction_status,
            commands::extraction::get_project_extractions,
            commands::extraction::list_extractions,
            commands::extraction::request_extraction_screenshot,
            commands::extraction::start_vision_extraction,
            commands::extraction::start_web_extraction,
            commands::extraction::stop_web_extraction,
            commands::extraction::update_extraction_session,
            commands::extraction::upload_extraction_annotations,
            commands::extraction::upload_state_structure,
            commands::file_browser::browse_directory,
            commands::file_browser::get_safe_browse_roots,
            commands::file_browser::read_file_content,
            commands::findings::get_finding_by_id,
            commands::findings::get_findings_by_status_cmd,
            commands::findings::get_findings_summary,
            commands::findings::get_task_findings,
            commands::findings::list_task_knowledge_cmd,
            commands::findings::provide_finding_response,
            commands::findings::resolve_finding,
            commands::findings::update_finding,
            commands::flow::add_sample_flow,
            commands::flow::cancel_flow_execution,
            commands::flow::compare_flow_versions,
            commands::flow::create_flow_version,
            commands::flow::create_sample_flow,
            commands::flow::delete_flow,
            commands::flow::delete_flow_version,
            commands::flow::export_flow_json,
            commands::flow::export_flow_yaml,
            commands::flow::export_flows_bulk,
            commands::flow::get_flow,
            commands::flow::get_flow_execution,
            commands::flow::get_flow_executions_count,
            commands::flow::get_flow_executions_filtered,
            commands::flow::get_flow_executions_paginated,
            commands::flow::get_flow_version,
            commands::flow::get_flows_by_tag,
            commands::flow::get_latest_flow_version,
            commands::flow::import_flow_json,
            commands::flow::import_flow_yaml,
            commands::flow::import_flows_bulk,
            commands::flow::list_flow_executions,
            commands::flow::list_flow_versions,
            commands::flow::list_flows,
            commands::flow::pause_flow_execution,
            commands::flow::provide_flow_input,
            commands::flow::restore_flow_version,
            commands::flow::resume_flow_execution,
            commands::flow::run_flow_execution,
            commands::flow::save_flow,
            commands::flow::start_flow_execution,
            commands::flow::step_flow_execution,
            commands::flow::step_into_flow,
            commands::flow::validate_flow,
            commands::global_log_sources::add_global_log_source,
            commands::global_log_sources::create_global_log_source_profile,
            commands::global_log_sources::delete_global_log_source,
            commands::global_log_sources::delete_global_log_source_profile,
            commands::global_log_sources::find_log_sources_with_ai,
            commands::global_log_sources::get_global_log_sources,
            commands::global_log_sources::migrate_project_sources_to_global,
            commands::global_log_sources::read_global_log_sources,
            commands::global_log_sources::read_log_sources_by_profile,
            commands::global_log_sources::save_global_log_sources,
            commands::global_log_sources::select_log_sources_for_context,
            commands::global_log_sources::set_default_log_source_profile,
            commands::global_log_sources::set_log_source_ai_selection_mode,
            commands::global_log_sources::update_global_log_source,
            commands::global_log_sources::update_global_log_source_profile,
            commands::hooks::create_hook,
            commands::hooks::delete_hook,
            commands::hooks::get_all_hooks,
            commands::hooks::get_hook,
            commands::hooks::reorder_hooks,
            commands::hooks::set_hook_enabled,
            commands::hooks::test_hook,
            commands::hooks::update_hook,
            commands::instances::delete_runner_instance,
            commands::instances::get_runner_identity,
            commands::instances::get_runner_instances,
            commands::instances::get_temp_spawn_placements,
            commands::instances::launch_runner_instance,
            commands::instances::list_monitors_for_placement,
            commands::instances::list_repo_worktrees,
            commands::instances::preview_spawn_placement,
            commands::instances::save_runner_instance,
            commands::instances::set_temp_spawn_placements,
            commands::instances::stop_runner_instance,
            commands::interaction::get_interaction_recording_status,
            commands::interaction::start_interaction_recording,
            commands::interaction::stop_interaction_recording,
            commands::issues::sync_issues_to_backend,
            commands::known_issues::create_known_issue,
            commands::known_issues::create_pattern_template,
            commands::known_issues::delete_known_issue,
            commands::known_issues::export_known_issues,
            commands::known_issues::find_issues_for_spec,
            commands::known_issues::import_known_issues,
            commands::known_issues::list_known_issues,
            commands::known_issues::list_pattern_templates,
            commands::known_issues::resolve_known_issue,
            commands::known_issues::update_known_issue,
            commands::learning::add_sample_learning_data,
            commands::learning::analyze_learning_data,
            commands::learning::clear_learning_data,
            commands::learning::export_learning_data,
            commands::learning::get_best_strategy,
            commands::learning::get_current_running_task,
            commands::learning::get_feedback_for_context,
            commands::learning::get_learning_dashboard_data,
            commands::learning::get_learning_insights,
            commands::learning::get_learning_outcomes_count,
            commands::learning::get_learning_outcomes_filtered,
            commands::learning::get_learning_outcomes_paginated,
            commands::learning::get_learning_patterns,
            commands::learning::get_learning_stats_by_date_range,
            commands::learning::get_learning_stats_summary,
            commands::learning::get_learning_summary,
            commands::learning::get_most_recent_task_with_checkpoints,
            commands::learning::get_recent_tasks_with_outcomes,
            commands::learning::import_learning_data,
            commands::learning::record_task_outcome,
            commands::library_sync::sync_api_requests_to_backend,
            commands::library_sync::sync_check_groups_to_backend,
            commands::library_sync::sync_checks_to_backend,
            commands::library_sync::sync_contexts_to_backend,
            commands::library_sync::sync_library_to_backend,
            commands::library_sync::sync_macros_to_backend,
            commands::library_sync::sync_prompt_snippets_to_backend,
            commands::library_sync::sync_shell_commands_to_backend,
            commands::lock_yield_policy_settings::get_lock_yield_policy_settings,
            commands::lock_yield_policy_settings::save_lock_yield_policy_settings,
            commands::logging::append_ai_output_log,
            commands::logging::append_render_log,
            commands::logging::clear_ai_output_log,
            commands::logging::clear_all_run_history,
            commands::logging::clear_render_log,
            commands::logging::delete_session_checkpoints,
            commands::logging::get_ai_output_log_path_cmd,
            commands::logging::get_render_log_path_cmd,
            commands::logging::list_session_checkpoints,
            commands::logging::load_ai_output_log,
            commands::logging::load_render_log,
            commands::log_api::clear_log_api_store,
            commands::log_api::sync_action_logs,
            commands::log_api::sync_ai_output_logs,
            commands::log_api::sync_all_logs,
            commands::log_api::sync_general_logs,
            commands::log_api::sync_image_logs,
            commands::log_api::sync_issues,
            commands::log_api::sync_project_logs,
            commands::log_api::sync_rag_logs,
            commands::mcp::call_mcp_tool,
            commands::mcp::connect_mcp_server,
            commands::mcp::create_mcp_server,
            commands::mcp::delete_mcp_server,
            commands::mcp::disconnect_mcp_server,
            commands::mcp::get_mcp_server,
            commands::mcp::get_mcp_server_status,
            commands::mcp::get_mcp_servers_status,
            commands::mcp::get_task_run_mcp_calls,
            commands::mcp::list_mcp_server_tools,
            commands::mcp::list_mcp_servers,
            commands::mcp::update_mcp_server,
            commands::meta_optimizer::activate_prompt_variant,
            commands::meta_optimizer::apply_meta_optimizer_recommendation,
            commands::meta_optimizer::build_golden_dataset,
            commands::meta_optimizer::capture_meta_optimizer_baseline,
            commands::meta_optimizer::convert_comparison_to_recommendation,
            commands::meta_optimizer::create_eval_spec,
            commands::meta_optimizer::create_prompt_canary,
            commands::meta_optimizer::delete_eval_spec,
            commands::meta_optimizer::evaluate_with_io,
            commands::meta_optimizer::generate_default_eval_spec,
            commands::meta_optimizer::get_agent_cascade_effect,
            commands::meta_optimizer::get_agent_cost_effectiveness,
            commands::meta_optimizer::get_agent_effectiveness,
            commands::meta_optimizer::get_agent_interaction_matrix,
            commands::meta_optimizer::get_canary_rollouts,
            commands::meta_optimizer::get_eval_results,
            commands::meta_optimizer::get_eval_specs,
            commands::meta_optimizer::get_golden_datasets,
            commands::meta_optimizer::get_meta_optimizer_failure_analysis,
            commands::meta_optimizer::get_meta_optimizer_progress,
            commands::meta_optimizer::get_meta_optimizer_recommendations,
            commands::meta_optimizer::get_meta_optimizer_runs,
            commands::meta_optimizer::get_meta_optimizer_snapshots,
            commands::meta_optimizer::get_model_profiles,
            commands::meta_optimizer::get_model_recommendations,
            commands::meta_optimizer::get_prompt_canary_status,
            commands::meta_optimizer::get_prompt_evolution_diff,
            commands::meta_optimizer::get_prompt_evolution_history,
            commands::meta_optimizer::get_prompt_group_metrics,
            commands::meta_optimizer::get_prompt_optimization_evidence,
            commands::meta_optimizer::get_prompt_optimization_status,
            commands::meta_optimizer::get_prompt_variant_content,
            commands::meta_optimizer::get_prompt_variants,
            commands::meta_optimizer::get_recommendation_outcomes,
            commands::meta_optimizer::get_robustness_reports,
            commands::meta_optimizer::promote_canary_rollout,
            commands::meta_optimizer::reevaluate_recommendation_outcome,
            commands::meta_optimizer::refresh_model_profiles,
            commands::meta_optimizer::reject_meta_optimizer_recommendation,
            commands::meta_optimizer::rollback_canary_rollout,
            commands::meta_optimizer::rollback_meta_optimizer_recommendation,
            commands::meta_optimizer::run_recommendation_eval,
            commands::meta_optimizer::run_robustness_test,
            commands::meta_optimizer::start_canary_rollout,
            commands::meta_optimizer::trigger_meta_optimizer,
            commands::mobile::capture_mobile_feedback,
            commands::mobile::capture_mobile_logcat,
            commands::mobile::capture_mobile_screenshot,
            commands::mobile::create_mobile_log,
            commands::mobile::create_mobile_state,
            commands::mobile::delete_mobile_data,
            commands::mobile::get_latest_mobile_state,
            commands::mobile::get_mobile_errors,
            commands::mobile::get_mobile_logs,
            commands::mobile::get_mobile_states,
            commands::mobile::list_mobile_devices,
            commands::mobile_settings::get_mobile_settings,
            commands::mobile_settings::save_mobile_settings,
            orchestration_loop::commands::get_multi_orchestration_loop_status,
            orchestration_loop::commands::get_orchestration_loop_status,
            orchestration_loop::commands::signal_orchestration_restart,
            orchestration_loop::commands::signal_orchestration_restart_by_id,
            orchestration_loop::commands::start_multi_orchestration_loop,
            orchestration_loop::commands::start_orchestration_loop,
            orchestration_loop::commands::stop_all_orchestration_loops,
            orchestration_loop::commands::stop_orchestration_loop,
            orchestration_loop::commands::stop_orchestration_loop_by_id,
            orchestration_loop::commands::start_orchestration_run,
            orchestration_loop::commands::stop_orchestration_run,
            orchestration_loop::commands::orchestration_run_status,
            orchestration_loop::commands::list_orchestration_runs,
            commands::orchestration_loop_configs::ol_delete_config,
            commands::orchestration_loop_configs::ol_get_config,
            commands::orchestration_loop_configs::ol_list_configs,
            commands::orchestration_loop_configs::ol_save_config,
            commands::orchestration_loop_configs::ol_toggle_favorite,
            commands::orchestration_loop_configs::ol_update_config,
            commands::otel_settings::get_otel_settings,
            commands::otel_settings::update_otel_settings,
            commands::performance_metrics::get_action_performance,
            commands::performance_metrics::get_element_resolution_metrics,
            commands::performance_metrics::get_performance_dashboard,
            commands::performance_metrics::get_success_rate_trend,
            commands::performance_metrics::get_transition_reliability,
            commands::playwright_settings::delete_playwright_test_password,
            commands::playwright_settings::get_playwright_settings,
            commands::playwright_settings::has_playwright_test_password,
            commands::playwright_settings::save_playwright_settings,
            process_capture::commands::delete_process_config,
            process_capture::commands::get_managed_processes,
            process_capture::commands::get_process_configs,
            process_capture::commands::get_process_log_context,
            process_capture::commands::get_process_output,
            process_capture::commands::get_process_session_output_from_db,
            process_capture::commands::get_process_sessions_from_db,
            process_capture::commands::rebuild_and_restart_process,
            process_capture::commands::restart_managed_process,
            process_capture::commands::save_process_config,
            process_capture::commands::search_process_logs,
            process_capture::commands::start_all_managed_processes,
            process_capture::commands::start_managed_process,
            process_capture::commands::stop_all_managed_processes,
            process_capture::commands::stop_managed_process,
            commands::productivity::acknowledge_advisory,
            commands::productivity::add_task_dependency,
            commands::productivity::approve_recommendation,
            commands::productivity::archive_plan,
            commands::productivity::auto_review_task,
            commands::productivity::backfill_completed_tasks_from_history,
            commands::productivity::check_path_claims,
            commands::productivity::decompose_plan,
            commands::productivity::get_coordinator_decisions,
            commands::productivity::get_coordinator_leader,
            commands::productivity::get_escalations,
            commands::productivity::get_fleet_health,
            commands::productivity::get_coord_http_base,
            commands::productivity::get_plan_recommendations,
            commands::productivity::get_plan_tasks,
            commands::productivity::get_recommendations,
            commands::productivity::get_reflection,
            commands::productivity::get_task_completion_report,
            commands::productivity::get_task_detail,
            commands::productivity::get_upcoming_claims,
            commands::productivity::launch_coordinator_session,
            commands::productivity::list_overlapping_intents,
            commands::productivity::list_plans,
            commands::productivity::list_plans_filtered,
            commands::productivity::list_workers,
            commands::productivity::preview_assignment_brief,
            commands::productivity::reject_recommendation,
            commands::productivity::resolve_escalation,
            commands::productivity::rewind_session,
            commands::productivity::search_knowledge,
            commands::productivity::spawn_worker_session,
            commands::productivity::stop_coordinator_session,
            commands::productivity::submit_task_completion_report,
            commands::productivity::summarize_session,
            commands::productivity::unarchive_plan,
            commands::project_logs::append_project_log,
            commands::project_logs::delete_project_config,
            commands::project_logs::get_project_directories,
            commands::project_logs::get_project_log_config,
            commands::project_logs::list_project_configs,
            commands::project_logs::read_log_source,
            commands::project_logs::read_project_logs,
            commands::project_logs::save_project_log_config,
            commands::rag::delete_rag_config,
            commands::rag::get_rag_config,
            commands::rag::get_rag_embedding_status,
            commands::rag::get_rag_storage_usage,
            commands::rag::import_rag_config,
            commands::rag::list_rag_configs,
            commands::rag::search_rag_elements,
            commands::rag::search_rag_elements_semantic,
            commands::rag::start_rag_processing,
            commands::recap::get_task_run_recap,
            commands::regression::get_recent_diagnoses_for_suite,
            commands::regression::get_regression_suite_by_id,
            commands::regression::list_regression_runs_for_suite,
            commands::regression::list_regression_suites,
            commands::regression::query_assertion_executions_for_suite,
            commands::regression::record_assertion_executions_batch,
            commands::regression::record_regression_diagnosis,
            commands::regression::record_regression_run,
            commands::regression::save_regression_suite,
            repo_detection::register_repo_with_coord,
            commands::saved_projects::add_saved_project,
            commands::saved_projects::list_saved_projects,
            commands::saved_projects::remove_saved_project,
            commands::saved_projects::save_saved_projects,
            commands::screenshot::capture_and_upload_screenshot,
            commands::screenshot::capture_screenshot,
            commands::screenshot::capture_screenshot_via_python,
            commands::screenshot::get_screenshot_monitors,
            commands::screenshots::list_screenshots,
            commands::script_emitter::emit_extraction_script,
            commands::script_emitter::emit_scripted_output_event,
            commands::scripted_output_settings::get_scripted_output_settings,
            commands::scripted_output_settings::save_scripted_output_settings,
            commands::security_settings::get_security_profiles,
            commands::security_settings::get_security_settings,
            commands::security_settings::update_security_settings,
            commands::self_healing_settings::delete_self_healing_api_key,
            commands::self_healing_settings::get_self_healing_settings,
            commands::self_healing_settings::has_self_healing_api_key,
            commands::self_healing_settings::save_self_healing_api_key,
            commands::self_healing_settings::save_self_healing_settings,
            // Plan 2026-05-22-coord-native-session-coordination Phase 2 —
            // unified Session primitive commands (coexist with legacy
            // `terminal_*` and `ai_session_*` until Phase 4 frontend cutover).
            commands::session::session_close,
            commands::session::session_describe,
            commands::session::session_focus,
            commands::session::session_list,
            commands::session::session_start,
            commands::session::session_steal,
            // Plan 2026-05-23-coord-native-sessions-phase-7-10 §Phase 7 —
            // "Continue elsewhere" cross-machine handoff trigger.
            commands::session::session_handoff,
            // Plan 2026-05-22-coord-native-session-coordination §D12 / Phase 4 —
            // active tenant resolver for the frontend TenantContext.
            commands::tenant::get_active_tenant,
            commands::tenant::set_active_tenant,
            commands::setup_wizard::check_setup_completed,
            commands::setup_wizard::complete_setup,
            commands::setup_wizard::detect_project_framework_for_setup,
            commands::setup_wizard::discover_claude_config_dirs,
            commands::setup_wizard::save_ai_provider_from_setup,
            commands::setup_wizard::save_dev_services_from_setup,
            commands::setup_wizard::save_log_sources_from_setup,
            commands::setup_wizard::scan_workspace_for_setup,
            commands::setup_wizard::suggest_dev_services_for_setup,
            commands::setup_wizard::suggest_log_sources_for_setup,
            commands::setup_wizard::suggest_process_configs_for_setup,
            commands::setup_wizard::suggest_workspace_sources_for_setup,
            commands::shell_commands::create_shell_command,
            commands::shell_commands::delete_shell_command,
            commands::shell_commands::execute_shell_command,
            commands::shell_commands::generate_shell_command_with_ai,
            commands::shell_commands::get_shell_command,
            commands::shell_commands::get_shell_command_categories,
            commands::shell_commands::get_shell_command_results,
            commands::shell_commands::list_shell_commands,
            commands::shell_commands::set_shell_command_enabled,
            commands::shell_commands::update_shell_command,
            commands::page_spec_store::delete_page_spec,
            commands::page_spec_store::load_user_specs,
            commands::page_spec_store::save_page_spec,
            commands::spec_drift::scan_spec_drift,
            spec_experimentation::commands::analyze_cross_page_consistency,
            spec_experimentation::commands::analyze_spec_element_coverage,
            spec_experimentation::commands::analyze_spec_freshness,
            spec_experimentation::commands::detect_broken_spec_assertions,
            spec_experimentation::commands::diff_spec_json,
            spec_experimentation::commands::diff_spec_versions,
            spec_experimentation::commands::extract_spec_compliance,
            spec_experimentation::commands::get_spec_accuracy_results,
            spec_experimentation::commands::get_spec_compliance_history,
            spec_experimentation::commands::get_spec_compliance_summary,
            spec_experimentation::commands::get_spec_version_history,
            spec_experimentation::commands::get_specs_needing_attention,
            spec_experimentation::commands::run_spec_mutation_test,
            spec_experimentation::commands::snapshot_current_spec,
            commands::spec_sync_state::push_spec_sync_state,
            commands::state_explorer::clear_exploration_history,
            commands::state_explorer::get_exploration_analysis_prompt,
            commands::state_explorer::get_exploration_history,
            commands::state_explorer::get_exploration_report,
            commands::state_explorer::get_exploration_strategies,
            commands::state_explorer::preview_exploration_plan,
            commands::state_explorer::start_exploration,
            commands::state_machine::clear_action_log,
            commands::state_machine::execute_transition,
            commands::state_machine::get_action_log_view,
            commands::state_machine::get_active_states,
            commands::state_machine::get_available_transitions,
            commands::state_machine::navigate_to_multiple_states,
            commands::state_machine::navigate_to_state,
            commands::state_machine_configs::sm_audit_capture_screenshot_bounds,
            commands::state_machine_configs::sm_backfill_capture_screenshot_dimensions,
            commands::state_machine_configs::sm_create_config,
            commands::state_machine_configs::sm_create_state,
            commands::state_machine_configs::sm_create_transition,
            commands::state_machine_configs::sm_delete_capture_screenshots,
            commands::state_machine_configs::sm_delete_config,
            commands::state_machine_configs::sm_delete_state,
            commands::state_machine_configs::sm_delete_transition,
            commands::state_machine_configs::sm_generate_static,
            commands::state_machine_configs::sm_get_capture_screenshot_image,
            commands::state_machine_configs::sm_get_capture_screenshots,
            commands::state_machine_configs::sm_get_config,
            commands::state_machine_configs::sm_get_thumbnails,
            commands::state_machine_configs::sm_import_config,
            commands::state_machine_configs::sm_list_configs,
            commands::state_machine_configs::sm_move_pending_screenshots,
            commands::state_machine_configs::sm_save_capture_screenshots,
            commands::state_machine_configs::sm_save_thumbnails,
            commands::state_machine_configs::sm_update_config,
            commands::state_machine_configs::sm_update_state,
            commands::state_machine_configs::sm_update_transition,
            commands::step_outputs::collect_step_outputs,
            commands::step_outputs::get_step_outputs_for_test_builder,
            commands::storage::clear_all_storage,
            commands::storage::delete_old_sessions,
            commands::storage::get_local_storage_usage,
            commands::storage::get_storage_paths,
            commands::storage::load_findings_data,
            commands::storage::read_image_as_base64,
            commands::storage::save_findings_data,
            commands::storage::save_screenshot_to_disk,
            commands::storage::save_video_to_disk,
            commands::task_sync::full_sync_ai_task,
            commands::task_sync::sync_ai_findings,
            commands::task_sync::sync_ai_session_ended,
            commands::task_sync::sync_ai_session_started,
            commands::task_sync::sync_ai_task_completed,
            commands::task_sync::sync_ai_task_created,
            commands::task_sync::sync_all_pending_ai_tasks,
            commands::task_sync::sync_deferred_questions,
            commands::terminal::terminal_ack,
            commands::terminal::terminal_cleanup_scrollback,
            commands::terminal::terminal_close,
            commands::terminal::terminal_collect_session_metadata,
            commands::terminal::terminal_create,
            commands::terminal::terminal_get_grid,
            commands::terminal::terminal_get_saved_scrollback,
            commands::terminal::terminal_get_scrollback,
            commands::terminal::terminal_grid_diff,
            commands::terminal::terminal_grid_search,
            commands::terminal::terminal_grid_text,
            commands::terminal::terminal_list,
            commands::terminal::terminal_migrate_session_account,
            commands::terminal::terminal_resize,
            commands::terminal::terminal_save_scrollback,
            commands::terminal::terminal_session_clear_restore_pending,
            commands::terminal::terminal_session_list_open,
            commands::terminal::terminal_session_mark_restore_pending,
            commands::terminal::terminal_session_record_close,
            commands::terminal::terminal_session_record_open,
            commands::terminal::terminal_set_title,
            commands::terminal::terminal_write,
            commands::terminal_analysis::analyze_architecture,
            commands::terminal_analysis::analyze_change_impact,
            commands::terminal_analysis::analyze_cross_tab,
            commands::terminal_analysis::analyze_page_architecture,
            commands::terminal_analysis::analyze_plan_progress,
            commands::terminal_analysis::analyze_session_summary,
            commands::terminal_analysis::get_latest_plan_content,
            // Pop-out terminal windows (Phase 1)
            commands::terminal_windows::assign_session_to_window,
            commands::terminal_windows::close_empty_terminal_windows,
            commands::terminal_windows::close_terminal_window,
            commands::terminal_windows::focus_runner_window,
            commands::terminal_windows::get_window_assignments,
            commands::terminal_windows::list_runner_windows,
            commands::terminal_windows::open_terminal_window,
            commands::test_orchestrator::delete_orchestration_plan,
            commands::test_orchestrator::execute_test_orchestration,
            commands::test_orchestrator::generate_test_from_orchestration,
            commands::test_orchestrator::get_saved_requests_for_orchestration,
            commands::test_orchestrator::list_orchestration_plans,
            commands::test_orchestrator::plan_test_orchestration,
            commands::test_orchestrator::save_orchestration_plan,
            commands::testing::analyze_page_playwright,
            commands::testing::analyze_page_playwright_script,
            commands::testing::analyze_page_vision,
            commands::testing::create_test_association,
            commands::testing::create_verification_test,
            commands::testing::delete_test_association,
            commands::testing::delete_verification_test,
            commands::testing::execute_test_by_id,
            commands::testing::execute_tests_by_ids,
            commands::testing::execute_verification_test,
            commands::testing::execute_verification_test_suite,
            commands::testing::export_all_tests_to_file,
            commands::testing::export_tests_to_file,
            commands::testing::generate_test_metadata,
            commands::testing::generate_test_with_ai,
            commands::testing::get_config_test_associations,
            commands::testing::get_task_run_test_results,
            commands::testing::get_test_results,
            commands::testing::get_test_type_info,
            commands::testing::get_verification_test,
            commands::testing::get_workflow_run_context,
            commands::testing::import_tests_from_file,
            commands::testing::list_recent_task_runs,
            commands::testing::list_verification_tests,
            commands::testing::update_verification_test,
            commands::testing::validate_test_definition,
            commands::tiered_info::cleanup_old_runs,
            commands::tiered_info::delete_ai_session,
            commands::tiered_info::get_ai_session_history,
            commands::tiered_info::get_config_statistics,
            commands::tiered_info::get_debugging_context,
            commands::tiered_info::get_debugging_context_prompt,
            commands::tiered_info::get_execution_options,
            commands::tiered_info::get_failed_runs,
            commands::tiered_info::get_flakiness_summary,
            commands::tiered_info::get_flaky_templates,
            commands::tiered_info::get_flaky_transitions,
            commands::tiered_info::get_recent_runs,
            commands::tiered_info::get_run_details,
            commands::tiered_info::record_run,
            commands::token_analytics::get_cost_by_model,
            commands::token_analytics::get_cost_by_phase,
            commands::token_analytics::get_cost_by_target_app,
            commands::token_analytics::get_daily_cost,
            commands::token_analytics::get_provider_latency,
            commands::token_analytics::get_task_run_costs,
            commands::token_analytics::get_token_usage_summary,
            commands::transcript::generate_workflow_standalone,
            commands::transcript::transcript_find_external_processes,
            commands::transcript::transcript_get_latest,
            commands::transcript::transcript_list_sessions,
            commands::transcript::transcript_read_session,
            commands::transcript::transcript_session_digests,
            commands::ui_bridge::ui_bridge_discover,
            commands::ui_bridge::ui_bridge_discover_states_from_fingerprints,
            commands::ui_bridge::ui_bridge_discover_states_native,
            commands::ui_bridge::ui_bridge_execute_action,
            commands::ui_bridge::ui_bridge_execute_component_action,
            commands::ui_bridge::ui_bridge_get_component,
            commands::ui_bridge::ui_bridge_get_components,
            commands::ui_bridge::ui_bridge_get_element,
            commands::ui_bridge::ui_bridge_get_elements,
            commands::ui_bridge::ui_bridge_get_snapshot,
            commands::ui_bridge::ui_bridge_reload_webview,
            commands::ui_bridge::ui_bridge_run_exploration,
            commands::ui_bridge::ui_bridge_run_exploration_native,
            commands::ui_bridge::ui_bridge_stop_exploration,
            commands::ui_bridge::ui_bridge_stop_exploration_native,
            commands::ui_bridge_baselines::sm_delete_baseline,
            commands::ui_bridge_baselines::sm_get_baseline,
            commands::ui_bridge_baselines::sm_list_baselines,
            commands::ui_bridge_baselines::sm_save_baseline,
            ui_error::clear_ui_error,
            ui_error::get_ui_error,
            ui_error::report_ui_error,
            commands::verification::clear_pending_verification,
            commands::verification::load_pending_verification,
            commands::verification::save_pending_verification,
            commands::verification::update_verification_status,
            commands::video::get_video_recording_status,
            commands::video::start_video_recording,
            commands::video::stop_video_recording,
            commands::watchers::create_watcher,
            commands::watchers::delete_watcher,
            commands::watchers::get_watcher,
            commands::watchers::list_watchers,
            commands::watchers::set_watcher_enabled,
            commands::watchers::update_watcher,
            commands::web_integration::get_web_integration_status,
            commands::web_integration::redeem_pair_code,
            commands::web_integration::save_web_integration_settings,
            commands::web_integration::test_web_integration_connection,
            commands::window_manager::activate_system_window,
            commands::window_manager::list_system_windows,
            commands::workflow_events::emit_workflow_event,
            commands::worktrees::merge_worktree,
            commands::worktrees::merge_worktree_force
            ]);
            move |invoke: tauri::ipc::Invoke<tauri::Wry>| -> bool {
                tauri_command_audit::record(invoke.message.command());
                inner(invoke)
            }
        })
        .manage(shared_app_state)
        .manage(launch_env.clone()) // Item 3: typed startup env snapshot, read once in run_app()
        .manage(bridge_compartment)
        .manage(execution_compartment)
        .manage(integration_compartment)
        .manage(health_compartment)
        .manage(storage_compartment)
        .manage(log_api_store)
        .manage(rag_state)
        .manage(instance_manager) // For multi-instance management (dev feature)
        .manage(session_manager) // For interactive AI session commands
        .manage(terminal_manager) // For embedded PTY terminal sessions
        .manage(tokio::sync::Mutex::new(
            qontinui_runner_lib::accessibility::AccessibilityManager::default(),
        )) // Native cross-platform accessibility API
        .manage(std::sync::Arc::new(
            commands::spec_sync_state::SpecSyncStateBus::new(),
        )) // P2 SSE remediation: useSpecSync progress mirror, consumed by /ui-bridge/sdk/spec-sync/{status,stream}
        // Tauri command handlers are registered via the central
        // `tauri::generate_handler![...]` block above. Each command module
        // (`commands/*.rs` and a few subsystem `commands.rs` files) ALSO
        // exposes a `pub fn plugin<R: Runtime>() -> TauriPlugin<R>` for
        // future plugin-based registration, but those are not currently
        // wired — Tauri 2's plugin path requires `plugin:<name>|<cmd>`
        // invoke prefixes and the frontend invokes commands bare, so the
        // plugin form was rolled back to the central handler at commit
        // `1f1d807f6`. The `plugin()` fns are kept as ready-to-go scaffolding
        // for any future migration that updates the frontend invoke sites.
        .setup(|app| {
            info!("Tauri application setup starting");

            // Boot-restore remediation item 1 — classify THIS boot (crash
            // recovery vs planned restart) exactly once, synchronously,
            // BEFORE the API server / frontend restore can race it.
            // `classify_boot` reads the PRIOR shutdown marker, stashes the
            // classification in a process-wide OnceLock, and immediately
            // re-marks the marker `clean:false` for the now-running process.
            // Consumers (`resume_ai_sessions` via mcp_api.rs, the terminal
            // restore path via `terminal_session_list_open`) read the stash —
            // a command-time file read would ALWAYS see `clean:false` after
            // this point and permanently classify "crash".
            {
                let marker_path = session::shutdown_marker::marker_path(
                    crate::mcp::types::get_mcp_api_port(),
                );
                let boot = session::shutdown_marker::classify_boot(&marker_path);
                if boot.crash_recovery {
                    warn!(
                        "boot classification: previous shutdown was NOT clean — crash recovery"
                    );
                } else {
                    info!("boot classification: previous shutdown was clean — planned restart");
                }
            }

            // Plan 2026-05-18-agent-spawn-coordination Phase 3 — stash a
            // global AppHandle so background tokio tasks (e.g. claim
            // heartbeats from `agent_claims`) can emit Tauri events
            // to the webview without taking an AppHandle parameter.
            tauri_app_handle::set(app.handle().clone());

            // Plan 2026-05-22-memories-on-coord-cross-machine.md Phase 5.G,
            // generalized by 2026-05-24-federation-verify-and-gitop.md
            // Phase 4 — initialize the process-wide observable-bridge
            // registry. Each bridge mediates a per-session pull / watch /
            // reconcile lifecycle (memory federation; git-ops next). Init
            // failure is non-fatal — the spawn sites iterate the registry
            // and an empty registry short-circuits all federation.
            {
                let mut bridges: Vec<
                    std::sync::Arc<dyn qontinui_runner_lib::observable_bridge::RunnerObservableBridge>,
                > = Vec::new();
                match qontinui_runner_lib::observable_bridge::memory::MemoryBridge::new() {
                    Ok(bridge) => {
                        let arc = std::sync::Arc::new(bridge);
                        // Keep the concrete Arc available as Tauri State
                        // (e.g. for runner-shutdown `shutdown_all`), and
                        // register it as a trait object for dispatch.
                        app.manage(arc.clone());
                        bridges.push(arc);
                        info!("memory federation bridge initialized");
                    }
                    Err(e) => {
                        warn!(
                            "memory federation bridge init failed ({}); feature disabled this session",
                            e
                        );
                    }
                }
                // GitOpBridge — the second observable category (`git_op`).
                // Registered unconditionally (default ON), mirroring the
                // memory bridge. Init failure is non-fatal and independent:
                // a failed git bridge must not disable memory federation.
                match qontinui_runner_lib::observable_bridge::git_ops::GitOpBridge::new() {
                    Ok(bridge) => {
                        let arc = std::sync::Arc::new(bridge);
                        // Keep the concrete Arc as Tauri State for
                        // runner-shutdown `shutdown_all` (hook teardown),
                        // and register it as a trait object for dispatch.
                        app.manage(arc.clone());
                        bridges.push(arc);
                        info!("git-op federation bridge initialized");
                    }
                    Err(e) => {
                        warn!(
                            "git-op federation bridge init failed ({}); git federation disabled this session",
                            e
                        );
                    }
                }
                qontinui_runner_lib::observable_bridge::init_registry(bridges);
            }

            // Plan 2026-05-22-coord-native-session-coordination Phase 2 —
            // unified Session primitive. Materialize the registry here
            // because the PTY / Claude-CLI transports need an `AppHandle`
            // (only available inside .setup). The registry is .manage()'d
            // so `commands::session::*` can pull it via `tauri::State`.
            //
            // The terminal manager is fetched via `app.state::<>()` so we
            // don't need a `move` closure on `.setup()` (other captures in
            // the existing setup body rely on non-`move` semantics; see
            // the comment block above this closure).
            {
                // Phase 0 multi-user readiness — normalize the operator's
                // legacy `machine.json` so the canonical `device_id` key is
                // present before any consumer reads it (the session machinery
                // below + the coord data-plane bearer). No-op when the file is
                // already canonical / missing.
                qontinui_runner_lib::pair::ensure_device_id_persisted();

                let app_handle = app.handle().clone();
                let term_state: tauri::State<'_, std::sync::Arc<terminal::TerminalManager>> =
                    app.state();
                let term_for_session = term_state.inner().clone();
                // Instance-scope the outbox dir so every spawn-test / named
                // secondary runner gets its OWN `instance-<name>/` outbox file
                // instead of racing the primary on a single shared
                // `session-outbox.jsonl`. `scope_path` is a no-op for the
                // primary (no `QONTINUI_INSTANCE_NAME`), so the primary keeps
                // resolving to the legacy unscoped path — its pending outbox
                // rows are never orphaned. `OutboxWriter::open` create_dir_all's
                // the parent, so the scoped dir is created automatically.
                let outbox_dir = instance::scope_path(
                    &dirs::home_dir()
                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                        .join(".qontinui")
                        .join("runner"),
                );
                let outbox_path = outbox_dir.join("session-outbox.jsonl");
                tracing::info!(
                    path = %outbox_path.display(),
                    instance = ?instance::instance_name(),
                    "session: resolved outbox path"
                );
                let outbox = match session::local_store::OutboxWriter::open(&outbox_path) {
                    Ok(o) => std::sync::Arc::new(o),
                    Err(e) => {
                        // Fall back to a tempdir outbox so the registry
                        // still works in dev / first-launch scenarios
                        // where ~/.qontinui isn't writable.
                        tracing::warn!(
                            error = %e,
                            path = %outbox_path.display(),
                            "session: outbox open failed — using ephemeral fallback"
                        );
                        let fallback = std::env::temp_dir()
                            .join("qontinui-runner-session-outbox.jsonl");
                        std::sync::Arc::new(
                            session::local_store::OutboxWriter::open(&fallback)
                                .expect("session: ephemeral outbox open failed"),
                        )
                    }
                };
                // Session-automation Phase 0 — keep an outbox handle for the
                // AI-session coord registrar so it writes to the SAME outbox the
                // CoordSync drain loop drains (registration reuses the existing
                // drain → auth → retry → 409-idempotency machinery).
                let registrar_outbox = outbox.clone();
                let coord_sync_facade = session::coord_sync::CoordSync::new(outbox);
                let pty_transport: session::DynTransport =
                    std::sync::Arc::new(session::transport::pty::PtyTransport::new(
                        term_for_session.clone(),
                        app_handle.clone(),
                    ));
                let claude_cli_transport: session::DynTransport = std::sync::Arc::new(
                    session::transport::claude_cli::ClaudeCliTransport::new(
                        term_for_session.clone(),
                        app_handle.clone(),
                    ),
                );
                let workflow_transport: session::DynTransport = std::sync::Arc::new(
                    session::transport::workflow::WorkflowTransport::new(),
                );
                let machine_id = dirs::home_dir()
                    .and_then(|h| std::fs::read(h.join(".qontinui").join("machine.json")).ok())
                    .and_then(|b| {
                        let v: serde_json::Value = serde_json::from_slice(&b).ok()?;
                        let s = v
                            .get("device_id")
                            .and_then(|x| x.as_str())
                            .or_else(|| v.get("machine_id").and_then(|x| x.as_str()))?;
                        uuid::Uuid::parse_str(s).ok()
                    })
                    .unwrap_or_else(uuid::Uuid::new_v4);
                // Session-automation Phase 0 (R1–R6) — register authenticated
                // AI sessions into coord.sessions with their task_run_id, so
                // they are visible + addressable + correctly stale to coord.
                // Managed as Tauri state so the interactive AI-session commands
                // (create/resume/send_user_message/close) can reach it.
                let ai_coord_registrar = std::sync::Arc::new(
                    claude_session::coord_register::AiCoordRegistrar::new(
                        registrar_outbox,
                        machine_id,
                    ),
                );
                app.manage(ai_coord_registrar);
                // Phase 3 — wire the coord-sync drain + heartbeat loops.
                // `attach_app_handle` enables the conflict-event emit;
                // `attach_registry` gives the heartbeat loop a way to
                // enumerate active sessions for the stale sweep.
                coord_sync_facade.attach_app_handle(app_handle.clone());
                let registry = session::SessionRegistry::new(
                    machine_id,
                    session::SessionTransports {
                        pty: pty_transport,
                        claude_cli: claude_cli_transport,
                        workflow: workflow_transport,
                    },
                    coord_sync_facade.clone(),
                );
                coord_sync_facade.attach_registry(&registry);

                // R2 (session-lifecycle-cleanup) — pane → coord-session-id
                // store, so a restored terminal pane RESUMES its prior coord
                // session instead of orphaning the row + minting a duplicate.
                // Co-located with the session outbox under `.qontinui/runner`.
                // Co-located with — and scoped identically to — the session
                // outbox above, so a secondary instance's pane→coord-session
                // map never collides with the primary's.
                let pane_store_path = instance::scope_path(
                    &dirs::home_dir()
                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                        .join(".qontinui")
                        .join("runner"),
                )
                .join("pane-sessions.json");
                let pane_store = std::sync::Arc::new(
                    match session::pane_store::PaneSessionStore::open(&pane_store_path) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                path = %pane_store_path.display(),
                                "session: pane store open failed — using ephemeral fallback"
                            );
                            let fallback = std::env::temp_dir()
                                .join("qontinui-runner-pane-sessions.json");
                            session::pane_store::PaneSessionStore::open(&fallback)
                                .expect("session: ephemeral pane store open failed")
                        }
                    },
                );
                app.manage(pane_store);

                // Phase 1 (pop-out terminal windows) — persisted window↔session
                // ownership + the runner's own window registry. Co-located with
                // the pane store under `.qontinui/runner`, same atomic-write
                // pattern. `ensure_main` records the "main" window on boot.
                // Namespaced by API port (mirrors the lifecycle store above) so
                // each runner instance owns its own windows — without this a
                // temp runner (9877+) would read/clobber the primary's
                // window-assignments and try to render its pop-outs.
                let wa_api_port = crate::mcp::types::get_mcp_api_port();
                let wa_file_name = if wa_api_port == 9876 {
                    "window-assignments.json".to_string()
                } else {
                    format!("window-assignments-{wa_api_port}.json")
                };
                let window_assignments_path = dirs::home_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join(".qontinui")
                    .join("runner")
                    .join(&wa_file_name);
                let window_assignments = std::sync::Arc::new(
                    match window_assignments::WindowAssignments::open(&window_assignments_path) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                path = %window_assignments_path.display(),
                                "window_assignments: open failed — using ephemeral fallback"
                            );
                            let fallback = std::env::temp_dir()
                                .join(format!("qontinui-runner-{wa_file_name}"));
                            window_assignments::WindowAssignments::open(&fallback)
                                .expect("window_assignments: ephemeral open failed")
                        }
                    },
                );
                window_assignments.ensure_main(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0),
                );
                // Phase 2: drop any session→window owner whose target window no
                // longer exists in the persisted set (e.g. a hand-edited or
                // partially-written file), reverting those sessions to "main"
                // so no tab is stranded on a window that will never be restored.
                let reconciled = window_assignments.reconcile_orphans();
                if !reconciled.is_empty() {
                    info!(
                        count = reconciled.len(),
                        "window_assignments: reconciled orphaned session owners → main"
                    );
                }
                // Phase 2: periodic geometry capture. The operator restarts the
                // primary by rebuild-and-kill, which never runs the clean quit
                // handler — so the only way a restored window lands at its last
                // position/size is to snapshot geometry while running. Runs off
                // the main thread; getters are Result-guarded (a busy/again
                // window just skips that tick) and `update_geometry` persists
                // only on change, so an idle window writes nothing.
                let wa_for_geo_poll = window_assignments.clone();
                let geo_poll_app = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        commands::terminal_windows::capture_open_geometry(
                            &geo_poll_app,
                            &wa_for_geo_poll,
                        );
                    }
                });
                app.manage(window_assignments);

                // Durable, backend-owned terminal-session lifecycle registry,
                // keyed by `claudeSessionId`. Source of truth for "which
                // Claude sessions exist and which grid zone each belongs to",
                // replacing the fragile frontend `localStorage` snapshot.
                // Co-located with the pane store + session outbox under
                // `.qontinui/runner` (same home-dir resolution + temp-dir
                // fallback). A background poll (spawned below) lazily flips
                // dead sessions to `closed`.
                // Namespace the registry file by API port so each runner
                // instance owns its own sessions — mirrors the frontend
                // `instance-storage.ts` convention (port 9876 → base name,
                // every other port → `-<port>` suffix). Without this a temp
                // runner (9877+) would read the primary's open records and
                // try to `claude --resume` the primary's live sessions.
                let lifecycle_api_port = crate::mcp::types::get_mcp_api_port();
                let lifecycle_file_name = if lifecycle_api_port == 9876 {
                    "terminal-sessions.json".to_string()
                } else {
                    format!("terminal-sessions-{}.json", lifecycle_api_port)
                };
                let lifecycle_store_path = dirs::home_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join(".qontinui")
                    .join("runner")
                    .join(&lifecycle_file_name);
                let lifecycle_store = std::sync::Arc::new(
                    match session::session_lifecycle_store::SessionLifecycleStore::open(
                        &lifecycle_store_path,
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                path = %lifecycle_store_path.display(),
                                "session: lifecycle store open failed — using ephemeral fallback"
                            );
                            let fallback = std::env::temp_dir().join(format!(
                                "qontinui-runner-{}",
                                lifecycle_file_name
                            ));
                            session::session_lifecycle_store::SessionLifecycleStore::open(&fallback)
                                .expect("session: ephemeral lifecycle store open failed")
                        }
                    },
                );
                let poll_lifecycle_store = lifecycle_store.clone();
                app.manage(lifecycle_store);

                // VT output sanitizer: terminal output is UNTRUSTED (whatever a
                // child process / remote host / `cat`'d file emits). This hook
                // runs FIRST on every PTY chunk — before the grid parser, the
                // scrollback tee, and the frontend emit — and strips OSC 52
                // (clipboard set/query, the headline security fix) and scrubs
                // control bytes out of OSC 0/1/2 (title) + OSC 8 (hyperlink)
                // payloads, while passing everything else (OSC 633 shell
                // integration, DEC ?2026 sync output, SGR/colors, bracketed
                // paste, …) through untouched. See `terminal::vt_sanitize`.
                //
                // Default-ON; set env `QONTINUI_TERMINAL_SANITIZE=0|false|off`
                // to disable (e.g. to inspect a raw stream for debugging) — in
                // that case the hook is simply not registered.
                if terminal::vt_sanitize::sanitize_enabled() {
                    term_for_session.interceptor().add_hook(Box::new(
                        terminal::vt_sanitize::VtSanitizeHook::new(),
                    ));
                }

                // (The usage-limit watcher and the fleet auto-response matcher
                // are both grid-scan pollers spawned once at startup alongside
                // the fleet fetch loop — they scan each terminal's rendered VT
                // grid rather than this raw byte stream, so they can see text
                // inside a full-screen TUI like Claude Code. See
                // `terminal::usage_limit::spawn_grid_scan_loop` and
                // `terminal::auto_response::spawn_grid_scan_loop`.)

                // Infrequent liveness poll for lazy close-detection. Every
                // 45s it snapshots open lifecycle records against the live
                // terminal manager + a single system process snapshot, runs
                // the pure `classify` decision core (asymmetric — NEVER closes
                // on uncertainty), and flips confidently-dead sessions to
                // `closed`. Detached for the process lifetime; wrapped so a
                // fallible tick never panics the loop.
                let poll_tm = term_for_session.clone();
                tauri::async_runtime::spawn(async move {
                    use std::collections::HashMap as StdHashMap;
                    use std::time::Duration;

                    use session::session_lifecycle_store::{classify, PollAction};

                    // claudeSessionId -> consecutive zero-descendant ticks,
                    // carried across ticks to debounce `NeedsConfirm`.
                    let mut consecutive_dead: StdHashMap<String, u32> = StdHashMap::new();
                    // claudeSessionId -> consecutive NO-MATCHING-TERMINAL
                    // ticks, carried across ticks to debounce the orphan
                    // close (`CloseNoTerminal`): a record matching nothing
                    // in this instance for ~4 ticks is an orphan, not
                    // uncertainty — without this it stays `open` for up to
                    // 7 days and re-qualifies for restore at every boot.
                    let mut consecutive_no_match: StdHashMap<String, u32> = StdHashMap::new();

                    loop {
                        tokio::time::sleep(Duration::from_secs(45)).await;

                        let open = poll_lifecycle_store.open_records();
                        if open.is_empty() {
                            // Forget any stale counters and prune, then idle.
                            consecutive_dead.clear();
                            consecutive_no_match.clear();
                            poll_lifecycle_store
                                .prune(chrono::Utc::now().timestamp_millis());
                            continue;
                        }

                        let live = poll_tm.list();

                        for rec in &open {
                            // Match the live terminal: by id first, then the
                            // (page_id, title, working_dir) triple as fallback.
                            let info = live
                                .iter()
                                .find(|i| i.id == rec.terminal_id)
                                .or_else(|| {
                                    live.iter().find(|i| {
                                        i.page_id == rec.page_id
                                            && rec
                                                .title
                                                .as_deref()
                                                .map(|t| t == i.title)
                                                .unwrap_or(false)
                                            && rec
                                                .working_dir
                                                .as_deref()
                                                .map(|w| w == i.working_dir)
                                                .unwrap_or(false)
                                    })
                                });

                            let (live_is_alive, descendant_count, snapshot_ok) = match info {
                                None => (None, 0usize, true),
                                Some(info) => {
                                    let pid = info.pid.unwrap_or(0);
                                    let (descendants, parent_map) =
                                        crate::process_capture::process_tree::discover_descendants_with_parent_map(pid)
                                            .await;
                                    // A totally-empty system parent_map means
                                    // the snapshot helper failed → Skip.
                                    let snapshot_ok = !parent_map.is_empty();
                                    let alive = crate::process_capture::health::pid_alive(pid);
                                    (Some(alive), descendants.len(), snapshot_ok)
                                }
                            };

                            let prior = consecutive_dead
                                .get(&rec.claude_session_id)
                                .copied()
                                .unwrap_or(0);
                            let prior_no_match = consecutive_no_match
                                .get(&rec.claude_session_id)
                                .copied()
                                .unwrap_or(0);
                            // Restore-pending records (a boot-restore typed
                            // `claude --resume` whose handshake isn't verified
                            // yet — or whose restore FAILED and awaits a retry)
                            // must never flip `poll-dead` / `no-terminal`:
                            // classify Skips them (and accumulates no ticks)
                            // unless the session is confidently alive. A mid-
                            // restore record keeps its OLD terminal_id until
                            // the re-assert, so it naturally matches nothing.
                            let restore_pending = rec.restore_pending_at.is_some();
                            let action = classify(
                                live_is_alive,
                                descendant_count,
                                prior,
                                prior_no_match,
                                snapshot_ok,
                                restore_pending,
                            );

                            match action {
                                PollAction::KeepAlive => {
                                    // Confidently alive — self-heal a stale
                                    // restore-pending marker (the frontend's
                                    // verified-handshake clear may have been
                                    // lost with a crash).
                                    if restore_pending {
                                        poll_lifecycle_store
                                            .clear_restore_pending(&rec.claude_session_id);
                                    }
                                    poll_lifecycle_store.touch(&rec.claude_session_id);
                                    consecutive_dead.remove(&rec.claude_session_id);
                                    consecutive_no_match.remove(&rec.claude_session_id);
                                }
                                PollAction::NeedsConfirm => {
                                    consecutive_dead
                                        .insert(rec.claude_session_id.clone(), prior + 1);
                                    // A terminal matched this tick — any
                                    // no-match streak is broken.
                                    consecutive_no_match.remove(&rec.claude_session_id);
                                    tracing::debug!(
                                        claude_session = %rec.claude_session_id,
                                        "session lifecycle poll: zero descendants — confirming before close"
                                    );
                                }
                                PollAction::Close => {
                                    poll_lifecycle_store
                                        .record_close(&rec.claude_session_id, "poll-dead");
                                    consecutive_dead.remove(&rec.claude_session_id);
                                    consecutive_no_match.remove(&rec.claude_session_id);
                                    tracing::info!(
                                        claude_session = %rec.claude_session_id,
                                        "session lifecycle poll: closing dead session"
                                    );
                                }
                                PollAction::NoMatchWait => {
                                    consecutive_no_match
                                        .insert(rec.claude_session_id.clone(), prior_no_match + 1);
                                    // A no-match tick breaks any zero-
                                    // descendant streak (the streaks are
                                    // mutually exclusive — a stale dead
                                    // counter must not survive terminal
                                    // churn and trigger an early poll-dead
                                    // close when the terminal re-matches).
                                    consecutive_dead.remove(&rec.claude_session_id);
                                    tracing::debug!(
                                        claude_session = %rec.claude_session_id,
                                        terminal = %rec.terminal_id,
                                        ticks = prior_no_match + 1,
                                        "session lifecycle poll: no matching terminal — debouncing before orphan close"
                                    );
                                }
                                PollAction::CloseNoTerminal => {
                                    // No matching terminal for several
                                    // consecutive ticks — an orphan row (e.g.
                                    // a ghost inherited from a prior process).
                                    // `"no-terminal"` is non-restorable.
                                    poll_lifecycle_store
                                        .record_close(&rec.claude_session_id, "no-terminal");
                                    consecutive_dead.remove(&rec.claude_session_id);
                                    consecutive_no_match.remove(&rec.claude_session_id);
                                    tracing::info!(
                                        claude_session = %rec.claude_session_id,
                                        terminal = %rec.terminal_id,
                                        "session lifecycle poll: closing orphan session (no matching terminal)"
                                    );
                                }
                                PollAction::Skip => {
                                    // Uncertain — do NOT touch the counters.
                                }
                            }
                        }

                        poll_lifecycle_store.prune(chrono::Utc::now().timestamp_millis());
                    }
                });

                // Plan §Phase 3/7/10 — start the coord-sync background loops
                // (drain, heartbeat, Phase 7 handoff receiver, Phase 10
                // cutover-flag poll). `.setup()` runs synchronously with no
                // ambient Tokio reactor, so these helpers' internal
                // `tokio::spawn` panics ("there is no reactor running, must be
                // called from the context of a Tokio 1.x runtime"). Wrap the
                // starts in `tauri::async_runtime::spawn` so they execute on
                // Tauri's managed runtime — matching the other setup-time
                // spawns below. Handles are intentionally dropped; the tasks
                // run detached for the lifetime of the process.
                //   Phase 7 receiver subscribes to coord's `/ws` Redis fan-out
                //   (`qontinui.sessions.*`) + a one-shot on-(re)connect catch-up
                //   GET, materializing each handoff_request as a child session.
                //   Phase 10 flag-poll only spawns when an `active_tenant_id`
                //   resolved from machine.json (None → dormant); dual-write
                //   stays off until `session_coordination_enabled` flips.
                let loop_registry = registry.clone();
                tauri::async_runtime::spawn(async move {
                    let _drain = loop_registry.coord_sync().start_drain_task();
                    let _heartbeat = loop_registry.coord_sync().start_heartbeat_task();
                    let _handoff_rx =
                        session::handoff::start_receiver_task(loop_registry.clone());
                    let _flag_poll = loop_registry.coord_sync().start_flag_poll_task();
                });
                app.manage(registry);

            }

            // Phase F.1 — register the deep-link `on_open_url` callback so
            // `qontinui://wake?intent=...` URLs delivered by the OS (cold
            // start) or forwarded by the single-instance plugin (warm start)
            // route into the wake handler, which fires an immediate scheduler
            // tick.
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let app_handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    let urls: Vec<String> =
                        event.urls().into_iter().map(|u| u.to_string()).collect();
                    wake_handler::handle_deep_link_urls(app_handle.clone(), urls);
                });
            }

            // Pull the typed startup-env snapshot (managed onto state above)
            // so this closure can read launch env vars without re-parsing.
            let setup_launch_env: tauri::State<launch_env::SharedLaunchEnv> = app.state();
            let setup_launch_env = setup_launch_env.inner().clone();

            let server_mode = setup_launch_env.server_mode;
            if server_mode {
                info!("QONTINUI_SERVER_MODE is set - running as headless server (no window, Restate forced on)");
            }

            // ── Programmatic window creation ───────────────────────────
            //
            // The declarative `windows[]` in tauri.conf.json is empty.
            // We create the main window here so that:
            //   - Secondary instances (test runners) get an isolated
            //     WebView2 user-data folder via `.data_directory()`,
            //     preventing profile-lock contention with the primary's
            //     `%LOCALAPPDATA%\com.qontinui.runner\EBWebView`.
            //   - The primary instance gets the exact same config it had
            //     declaratively (maximized, 1400×800, min 1200×700).
            //
            // This replaces the old "create declarative window then
            // destroy-and-recreate for secondaries" approach, which
            // failed because Tauri's declarative window was already
            // holding the default profile lock before .setup() ran.
            {
                use tauri::Manager;

                // Resolve via launch_env so a runner launched standalone
                // (no supervisor setting WEBVIEW2_USER_DATA_FOLDER) still
                // gets the same per-runner profile layout. The launch_env
                // field delegates to crate::instance::webview2_data_dir(),
                // which prefers the env var and falls back to the shared
                // qontinui_types helper. Returns None on non-Windows.
                let data_dir: Option<std::path::PathBuf> =
                    setup_launch_env.webview2_user_data_dir.clone();
                let is_secondary = instance::is_secondary();

                if let Some(ref dir) = data_dir {
                    let _ = std::fs::create_dir_all(dir);
                    info!(
                        "Creating window with isolated WebView2 profile (WEBVIEW2_USER_DATA_FOLDER={})",
                        dir.display()
                    );
                }

                // Always create the window programmatically. tauri.conf.json
                // has "windows": [] so there's no declarative window.
                // This lets us set data_directory() for test runners
                // (isolated WebView2 profiles) using the same code path.
                if server_mode {
                    info!("Skipping main window creation (server mode)");
                    // If Tauri's Win32 event loop exits immediately without any window, fall back
                    // to a hidden 0x0 tool window here. See plan Phase 1.5 validation.
                } else {
                    use crate::window_placement::WindowPlacement;

                    let url = tauri::WebviewUrl::App("index.html".into());

                    // ── Resolve placement + decorations ────────────────
                    //
                    // Three input paths funnel into one `WindowPlacement`:
                    //   1. Supervisor env vars (QONTINUI_WINDOW_X/Y/W/H/DECORATIONS)
                    //      — set for temp runners (test-*). Read once via
                    //      RunnerLaunchEnv (Item 3) and surfaced as
                    //      typed `window_hints`.
                    //   2. Named-instance config (settings.json
                    //      `runner_instances[name].spawn_placement`) — used
                    //      when QONTINUI_INSTANCE_NAME is set without env-var
                    //      coords.
                    //   3. Default — primary maximizes; bare secondary uses
                    //      the (100, 100) fallback so the window is on-screen.
                    let window_hints = &setup_launch_env.window;
                    let env_pos: Option<(f64, f64)> = match (window_hints.x, window_hints.y) {
                        (Some(x), Some(y)) => Some((x as f64, y as f64)),
                        _ => None,
                    };
                    let env_size: Option<(f64, f64)> =
                        match (window_hints.width, window_hints.height) {
                            (Some(w), Some(h)) => Some((w as f64, h as f64)),
                            _ => None,
                        };
                    // QONTINUI_WINDOW_DECORATIONS=0 forces a borderless
                    // window; default is true (Tauri's default chrome).
                    // Borderless lets a placement land flush with the
                    // monitor's edge — the few-pixel right-of-edge inset
                    // people see with chrome on is the OS window border.
                    let env_decorations = window_hints.decorations;

                    // Named-instance fallback: only kicks in when the
                    // supervisor didn't push a placement via env vars.
                    let named_placement = if env_pos.is_none() && is_secondary {
                        std::env::var("QONTINUI_INSTANCE_NAME").ok().and_then(|name| {
                            crate::settings::get_runner_instances()
                                .into_iter()
                                .find(|c| c.name == name)
                                .and_then(|c| c.spawn_placement)
                                .and_then(|p| {
                                    match crate::spawn_placement::resolve_to_global_physical(
                                        app.handle(),
                                        &p,
                                    ) {
                                        Ok(r) => {
                                            info!(
                                                "Named runner '{}' resolving own placement: {} → ({}, {}) {}x{}",
                                                name, r.monitor_label, r.global_x, r.global_y, r.width, r.height,
                                            );
                                            Some((r, p.decorations))
                                        }
                                        Err(e) => {
                                            warn!(
                                                "Named runner '{}' placement resolution failed: {}",
                                                name, e
                                            );
                                            None
                                        }
                                    }
                                })
                        })
                    } else {
                        None
                    };

                    let resolved_pos = env_pos.or_else(|| {
                        named_placement
                            .as_ref()
                            .map(|(r, _)| (r.global_x as f64, r.global_y as f64))
                    });
                    let resolved_size = env_size.or_else(|| {
                        named_placement
                            .as_ref()
                            .map(|(r, _)| (r.width as f64, r.height as f64))
                    });
                    let resolved_decorations = env_decorations.or_else(|| {
                        named_placement.as_ref().and_then(|(_, d)| *d)
                    });

                    let initial_size = resolved_size.unwrap_or((1400.0, 800.0));

                    let placement = if let Some((x, y)) = resolved_pos {
                        info!(
                            "Window placement: positioned at ({}, {}) size ({}, {})",
                            x, y, initial_size.0, initial_size.1
                        );
                        WindowPlacement::Positioned {
                            x: x as i32,
                            y: y as i32,
                            w: initial_size.0 as u32,
                            h: initial_size.1 as u32,
                        }
                    } else if is_secondary {
                        WindowPlacement::SecondaryDefault
                    } else {
                        WindowPlacement::Maximized
                    };

                    let mut builder = tauri::WebviewWindowBuilder::new(app, "main", url)
                        .title("Qontinui Runner")
                        .inner_size(initial_size.0, initial_size.1)
                        .min_inner_size(1200.0, 700.0)
                        .fullscreen(false)
                        .resizable(true)
                        .decorations(resolved_decorations.unwrap_or(true))
                        // Phase P2.2 of `tmp_plans/sw-cache-invalidation.md`:
                        // mark the embedded index.html as `no-store` so a
                        // webview that survives a binary swap can't serve a
                        // stale shell whose <script src> tags point at
                        // hashed asset filenames the new bundle no longer
                        // contains. Hashed `/assets/*` responses pass
                        // through with their default headers.
                        .on_web_resource_request(asset_headers::stamp_no_store_on_index);

                    if let Some(ref dir) = data_dir {
                        builder = builder.data_directory(dir.clone());
                    }

                    // Inject the intended API port as a global so the frontend's
                    // synchronous port-resolution fast-path (`window.__QONTINUI_PORT__`,
                    // used by useFileLockTracking / useRegistryAwareness /
                    // useMidSessionProbe / LaunchMenu / useSessionManager / etc.)
                    // resolves to the *actual* runner port on temp/secondary instances
                    // instead of silently falling through to the hardcoded 9876.
                    // Without this, hooks on a temp runner route their reads at the
                    // primary. The async `get_api_port` IPC + `setApiPort` path
                    // remains the source of truth if the bound port differs from the
                    // intended one (port-fallback rare case).
                    let intended_api_port = crate::mcp::types::get_mcp_api_port();
                    builder = builder.initialization_script(format!(
                        "window.__QONTINUI_PORT__ = {};",
                        intended_api_port
                    ));

                    builder = placement.configure_builder(builder);

                    match builder.build() {
                        Ok(win) => {
                            placement.finalize(&win);
                            let _ = win.show();
                            let _ = win.set_focus();
                            info!(
                                "Main window created (secondary={}, isolated={}, placement={:?})",
                                is_secondary,
                                data_dir.is_some(),
                                placement
                            );
                        }
                        Err(e) => {
                            error!("Failed to create main window: {}", e);
                            return Err(Box::new(e));
                        }
                    }

                    // Phase 2: recreate persisted pop-out terminal windows now
                    // that the main window is up. Best-effort by contract — never
                    // blocks startup; a per-window build failure is logged and
                    // skipped. Only runs in windowed mode (inside this `else`), so
                    // server/headless instances never spawn pop-outs.
                    let restored = commands::terminal_windows::restore_pop_out_windows(
                        app.handle(),
                        &app.state::<std::sync::Arc<window_assignments::WindowAssignments>>(),
                    );
                    if restored > 0 {
                        info!(restored, "Recreated pop-out terminal windows on boot");
                    }
                }
            }

            // SQLite→PG data migration removed (migration complete, PG is primary)

            // Start MCP API server in background BEFORE any synchronous bridge/seed
            // work below. Spawning early lets /health bind and respond during the
            // WebView2 cold-profile init that follows, preventing supervisor health
            // probes from timing out on temp runners. The server only needs the
            // captures made above (mcp_app_state, mcp_rag_state, api_port,
            // mcp_instance_manager), all of which are populated earlier in main().
            let api_port = crate::mcp::types::get_mcp_api_port();
            info!("Starting MCP API server on port {}", api_port);
            let mcp_app_handle = app.handle().clone();
            let mcp_instance_manager = app.state::<Arc<instance_manager::InstanceManager>>().inner().clone();
            tauri::async_runtime::spawn(async move {
                info!("MCP API server task starting...");
                match mcp_api::start_server(mcp_app_state, mcp_rag_state, mcp_app_handle, api_port, mcp_instance_manager).await {
                    Ok(_) => info!("MCP API server stopped normally"),
                    Err(e) => error!("MCP API server error: {}", e),
                }
            });

            // Scan for crash dumps written by the previous process. Run in
            // parallel with the MCP server spawn above — the scan is a
            // read-only disk walk + ~1 file parse and completes in ms, so we
            // don't bother awaiting it before accepting /health requests.
            // Worst case: a request that lands in the first few ms after
            // startup sees `recent_crash: null` and then the next one flips
            // to the populated value.
            tauri::async_runtime::spawn(async move {
                let dir = crate::logging::get_crash_dump_dir();
                crash_dump_app_state.crash_dumps.scan_on_startup(&dir).await;
            });

            // Rehydrate the productivity-stack upcoming-file registry from
            // PG. The registry is in-memory, so without this any Coordinator
            // / dispatcher lookups in the first few seconds after restart
            // would miss claims attached to non-terminal tasks. See
            // productivity-stack plan §3 "Persistence".
            {
                let upcoming_state: Arc<AppState> =
                    app.state::<Arc<AppState>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    upcoming_state
                        .upcoming_file_registry
                        .rehydrate_from_pg(&upcoming_state.pg_db)
                        .await;
                });
            }

            // Clear runner log files from previous session
            executor::FileLogger::clear_logs();
            dom_capture::DomCaptureLogger::clear_captures();
            // Clear logging.rs-managed files that FileLogger doesn't cover
            let dev_logs = crate::paths::get_dev_logs_dir();
            for filename in ["runner-render.jsonl", "ai-output.jsonl"] {
                let path = dev_logs.join(filename);
                if path.exists() {
                    if let Err(e) = std::fs::remove_file(&path) {
                        warn!("Failed to clear {}: {}", filename, e);
                    }
                }
            }
            info!("Cleared previous runner log files");

            // Seed default log sources if none configured
            settings::seed_default_log_sources_if_empty();

            // Seed demo workflows on first launch (if no demo workflows exist)
            {
                let seed_pg = app
                    .state::<Arc<crate::commands::AppState>>()
                    .pg_db
                    .clone();
                demo_workflows::seed_demo_workflows_if_needed(&seed_pg);

                // Slash command sync and built-in issue pattern seeding removed — all persistence now via PgDb.
            }

            // Window starts maximized via programmatic builder above

            // Initialize bridge manager for multi-bridge support
            // This replaces the legacy single python_bridge with a manager that can handle
            // multiple concurrent bridges (GUI + headless modes)
            info!("Initializing bridge manager");
            let app_state = app.state::<Arc<AppState>>();
            let bridge_manager = Arc::new(executor::BridgeManager::new(app.handle().clone()));

            // Check for headless-only mode from environment variable
            // This is intended for server deployments where GUI is not available
            let headless_only_env = std::env::var("QONTINUI_HEADLESS_ONLY")
                .map(|v| v == "1" || v.to_lowercase() == "true")
                .unwrap_or(false);
            let headless_only = server_mode || headless_only_env;

            if headless_only {
                info!(
                    "Headless-only mode enabled (server_mode={}, QONTINUI_HEADLESS_ONLY={})",
                    server_mode, headless_only_env
                );
                bridge_manager.set_headless_only(true);
            }

            {
                let app_state_for_bridge = app.state::<Arc<AppState>>().inner().clone();
                tauri::async_runtime::block_on(async {
                    let mut guard = app_state_for_bridge.bridge_manager.lock().await;
                    *guard = Some(bridge_manager.clone());
                });
            }
            info!("Bridge manager initialized (headless_only={})", headless_only);

            // Auto-start Python executor for screenshot capture and other features
            // In headless-only mode, creates a headless bridge instead of GUI bridge
            // In normal mode, creates the default GUI bridge via bridge manager
            if headless_only {
                info!("Headless-only mode: skipping default bridge creation (create bridges on-demand via API)");
            } else {
                info!("Auto-starting Python executor via bridge manager");
                tauri::async_runtime::block_on(async {
                    match bridge_manager.get_or_create_default_bridge().await {
                        Ok(bridge_id) => {
                            info!("Python executor auto-started successfully (bridge_id={})", bridge_id);
                        }
                        Err(e) => {
                            error!("Failed to auto-start Python executor: {}", e);
                            error!("Screenshot capture and other features will not work until the executor is started");
                            // Don't fail app startup, just log the error
                        }
                    }
                });
            }

            // Initialize extraction executor (starts on-demand when first extraction is requested)
            info!("Initializing extraction executor (will start on-demand)");
            let mut extraction_lock = crate::safe_lock::safe_lock_or_recover(&app_state.extraction_executor, "extraction_executor");
            let extraction_executor = executor::ExtractionExecutor::new(app.handle().clone());
            *extraction_lock = Some(extraction_executor);
            drop(extraction_lock);

            // MCP API server already spawned earlier (before bridge/seed work) so
            // that /health binds during WebView2 cold-profile init.

            // Start heartbeat background task for fleet registration
            heartbeat::start_heartbeat(heartbeat_app_state);

            // Legacy headless/back-end email/password auto-login REMOVED
            // (Cognito-legacy-auth teardown). The web backend is Cognito-only;
            // there is no `/jwt/login` for a headless temp runner to call, and
            // Cognito PKCE inherently requires a browser. Supervisor-spawned
            // runners now reach Tier 2 via the browser/SSO Cognito sign-in
            // (`commands::auth::cognito_sign_in`) like any other runner; once a
            // device-JWT is persisted, the device-JWT refresher keeps it fresh.

            // Phase 3 — web-backend integration is now driven entirely by
            // the unified WebSocket relay (`crate::mcp::backend_relay`).
            // The relay is launched from `mcp_api::start_server` once
            // `Arc<ApiState>` is available; it reads `WebIntegrationSettings`
            // on every reconnect and observes `ServerModeState::shutdown()`
            // for hot-reload. There is no separate HTTP register/heartbeat
            // loop in this phase.

            // Register this runner in the PostgreSQL instance registry.
            // Primary registers itself; secondaries register via heartbeat to the primary.
            if !instance::is_secondary() {
                let im = app.state::<Arc<instance_manager::InstanceManager>>().inner().clone();
                let startup_pg = app.state::<Arc<commands::AppState>>().pg_db.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    im.register_self_as_primary().await;
                    // Clean up stale entries from previous sessions
                    instance_health::startup_cleanup(&startup_pg).await;
                });

                // Start background health monitor for secondary instances
                let health_pg = app.state::<Arc<commands::AppState>>().pg_db.clone();
                let health_im = app.state::<Arc<instance_manager::InstanceManager>>().inner().clone();
                instance_health::start_instance_health_monitor(health_pg, health_im);
            }

            // Start scheduler service in background (skip for secondary instances to avoid duplicate executions)
            if instance::is_secondary() {
                info!("Secondary instance — skipping scheduler service");
            } else {
                info!("Starting scheduler service");
                let scheduler_app_state: Arc<commands::AppState> =
                    app.state::<Arc<commands::AppState>>().inner().clone();
                let scheduler_pg = scheduler_app_state.pg_db.clone();
                tauri::async_runtime::spawn(async move {
                    // Wait briefly for MCP API server to bind
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    scheduler_service::start_scheduler_service(scheduler_pg, scheduler_app_state)
                        .await;
                });
            }

            // Phase 1.5 — transcript-tail populator. Watches Claude CLI's
            // per-session JSONL transcripts and writes Edit/Write/MultiEdit
            // touches into `coord.session_touched_files` so the per-terminal
            // commit traffic light has rows to read for PTY-launched AI tabs
            // (the dominant launch path; SDK chat sessions populate via
            // `auto_register_file` and don't need this watcher).
            {
                let tw_pg = app.state::<Arc<commands::AppState>>().pg_db.clone();
                let tw_app_handle = app.handle().clone();
                // Resolve the runner-tracked workspace root (best-effort).
                let workspace_paths: Vec<String> =
                    match crate::mcp::shared::get_workspace_paths_internal() {
                        Ok((root, _, _)) => vec![root.to_string_lossy().to_string()],
                        Err(e) => {
                            tracing::warn!(
                                "transcript_watcher: get_workspace_paths_internal failed: {}",
                                e
                            );
                            Vec::new()
                        }
                    };
                // Commit ↔ session lineage push-report (Population path 2): the
                // watcher reuses the AiCoordRegistrar's outbox to enqueue
                // `commit_report` rows on observed `git push`es.
                let tw_registrar = app
                    .try_state::<Arc<claude_session::coord_register::AiCoordRegistrar>>()
                    .map(|s| s.inner().clone());
                if let Err(e) = crate::terminal::transcript_watcher::start_transcript_watcher(
                    tw_app_handle,
                    tw_pg,
                    workspace_paths,
                    tw_registrar,
                ) {
                    tracing::warn!("transcript watcher failed to start: {}", e);
                }
            }

            // Auto-load the default state machine into the Python bridge so
            // that `POST /state-machine/navigate` and other bridge-dispatched
            // endpoints work immediately after startup without requiring a
            // manual click on the State Machine page's "Load into Runtime"
            // button. Best-effort — never blocks startup, never panics.
            {
                let sm_app_state: Arc<commands::AppState> =
                    app.state::<Arc<commands::AppState>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    crate::mcp::state_machine::auto_load_default_state_machine(&sm_app_state).await;
                });
            }

            // Start the state-discovery drift detector (Step 6). Scores recent
            // co-occurrence observations against the currently loaded compiled
            // SM every 10 min, persists to `state_discovery_drift_scores`, and
            // logs a WARN when fit_score has been below 0.9 for 3 windows in a
            // row. Fire-and-forget: a transient failure inside the loop is
            // logged and the next tick runs normally; process death kills the
            // task cleanly.
            {
                let drift_app_state: Arc<commands::AppState> =
                    app.state::<Arc<commands::AppState>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    crate::state_discovery::run_drift_detector(drift_app_state).await;
                });
            }

            // Start error monitor service in background
            info!("Starting error monitor service");
            let error_monitor_config = ErrorMonitorConfig::default();

            // Start the service and store the handle in AppState (all within async context)
            let app_state_for_error_monitor = app.state::<Arc<AppState>>().inner().clone();
            tauri::async_runtime::spawn(async move {
                // Start the error monitor service (this spawns the service loop internally)
                let error_monitor_handle =
                    start_error_monitor_async(error_monitor_pg, error_monitor_config).await;

                // Store the handle
                let mut handle_lock = app_state_for_error_monitor.error_monitor_handle.lock().await;
                *handle_lock = Some(error_monitor_handle);
                info!("Error monitor service started and handle stored in AppState");
            });

            if server_mode {
                // Server mode forces Restate on; persist so the block below picks it up.
                let mut s = crate::settings::load_settings();
                if !s.restate.enabled {
                    s.restate.enabled = true;
                    if let Err(e) = crate::settings::save_settings(&s) {
                        warn!("Failed to persist restate.enabled=true for server mode: {}", e);
                    } else {
                        info!("Server mode: forced restate.enabled=true");
                    }
                }
            }

            // Initialize process capture manager
            info!("Initializing process capture manager");
            let app_state_for_pcm = app.state::<Arc<AppState>>().inner().clone();
            let app_handle_for_pcm = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Wait for error monitor handle to be ready
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                let error_monitor = {
                    let handle_lock = app_state_for_pcm.error_monitor_handle.lock().await;
                    handle_lock.clone()
                };
                let error_monitor_arc =
                    Arc::new(tokio::sync::RwLock::new(error_monitor));

                let manager = Arc::new(process_capture::ProcessCaptureManager::new(
                    error_monitor_arc,
                    app_handle_for_pcm,
                ));

                // Load configs from settings and register them
                let mut configs = settings::get_managed_process_configs();

                // Backfill build commands for configs that don't have them yet
                // (ProcessConfigExt trait provides backfill_build_command after
                // the DTO moved to qontinui_types::process_management).
                use crate::process_capture::types::ProcessConfigExt;
                for config in &mut configs {
                    config.backfill_build_command();
                }

                // In dev-mode, upgrade legacy configs, dedupe dev-service ports,
                // and inject missing dev services. Persist if anything changed so
                // the cleanup survives future boots (otherwise duplicates can
                // re-accumulate in settings.json across sessions).
                if dev_services::is_dev_mode() {
                    if let Some(workspace) = dev_services::find_workspace_root() {
                        let mutated =
                            dev_services::upgrade_and_dedupe_configs(&mut configs, &workspace);

                        let missing = dev_services::get_missing_dev_services(&workspace, &configs);
                        let injected = !missing.is_empty();
                        if injected {
                            info!(
                                "Dev-mode: injecting {} default dev services for workspace {}",
                                missing.len(),
                                workspace.display()
                            );
                            for svc in &missing {
                                info!("  -> {} (group {}, port {:?})", svc.name, svc.start_group, svc.health_port);
                            }
                            configs.extend(missing);
                        }

                        if mutated || injected {
                            if let Err(e) =
                                settings::replace_managed_process_configs(configs.clone())
                            {
                                warn!(
                                    "Failed to persist dev-service config cleanup: {}. \
                                     Runtime state is still correct but duplicates may \
                                     re-appear on next boot.",
                                    e
                                );
                            } else {
                                info!("Dev-mode: persisted upgraded/deduped/injected configs to settings");
                            }
                        }
                    }
                }

                // Inject Restate server if durable execution is enabled
                let restate_settings = settings::load_settings().restate.clone();
                if restate_settings.enabled {
                    if restate_settings.is_external() {
                        info!(
                            "Restate durable execution enabled — using external server at {} / {}; skipping local spawn",
                            restate_settings.admin_url(),
                            restate_settings.ingress_url()
                        );
                    } else {
                        info!("Restate durable execution enabled — preparing server");
                        let app_data_dir = dirs::data_dir()
                            .unwrap_or_else(|| std::path::PathBuf::from("."))
                            .join("qontinui-runner");
                        match restate::lifecycle::ensure_restate_binary(
                            &restate_settings,
                            &app_data_dir,
                        )
                        .await
                        {
                            Ok(binary_path) => {
                                let restate_config =
                                    restate::lifecycle::build_restate_process_config(
                                        &restate_settings,
                                        &binary_path,
                                        &app_data_dir,
                                    );
                                info!(
                                    "Restate server config: binary={}, health_port={:?}, group={}",
                                    restate_config.command,
                                    restate_config.health_port,
                                    restate_config.start_group
                                );
                                configs.push(restate_config);
                            }
                            Err(e) => {
                                error!("Failed to prepare Restate binary: {} — workflows will use legacy execution", e);
                            }
                        }
                    }
                }

                // Filter out dev_only services in production
                let is_dev = dev_services::is_dev_mode();
                for config in configs {
                    if config.enabled && (is_dev || !config.dev_only) {
                        manager.register(config).await;
                    }
                }

                // Auto-start processes (skip for secondary instances to avoid port conflicts)
                let is_secondary = instance::is_secondary();
                if is_secondary {
                    info!("Secondary instance — skipping managed process auto-start");
                } else {
                    // Reclaim any orphan descendant trees left behind by a
                    // prior runner session that crashed without a graceful
                    // shutdown (Phase 4). PID-reuse-safe via per-PID
                    // creation-time fingerprint; clears the persisted state
                    // file after running so a crash mid-loop doesn't loop.
                    process_capture::orphan_state::reclaim_orphans().await;
                    manager.start_auto_processes().await;
                }

                // Store the manager
                let mut manager_lock = app_state_for_pcm.process_capture_manager.lock().await;
                *manager_lock = Some(manager.clone());
                info!("Process capture manager initialized");

                // After processes are started, start Restate HTTP endpoint and register with server
                if restate_settings.enabled {
                    // Start the Restate service HTTP endpoint (where Restate calls back into)
                    #[cfg(feature = "restate")]
                    {
                        let endpoint_port = restate_settings.service_endpoint_port;
                        let restate_app_state = app_state_for_pcm.clone();
                        let restate_config_storage = match crate::config_storage::ConfigStorage::new() {
                            Ok(cs) => Arc::new(tokio::sync::Mutex::new(cs)),
                            Err(e) => {
                                warn!("ConfigStorage init for Restate failed, using degraded: {}", e);
                                Arc::new(tokio::sync::Mutex::new(
                                    crate::config_storage::ConfigStorage::new_degraded()
                                ))
                            }
                        };
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = restate::http_endpoint::start_restate_endpoint(
                                endpoint_port,
                                restate_app_state,
                                restate_config_storage,
                            ).await {
                                error!("Restate HTTP endpoint failed: {}", e);
                            }
                        });
                    }

                    // Register our endpoint with the Restate server once both are ready
                    let rs = restate_settings.clone();
                    tauri::async_runtime::spawn(async move {
                        // When using an external Restate server we cannot probe a
                        // local port for it — trust that it's up and skip the wait.
                        let admin_ready = if rs.is_external() {
                            true
                        } else {
                            crate::process_capture::health::wait_for_port_ready(
                                rs.admin_port,
                                std::time::Duration::from_secs(60),
                            )
                            .await
                        };

                        if admin_ready {
                            // Also wait for our service endpoint to be ready
                            if crate::process_capture::health::wait_for_port_ready(
                                rs.service_endpoint_port,
                                std::time::Duration::from_secs(30),
                            )
                            .await
                            {
                                // Register the runner's service endpoint with Restate
                                if let Err(e) = restate::lifecycle::register_service_endpoint(
                                    &rs,
                                    rs.service_endpoint_port,
                                )
                                .await
                                {
                                    error!("Failed to register Restate service endpoint: {}", e);
                                }
                            } else {
                                error!("Restate service endpoint port {} not ready after 30s", rs.service_endpoint_port);
                            }
                        } else {
                            error!("Restate admin port {} not ready after 60s", rs.admin_port);
                        }
                    });

                    // Start health watchdog (no-op when external)
                    restate::lifecycle::spawn_restate_watchdog(
                        manager.clone(),
                        restate_settings.clone(),
                    );
                }
            });

            // Start Doctor health monitoring service in background
            info!("Starting Doctor health monitoring service");
            let app_state_for_doctor = app.state::<Arc<AppState>>().inner().clone();
            let app_handle_for_doctor = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let doctor_config = DoctorConfig::default();
                let (doctor_handle, mut doctor_event_rx) =
                    start_doctor_async(doctor_config).await;

                // Store the handle in AppState
                let mut handle_lock = app_state_for_doctor.doctor_handle.lock().await;
                *handle_lock = Some(doctor_handle);
                info!("Doctor service started and handle stored in AppState");

                // Bridge Doctor events to Tauri frontend events
                drop(handle_lock); // Release lock before entering event loop
                while let Some(event) = doctor_event_rx.recv().await {
                    let event_name = match &event {
                        doctor::DoctorEvent::Warning { .. } => "doctor-warning",
                        doctor::DoctorEvent::Stuck { .. } => "doctor-stuck",
                        doctor::DoctorEvent::Healthy { .. } => "doctor-healthy",
                    };
                    if let Err(e) = tauri::Emitter::emit(&app_handle_for_doctor, event_name, &event) {
                        tracing::warn!("Failed to emit Doctor event: {}", e);
                    }
                }
            });

            // Embedding backfill job removed (SQLite embeddings deprecated, PG handles this)

            // Start auto-yield-on-idle policy task (lock-yield-protocol-plan
            // §Open Q4). Polls every 5s; releases held file locks when the
            // holder has been stdout-idle for `idle_threshold_secs` AND
            // the oldest waiter has been blocked for `min_wait_secs`.
            // Disabled by default; user opts in via Settings → Lock-yield.
            info!("Starting auto-yield-on-idle policy task");
            {
                let app_handle_for_yield = app.handle().clone();
                let app_state_for_yield = app.state::<Arc<AppState>>().inner().clone();
                let session_manager_for_yield = app
                    .state::<Arc<claude_session::SessionManager>>()
                    .inner()
                    .clone();
                executor::auto_yield_policy::spawn(
                    app_handle_for_yield,
                    app_state_for_yield.file_lock_manager.clone(),
                    session_manager_for_yield,
                    app_state_for_yield.event_broadcast.clone(),
                );
            }

            // Start memory consolidation scheduler in background
            info!("Starting memory consolidation scheduler");
            let scheduler_pg = app.state::<Arc<AppState>>().inner().pg_db.clone();
            memory::scheduler::start_memory_scheduler(
                scheduler_pg,
                memory::scheduler::MemorySchedulerConfig::default(),
            );

            // Start the productivity-stack session-file-snapshot pruner.
            // 24h interval; 7-day retention. See
            // `database/pg/session_file_snapshots.rs::SNAPSHOT_RETENTION_DAYS`.
            info!("Starting session-file-snapshot pruner");
            let snapshot_pruner_pg = app.state::<Arc<AppState>>().inner().pg_db.clone();
            crate::database::pg::session_file_snapshots::start_session_snapshot_pruner(
                snapshot_pruner_pg,
            );

            // Start the Rust deconflicter loop — §4.1 of
            // plans/2026-05-13-coord-as-deconflicter-plan.md. Consumes
            // the `touch_events_rx` we created alongside AppState's
            // `touch_events_tx`. The loop runs until process exit (the
            // sender lives in AppState; receivers exit cleanly on
            // `RecvError::Closed`).
            info!("Starting Rust deconflicter loop");
            let deconflicter_pg = app.state::<Arc<AppState>>().inner().pg_db.clone();
            let deconflicter_app_handle = app.handle().clone();
            crate::coordinator::deconflicter::DeconflicterLoop::start(
                deconflicter_pg,
                deconflicter_app_handle,
                touch_events_rx,
            );

            // Start dreamer (formal reasoning) scheduler in background
            info!("Starting dreamer scheduler");
            let dreamer_pg = app.state::<Arc<AppState>>().inner().pg_db.clone();
            memory::scheduler::start_dreamer_scheduler(
                dreamer_pg,
                memory::scheduler::MemorySchedulerConfig::default(),
            );

            // Auto-start MCP servers marked with auto_start in background
            let app_state_for_mcp_auto = app.state::<Arc<AppState>>().inner().clone();
            tauri::async_runtime::spawn(async move {
                let mcp_manager = app_state_for_mcp_auto.mcp_client_manager.lock().await;
                mcp_manager.start_auto_start_servers().await;
            });

            // Cloud relay auto-start is handled in mcp_api.rs where ApiState is available

            // Forward circuit breaker state-change events to the frontend
            let app_handle_cb = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut rx = crate::ai_provider::circuit_breaker::event_receiver();
                loop {
                    match rx.recv().await {
                        Ok(event) => {
                            let _ = tauri::Emitter::emit(&app_handle_cb, "circuit-breaker-change", &event);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Circuit breaker event listener lagged by {} events", n);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            // Check Claude CLI auth status on startup
            let app_handle_for_auth = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Small delay to let the frontend initialize
                tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

                let status = commands::ai_settings::get_cli_auth_status();
                if status.is_cli_provider && status.expired {
                    tracing::warn!(
                        "Claude CLI OAuth token is expired (expired {} minutes ago)",
                        status.minutes_until_expiry.unwrap_or(0).abs()
                    );
                    if let Err(e) =
                        tauri::Emitter::emit(&app_handle_for_auth, "cli-auth-expired", &status)
                    {
                        tracing::warn!("Failed to emit cli-auth-expired event: {}", e);
                    }
                } else if status.is_cli_provider && status.has_credentials {
                    info!(
                        "Claude CLI auth valid ({} minutes remaining)",
                        status.minutes_until_expiry.unwrap_or(0)
                    );
                }

                // Weekly-usage-aware account selection. `pick_best_account`
                // internally checks `account_selection_mode == LeastUsage`
                // and returns immediately otherwise — no need to gate the
                // call here. Refresh the usage snapshot FIRST so the initial
                // pick can rank accounts by weekly-usage headroom rather than
                // falling back to cooldown-only ordering.
                info!("Refreshing Claude account usage snapshot at startup...");
                commands::ai_settings::refresh_account_usage_snapshot().await;
                info!("Running account selection at startup...");
                tokio::task::spawn_blocking(ai_provider::pick_best_account)
                    .await
                    .ok();

                // Keep the usage snapshot fresh for headless / co-pilot-only
                // runners whose Settings/Terminal UI never polls usage. This
                // loop ONLY refreshes the cache — re-picking is deferred to
                // the next unit of AI work (so warm-provider prompt-cache
                // locality within a unit is preserved). `refresh_*` is a
                // no-op unless ≥2 accounts are configured.
                tauri::async_runtime::spawn(async {
                    let mut tick =
                        tokio::time::interval(tokio::time::Duration::from_secs(10 * 60));
                    tick.tick().await; // consume the immediate first tick (just refreshed)
                    loop {
                        tick.tick().await;
                        commands::ai_settings::refresh_account_usage_snapshot().await;
                    }
                });
            });

            // Bootstrap World State Verifier live config from persisted
            // settings. Must run before any agentic verification iteration
            // so the loop picks up the persisted mode/endpoint/model.
            // Falls back to env vars when no persisted settings exist.
            verification::WsvConfig::init_from_persisted();

            // Restore previously-running instances (primary instance only).
            // The session file only exists if the previous process was killed
            // (e.g. by a rebuild) rather than closed intentionally by the user.
            if !instance::is_secondary() {
                let restore_ids = instance_manager::load_and_clear_active_instances();
                if !restore_ids.is_empty() {
                    info!(
                        "Restoring {} previously-active instance(s): {:?}",
                        restore_ids.len(),
                        restore_ids
                    );
                    let im = app.state::<Arc<instance_manager::InstanceManager>>().inner().clone();
                    let app_handle_for_restore = app.handle().clone();
                    tauri::async_runtime::spawn(async move {
                        // Brief delay to let the primary instance finish initializing
                        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                        let configs = settings::get_runner_instances();
                        let mut restored = 0u32;
                        for id in &restore_ids {
                            let Some(config) = configs.iter().find(|c| &c.id == id) else {
                                tracing::warn!(
                                    "Instance '{}' was active but no longer in settings — skipping",
                                    id
                                );
                                continue;
                            };

                            // Wait for the port to become free (previous process may still be dying)
                            if crate::process_capture::health::is_port_in_use(config.port) {
                                info!(
                                    "Port {} still in use, waiting up to 5 s for instance '{}'",
                                    config.port, config.name
                                );
                                let free = tokio::task::spawn_blocking({
                                    let port = config.port;
                                    move || instance_manager::wait_for_port_free(
                                        port,
                                        std::time::Duration::from_secs(5),
                                    )
                                })
                                .await
                                .unwrap_or(false);
                                if !free {
                                    error!(
                                        "Port {} still occupied — skipping restore of instance '{}'",
                                        config.port, config.name
                                    );
                                    continue;
                                }
                            }

                            match im
                                .launch_instance_with_app(config, Some(&app_handle_for_restore))
                                .await
                            {
                                Ok(pid) => {
                                    info!(
                                        "Restored instance '{}' (PID: {}, port: {})",
                                        config.name, pid, config.port
                                    );
                                    restored += 1;
                                }
                                Err(e) => {
                                    error!("Failed to restore instance '{}': {}", config.name, e);
                                }
                            }
                        }

                        // Notify the frontend so the instances panel refreshes immediately
                        if restored > 0 {
                            let _ = tauri::Emitter::emit(
                                &app_handle_for_restore,
                                "runner-instances-restored",
                                &serde_json::json!({ "count": restored }),
                            );
                        }
                    });
                }
            }

            // Clean up old security audit events based on retention policy
            {
                let security_settings = crate::settings::get_security_settings();
                if security_settings.audit_enabled {
                    let pg_for_audit = app.state::<Arc<commands::AppState>>().pg_db.clone();
                    let retention_days = security_settings.audit_retention_days;
                    tauri::async_runtime::spawn(async move {
                        match pg_for_audit.cleanup_old_audit_events(retention_days).await {
                            Ok(deleted) if deleted > 0 => {
                                info!("Audit cleanup: deleted {} events older than {} days", deleted, retention_days);
                            }
                            Ok(_) => {}
                            Err(e) => {
                                warn!("Audit cleanup failed: {}", e);
                            }
                        }
                    });
                }
            }

            // Spawn the process log cleanup loop + orphaned-session cleanup.
            {
                let pg_for_logs = app.state::<Arc<commands::AppState>>().pg_db.clone();
                let pg_for_orphans = pg_for_logs.clone();

                // Capture the cutoff BEFORE spawning the cleanup task. Only
                // sessions started before this timestamp are considered
                // orphans, which protects new sessions created by the current
                // runner's event_loop from being clobbered.
                let orphan_cutoff = chrono::Utc::now().to_rfc3339();

                // Mark orphaned 'running' sessions as 'failed' on startup.
                // These can only exist if a previous runner died unexpectedly
                // (the event_loop normally updates state on natural exit).
                tauri::async_runtime::spawn(async move {
                    match pg_for_orphans
                        .mark_orphaned_running_sessions_as_failed(&orphan_cutoff)
                        .await
                    {
                        Ok(n) if n > 0 => {
                            info!(
                                "Marked {} orphaned 'running' process sessions as 'failed'",
                                n
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!("Failed to clean up orphaned process sessions: {}", e);
                        }
                    }
                });

                tauri::async_runtime::spawn(async move {
                    process_capture::cleanup::run_process_log_cleanup_loop(pg_for_logs).await;
                });
            }

            info!("Tauri application setup complete");
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                info!("Window close requested");

                // Phase 1: a pop-out terminal window ("term-N") closing must NOT
                // run the main-window app-quit cleanup below. Reassign its
                // sessions back to "main" (never orphan a PTY), emit the
                // window-closed / session-assignment-changed events, and return.
                // Only the "main" window falls through to the shutdown path.
                let win_label = window.label().to_string();
                if win_label != window_assignments::MAIN_WINDOW_LABEL
                    && commands::terminal_windows::handle_window_close(window.app_handle(), &win_label)
                {
                    return;
                }

                // Phase 2: the main window is closing → the app is going down.
                // Flag the shutdown FIRST so any pop-out `CloseRequested` that
                // fires during teardown PRESERVES its record (restored next
                // boot) rather than removing it, then snapshot every open
                // pop-out's final geometry while the windows still exist.
                commands::terminal_windows::mark_app_quitting();
                if let Some(wa) =
                    window.try_state::<Arc<window_assignments::WindowAssignments>>()
                {
                    commands::terminal_windows::capture_open_geometry(window.app_handle(), &wa);
                }

                let app_state = window.state::<Arc<AppState>>();

                // Intentional close — clear the session file so instances are NOT
                // restored on the next normal startup.  (If the process is killed
                // by a rebuild, this handler doesn't run and the file persists,
                // which is exactly what we want.)
                instance_manager::clear_active_instances();

                // Deregister this instance from the runner_instances table.
                // For the primary: mark stopped so secondaries know it's gone.
                // For secondaries: remove the row entirely and notify the primary.
                {
                    let pg = app_state.pg_db.clone();
                    let own_port = app_state
                        .api_port
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let is_secondary = instance::is_secondary();
                    let primary_port = instance::primary_port();
                    let id = format!(
                        "{}-{}",
                        if is_secondary { "ext" } else { "primary" },
                        own_port
                    );

                    // Best-effort: spawn a quick runtime to clean up DB + HTTP
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build();
                        if let Ok(rt) = rt {
                            rt.block_on(async {
                                if is_secondary {
                                    // Remove from DB
                                    let _ = pg.remove_runner_instance(&id).await;
                                    // Also try removing by port (covers different ID formats)
                                    let _ = pg
                                        .cleanup_dead_runner_instances(0)
                                        .await;
                                    info!(
                                        "Deregistered secondary instance (port={}) from DB",
                                        own_port
                                    );

                                    // Notify primary (best-effort)
                                    if let Some(pp) = primary_port {
                                        let client = reqwest::Client::builder()
                                            .timeout(std::time::Duration::from_secs(2))
                                            .build()
                                            .ok();
                                        if let Some(client) = client {
                                            let url = format!(
                                                "http://127.0.0.1:{}/instances/{}/stop",
                                                pp, id
                                            );
                                            let _ = client.post(&url).send().await;
                                        }
                                    }
                                } else {
                                    // Primary: mark as stopped (not delete — secondaries
                                    // may query it to detect primary is gone)
                                    let _ = pg
                                        .update_runner_instance_heartbeat(
                                            &id, Some(0), "stopped",
                                        )
                                        .await;
                                    info!("Marked primary instance as stopped in DB");
                                }
                            });
                        }
                    });
                }

                // ── Explicit shutdown ordering ──
                let app_state_clone = app_state.inner().clone();

                let shutdown_handle = std::thread::spawn(move || {
                    // Build a small current-thread runtime for the cleanup work
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build();

                    match rt {
                        Ok(rt) => {
                            // Stop all bridges via bridge manager
                            rt.block_on(async {
                                let manager_guard =
                                    app_state_clone.bridge_manager.lock().await;
                                if let Some(ref manager) = *manager_guard {
                                    info!("Stopping all bridges via bridge manager");
                                    manager.remove_all().await;
                                }
                            });

                            // Release ADB forwards and reverses installed by the
                            // USB scanner so they don't linger in
                            // `adb forward --list` / `adb reverse --list` across
                            // runner restarts. Graceful path only — the
                            // supervisor force-kills via taskkill /F and this
                            // code never runs for temp runners. See plan
                            // adb-forwarder-port.md §1.6a.
                            rt.block_on(async {
                                if let Some(usb) =
                                    app_state_clone.usb_transport.get()
                                {
                                    info!("Releasing ADB forwards and reverses on shutdown");
                                    usb.release_all().await;
                                }
                            });
                        }
                        Err(e) => {
                            error!("Failed to create shutdown runtime: {}", e);
                        }
                    }

                    // Stop the extraction executor. Safe to call here now that it no longer
                    // owns an Arc<tokio::runtime::Runtime> — stop_internal() is pure
                    // synchronous Python-subprocess teardown.
                    if let Ok(mut guard) = app_state_clone.extraction_executor.lock() {
                        if let Some(mut ee) = guard.take() {
                            info!("Stopping extraction executor");
                            let _ = ee.stop();
                        }
                    }
                });

                // Wait for the dedicated shutdown thread, but with a hard
                // upper bound so a hung bridge or extractor can't freeze the
                // close handler (and with it the whole window). Without this
                // cap users see the X button "do nothing" — the handler is
                // actually running, just blocked on a shutdown step.
                //
                // Any cleanup still running past the deadline continues in
                // the background thread after we return from the close
                // handler. The process then exits on its normal Tauri path;
                // if something is still holding it up, the explicit
                // `std::process::exit` below forces termination.
                const SHUTDOWN_JOIN_DEADLINE_MS: u64 = 3_000;
                let deadline = std::time::Instant::now()
                    + std::time::Duration::from_millis(SHUTDOWN_JOIN_DEADLINE_MS);
                // Poll instead of blocking `.join()` so we can bail out on
                // timeout. `JoinHandle::is_finished` is stable and doesn't
                // require `nightly`.
                while !shutdown_handle.is_finished() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                if !shutdown_handle.is_finished() {
                    warn!(
                        "Shutdown thread did not finish within {}ms — continuing \
                         window close; any remaining cleanup will be killed by \
                         process exit",
                        SHUTDOWN_JOIN_DEADLINE_MS
                    );
                }

                // ── Graceful drain (Phase 2) ──
                //
                // Before the hard session-close + taskkill below, run the
                // bounded drain so even a DIRECT exit (X button / programmatic
                // app.exit that didn't route through the supervisor's
                // POST /drain) flushes in-flight AI turns to output_log,
                // stashes dirty worktrees to refs/wip/*, and heartbeats coord
                // claims. `drain()` is idempotent: if the supervisor already
                // hit POST /drain, this returns instantly (already_drained).
                //
                // The timeout is clamped well under the force-exit watchdog so
                // a stuck session can't wedge the close handler. The drain runs
                // synchronously on this thread (it's pure blocking git +
                // bounded polling), which is fine — the watchdog at the bottom
                // is the ultimate backstop.
                {
                    let exit_drain_timeout = std::cmp::min(
                        drain::configured_timeout(),
                        std::time::Duration::from_secs(8),
                    );
                    let summary =
                        drain::drain(&window.app_handle().clone(), exit_drain_timeout);
                    info!(
                        "Exit-seam drain: drained={} wip_refs={} claims={} timed_out={} \
                         elapsed_ms={} already_drained={}",
                        summary.drained_sessions,
                        summary.wip_refs_written,
                        summary.claims_persisted,
                        summary.timed_out,
                        summary.elapsed_ms,
                        summary.already_drained
                    );

                    // Phase 4 — the ExitRequested seam is the catch-all CLEAN
                    // shutdown signal: even a direct X-button / programmatic
                    // app.exit that produced an `already_drained` no-op above
                    // (or ran with zero sessions) is still a PLANNED exit, not
                    // a crash. Stamp the marker `clean:true` here unconditionally
                    // so the next boot's resume classifies a quiet (planned)
                    // restart. `drain()` also stamps it on a fresh pass; this is
                    // the idempotent backstop for the no-op / zero-session case.
                    let marker_path = session::shutdown_marker::marker_path(
                        crate::mcp::types::get_mcp_api_port(),
                    );
                    session::shutdown_marker::mark_clean_shutdown(&marker_path);
                }

                // Close all interactive Claude sessions
                if let Some(sm) =
                    window.try_state::<Arc<claude_session::SessionManager>>()
                {
                    sm.close_all_sessions();
                }

                // Close all embedded terminal sessions
                if let Some(tm) =
                    window.try_state::<Arc<terminal::TerminalManager>>()
                {
                    tm.close_all();
                }

                // Kill any orphaned AI (Claude CLI) processes tracked by the PID tracker.
                // This catches processes that survived session close (e.g., cmd.exe /c claude).
                {
                    let pids_to_kill: Vec<u32> = {
                        if let Ok(mut pids) = app_state.ai_pid_tracker.lock() {
                            let copy = pids.clone();
                            pids.clear();
                            copy
                        } else {
                            Vec::new()
                        }
                    };
                    if !pids_to_kill.is_empty() {
                        info!(
                            "Killing {} orphaned AI process(es) on shutdown: {:?}",
                            pids_to_kill.len(),
                            pids_to_kill
                        );
                        for pid in &pids_to_kill {
                            let result = std::process::Command::new("taskkill")
                                .args(["/F", "/T", "/PID", &pid.to_string()])
                                .output();
                            match result {
                                Ok(output) => {
                                    if output.status.success() {
                                        info!("Killed AI process tree for PID {}", pid);
                                    } else {
                                        // Process may have already exited — not an error
                                        info!("AI process PID {} already exited", pid);
                                    }
                                }
                                Err(e) => {
                                    error!("Failed to taskkill PID {}: {}", pid, e);
                                }
                            }
                        }
                    }
                }

                // Stop all managed processes
                let app_state_pcm = app_state.inner().clone();
                tauri::async_runtime::spawn(async move {
                    let manager_lock = app_state_pcm.process_capture_manager.lock().await;
                    if let Some(ref manager) = *manager_lock {
                        info!("Stopping all managed processes");
                        manager.stop_all().await;
                        info!("All managed processes stopped");
                    }
                });

                // Stop error monitor service
                let app_state_clone2 = app_state.inner().clone();
                tauri::async_runtime::spawn(async move {
                    let handle_lock = app_state_clone2.error_monitor_handle.lock().await;
                    if let Some(ref handle) = *handle_lock {
                        info!("Stopping error monitor service");
                        if let Err(e) = handle.stop().await {
                            error!("Failed to stop error monitor service: {}", e);
                        } else {
                            info!("Error monitor service stopped");
                        }
                    }
                });

                // Stop Doctor health monitoring service
                let app_state_clone3 = app_state.inner().clone();
                tauri::async_runtime::spawn(async move {
                    let handle_lock = app_state_clone3.doctor_handle.lock().await;
                    if let Some(ref handle) = *handle_lock {
                        info!("Stopping Doctor service");
                        if let Err(e) = handle.shutdown().await {
                            error!("Failed to stop Doctor service: {}", e);
                        } else {
                            info!("Doctor service stopped");
                        }
                    }
                });

                // Stop trigger service
                tauri::async_runtime::spawn(async move {
                    crate::trigger_system::stop_trigger_service().await;
                });

                // ── Explicit exit request ──
                //
                // Tauri's automatic "last-window-closed → exit" chain breaks
                // in this app:
                //   - WebView2's destroy on Windows sometimes hangs (see the
                //     `Failed to unregister class Chrome_WidgetWin_0` error
                //     that surfaces when the process is force-killed later).
                //   - Long-lived `tauri::async_runtime::spawn` tasks (mDNS
                //     scanner, workflow event bus, instance manager, backend
                //     relay poll, AI-settings checker) never complete, so
                //     Tauri's runtime shutdown has no clean point to drain.
                //
                // Calling `app_handle.exit(0)` explicitly bypasses the
                // window-destruction path and fires `RunEvent::ExitRequested`
                // directly. Tauri then aborts the runtime and returns from
                // `app.run`, and the process exits as soon as `main()`
                // returns. This is the polite, ordered shutdown path.
                let exit_handle = window.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    // Let the other spawned cleanup tasks run their first
                    // tick before we pull the rug out — in practice they
                    // all complete within ~500ms of the handler returning.
                    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
                    info!("Requesting Tauri app exit");
                    exit_handle.exit(0);
                });

                // ── Force-exit watchdog (safety net) ──
                //
                // Even with the explicit `app_handle.exit(0)` above, Tauri's
                // event loop can still stall if WebView2 or a tao internal
                // handler is blocked. This detached OS thread is the last
                // line of defense: after a short grace period it calls
                // `std::process::exit(0)` unconditionally. If Tauri exits
                // cleanly first, the thread is killed with the process and
                // the force-exit is never reached.
                //
                // Grace is short (3s) because our cleanup completes in
                // ~1.2s and `app_handle.exit(0)` is scheduled for 1.5s.
                // Anything past 3s is definitely stalled.
                const FORCE_EXIT_GRACE_SECS: u64 = 3;
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(
                        FORCE_EXIT_GRACE_SECS,
                    ));
                    warn!(
                        "Force-exit watchdog: Tauri did not terminate within \
                         {}s of the close handler returning — exiting process",
                        FORCE_EXIT_GRACE_SECS
                    );
                    std::process::exit(0);
                });
            }
        })
        .build(tauri::generate_context!())?;

    info!("Tauri application built successfully");
    app.run(|_, event| {
        if let tauri::RunEvent::ExitRequested { .. } = event {
            info!("Application exit requested");
            // Phase 2: ensure the shutdown flag is set on ANY exit path (incl.
            // a programmatic `app.exit(0)` that didn't route through the main
            // window's close handler), so pop-out teardown preserves records.
            commands::terminal_windows::mark_app_quitting();
        }
    });

    Ok(())
}
