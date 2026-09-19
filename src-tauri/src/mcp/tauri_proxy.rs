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
}

/// The `terminal_create` proxy's spawn-tenant admission: the shared
/// [`crate::commands::terminal::admit_spawn_tenant`], answered as the invoke
/// error a refused `terminal_create` command would return.
fn spawn_tenant_or_invoke_error(
    raw: Option<&str>,
) -> Result<Option<uuid::Uuid>, TauriInvokeResponse> {
    crate::commands::terminal::admit_spawn_tenant(raw).map_err(TauriInvokeResponse::err)
}

impl TauriInvokeResponse {
    fn ok(data: Value) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }
    fn err(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(msg.into()),
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
) -> Result<Json<TauriInvokeResponse>, (StatusCode, Json<ForbiddenResponse>)> {
    if !ALLOWED_PROXIED_COMMANDS.contains(&req.command.as_str()) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ForbiddenResponse {
                error: "command not in safelist",
                command: req.command,
                allowed: ALLOWED_PROXIED_COMMANDS,
            }),
        ));
    }

    info!(command = %req.command, "tauri_proxy: dispatching command");

    let response = dispatch(state, req).await;
    Ok(Json(response))
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
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
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
                #[serde(default)]
                tenant_id: Option<String>,
            }
            let a = match serde_json::from_value::<Args>(req.args) {
                Ok(v) => v,
                Err(e) => return TauriInvokeResponse::err(format!("bad args: {}", e)),
            };
            // Coord device drain (plan `2026-09-13-drained-runner-never-reaches-idle`,
            // D3): this is the HTTP proxy of `terminal_create`, not the runner UI,
            // so its caller is autonomous and is deferred while the drain holds.
            if let crate::coord_drain_state::DrainGate::Defer { reason, .. } =
                crate::coord_drain_state::drain_gate_for_work(
                    crate::coord_drain_state::SpawnOrigin::Unknown,
                    &format!(
                        "proxy_terminal:{}",
                        a.title.as_deref().unwrap_or("untitled")
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

            let (working_dir, isolated_ctx) =
                crate::agent_worktree::isolated_edit::acquire_for_terminal(
                    effective_intent_repo.as_deref(),
                    a.title.as_deref().unwrap_or("Terminal edit session"),
                    a.working_dir,
                    a.agent_session_id,
                    spawn_tenant,
                )
                .await;

            // The shared session-env contribution (`QONTINUI_SESSION_WORKTREES`
            // + the configured plan directories) onto the PTY, derived before
            // the ctx is parked. See `agent_worktree::session_env`.
            let extra_env =
                crate::agent_worktree::session_env::session_extra_env(isolated_ctx.as_ref());

            match tm.create(
                a.title,
                working_dir,
                a.page_id,
                a.cols,
                a.rows,
                state.app_handle.clone(),
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
            ) {
                Ok(info) => {
                    if let Some(ctx) = isolated_ctx {
                        if let Some(session) = tm.get(&info.id) {
                            session.set_isolated_edit_ctx(ctx);
                        }
                    }
                    TauriInvokeResponse::ok(serde_json::to_value(&info).unwrap_or(Value::Null))
                }
                Err(e) => TauriInvokeResponse::err(e),
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

        // Unreachable: safelist check above prevents anything else.
        _ => TauriInvokeResponse::err(format!("command '{}' not implemented", req.command)),
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
        let error = refusal.error.unwrap_or_default();
        assert!(error.starts_with("terminal:tenant_not_paired:"), "{error}");
        assert_eq!(
            spawn_tenant_or_invoke_error(Some(&a.to_string())).ok(),
            Some(Some(a))
        );
    }
}
