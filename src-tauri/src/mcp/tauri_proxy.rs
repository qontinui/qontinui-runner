//! Safelisted HTTP proxy for Tauri commands (Tier 2.1 of the UI Bridge improvement plan).
//!
//! Exposes `POST /ui-bridge/tauri/invoke` so external agents can call a curated
//! subset of Tauri commands without resorting to `page/evaluate + __TAURI_INTERNALS__`.
//!
//! ## Design
//! - Hard safelist: only commands in `ALLOWED_PROXIED_COMMANDS` are accepted.
//! - Direct Rust dispatch: each allowed command is wired to the same underlying
//!   logic the Tauri command uses.  No JS round-trip, no IPC overhead.
//! - AppHandle / managed state: fetched from `ApiState.app_handle` which is
//!   available in every Axum handler via `State<Arc<ApiState>>`.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::Json};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::Manager as _;
use tracing::info;

use crate::mcp::types::ApiState;
use crate::terminal::TerminalManager;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

// ============================================================================
// Safelist
// ============================================================================

/// Exact set of Tauri command names that may be invoked via the HTTP proxy.
/// Any command not listed here returns HTTP 403.
pub const ALLOWED_PROXIED_COMMANDS: &[&str] = &[
    "setting_get",
    "setting_set",
    "terminal_create",
    "terminal_write",
    "terminal_close",
    "list_terminals",
    "get_claude_config_dirs",
    "check_accounts_usage",
    // The remote-interactivity probe sweep (plan
    // `2026-09-20-remote-session-interactivity-is-a-query-and-both-halves-hold`,
    // A3): a headless agent measures a device's sessions without a webview.
    // It mints grants and attaches with this runner's own device credential,
    // exactly as the Fleet view does, and carries no input byte by
    // construction (`ProbeFrameSink`).
    "remote_interactivity_probe",
    // END a session on another device under an attach grant (plan
    // `2026-09-30-close-remote-sessions-from-the-local-runner`, Phase 3) — the
    // door a headless harness / UI Bridge drives.
    "remote_session_end",
    // The headless forced-migration run (plan
    // `2026-11-26-terminal-migration-never-loses-a-session-has-no-forced-migration-run`,
    // Phase 2a): the operator migration, and the provisional lifecycle row it
    // looks the session up by — which the proxied `terminal_create` does not
    // write (only the webview's `terminal_session_record_open` call does).
    "terminal_session_record_open",
    "terminal_migrate_session_account",
];

// ============================================================================
// Request / Response types
// ============================================================================

/// Request body for `POST /ui-bridge/tauri/invoke`.
#[derive(Debug, Deserialize)]
pub struct TauriInvokeRequest {
    /// Tauri command name (must be in `ALLOWED_PROXIED_COMMANDS`).
    pub command: String,
    /// Arguments object — deserialized per-command in the dispatch match.
    #[serde(default)]
    pub args: Value,
}

/// Unified response envelope.
#[derive(Debug, Serialize)]
pub struct TauriInvokeResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Machine-readable refusal code, on the few refusals that carry one
    /// (`invalid_edit_sibling`). Absent everywhere else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The HTTP status this response is answered with. `None` = 200, which is
    /// what every arm answered before typed refusals existed (an invoke error
    /// rides in `error` with `success: false`); a typed input refusal sets 400.
    #[serde(skip)]
    pub status: Option<StatusCode>,
}

/// The arguments of the proxied `terminal_create` command.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalCreateArgs {
    title: Option<String>,
    working_dir: Option<String>,
    page_id: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
    /// Phase 2 of `plans/2026-05-28-isolate-session-edit-work-in-worktrees.md`.
    /// When `Some(repo_slug)` AND `QONTINUI_AGENT_WORKTREE_MODE` is on,
    /// allocate an isolated worktree for that repo and use it as the
    /// PTY cwd instead of `working_dir`. Mirrors the `intent_repo`
    /// argument on the `terminal_create` Tauri command.
    intent_repo: Option<String>,
    /// Stable session id of the agent creating this terminal,
    /// supplied by the proxy caller (the initiator's context).
    /// Folded into the isolated-worktree claim's owner token so
    /// distinct agent sessions on one machine are distinct
    /// holders. Absent → None.
    #[serde(default)]
    agent_session_id: Option<uuid::Uuid>,
    /// The caller's explicit "start it anyway" past a CRITICAL
    /// resource-guard verdict (plan
    /// `2026-08-07-runner-resource-guard-and-session-protection`
    /// §Part D). Mirrors the `resource_override` argument on the
    /// `terminal_create` Tauri command this route proxies, and
    /// defaults to `false`, so every existing caller keeps the
    /// unattended posture it has today: the floor is respected and
    /// the typed refusal comes back as the invoke error.
    ///
    /// It exists so the refusal is actually overridable from here.
    /// A caller that IS fronted by a UI (an external tool that can
    /// show its own dialog) recognises the
    /// `resource_guard:critical:` prefix, asks its operator, and
    /// re-invokes with `resourceOverride: true` — the same
    /// refuse-ask-retry loop `src/lib/resourceGuard.ts` runs in the
    /// webview. Without the field that loop had nowhere to land:
    /// the second attempt would have been refused identically, and
    /// the comment promising an override would have been describing
    /// a path that did not exist.
    #[serde(default)]
    resource_override: bool,
    /// The tenant to spawn this session for — the `tenant_id`
    /// argument of the `terminal_create` Tauri command this route
    /// proxies, with the same contract: absent/blank keeps the
    /// machine default, a malformed uuid is refused, and a tenant
    /// this runner holds no coord credential for refuses the spawn
    /// (plan `2026-09-10-spawn-tenant-never-reaches-the-session-coord-credential`).
    ///
    /// This struct drops unknown keys, so a selector spelled any other way spawns
    /// on the machine default with `success: true`. `tenant_id` is the Tauri
    /// command's own spelling, and `tenant` is `POST /coord-mcp/provision-session`'s.
    #[serde(default, alias = "tenant_id", alias = "tenant")]
    tenant_id: Option<String>,
    /// The repos the session will EDIT beside `intentRepo` — the
    /// `edit_siblings` argument of the `terminal_create` Tauri
    /// command this route proxies (plan
    /// `2026-10-10-a-session-cannot-declare-the-sibling-it-will-edit-so-the-store-override-hides-its-edits`).
    /// A bad entry is a typed 400 `invalid_edit_sibling`.
    #[serde(default, alias = "edit_siblings")]
    edit_siblings: Vec<String>,
}

/// The `terminal_create` proxy's resource-guard precheck, run before the
/// allocation: a CRITICAL refusal after it would leave the `git worktree add`
/// on disk, one more tree per retry. Answered through the SAME
/// [`TauriInvokeResponse::spawn_failed`] the failed-create arm uses, so one
/// refusal has one answer whichever gate gave it
/// ([`crate::resource_guard::precheck_spawn`]'s contract).
fn spawn_precheck_or_invoke_error(
    check: impl FnOnce() -> Result<(), String>,
) -> Result<(), TauriInvokeResponse> {
    check().map_err(TauriInvokeResponse::spawn_failed)
}

/// The `terminal_create` proxy's spawn-tenant admission: the shared
/// [`crate::commands::terminal::admit_spawn_tenant`], answered through the
/// same typed path the failed-create arm uses
/// ([`TauriInvokeResponse::spawn_failed`]) — so a `terminal:tenant_<kind>:`
/// refusal carries `TERMINAL_TENANT_<KIND>` and its status whichever gate gave
/// it, the precheck here or the identity seam inside the spawn.
fn spawn_tenant_or_invoke_error(
    raw: Option<&str>,
) -> Result<Option<uuid::Uuid>, TauriInvokeResponse> {
    crate::commands::terminal::admit_spawn_tenant(raw).map_err(TauriInvokeResponse::spawn_failed)
}

impl TauriInvokeResponse {
    fn ok(data: Value) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            code: None,
            status: None,
        }
    }
    fn err(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(msg.into()),
            code: None,
            status: None,
        }
    }
    /// The answer to a refused spawn — the resource-guard precheck before the
    /// allocation and a failed `tm.create` after it. A recognised refusal
    /// family carries its `code` and typed status, by the rule `POST /terminals`
    /// and the backend relay share
    /// ([`crate::terminal::spawn_refusal::spawn_refusal_code`]); the text stays
    /// verbatim, so a caller matching the `resource_guard:critical:` prefix is
    /// unaffected. Anything unrecognised keeps the untyped 200 error.
    fn spawn_failed(error: String) -> Self {
        match crate::terminal::spawn_refusal::spawn_refusal_code(&error) {
            Some((status, code)) => Self {
                success: false,
                data: None,
                error: Some(error),
                code: Some(code),
                status: Some(status),
            },
            None => Self::err(error),
        }
    }
    /// A refused `editSiblings` input: a typed 400 `invalid_edit_sibling`,
    /// answered before any allocation (vet V9).
    fn edit_sibling_refusal(
        refusal: crate::agent_worktree::edit_sibling::EditSiblingRefusal,
    ) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(refusal.message.clone()),
            code: Some(refusal.code().to_string()),
            status: Some(StatusCode::BAD_REQUEST),
        }
    }
}

// ============================================================================
// 403 body
// ============================================================================

#[derive(Debug, Serialize)]
pub(crate) struct ForbiddenResponse {
    error: &'static str,
    command: String,
    allowed: &'static [&'static str],
}

// ============================================================================
// Handler
// ============================================================================

/// `POST /ui-bridge/tauri/invoke`
///
/// Dispatches the requested command to its underlying Rust implementation after
/// verifying it is in the safelist.
pub async fn tauri_invoke_handler(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<TauriInvokeRequest>,
) -> Result<(StatusCode, Json<TauriInvokeResponse>), (StatusCode, Json<ForbiddenResponse>)> {
    if let Some(refusal) = safelist_refusal(&req.command) {
        return Err(refusal);
    }

    info!(command = %req.command, "tauri_proxy: dispatching command");

    // Detached, so a client that disconnects cannot drop a command mid-way. The
    // `terminal_create` arm's create runs off-worker, and dropped there it would
    // release its isolated context's claims under the terminal it opened.
    let response = crate::spawn_off_worker::run_detached(dispatch(state, req)).await;
    Ok((response.status.unwrap_or(StatusCode::OK), Json(response)))
}

/// The handler's 403 for a command outside [`ALLOWED_PROXIED_COMMANDS`];
/// `None` admits it to [`dispatch`].
fn safelist_refusal(command: &str) -> Option<(StatusCode, Json<ForbiddenResponse>)> {
    if ALLOWED_PROXIED_COMMANDS.contains(&command) {
        return None;
    }
    Some((
        StatusCode::FORBIDDEN,
        Json(ForbiddenResponse {
            error: "command not in safelist",
            command: command.to_string(),
            allowed: ALLOWED_PROXIED_COMMANDS,
        }),
    ))
}

// ============================================================================
// Dispatch — one arm per safelisted command
// ============================================================================

async fn dispatch(state: Arc<ApiState>, req: TauriInvokeRequest) -> TauriInvokeResponse {
    match req.command.as_str() {
        // ── settings ─────────────────────────────────────────────────────────
        "setting_get" => {
            #[derive(Deserialize)]
            struct Args {
                key: String,
            }
            let a = match serde_json::from_value::<Args>(req.args) {
                Ok(v) => v,
                Err(e) => return TauriInvokeResponse::err(format!("bad args: {}", e)),
            };
            match state.app_state.pg_db.get_setting(&a.key).await {
                Ok(opt) => TauriInvokeResponse::ok(opt.unwrap_or(Value::Null)),
                Err(e) => TauriInvokeResponse::err(e),
            }
        }

        "setting_set" => {
            #[derive(Deserialize)]
            struct Args {
                key: String,
                value: Value,
            }
            let a = match serde_json::from_value::<Args>(req.args) {
                Ok(v) => v,
                Err(e) => return TauriInvokeResponse::err(format!("bad args: {}", e)),
            };
            match state.app_state.pg_db.set_setting(&a.key, &a.value).await {
                Ok(()) => TauriInvokeResponse::ok(serde_json::json!({
                    "success": true,
                    "message": format!("Setting '{}' saved", a.key)
                })),
                Err(e) => TauriInvokeResponse::err(e),
            }
        }

        // ── terminals ────────────────────────────────────────────────────────
        "terminal_create" => {
            let a = match serde_json::from_value::<TerminalCreateArgs>(req.args) {
                Ok(v) => v,
                Err(e) => return TauriInvokeResponse::err(format!("bad args: {}", e)),
            };
            // Coord device drain (plan `2026-09-13-drained-runner-never-reaches-idle`,
            // D3): this is the HTTP proxy of `terminal_create`, not the runner UI,
            // so its caller is autonomous and is deferred while the drain holds.
            if let crate::coord_drain_state::DrainGate::Defer { reason, .. } =
                crate::coord_drain_state::drain_gate_for_work(
                    crate::coord_drain_state::SpawnOrigin::Unknown,
                    // Caller-supplied title — BOUNDED (review N4): a caller
                    // must not be able to grow the banner's deferred set.
                    &crate::coord_drain_state::bounded_work_key(
                        "proxy_terminal",
                        a.title.as_deref().unwrap_or("untitled"),
                    ),
                )
            {
                return TauriInvokeResponse::err(reason);
            }
            let spawn_tenant = match spawn_tenant_or_invoke_error(a.tenant_id.as_deref()) {
                Ok(tenant) => tenant,
                Err(refusal) => return refusal,
            };
            let tm: Arc<TerminalManager> = state
                .app_handle
                .state::<Arc<TerminalManager>>()
                .inner()
                .clone();

            // L2 (shared-checkout coordination gap fix) — derive
            // `intent_repo` from `working_dir` when the caller didn't
            // declare one (no-op until `QONTINUI_AGENT_WORKTREE_MODE` is on).
            let effective_intent_repo: Option<String> = a.intent_repo.clone().or_else(|| {
                a.working_dir.as_deref().and_then(|wd| {
                    let derived = crate::agent_worktree::canonical_paths::repo_slug_for_path(
                        std::path::Path::new(wd),
                    );
                    if let Some(ref repo) = derived {
                        tracing::debug!(
                            working_dir = %wd,
                            derived_intent_repo = %repo,
                            "tauri_proxy terminal_create: derived intent_repo from working_dir"
                        );
                    }
                    derived
                })
            });

            // Judged before the allocate, against the EFFECTIVE intent repo, so
            // a typo is a typed 400 rather than a silent shared-checkout pane.
            let edit_siblings =
                match crate::agent_worktree::edit_sibling::validate_spawn_edit_siblings(
                    effective_intent_repo.as_deref(),
                    &a.edit_siblings,
                ) {
                    Ok(v) => v,
                    Err(refusal) => return TauriInvokeResponse::edit_sibling_refusal(refusal),
                };

            // The resource guard's CRITICAL verdict, judged before the
            // allocation for the reason `POST /terminals` judges it there.
            // `a.resource_override` is the caller's explicit "start it anyway".
            if let Err(refusal) = spawn_precheck_or_invoke_error(|| {
                crate::resource_guard::precheck_spawn("terminal session", a.resource_override)
            }) {
                return refusal;
            }

            let (working_dir, isolated_ctx) =
                crate::agent_worktree::isolated_edit::acquire_for_terminal(
                    effective_intent_repo.as_deref(),
                    a.title.as_deref().unwrap_or("Terminal edit session"),
                    a.working_dir,
                    a.agent_session_id,
                    spawn_tenant,
                    &edit_siblings,
                )
                .await;

            // The shared session-env contribution (`QONTINUI_SESSION_WORKTREES`
            // + the configured plan directories) onto the PTY, derived before
            // the ctx is parked. See `agent_worktree::session_env`. The admitted
            // spawn tenant selects that tenant's plan/prompt directories where
            // the device keys them — a lookup key, never an attribution claim.
            let spawn_tenant_key = spawn_tenant.map(|t| t.to_string());
            let extra_env = crate::agent_worktree::session_env::session_extra_env(
                isolated_ctx.as_ref(),
                spawn_tenant_key.as_deref(),
            );
            // What the post-create coord wiring needs, kept before the spawn
            // consumes them.
            let created = ProxyTerminalCreated {
                title: a.title.clone(),
                intent_repo: effective_intent_repo,
                working_dir: working_dir.clone(),
                spawn_tenant,
            };

            let create_app_handle = state.app_handle.clone();
            // Off the API runtime's workers: this create can block for seconds to
            // minutes (see `spawn_off_worker`).
            let create_result = {
                let tm = tm.clone();
                crate::spawn_off_worker::run_terminal_create_off_worker(move || {
                    // door-scan: spawn — the door's real spawn; the context is in hand until here.
                    tm.create(
                        a.title,
                        working_dir,
                        a.page_id,
                        a.cols,
                        a.rows,
                        create_app_handle,
                        None,
                        extra_env,
                        // UNATTENDED by default — respect the critical floor. This is
                        // the HTTP proxy for the `terminal_create` Tauri command, used
                        // by external tooling that has no webview to show a dialog in,
                        // so an absent `resourceOverride` is `false` and the typed
                        // refusal is returned verbatim as the invoke response's error.
                        // A caller that IS fronted by a UI recognises the
                        // `resource_guard:critical:` prefix, asks its own operator, and
                        // re-invokes with `resourceOverride: true` — see the field's
                        // doc. Nothing is ever assumed on the caller's behalf here.
                        a.resource_override,
                        // The account is chosen after this point, so the every-account
                        // mint applies.
                        crate::terminal::TrustArm::AccountChosenLater,
                        spawn_tenant,
                        // External tooling through the HTTP proxy: reason unnamed.
                        crate::coord_drain_state::SpawnOrigin::Unknown,
                    )
                })
                .await
            };
            match create_result {
                Ok(info) => {
                    let door = ProxyTerminalCreateDoor {
                        terminal_manager: tm.clone(),
                        session_registry: state
                            .app_handle
                            .try_state::<Arc<crate::session::SessionRegistry>>()
                            .map(|r| r.inner().clone()),
                        app_handle: Some(state.app_handle.clone()),
                    };
                    proxy_terminal_created(&door, info, isolated_ctx, created).await
                }
                Err(e) => {
                    // The allocation (if this call made one) has its fresh tree
                    // removed when no child started and it holds nothing, and is
                    // reported spawn-failed, as on every `acquire_for_terminal`
                    // door; the context's Drop then releases its claims.
                    if let Some(ctx) = isolated_ctx {
                        ctx.remove_unspawned(
                            "tauri_proxy terminal_create",
                            e.child_started,
                            &e.message,
                        );
                    }
                    TauriInvokeResponse::spawn_failed(e.message)
                }
            }
        }

        "terminal_write" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                terminal_id: String,
                data: String,
            }
            let a = match serde_json::from_value::<Args>(req.args) {
                Ok(v) => v,
                Err(e) => return TauriInvokeResponse::err(format!("bad args: {}", e)),
            };
            let tm: Arc<TerminalManager> = state
                .app_handle
                .state::<Arc<TerminalManager>>()
                .inner()
                .clone();
            let session = match tm.get(&a.terminal_id) {
                Some(s) => s,
                None => {
                    return TauriInvokeResponse::err(format!(
                        "Terminal not found: {}",
                        a.terminal_id
                    ))
                }
            };
            let bytes = match STANDARD.decode(&a.data) {
                Ok(b) => b,
                Err(e) => return TauriInvokeResponse::err(format!("Invalid base64 data: {}", e)),
            };
            match session.write(
                &bytes,
                crate::terminal::session::PtyWriteCaller::TauriInvokeProxy,
            ) {
                Ok(()) => TauriInvokeResponse::ok(serde_json::json!({ "success": true })),
                Err(e) => TauriInvokeResponse::err(e),
            }
        }

        "terminal_close" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                terminal_id: String,
            }
            let a = match serde_json::from_value::<Args>(req.args) {
                Ok(v) => v,
                Err(e) => return TauriInvokeResponse::err(format!("bad args: {}", e)),
            };
            let tm: Arc<TerminalManager> = state
                .app_handle
                .state::<Arc<TerminalManager>>()
                .inner()
                .clone();
            // Same honest remote-close outcome as the Tauri command (plan
            // 2026-09-16-remote-tab-cannot-be-released-so-the-target-terminal-stays-claimed,
            // Phase 1) — this door is what a headless harness drives.
            let remote_probe =
                crate::commands::remote_attach::probe_remote_close(&tm, &a.terminal_id);
            let id = a.terminal_id.clone();
            match spawn_blocking_tracked(move || tm.close(&id)).await {
                Ok(Ok(())) => {
                    let mut body = serde_json::json!({ "success": true });
                    if let Some(report) = remote_probe.map(|probe| probe.report()) {
                        body["message"] = Value::String(report.message);
                        body["remoteDetach"] = report.remote_detach;
                    }
                    TauriInvokeResponse::ok(body)
                }
                Ok(Err(e)) => TauriInvokeResponse::err(e),
                Err(e) => TauriInvokeResponse::err(format!("Join error: {}", e)),
            }
        }

        "list_terminals" => {
            let tm: Arc<TerminalManager> = state
                .app_handle
                .state::<Arc<TerminalManager>>()
                .inner()
                .clone();
            let terminals = tm.list();
            TauriInvokeResponse::ok(serde_json::json!({ "terminals": terminals }))
        }

        // ── session lifecycle / account migration ────────────────────────────
        "terminal_session_record_open" => match lifecycle_store(&state.app_handle) {
            Ok(store) => proxy_session_record_open(&store, req.args),
            Err(e) => e,
        },

        "terminal_migrate_session_account" => {
            let store = match lifecycle_store(&state.app_handle) {
                Ok(store) => store,
                Err(e) => return e,
            };
            let plan = match proxy_plan_session_migration(&store, req.args) {
                Ok(plan) => plan,
                Err(refusal) => return refusal,
            };
            // No timeout here, deliberately: the migration resolves only once
            // the resumed CLI is proven alive or given up on, which can take
            // ~11-17 minutes (`terminal_migrate_session_account`'s doc). The
            // router imposes none either (no `TimeoutLayer` in
            // `mcp_api::create_router`; `axum::serve` sets no request deadline),
            // and `run_detached` in the handler keeps the migration running
            // to its end if the caller's own HTTP timeout gives up first.
            command_response(
                crate::commands::terminal::run_session_migration(
                    &state.app_handle,
                    plan,
                    PROXY_MIGRATION_ORIGIN,
                )
                .await,
            )
        }

        "remote_session_end" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                device_id: String,
                session_id: String,
                #[serde(default)]
                force: bool,
                #[serde(default)]
                tenant: Option<String>,
            }
            let a = match serde_json::from_value::<Args>(req.args) {
                Ok(v) => v,
                Err(e) => return TauriInvokeResponse::err(format!("bad args: {}", e)),
            };
            let tm: Arc<TerminalManager> = state
                .app_handle
                .state::<Arc<TerminalManager>>()
                .inner()
                .clone();
            let base = crate::commands::remote_attach::coord_base_for(&state.app_handle);
            match crate::commands::remote_end::end_remote_session(
                &tm,
                &base,
                &a.device_id,
                &a.session_id,
                a.force,
                a.tenant,
            )
            .await
            {
                Ok(result) => match serde_json::to_value(&result) {
                    Ok(v) => TauriInvokeResponse::ok(v),
                    Err(e) => TauriInvokeResponse::err(format!("serialize: {}", e)),
                },
                Err(e) => TauriInvokeResponse::err(e),
            }
        }

        // ── config / accounts ────────────────────────────────────────────────
        "get_claude_config_dirs" => {
            let dirs = crate::settings::get_claude_config_dirs();
            TauriInvokeResponse::ok(serde_json::json!({ "dirs": dirs }))
        }

        "check_accounts_usage" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                config_dirs: Vec<String>,
            }
            let a = match serde_json::from_value::<Args>(req.args) {
                Ok(v) => v,
                Err(e) => return TauriInvokeResponse::err(format!("bad args: {}", e)),
            };
            match crate::commands::ai_settings::check_accounts_usage(a.config_dirs).await {
                Ok(cmd) => TauriInvokeResponse::ok(serde_json::json!({
                    "success": cmd.success,
                    "message": cmd.message,
                    "data": cmd.data,
                })),
                Err(e) => TauriInvokeResponse::err(e),
            }
        }

        // ── remote interactivity ─────────────────────────────────────────────
        "remote_interactivity_probe" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                device_id: String,
                #[serde(default)]
                trigger: Option<String>,
            }
            let a = match serde_json::from_value::<Args>(req.args) {
                Ok(v) => v,
                Err(e) => return TauriInvokeResponse::err(format!("bad args: {}", e)),
            };
            // A headless caller is not the Fleet view: `fleet_view` would
            // stamp the device into the scheduler's sweep set for a week on
            // behalf of an operator who never opened it. So the proxy passes
            // an explicit `manual` through and otherwise runs `unspecified`
            // (which, unlike manual, honours the attempt memory); any other
            // trigger is refused.
            let trigger = match proxy_probe_trigger(a.trigger.as_deref()) {
                Ok(t) => t,
                Err(e) => return TauriInvokeResponse::err(e),
            };
            match crate::commands::remote_interactivity_probe::run_probe_command(
                &state.app_handle,
                &a.device_id,
                Some(trigger),
            )
            .await
            {
                Ok(report) => TauriInvokeResponse::ok(report),
                Err(e) => TauriInvokeResponse::err(e),
            }
        }

        // Unreachable: safelist check above prevents anything else.
        _ => TauriInvokeResponse::err(format!("command '{}' not implemented", req.command)),
    }
}

// ============================================================================
// terminal_create — post-create coord wiring
// ============================================================================

/// What the `terminal_create` arm's post-create step needs beyond the
/// arguments. Built from `ApiState.app_handle` in [`dispatch`]; a test builds
/// it from bare managers (no `AppHandle`, no webview), which is what lets a
/// unit test enter [`proxy_terminal_created`].
pub(crate) struct ProxyTerminalCreateDoor {
    pub terminal_manager: Arc<TerminalManager>,
    /// `None` only if the registry is not in app state — a boot-ordering fault
    /// [`crate::commands::terminal::coord_wiring::finish_terminal_create`]
    /// logs; the pane is still created.
    pub session_registry: Option<Arc<crate::session::SessionRegistry>>,
    /// For the repo-detection emit. `None` in a unit test.
    pub app_handle: Option<tauri::AppHandle>,
}

/// The arm's resolved inputs the coord wiring reads, taken before the spawn
/// consumes them.
pub(crate) struct ProxyTerminalCreated {
    pub title: Option<String>,
    pub intent_repo: Option<String>,
    pub working_dir: Option<String>,
    pub spawn_tenant: Option<uuid::Uuid>,
}

/// The invoke proxy's `terminal_create` arm after a successful spawn: the SAME
/// post-create coord wiring the Tauri command runs
/// ([`crate::commands::terminal::coord_wiring::finish_terminal_create`]) —
/// parking the isolated edit context, registering and binding the pane's
/// `terminal_shell` coord session — then the invoke answer.
///
/// Plan `2026-11-05-remote-session-end-reaches-the-target-but-cannot-end-a-real-session`
/// Phase 4: this arm used to return straight after `create`, so an
/// invoke-created pane had no `terminal_shell` coord row,
/// `GET /terminals/{id}/coord-session` read `null`, and a remote end of it
/// answered `not_found`.
pub(crate) async fn proxy_terminal_created(
    door: &ProxyTerminalCreateDoor,
    info: crate::terminal::types::TerminalInfo,
    isolated_ctx: Option<crate::agent_worktree::isolated_edit::IsolatedEditContext>,
    created: ProxyTerminalCreated,
) -> TauriInvokeResponse {
    let intent = crate::commands::terminal::coord_wiring::terminal_shell_intent(
        created.title.as_deref(),
        "Terminal shell session",
        created.intent_repo,
        None,
        None,
        created.working_dir.as_deref(),
        created.spawn_tenant,
    );
    crate::commands::terminal::coord_wiring::finish_terminal_create(
        &door.terminal_manager,
        door.session_registry.as_ref(),
        door.app_handle.as_ref(),
        &info.id,
        isolated_ctx,
        intent,
        crate::commands::terminal::coord_wiring::CoordRegistration::FreshPinned,
        created.working_dir,
        "tauri_proxy_terminal_create",
    )
    .await;
    TauriInvokeResponse::ok(serde_json::to_value(&info).unwrap_or(Value::Null))
}

// ============================================================================
// terminal_session_record_open / terminal_migrate_session_account
// ============================================================================

/// The managed lifecycle store, or the invoke error a boot-ordering fault
/// answers (`state()` would panic the request instead).
fn lifecycle_store(
    app: &tauri::AppHandle,
) -> Result<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>, TauriInvokeResponse>
{
    app.try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| TauriInvokeResponse::err("session lifecycle store is not initialised"))
}

/// A `CommandResponse` result as the invoke answer, the shape the
/// `check_accounts_usage` arm answers: `{success, message, data}` in `data`.
fn command_response(
    result: Result<crate::commands::CommandResponse, String>,
) -> TauriInvokeResponse {
    match result {
        Ok(cmd) => TauriInvokeResponse::ok(serde_json::json!({
            "success": cmd.success,
            "message": cmd.message,
            "data": cmd.data,
        })),
        Err(e) => TauriInvokeResponse::err(e),
    }
}

/// The `terminal_session_record_open` arm's whole body after the store is in
/// hand: the webview's own command
/// ([`crate::commands::terminal::record_session_open`]) over the same camelCase
/// args. Unknown keys are a `bad args` error, never silently dropped.
pub(crate) fn proxy_session_record_open(
    store: &Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>,
    args: Value,
) -> TauriInvokeResponse {
    match serde_json::from_value::<crate::commands::terminal::SessionRecordOpenArgs>(args) {
        Ok(a) => command_response(crate::commands::terminal::record_session_open(store, a)),
        Err(e) => TauriInvokeResponse::err(format!("bad args: {}", e)),
    }
}

/// The spawn origin the proxied migration's replacement pane carries. This is
/// an HTTP door any local process reaches, so its caller's reason is
/// unnamed, exactly as on the proxied `terminal_create`: `Unknown`, which is
/// autonomous (fail closed), never the webview's `OperatorTerminal`.
const PROXY_MIGRATION_ORIGIN: crate::coord_drain_state::SpawnOrigin =
    crate::coord_drain_state::SpawnOrigin::Unknown;

/// The arguments of the proxied `terminal_migrate_session_account` command.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MigrateSessionAccountArgs {
    claude_session_id: String,
    #[serde(default)]
    target_config_dir: Option<String>,
}

/// The `terminal_migrate_session_account` arm up to the migration itself: its
/// args, then the command's own refusals
/// ([`crate::commands::terminal::plan_session_migration`]) as invoke errors.
/// What it returns is what the arm hands to
/// [`crate::commands::terminal::run_session_migration`].
pub(crate) fn proxy_plan_session_migration(
    store: &crate::session::session_lifecycle_store::SessionLifecycleStore,
    args: Value,
) -> Result<crate::commands::terminal::SessionMigrationPlan, TauriInvokeResponse> {
    let a = serde_json::from_value::<MigrateSessionAccountArgs>(args)
        .map_err(|e| TauriInvokeResponse::err(format!("bad args: {}", e)))?;
    crate::commands::terminal::plan_session_migration(
        store,
        &a.claude_session_id,
        a.target_config_dir,
    )
    .map_err(TauriInvokeResponse::err)
}

/// The `remote_interactivity_probe` trigger a headless proxy caller may run:
/// absent/blank → `unspecified`, `manual` → `manual`, anything else (notably
/// `fleet_view`, which would stamp the device into the scheduler's sweep set)
/// refused.
fn proxy_probe_trigger(raw: Option<&str>) -> Result<&'static str, String> {
    match raw.map(str::trim) {
        None | Some("") => Ok("unspecified"),
        Some("manual") => Ok("manual"),
        Some(t) => Err(format!(
            "remote_interactivity_probe:trigger_not_allowed: {t:?} — the proxy runs only \
             trigger \"manual\" (or none)"
        )),
    }
}

// ============================================================================
// Router
// ============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::post;
    axum::Router::new().route("/ui-bridge/tauri/invoke", post(tauri_invoke_handler))
}

#[cfg(test)]
mod probe_trigger_tests {
    use super::proxy_probe_trigger;

    #[test]
    fn the_proxy_maps_probe_triggers() {
        assert_eq!(proxy_probe_trigger(None), Ok("unspecified"));
        assert_eq!(proxy_probe_trigger(Some("")), Ok("unspecified"));
        assert_eq!(proxy_probe_trigger(Some("   ")), Ok("unspecified"));
        assert_eq!(proxy_probe_trigger(Some("manual")), Ok("manual"));
        for refused in ["fleet_view", "scheduler", "Manual", "anything"] {
            let e = proxy_probe_trigger(Some(refused)).unwrap_err();
            assert!(
                e.starts_with("remote_interactivity_probe:trigger_not_allowed:"),
                "{e}"
            );
        }
    }
}

#[cfg(test)]
mod edit_sibling_tests {
    use super::*;

    /// Plan 2026-10-10 Phase 2 (vet V9): the proxy answers a bad `editSiblings`
    /// entry with a typed 400 `invalid_edit_sibling`, and every other response
    /// keeps the 200 it always had.
    #[test]
    fn a_bad_edit_sibling_is_a_typed_400_and_nothing_else_changes_status() {
        let refusal = crate::agent_worktree::edit_sibling::validate_spawn_edit_siblings(
            Some("qontinui-coord"),
            &["../x".to_string()],
        )
        .unwrap_err();
        let r = TauriInvokeResponse::edit_sibling_refusal(refusal);
        assert_eq!(r.status, Some(StatusCode::BAD_REQUEST));
        let body = serde_json::to_value(&r).unwrap();
        assert_eq!(body["code"], "invalid_edit_sibling");
        assert_eq!(body["success"], false);
        assert!(body.get("status").is_none(), "status is not serialised");

        assert_eq!(TauriInvokeResponse::err("x").status, None);
        assert_eq!(TauriInvokeResponse::ok(Value::Null).status, None);
        let ok = serde_json::to_value(TauriInvokeResponse::ok(Value::Null)).unwrap();
        assert!(ok.get("code").is_none());
    }
}

#[cfg(test)]
mod spawn_tenant_tests {
    use super::*;

    /// N4 (review of plan 2026-09-10). The `terminal_create` proxy answers a
    /// tenant this runner holds no credential for with the same typed invoke
    /// error the Tauri command returns, and admits a paired one.
    #[test]
    fn the_terminal_create_proxy_refuses_an_unpaired_spawn_tenant() {
        let amb = crate::test_env::isolated_ambient();
        let (a, b) = (uuid::Uuid::from_u128(0xA1), uuid::Uuid::from_u128(0xB2));
        amb.write_active_tenant_id(a);
        crate::auth::AuthManager::new()
            .store_tenant_device_jwt(&a, "header.payload.signature")
            .unwrap();

        let refusal =
            spawn_tenant_or_invoke_error(Some(&b.to_string())).expect_err("unpaired → refused");
        assert!(!refusal.success);
        assert_eq!(refusal.code.as_deref(), Some("TERMINAL_TENANT_NOT_PAIRED"));
        assert_eq!(refusal.status, Some(StatusCode::BAD_REQUEST));
        let error = refusal.error.unwrap_or_default();
        assert!(error.starts_with("terminal:tenant_not_paired:"), "{error}");
        // The same text through the failed-create arm gives the same answer.
        let via_create = TauriInvokeResponse::spawn_failed(error.clone());
        assert_eq!(
            via_create.code.as_deref(),
            Some("TERMINAL_TENANT_NOT_PAIRED")
        );
        assert_eq!(via_create.status, Some(StatusCode::BAD_REQUEST));
        assert_eq!(
            spawn_tenant_or_invoke_error(Some(&a.to_string())).ok(),
            Some(Some(a))
        );
    }

    /// Coord finding `659b574a`'s defect class on this door: a selector spelled
    /// any other way than `tenantId` used to be dropped, spawning on the machine
    /// default with `success: true`. The Tauri command's own argument is
    /// `tenant_id`, so that spelling was the one a caller would reach for.
    #[test]
    fn the_terminal_create_proxy_selects_the_tenant_under_every_spelling() {
        for key in ["tenantId", "tenant_id", "tenant"] {
            let args: TerminalCreateArgs =
                serde_json::from_value(serde_json::json!({ key: "t-1" })).unwrap();
            assert_eq!(args.tenant_id.as_deref(), Some("t-1"), "{key}");
        }
        assert!(
            serde_json::from_value::<TerminalCreateArgs>(serde_json::json!({
                "tenantId": "t-1",
                "tenant": "t-2",
            }))
            .is_err(),
            "two spellings at once is a duplicate field, never a silent pick"
        );
    }
}

#[cfg(test)]
mod spawn_refusal_tests {
    use super::*;
    use crate::terminal::spawn_refusal::{refusal_family_fixtures, UNRECOGNISED_SPAWN_FAILURES};

    fn answer(r: &TauriInvokeResponse) -> (StatusCode, Value) {
        (
            r.status.unwrap_or(StatusCode::OK),
            serde_json::to_value(r).unwrap(),
        )
    }

    /// Plan `2026-11-08-…-leaves-the-tree-on-disk` Phase 0: one case per
    /// refusal family, each text built by its real producer, answered with the
    /// code and typed status `POST /terminals` gives it — and the text verbatim.
    #[test]
    fn a_refused_spawn_keeps_its_typed_answer_on_the_proxy() {
        for (text, status, code) in refusal_family_fixtures() {
            let (got, body) = answer(&TauriInvokeResponse::spawn_failed(text.clone()));
            assert_eq!(got, status, "{text}");
            assert_eq!(body["code"], code, "{text}");
            assert_eq!(body["error"], text.as_str());
            assert_eq!(body["success"], false);
        }
    }

    /// The precheck's answer equals the failed-create arm's for the same text,
    /// so a caller cannot tell which gate answered.
    #[test]
    fn the_proxy_precheck_answers_as_the_failed_create_arm_does() {
        for (text, _, _) in refusal_family_fixtures() {
            let t = text.clone();
            let precheck = spawn_precheck_or_invoke_error(move || Err(t))
                .expect_err("a refusal is an invoke error");
            assert_eq!(
                answer(&precheck),
                answer(&TauriInvokeResponse::spawn_failed(text.clone())),
                "{text}"
            );
        }
        assert!(spawn_precheck_or_invoke_error(|| Ok(())).is_ok());
    }

    /// Anything unrecognised keeps today's untyped 200 invoke error.
    #[test]
    fn any_other_spawn_failure_stays_an_untyped_invoke_error() {
        for text in UNRECOGNISED_SPAWN_FAILURES {
            let (status, body) = answer(&TauriInvokeResponse::spawn_failed(text.to_string()));
            assert_eq!(status, StatusCode::OK, "{text}");
            assert!(body.get("code").is_none(), "{text}");
            assert_eq!(body["error"], *text);
        }
    }
}

#[cfg(test)]
mod terminal_create_coord_wiring_tests {
    use super::*;
    use crate::commands::terminal::coord_wiring::test_support;

    /// ARMING TEST for plan
    /// `2026-11-05-remote-session-end-reaches-the-target-but-cannot-end-a-real-session`
    /// Phase 4: a pane created through the invoke proxy's `terminal_create`
    /// arm carries a non-null `GET /terminals/{id}/coord-session`.
    ///
    /// Enters through [`proxy_terminal_created`], the arm's whole body after a
    /// successful spawn — the spawn itself is a dead fixture pane, because
    /// `TerminalManager::create` needs a live `AppHandle`. If the arm stops
    /// running the shared post-create wiring, the pane is never bound and this
    /// fails.
    #[tokio::test]
    async fn an_invoke_created_terminal_is_bound_to_a_coord_session() {
        let fx = test_support::registry();
        let tm = Arc::new(TerminalManager::new());
        let door = ProxyTerminalCreateDoor {
            terminal_manager: tm.clone(),
            session_registry: Some(fx.registry.clone()),
            app_handle: None,
        };
        let session = Arc::new(crate::terminal::session::tests::make_test_session(
            Arc::new(std::sync::Mutex::new(Vec::new())),
        ));
        let info = session.info();
        tm.insert_for_test(&info.id, session);

        let reply = proxy_terminal_created(
            &door,
            info,
            None,
            ProxyTerminalCreated {
                title: Some("invoke-created pane".to_string()),
                intent_repo: None,
                working_dir: None,
                spawn_tenant: None,
            },
        )
        .await;
        assert!(reply.success, "{:?}", reply.error);
        let id = reply.data.as_ref().expect("terminal info")["id"]
            .as_str()
            .expect("id")
            .to_string();

        let Json(body) = crate::mcp::terminals::coord_session_for(&tm, &id)
            .unwrap_or_else(|_| panic!("terminal {id} is registered"));
        let body = serde_json::to_value(&body).expect("serializes");
        let coord = body["data"]["coord_session_id"]
            .as_str()
            .unwrap_or_else(|| panic!("coord-session must be non-null: {body}"));
        let coord = uuid::Uuid::parse_str(coord).expect("a uuid");
        assert_eq!(tm.get(&id).unwrap().coord_session_id(), Some(coord));
    }
}

#[cfg(test)]
mod session_migration_proxy_tests {
    //! Plan `2026-11-26-terminal-migration-never-loses-a-session-has-no-forced-migration-run`
    //! Phase 2a: the headless forced-migration run reaches the operator
    //! migration and the lifecycle record-open through this proxy.
    //!
    //! [`dispatch`] needs an `ApiState`, whose `AppHandle` no unit test can
    //! build (this crate has no `tauri::test` mock), so these enter at the arms'
    //! bodies — [`safelist_refusal`] (the handler's 403),
    //! [`proxy_session_record_open`] and [`proxy_plan_session_migration`] —
    //! with a source pin that [`dispatch`] routes each name to them.
    use super::*;
    use crate::session::session_lifecycle_store::SessionLifecycleStore;

    const MIGRATE: &str = "terminal_migrate_session_account";
    const RECORD_OPEN: &str = "terminal_session_record_open";

    fn store() -> (tempfile::TempDir, Arc<SessionLifecycleStore>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json"))
            .expect("store opens");
        (dir, Arc::new(store))
    }

    fn record_open(store: &Arc<SessionLifecycleStore>, csid: &str, config_dir: &str) {
        let reply = proxy_session_record_open(
            store,
            serde_json::json!({
                "claudeSessionId": csid,
                "configDir": config_dir,
                "workingDir": "/tmp/repo",
                "zoneIndex": 0,
                "terminalId": "term-proxy-1",
            }),
        );
        assert!(reply.success, "{:?}", reply.error);
    }

    #[test]
    fn both_commands_are_safelisted_so_the_proxy_no_longer_answers_403() {
        for command in [MIGRATE, RECORD_OPEN] {
            assert!(ALLOWED_PROXIED_COMMANDS.contains(&command), "{command}");
            assert!(
                safelist_refusal(command).is_none(),
                "{command} must reach dispatch, not the 403"
            );
        }
        let (status, Json(body)) =
            safelist_refusal("terminal_definitely_not_proxied").expect("still refused");
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body.error, "command not in safelist");
    }

    /// The safelist admits a name; only a `dispatch` arm makes it do anything
    /// other than the "not implemented" fallthrough. Pinned on the source,
    /// since `dispatch` itself cannot run without an `AppHandle`.
    #[test]
    fn dispatch_routes_each_command_to_its_arm_body() {
        let src = include_str!("tauri_proxy.rs");
        let dispatch_src = &src[src.find("async fn dispatch(").expect("dispatch")..];
        let dispatch_src = &dispatch_src[..dispatch_src.find("\n}\n").expect("end of dispatch")];
        // Each arm's text runs from its label to the NEXT arm label (or the
        // end of `dispatch`), so a call in a later arm cannot satisfy it.
        let next_arm = regex::Regex::new(r#"\n\s*(?:"[a-z_]+"|_) =>"#).expect("static pattern");
        let arm_body = |command: &str| -> &str {
            let label = format!("\"{command}\" =>");
            let at = dispatch_src
                .find(&label)
                .unwrap_or_else(|| panic!("no dispatch arm for {command}"));
            let rest = &dispatch_src[at + label.len()..];
            let end = next_arm.find(rest).map_or(rest.len(), |m| m.start());
            &rest[..end]
        };
        for (command, call) in [
            (RECORD_OPEN, "proxy_session_record_open("),
            (MIGRATE, "proxy_plan_session_migration("),
            (MIGRATE, "run_session_migration("),
            (MIGRATE, "PROXY_MIGRATION_ORIGIN"),
        ] {
            assert!(
                arm_body(command).contains(call),
                "{command}'s arm must use {call}"
            );
        }
        assert!(
            !arm_body(MIGRATE).contains("OperatorTerminal"),
            "the proxied migration must not label its spawn an operator's"
        );
    }

    /// Review finding 1: the proxied migration's replacement pane is not an
    /// operator spawn — the focus coalescer, `intent.spawn_origin` and
    /// `is_autonomous` all read this.
    #[test]
    fn the_proxied_migration_spawns_under_a_non_operator_origin() {
        use crate::coord_drain_state::SpawnOrigin;
        assert_eq!(PROXY_MIGRATION_ORIGIN, SpawnOrigin::Unknown);
        assert!(PROXY_MIGRATION_ORIGIN.is_autonomous());
        assert_ne!(PROXY_MIGRATION_ORIGIN, SpawnOrigin::OperatorTerminal);
    }

    /// Point the machine-global roster (`claude-accounts.json`) inside the
    /// fixture. `IsolatedAmbient` does not capture `XDG_CONFIG_HOME`, so the
    /// returned guard restores it; bind it AFTER the fixture so it drops first.
    /// Not on macOS, where `dirs::config_dir` ignores `XDG_CONFIG_HOME`.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn pin_roster_inside(amb: &crate::test_env::IsolatedAmbient) -> crate::test_env::EnvVarRestore {
        let restore = crate::test_env::EnvVarRestore::capture(&["XDG_CONFIG_HOME"]);
        std::env::set_var("XDG_CONFIG_HOME", amb.dir().join("xdg-config"));
        let roster = crate::claude_accounts::claude_accounts_file_path().expect("roster path");
        assert!(
            roster.starts_with(amb.dir()),
            "the roster escaped the fixture: {}",
            roster.display()
        );
        restore
    }

    #[test]
    fn a_proxied_record_open_writes_the_lifecycle_row() {
        let (_dir, store) = store();
        assert!(store.get("csid-proxy-1").is_none());

        let reply = proxy_session_record_open(
            &store,
            serde_json::json!({
                "claudeSessionId": "csid-proxy-1",
                "configDir": "/accounts/a",
                "zoneIndex": 2,
                "terminalId": "term-proxy-1",
            }),
        );
        assert!(reply.success, "{:?}", reply.error);
        let data = reply.data.expect("payload");
        assert_eq!(data["success"], true);
        // The command's own WRITTEN-vs-BOUND report rides through.
        assert_eq!(data["data"]["recorded"], true, "{data}");
        assert_eq!(data["data"]["confirmed"], false, "{data}");

        let row = store.get("csid-proxy-1").expect("the row is in the store");
        assert_eq!(row.config_dir.as_deref(), Some("/accounts/a"));
        assert_eq!(row.terminal_id, "term-proxy-1");
        assert_eq!(row.zone_index, 2);
        assert_eq!(row.page_id, "default");
    }

    #[test]
    fn a_proxied_record_open_refuses_bad_args_and_writes_nothing() {
        let (_dir, store) = store();
        // A snake_case key is an unknown field, never a silently dropped one.
        let misspelled = proxy_session_record_open(
            &store,
            serde_json::json!({
                "claudeSessionId": "csid-x",
                "config_dir": "/accounts/a",
                "zoneIndex": 0,
                "terminalId": "term-x",
            }),
        );
        assert!(!misspelled.success);
        assert!(
            misspelled
                .error
                .as_deref()
                .unwrap_or("")
                .starts_with("bad args:"),
            "{:?}",
            misspelled.error
        );
        // The command's own blank-identity guard answers through the proxy.
        let blank = proxy_session_record_open(
            &store,
            serde_json::json!({
                "claudeSessionId": "csid-y",
                "zoneIndex": 0,
                "terminalId": "  ",
            }),
        );
        assert!(!blank.success);
        assert!(
            blank
                .error
                .as_deref()
                .unwrap_or("")
                .contains("terminalId must be non-empty"),
            "{:?}",
            blank.error
        );
        assert!(store.get("csid-x").is_none());
        assert!(store.get("csid-y").is_none());
    }

    #[test]
    fn a_proxied_migration_of_an_unknown_session_is_the_commands_refusal() {
        let (_dir, store) = store();
        let refusal = proxy_plan_session_migration(
            &store,
            serde_json::json!({
                "claudeSessionId": "csid-never-recorded",
                "targetConfigDir": "/accounts/b",
            }),
        )
        .expect_err("no row → refused");
        assert!(!refusal.success);
        assert_eq!(refusal.status, None, "an invoke error, not a 403");
        assert_eq!(
            refusal.error.as_deref(),
            Some("unknown session: csid-never-recorded")
        );
    }

    #[test]
    fn a_proxied_migration_refuses_bad_args() {
        let (_dir, store) = store();
        for args in [
            serde_json::json!({}),
            serde_json::json!({ "claude_session_id": "csid" }),
        ] {
            let refusal =
                proxy_plan_session_migration(&store, args.clone()).expect_err("bad args → refused");
            assert!(
                refusal
                    .error
                    .as_deref()
                    .unwrap_or("")
                    .starts_with("bad args:"),
                "{args}: {:?}",
                refusal.error
            );
        }
    }

    /// A row recorded through the proxy is the row the proxied migration finds:
    /// with an explicit target that is not a configured account, the refusal is
    /// the not-configured one, past the lookup.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_proxied_migration_finds_the_proxied_row_and_refuses_an_unconfigured_target() {
        let amb = crate::test_env::isolated_ambient();
        let _xdg = pin_roster_inside(&amb);
        let (dir, store) = store();
        record_open(&store, "csid-proxy-2", "/accounts/a");
        let unconfigured = dir.path().join("never-configured").display().to_string();

        let refusal = proxy_plan_session_migration(
            &store,
            serde_json::json!({
                "claudeSessionId": "csid-proxy-2",
                "targetConfigDir": unconfigured,
            }),
        )
        .expect_err("unconfigured target → refused");
        assert_eq!(
            refusal.error.as_deref(),
            Some(format!("'{unconfigured}' is not a configured claude_config_dir").as_str())
        );
    }

    /// The roster is written inside the fixture (the path is checked first,
    /// as `commands::ai_settings`'s roster tests do), so a target equal to the
    /// session's own configured account reaches the equals refusal; a
    /// different configured target yields a plan.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_proxied_migration_onto_the_sessions_own_account_is_refused() {
        let amb = crate::test_env::isolated_ambient();
        let _xdg = pin_roster_inside(&amb);
        let a = amb.dir().join("acct-a").display().to_string();
        let b = amb.dir().join("acct-b").display().to_string();
        let dirs = vec![a.clone(), b.clone()];
        crate::claude_accounts::update(move |r| r.claude_config_dirs = dirs).expect("write roster");

        let (_dir, store) = store();
        record_open(&store, "csid-proxy-3", &a);

        let same = proxy_plan_session_migration(
            &store,
            serde_json::json!({ "claudeSessionId": "csid-proxy-3", "targetConfigDir": a }),
        )
        .expect_err("same account → refused");
        assert_eq!(
            same.error.as_deref(),
            Some("target account equals the session's current account")
        );

        let plan = proxy_plan_session_migration(
            &store,
            serde_json::json!({ "claudeSessionId": "csid-proxy-3", "targetConfigDir": b }),
        )
        .expect("a configured other account plans");
        assert_eq!(plan.src, a);
        assert_eq!(plan.dst, b);
        assert!(plan.explicit_target);
        assert_eq!(plan.record.claude_session_id, "csid-proxy-3");
    }
}
