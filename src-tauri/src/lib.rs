// Large `serde_json::json!` macros in the database export need this. Moved
// from `main.rs` with the tree — `database` compiles here now.
#![recursion_limit = "256"]
// Many modules are in active development with planned integrations.
#![allow(dead_code)]
// API response types are intentionally detailed.
#![allow(clippy::type_complexity)]
// Refactoring to structs is tracked separately.
#![allow(clippy::too_many_arguments)]

use base64::{engine::general_purpose::STANDARD, Engine};
use std::fs;
use std::sync::OnceLock;
use tauri::Manager;

pub mod accessibility;
// Pure install-interception core (classify + gate + wire types), shared by the
// `qontinui-runner` bin (via `install_effects_producer::intercept`) AND the
// standalone `qontinui-shim` Windows `.exe` shadow stub. Lifted into the lib
// crate because a second bin cannot import from the runner bin's module tree.
// Pure logic only — no async, no coord/keyring deps. See the module doc.
pub mod intercept_core;
pub mod observable_bridge;
// Windows CREATE_NO_WINDOW spawn helpers. Declared in BOTH the lib and the
// runner bin (same file, like `coord_doctor`) so lib-crate modules
// (`profile_cli`, `env_agent`) can suppress console-window flashes too.
pub mod process_helpers;
pub mod profile_cli;
// Out-of-process runner discovery: the bound-API-port breadcrumb. In the LIB
// crate for the same reason as `intercept_core` — the WRITER is the runner bin
// (`intercept::set_bound_port`) and a READER is the standalone `qontinui-shim`
// `.exe`, and a second bin cannot import from the runner bin's module tree. One
// module ⇒ one schema ⇒ writer and reader cannot drift. See the module doc.
pub mod profiles;
pub mod relay_envelopes;
pub mod runner_breadcrumb;
pub mod schema_export;
pub mod tauri_event_payloads;

// Temp-file-then-rename writer. Declared in BOTH the lib and the runner bin
// (same file, like `process_helpers` / `coord_doctor`) because
// `secure_storage` — which compiles into both crates — needs it: the encrypted
// token store MUST be written atomically, or a reader racing the device-JWT
// refresher's ~5-minute rewrite sees a truncated file and reports the store
// corrupt (which, with the fail-closed sign-out marker, reads as a logout the
// operator never performed).
pub mod fs_atomic;

// Exposed for the `qontinui_profile device pair` CLI (and any other binary
// that needs the encrypted token store outside the Tauri runtime). Both
// modules are Tauri-free.
pub mod auth;
pub mod fs_perms;
/// Canonical reader for `~/.qontinui/machine.json` — the machine's ONE
/// durable `device_id`. Declared in `main.rs` too, because `auth` compiles
/// into both crates and must consult it before falling back to its own cache.
pub mod machine_identity;
pub mod secure_storage;

// Machine-side dev-environment capture agent (feat/devenv-environments). Runs
// on a developer's machine, captures that machine's real dev-environment
// configuration (SECRET-FREE), and POSTs it to the qontinui-web backend so the
// server computes drift vs a canonical machine. Auth is a per-machine API key
// (`X-Machine-Key: mk_<token>`), NOT a user JWT. Lives in the lib crate so both
// the `qontinui_profile env` CLI and the Tauri runner GUI share one code path.
pub mod env_agent;

// Device-pairing flow (headless + browser-mediated). Lifted out of
// `bin/qontinui_profile.rs` so both the CLI and the Tauri runner GUI
// share one code path. See `pair.rs` for the canonical wire shapes.
pub mod pair;

// Cognito Hosted-UI sign-in (RFC 8252 PKCE). Phase 5 of the
// unified-Cognito-identity plan. Tauri-free (loopback + system browser); the
// `cognito_sign_in` Tauri command in `commands::auth` drives it.
pub mod cognito;

// Claude Code process-topology env markers and the strip rule that governs
// them (plan `2026-07-28-runner-transcript-persistence-env-leak`). One home for
// the marker names so the spawn-site strips, the startup warning and the
// `coord doctor` check can never disagree on the spelling.
pub mod claude_env;

// `coord doctor` self-check (plan 2026-06-13 Phase 4). Lifted into the lib so
// BOTH the standalone `coord_doctor` bin and the in-app Tauri command
// (`crate::coord_doctor` in the runner binary) share one driver + formatter +
// the 7-check wiring. Reuses the lib's `auth`/`pair`/`secure_storage`/`profiles`
// modules (which compile into the runner binary too), so the bin and the
// command produce an identical report.
pub mod coord_doctor;

// Tier-0 "Looping Agent" pure core (plan `merge-shepherd-fixer-PLAN.md`
// Phase 1): registry types + JSON store, rendered-grid idle/context-low
// predicates, the supervisor's per-tick decision core, and the bundled
// playbook + prompt builders. Lives in the lib crate so the whole decision
// surface is type-checked and tested by `cargo test --lib` (the runner bin's
// module tree is silently skipped by `--lib`); the impure glue that spawns
// visible tabs lives in the bin at `src/looping_agent_supervisor.rs`.
pub mod looping_agent;

// Harness markdown -> work-unit adapter (plan
// `2026-06-18-harness-markdown-to-workunit-adapter`, P2 of the plan-decoupling
// program). Phase 1 = the pure parser that turns operator plan markdown into a
// structured work-unit (slug + opaque status + phase sub-units + dependency
// edges), richer than coord's old slug+status projection. Later phases add the
// push client + trigger as siblings under this module.
pub mod plan_workunit_adapter;

// ============================================================================
// Test-only: shared process-wide env lock
// ============================================================================

/// A single process-wide lock that serializes every test which reads or
/// mutates a `std::env` variable. `std::env` is process-global, so two tests
/// touching the same var in parallel race — one clobbers the value mid-read,
/// the code-under-test sees the wrong value, and CI reddens
/// non-deterministically (the flake class fixed 2026-07-11; cf.
/// `qontinui_shim::resolve_real_in`). ONE lock per test binary is the correct
/// granularity: the lib test binary and the runner-bin test binary run as
/// separate processes, so each crate root (`lib.rs`, `main.rs`) defines its
/// own. Poison-recovering so a panicking test can't cascade-fail the rest.
#[cfg(test)]
pub(crate) mod test_env {
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Acquire the shared env lock. Hold the returned guard for the whole
    /// body of any test that touches `std::env`.
    pub(crate) fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// RAII guard that restores the captured env vars to their pre-capture
    /// values on drop (including the panic path). Use for tests that mutate a
    /// process-global var which may already be set in the environment (e.g.
    /// `DATABASE_URL` in dev / DB-gated CI) so the test can't leak its value —
    /// or its removal — to sibling tests in the same binary.
    pub(crate) struct EnvVarRestore {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvVarRestore {
        pub(crate) fn capture(keys: &[&'static str]) -> Self {
            let saved = keys.iter().map(|&k| (k, std::env::var_os(k))).collect();
            Self { saved }
        }
    }

    impl Drop for EnvVarRestore {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

// ============================================================================
// Main window label abstraction
// ============================================================================

/// The main window label. Always `"main"` — the window is created
/// programmatically in `.setup()` for both primary and secondary
/// instances. Secondary instances get an isolated WebView2 profile via
/// `data_directory()` but share the same label.
///
/// Set once at startup by `main.rs` via [`set_main_window_label`] and read
/// by all callsites via [`get_main_window_label`].
static MAIN_WINDOW_LABEL: OnceLock<String> = OnceLock::new();

/// Set the main window label. Call exactly once from `main.rs` during setup.
pub fn set_main_window_label(label: &str) {
    let _ = MAIN_WINDOW_LABEL.set(label.to_string());
}

/// Get the main window label. Returns `"main"` if never explicitly set.
pub fn get_main_window_label() -> &'static str {
    MAIN_WINDOW_LABEL
        .get()
        .map(|s| s.as_str())
        .unwrap_or("main")
}

/// Convenience: get the main WebviewWindow from an AppHandle.
/// Uses the label from [`get_main_window_label`].
pub fn get_main_window(app: &tauri::AppHandle) -> Option<tauri::WebviewWindow> {
    app.get_webview_window(get_main_window_label())
}

/// Read an image file and return it as a base64 data URL
#[tauri::command]
fn read_image_as_base64(path: String) -> Result<String, String> {
    // Read the file
    let data = fs::read(&path).map_err(|e| format!("Failed to read file: {}", e))?;

    // Determine MIME type from extension
    let mime_type = if path.to_lowercase().ends_with(".png") {
        "image/png"
    } else if path.to_lowercase().ends_with(".jpg") || path.to_lowercase().ends_with(".jpeg") {
        "image/jpeg"
    } else if path.to_lowercase().ends_with(".gif") {
        "image/gif"
    } else if path.to_lowercase().ends_with(".webp") {
        "image/webp"
    } else {
        "application/octet-stream"
    };

    // Encode to base64 and return as data URL
    let base64_data = STANDARD.encode(&data);
    Ok(format!("data:{};base64,{}", mime_type, base64_data))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window(crate::get_main_window_label()) {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![read_image_as_base64])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// ==========================================================================
// Relocated from `main.rs` (Phase 2) — the bin-only module tree. Files did
// not move; only these declarations did. `main.rs` keeps a
// `pub use qontinui_runner_lib::<m>;` shim for each, deleted in Phase 3.
// ==========================================================================
pub mod action_service;
pub mod agent_authorization;
pub mod agent_claims;
pub mod agent_commands;
pub mod agent_daemons;
pub mod agent_http;
pub mod agent_pusher;
pub mod agent_runtime;
pub mod agent_token;
pub mod agent_worktree;
pub mod agentic_verification;
pub mod ai_pricing;
pub mod ai_provider;
pub mod ai_router;
pub mod ai_workflows;
pub mod api_config;
pub mod api_request;
pub mod asset_headers;
pub mod auto_commit;
pub mod backup;
pub mod blind_spots;
pub mod build_drift;
pub mod bundled_resources;
pub mod check_executor;
pub mod check_generation;
pub mod ci_node;
pub mod claude_accounts;
pub mod claude_protocol;
pub mod claude_session;
pub mod click_overlay;
pub mod commands;
pub mod comparison;
pub mod config;
pub mod config_facade;
pub mod config_storage;
pub mod constraint_engine;
pub mod container;
pub mod context;
pub mod coord_doctor_cmd;
pub mod coord_http;
pub mod coord_mcp;
pub mod coord_questions;
pub mod coordinator;
pub mod cost_management;
pub mod crash_dumps;
pub mod crash_observability;
pub mod credential_helper;
pub mod database;
pub mod debug_lifecycle;
pub mod demo_workflows;
pub mod dev_services;
pub mod dirty_poller;
pub mod discoveries;
pub mod display;
pub mod doctor;
pub mod dom_capture;
pub mod drain;
pub mod embedded_pg;
pub mod error;
pub mod error_monitor; // Must be declared before error (error re-exports ErrorSeverity from error_monitor)
pub mod event_system;
pub mod execution_context;
pub mod execution_core;
pub mod executor;
pub mod exploration;
pub mod findings;
pub mod fixer;
pub mod fleet;
pub mod fleet_commands;
pub mod flow_control;
#[cfg(test)]
pub mod flywheel_e2e_tests;
pub mod follow_up;
pub mod git_status_subset;
pub mod git_supervision;
pub mod graphql;
pub mod health_monitor;
pub mod heartbeat;
pub mod helper_tasks;
pub mod install_effects_producer;
pub mod instance;
pub mod instance_health;
pub mod instance_manager;
pub mod iteration_bundle;
#[cfg(windows)]
pub mod job_object;
pub mod knowledge_acquisition;
pub mod known_issues;
pub mod launch_env;
pub mod log_consolidation;
pub mod logging;
pub mod looping_agent_coord;
pub mod looping_agent_supervisor;
pub mod macros;
pub mod mcp;
pub mod mcp_api;
pub mod mcp_client;
pub mod mcp_embedded;
pub mod memory;
pub mod meta_optimizer;
pub mod middleware;
pub mod observer_registry;
pub mod online_learning;
pub mod orchestration_loop;
pub mod orchestration_loop_configs;
pub mod orchestrator;
pub mod otel;
pub mod outbound_trace; // Plan 2026-07-08-ui-bridge-reach-and-verify-gated-flows P5 — redacted outbound-call trace
pub mod paths;
pub mod planning_bridge;
pub mod playwright;
pub mod pm_detect;
pub mod process_capture;
pub mod productivity;
pub mod projects;
pub mod prompt_library;
pub mod prompt_snippets;
pub mod prompts;
pub mod rag;
pub mod recording;
pub mod reflection;
pub mod regression_api;
pub mod repo_detection;
pub mod resource_guard;
pub mod restate;
pub mod rework;
pub mod routing;
pub mod runtime_env;
pub mod safe_lock;
pub mod saved_api_requests;
pub mod scenarios;
pub mod scheduler;
pub mod scheduler_service;
pub mod schema_registry;
pub mod screen;
pub mod sdk_features;
pub mod security;
pub mod semantic_conventions;
pub mod server_mode;
pub mod session; // Plan 2026-05-22-coord-native-session-coordination Phase 2 — unified Session primitive
pub mod session_attribution;
pub mod session_bus; // Session Bus Phase 3b — gated directed-message delivery executor
pub mod session_pr_reconciler; // Runner-local per-session PR attribution → project.session_prs (Terminal dropdown)
pub mod settings;
pub mod skills;
pub mod slash_commands;
pub mod spawn_placement;
pub mod spec_api;
pub mod spec_experimentation;
pub mod spec_utils;
pub mod startup_panic;
pub mod state_discovery;
pub mod state_explorer;
pub mod state_machine_configs;
pub mod stats;
pub mod step_event_builder;
pub mod step_executor;
pub mod step_injection;
pub mod step_metadata;
pub mod step_output;
pub mod step_registry;
pub mod step_types;
pub mod steps;
pub mod storage;
pub mod str_utils;
pub mod subagent;
pub mod summary_generator;
pub mod tauri_app_handle;
pub mod tauri_command_audit;
pub mod terminal;
pub mod test_executor;
pub mod test_orchestrator;
pub mod ticket_system;
#[cfg(test)]
pub mod tier_matrix_tests;
pub mod tiered_info;
pub mod timeout_config;
pub mod trace_api;
pub mod tracing_layers;
pub mod trigger_system;
pub mod tunnel;
pub mod ui_bridge_evaluate;
pub mod ui_bridge_invoke;
pub mod ui_bridge_invoke_probe;
pub mod ui_bridge_plugin;
pub mod ui_error;
pub mod unified_ai_session;
pub mod unified_workflow_executor;
pub mod unified_workflows;
pub mod util;
pub mod validation;
pub mod verification;
pub mod vga;
pub mod video_recorder;
pub mod vision;
pub mod wake_handler; // Phase F.1 — qontinui:// custom-URL deep-link wake handler
pub mod webview_recovery; // Dead-webview detection (WebView2 ProcessFailed) + in-process recovery
pub mod win32_compat;
pub mod window_assignments;
pub mod window_manager;
pub mod window_placement;
pub mod workflow;
pub mod workflow_event_bus;
pub mod workflow_generation;
pub mod workflow_queue;
pub mod workflow_state;
pub mod workspace_paths;
pub mod worktree;
pub mod wrappers;
pub mod zombie_sweep;

// Mirrors main.rs's `use commands::AppState;` re-export. 26 call sites
// spell it `crate::AppState` and 103 spell it `crate::commands::AppState`;
// both must resolve against this crate root now that the tree lives here.
pub use commands::AppState;
