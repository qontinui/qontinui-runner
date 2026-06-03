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
                )
                .await;

            match tm.create(
                a.title,
                working_dir,
                a.page_id,
                a.cols,
                a.rows,
                state.app_handle.clone(),
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
            match session.write(&bytes) {
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
            let id = a.terminal_id.clone();
            match tokio::task::spawn_blocking(move || tm.close(&id)).await {
                Ok(Ok(())) => TauriInvokeResponse::ok(serde_json::json!({ "success": true })),
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
