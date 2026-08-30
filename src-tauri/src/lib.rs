use base64::{engine::general_purpose::STANDARD, Engine};
use std::fs;
use std::sync::OnceLock;
use tauri::Manager;

// Self-alias so `qontinui_runner_lib::…` paths resolve INSIDE this crate too.
//
// Seven modules (`process_helpers`, `auth`, `fs_atomic`, …) are compiled into
// BOTH this lib and the `qontinui-runner` bin, and the bin reaches lib items by
// their external path. Without this alias a call site in one of those shared
// modules would need a different path depending on which crate is compiling it.
// With it, `qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked` is
// one spelling that works everywhere — which is what lets the blocking-pool
// counter be a single static shared by both crates instead of two that each
// see half the traffic.
extern crate self as qontinui_runner_lib;

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
pub mod git_posture;
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

// Runner instance identity from the process env (`QONTINUI_INSTANCE_NAME`) —
// the ONE primary/secondary predicate. In the lib for the same reason as
// `fs_atomic`: the runner bin's tier-persist guard and the `qontinui_profile`
// bin's headless pair door must agree on "is this a secondary?", and a second
// bin cannot import from the runner bin's module tree. `crate::instance`
// (bin-only — it reaches into `crate::mcp::types`) re-exports it, so there is
// exactly one implementation. See `profiles::promote_tier_to_account`.
pub mod instance_env;

// Exposed for the `qontinui_profile device pair` CLI (and any other binary
// that needs the encrypted token store outside the Tauri runtime). Both
// modules are Tauri-free.
pub mod auth;
pub mod fs_perms;
pub mod machine_identity;
pub mod secure_storage;
/// Canonical reader for `~/.qontinui/machine.json` — the machine's ONE
/// durable `device_id`. Declared in `main.rs` too, because `auth` compiles
/// into both crates and must consult it before falling back to its own cache.
/// The machine's tenant pin — moved out of the bin-only `session` tree so the
/// LIB can read it too (plan
/// `2026-08-31-coord-mcp-credential-selection-by-binding-provenance` Phase 5a).
/// `coord_doctor` lives here and must probe with the same credential the
/// bin-side proxy selects; duplicating the pin classifier to reach it would
/// re-create, between two readers, exactly the selection divergence Phase 5a
/// exists to close. `session::tenant_pin` re-exports this, so every existing
/// `crate::session::tenant_pin::…` path in the bin keeps resolving.
pub mod tenant_pin;

// MCP tool-output spill store (plan `2026-08-20-runner-mcp-tool-output-spill`,
// Phase 2). In the LIB crate for the SAME reason as `runner_breadcrumb`: the
// WRITER is a second bin (`bin/wrappers_mcp.rs`, the stdio MCP server), a
// second bin cannot import from the runner bin's module tree, and the READER
// must agree with the writer on the on-disk record byte for byte. One module ⇒
// one schema ⇒ writer and reader cannot drift. Builds on `fs_atomic` (whole
// records appear at once or not at all) and `fs_perms` (bodies are stored
// unredacted, so the on-disk control is the one that matters).
pub mod mcp_spill;

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

// `config report` — the effective configuration by layer, with per-value
// provenance (plan 2026-08-20-effective-config-provenance-and-env-generation).
// In the lib for the SAME reason as `coord_doctor`: the standalone
// `config_report` bin and the in-app Tauri command share one layer table, one
// driver and one formatter. Ten of the fifteen layers live in BIN-only modules,
// so their readings are injected as data via `ConfigReportInputs` — the
// `DoctorInputs` pattern — and the headless bin honestly reports them as
// `Unknown` rather than omitting the rows.
pub mod config_report;
pub mod coord_mcp_config;

// Env GENERATIONS — the Phase 3 half of the same plan. The runner's process
// env, the once-in-`main()` launch snapshot and the env a PTY child actually
// receives are three different ages of the same variable (on Windows the PTY
// one is genuinely FRESHER: `portable-pty` re-reads the HKCU/HKLM `Environment`
// registry keys over the process env), which is why an operator's flag flip is
// three restarts deep. This module models those generations, diffs them, and —
// the security-critical part — withholds credential-classed values at the MODEL
// layer, so a value that must not be printed never reaches a renderer at all.
// In the lib for the same reason as `config_report`: shared types + one
// byte-stable formatter; the CAPTURE of each generation is bin-side, since
// `launch_env` and `terminal` are BIN-only modules.
pub mod env_generations;

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

// Wedge diagnostics (Phase 4 of
// `2026-08-30-runner-blocking-pool-exhaustion-and-wedge-diagnostics`). In the
// LIB crate so both crates' `spawn_blocking` sites share ONE counter.
pub mod wedge_diagnostics;

// Panic supervisor for the long-lived background loops (plan
// `2026-09-03-coord-row-get-panic-class-closed-by-lint-and-supervisor` Phase
// 4). In the LIB crate because both crates start loops — `fleet`/`terminal`
// in the bin, `plan_workunit_adapter` here — and `/health` must read ONE
// registry. Restart-on-panic with a bounded backoff; nothing on the recording
// path awaits or touches PG.
pub mod worker_supervisor;

/// Repo → owning-tenant resolution with TTL'd caches (Phase 6 of
/// `2026-08-29-runner-work-scoped-writes-default-tenant-credential`). In the
/// lib crate because the plan adapter above is its principal consumer.
pub mod repo_tenant;

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
