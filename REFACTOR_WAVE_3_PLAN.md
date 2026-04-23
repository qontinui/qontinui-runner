# Refactor Wave 3 — Deferred Work Plan

Status: **Deferred from the 2026-04-23 refactor session.** Wave 1 (hardener rule trait, loop_handlers decide/execute split, ui_bridge partial split, endpoint registry) and Wave 2A POC (5 modules converted to Tauri plugins) are already landed on `main`. This plan covers the remaining three workstreams.

Baseline commits:
- `61fb15f83` — Wave 1 structural refactors
- `c74ea498d` — Commands plugin migration, 5 modules

---

## Workstream A — Finish `ui_bridge/` extraction

**Scope:** `src-tauri/src/mcp/ui_bridge/mod.rs` is currently ~10,078 lines after Wave 1. It still holds ~150 handlers plus `routes()` and `route_manifest()`. Extract the remaining handler families into submodules.

**Families remaining (priority order — smallest and most self-contained first):**

1. `errors.rs` — error_snapshots, error_report, error_sessions (start/end/list), error_baselines (capture/compare), diagnostics, readiness, diagnose_stuck, page_health (~10 handlers).
2. `network.rs` — network_requests, in_flight, wait, single, network_chains, browser_events, timeline, console_errors (+ clear), performance_entries (~10 handlers).
3. `exploration.rs` — start/status/results/stop_ui_bridge_exploration, discover_states_from_renders, list_windows (~6 handlers).
4. `screenshots.rs` — annotated_screenshot, capture_*, apply_annotation, annotations CRUD, media/* find/audit/snapshot, element_images, image-diff, element_screenshot (~20 handlers).
5. `page.rs` — navigate, refresh, hard_refresh, close, back, forward, evaluate (+raw/safe/batch), set_tab, activate_tab, navigate_and_wait, summary, query_selector, scroll (~15 handlers).
6. `elements.rs` — get_elements, get_element, assert_element, execute_action, batch_actions, wait_for_element (+ variants), find, find_by_text, click_by_text, click_by_selector, read_value, type_into (~18 handlers).
7. `ai.rs` — ai_search, ai_find, ai_execute, ai_assert (+ batch), ai_snapshot, ai_summary, ai_semantic_search, ai/diff, ai/analyze/*, ai/recovery/*, ai/media/*, action_plan cache (~15 handlers).
8. `intents.rs` — intents/* CRUD + find + execute, state_machine (states/transitions), wait_for_route/navigation/idle/element_state/element_stable/targets (~15 handlers).
9. `capabilities.rs` — capabilities, keyboard_shortcuts, idle_status, workflow endpoints, render_log, invoke proxy (`/commands`, `/invoke/{name}`), control/batch, control/batch-execute, batch, with_diff, execute_action_plan (~15 handlers).

**Pattern (proven in Wave 1 passes):**

- Each family module exposes `pub fn routes() -> axum::Router<Arc<ApiState>>` and `pub fn route_entries() -> &'static [(&'static str, &'static str)]`.
- `mod.rs::routes()` merges submodule routers: `.merge(errors::routes()).merge(network::routes())...`.
- `mod.rs::route_manifest()` already derives from per-submodule `route_entries()` via a `OnceLock`-cached concatenation. Just extend the list as each family is extracted.
- Shared helpers already live in `ui_bridge/helpers.rs` (extracted Wave 1). Reference via `use super::helpers::{...}`.
- Re-export handlers via `pub use <family>::*;` in `mod.rs` to preserve any external caller paths.

**Constraints:**

- Do not change URL paths, methods, request/response types.
- Do not collapse `/control/*` vs `/ai/*` alias duplicates in this pass — that's Workstream D below, and it should come after all families are extracted.
- Build after each family via `cargo-guard.sh check`.

**Out-of-scope for this workstream:** the `manifest_drift_tests` test at `mod.rs:10067` references a no-longer-existent `src/mcp/ui_bridge.rs` path; it needs a separate fix to scan all submodule files.

**Estimated effort:** 3–5 hours, agent-executable in 2 parallel passes (families 1-4 in one agent, 5-9 in another; they don't share files beyond mod.rs which requires serialization on route registration).

---

## Workstream B — Continue `commands/` → Tauri plugin migration

**Scope:** ~85 command modules still use the central `tauri::generate_handler![...]` block in `main.rs`. Convert them to the plugin pattern established in commit `c74ea498d`.

**Pattern (documented in `src-tauri/src/commands/mod.rs` header):**

Per module:
```rust
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_<module_name>")
        .invoke_handler(tauri::generate_handler![
            fn_one,
            fn_two,
            // ... every pub #[tauri::command]
        ])
        .build()
}
```

Plugin naming: `qontinui_<module>` to avoid collisions with Tauri's own plugins.

**Documented footgun:** handlers taking `AppHandle` / `Window` / `WebviewWindow` (not `AppHandle<R>` / `Window<R>` / `WebviewWindow<R>`) must be rewritten with `<R: Runtime>` generics. The generated handler fixes the runtime to `Wry`, which doesn't unify with the plugin's `R`. Any batch conversion must scan for and rewrite these parameters.

**Migration batches (suggested grouping, ~15 modules per batch):**

**Batch 1 — Simple CRUD modules (likely low footgun hit rate):**
- `checkpoints`, `checks`, `comparison`, `container_settings`, `dag_workflows`, `database`, `dataset`, `debug` *(done)*, `discoveries`, `dev_findings` *(done)*, `event_search`, `findings`, `hooks`, `issues`, `known_issues`.

**Batch 2 — Settings modules:**
- `ai_settings`, `accessibility`, `execution_variables`, `mobile_settings`, `otel_settings`, `playwright_settings`, `security_settings`, `self_healing_settings`, `web_integration`.

**Batch 3 — Reporting / read-only:**
- `activity_timeline`, `agentic_metrics`, `ai_data`, `cost_dashboard`, `learning`, `performance_metrics`, `recap`, `terminal_analysis`, `token_analytics`, `transcript`.

**Batch 4 — Tool-heavy modules:**
- `accessibility`, `adaptive_learning`, `ai_generation`, `ai_session`, `backup`, `checkpoint_browser`, `config`, `context`, `library_sync`, `logging`, `meta_optimizer`, `rag`.

**Batch 5 — Most-state-coupled (do last):**
- `auth`, `execution` (directory — needs special handling for sub-modules), `execution_reporting`, `extraction`, `interaction`, `screenshot`, `screenshots`, `state_explorer`, `state_machine`, `state_machine_configs`, `storage`, `testing`, `ui_bridge`, `verification`, `video`, `websocket`, `workflow_events`, `task_sync`, `global_log_sources`, `spec_drift`, `project_logs`, `step_outputs`, `test_orchestrator`, `tiered_info`, `ui_bridge_baselines`, `watchers`, `setup_wizard`, `shell_commands`, `script_emitter`, `mobile`, `mcp`, `instances`, `flow`, `file_browser` *(done)*, `clipboard` *(done)*, `window_manager` *(done)*.

**Directory-style modules** (like `commands/execution/`) need per-submodule conversion or a flattening into `execution::plugin()` that handles sub-module handlers — decide per-module at conversion time.

**Verification per batch:** run `cargo-guard.sh check` after every module, not just the whole batch. A broken plugin conversion is easier to debug in isolation.

**Estimated effort:** 2–3 hours per batch, 5 batches = 10–15 hours total. Batches 1–3 are straightforward; 4–5 likely have more footguns.

---

## Workstream C — AppState compartmentalization (Move 2B)

**Scope:** `src-tauri/src/commands/mod.rs::AppState` has 30+ fields mixing `Mutex`, `TokioMutex`, `Arc<RwLock>`, `AtomicBool`, `AtomicU16`, `OnceCell`. Group into cohesive sub-structs.

**Proposed compartments:**

```rust
pub struct BridgeState {
    pub bridge_manager: TokioMutex<Option<Arc<BridgeManager>>>,
    pub extraction_executor: Mutex<Option<ExtractionExecutor>>,
    pub url_lock_manager: Arc<UrlLockManager>,
    pub file_registry_manager: Arc<FileRegistryManager>,
    pub file_lock_manager: Arc<FileLockManager>,
    pub ui_bridge_failure_tracker: UiBridgeFailureTracker,
    pub exploration_cancel: Arc<TokioMutex<Option<CancellationToken>>>,
}

pub struct ExecutionState {
    pub run_cost_trackers: TokioMutex<HashMap<String, Arc<RunCostTrackers>>>,
    pub orchestration_loops: SharedLoopStates,
    pub working_representation_cache: Arc<WorkingRepresentationCache>,
    pub process_capture_manager: TokioMutex<Option<Arc<ProcessCaptureManager>>>,
    pub container_executor: TokioMutex<Option<IsolatedExecutor>>,
    pub run_recording_handler: Arc<RunRecordingHandler>,
    pub current_config: Mutex<Option<QontinuiConfig>>,
    pub ai_pid_tracker: Arc<Mutex<Vec<u32>>>,
    pub canvas_state: Arc<RwLock<CanvasState>>,
}

pub struct IntegrationState {
    pub sdk_connection: Arc<TokioMutex<SdkConnectionManager>>,
    pub mcp_client_manager: TokioMutex<McpClientManager>,
    pub server_mode: Arc<RwLock<Option<ServerModeState>>>,
    pub token_flow: Arc<TokenFlowStore>,
    pub usb_transport: Arc<OnceCell<UsbTransport>>,
    pub event_broadcast: broadcast::Sender<serde_json::Value>,
}

pub struct HealthState {
    pub doctor_handle: TokioMutex<Option<DoctorHandle>>,
    pub error_monitor_handle: TokioMutex<Option<ErrorMonitorHandle>>,
    pub crash_dumps: Arc<CrashDumpState>,
    pub ui_error: Arc<UiErrorState>,
    pub api_ready: AtomicBool,
    pub api_port: AtomicU16,
}

pub struct StorageState {
    pub pg_db: Arc<PgDb>,
    pub local_storage: Arc<Mutex<LocalStorage>>,
    pub video_recorder: Arc<Mutex<VideoRecordingService>>,
    pub display_processor: Arc<TokioMutex<DisplayProcessor>>,
}

pub struct AppState {
    pub bridge: Arc<BridgeState>,
    pub execution: Arc<ExecutionState>,
    pub integration: Arc<IntegrationState>,
    pub health: Arc<HealthState>,
    pub storage: Arc<StorageState>,
}
```

**Why sequence this AFTER Workstream B:**

Every plugin converted under Workstream B becomes an opportunity to take only the compartment it needs — e.g., `commands::clipboard::plugin()` takes `Arc<IntegrationState>`, not the whole AppState. Compartmenting now and then converting to plugins later updates every call site *twice*. Doing them together per module is the only sane sequencing.

**Migration per module (bundled with Workstream B):**

For each module migrated to a plugin:
1. Identify which AppState fields it reads.
2. Identify which compartment owns those fields.
3. Change `State<'_, Arc<AppState>>` to `State<'_, Arc<CompartmentState>>` in handler signatures.
4. Update call sites from `state.foo_field` to `state.foo_field` (same, now on compartment).
5. Ensure main.rs `.manage()` calls register each compartment as a separate state.

**Constraint:** during migration, the old `AppState` must still delegate-expose its old field access for un-migrated modules. Strategy: keep fields on `AppState` during migration; the fields become references/clones of the compartment. Remove them only once every consumer is migrated.

Or simpler: `AppState` derefs to its own fields that are cloned `Arc<Compartment>`s, and new code accesses via `state.bridge.bridge_manager` while old code still uses `state.bridge_manager` until the module migrates. This avoids the two-pass problem by tolerating both during the transition.

**Estimated effort:** 1–2 hours of compartment-design + tooling-up. Then ~30 extra minutes per module migrated under Workstream B.

---

## Workstream D — Typed errors via `thiserror` (Move 6)

**Scope:** Every `-> Result<T, String>` internal function. Introduce per-domain error enums, keep Tauri ABI stable via `impl From<X> for String`.

**Why sequence this AFTER Workstream C:**

Command signatures will churn during C (state parameter types change). Running D concurrently means two signature rewrites per function. Do C first, then D against settled signatures.

**Proposed error hierarchy:**

```rust
// src-tauri/src/error.rs (expand existing)
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("{0}")]
    BadRequest(String),
    #[error("authorization failed: {0}")]
    Unauthorized(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Bridge(#[from] BridgeError),
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<CommandError> for String {
    fn from(e: CommandError) -> String { e.to_string() }
}

// Per-domain errors:
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("bridge not initialized")]
    NotInitialized,
    #[error("transport error: {0}")]
    Transport(String),
    #[error("timeout after {0}ms")]
    Timeout(u64),
    #[error("assertion failed: {detail}")]
    AssertFailed { detail: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
```

Each compartment (`bridge`, `execution`, `integration`, etc.) gets its own error type. Command signatures become `Result<T, CommandError>` and the Tauri boundary converts to `String` automatically.

**Rollout (per module, small batches):**

1. Convert handler signatures module-by-module. Use `?` with `CommandError::from(...)` conversions.
2. Internal helpers that currently return `Result<T, String>` can be migrated eagerly when they clearly sit behind a command boundary. Leave cross-module helpers that return `String` alone until both caller and callee are migrated.
3. Preserve the exact error message format externally — users or parsers may depend on current strings. Use `#[error("...")]` patterns that reproduce the current text.

**UiBridgeError already exists** at `src-tauri/src/mcp/ui_bridge/types.rs` with error codes, classification, recovery hints. Don't replace it; bridge from `UiBridgeError` to `BridgeError` via a `From` impl so the rest of the codebase can benefit from the classification.

**Estimated effort:** 8–12 hours total, spread across multiple sessions. Can be done incrementally — partial migration is fine, as long as the `From` impls keep the boundary stable.

---

## Suggested session ordering

1. **Session N+1:** Workstream A (finish ui_bridge extraction) + Workstream B Batch 1 in parallel. Low risk, high readability win.
2. **Session N+2:** Workstream C compartment design + AppState delegation shim. Build tooling for per-module migration.
3. **Session N+3:** Workstream B Batch 2–3 migrations, each with Workstream C compartment scoping applied to the migrated module.
4. **Session N+4:** Workstream B Batch 4, continuing C per-module.
5. **Session N+5:** Workstream B Batch 5, wrapping up remaining modules. Delete the central `generate_handler![...]` in main.rs.
6. **Session N+6:** Workstream C cleanup — drop delegation shim, remove legacy AppState fields.
7. **Session N+7+:** Workstream D (typed errors) in batches.

## Out-of-scope items tracked for cleanup

- `manifest_drift_tests` in `src-tauri/src/mcp/ui_bridge/mod.rs:10067` references a no-longer-existent file path. Fix during Workstream A.
- `/control/*` vs `/ai/*` alias collapse — after all ui_bridge families are extracted, introduce an `alias!(control, ai, path, method, handler)` helper and collapse the ~40 duplicate routes.
- ui_bridge hardcoded URL literals at `mod.rs:2774, 2785` and `helpers.rs:254, 265` — should use `crate::api_config::get_ipc_response_url()`. Noted by the Wave 1 endpoint-registry agent.
- Pre-existing snake-case warning in `src-tauri/src/workflow_generation/hardener/mod.rs:2866` (unrelated to Wave 1).
