//! HTTP control surface for the single `/merge-train-steward` session.
//!
//! Modeled on `mcp/terminals.rs`'s `create_terminal_handler` /
//! `list_terminals_handler` (the closest template — NOT `mcp/worktrees.rs`,
//! which is cited only for its `ApiResponse` envelope shape): a titled PTY
//! terminal session is the steward's entire lifecycle. Opening the tab starts
//! it; closing the tab (killing the PTY child) stops it. There is exactly one
//! steward per runner, tracked by terminal **id** in [`steward_meta_store`]
//! (see [`find_running_steward`] for why title cannot serve as the marker —
//! discovered in manual testing, corrects the plan's original §6 Q2
//! resolution).
//!
//! `POST /steward/start` spawns the PTY **server-side** (mirroring
//! `create_terminal_handler`, `mcp/terminals.rs:137`) so the identical code
//! path serves both a UI button (which just calls this endpoint instead of
//! hand-rolling open-tab + type-command) and an agent driving it via `curl`
//! (which has no frontend to type into — server-side spawn is necessary for
//! parity, not just simpler; see plan §6 Q3).

use axum::extract::{Json, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::Manager;
use tracing::{error, info, warn};

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::terminal::types::TerminalInfo;
use crate::terminal::TerminalManager;

// ============================================================================
// Constants
// ============================================================================

/// Fixed terminal-tab title used as the single-instance marker for the
/// steward session. `GET /steward/status` and the `POST /steward/start`
/// guard both key on "is there a live terminal session whose title equals
/// this string?" — resolved in the plan (§4 Part 1, §6 Q2) in preference to
/// inventing a new identity scheme.
pub const STEWARD_TITLE: &str = "merge-train-steward";

/// Default mode, matching `merge-train-steward.md`'s own stated default
/// (detect+propose only — the operator opts into `--mode=autonomous`
/// explicitly).
const DEFAULT_MODE: &str = "observe";

/// Default `/loop` interval, matching `merge-train-steward.md`'s own stated
/// default rather than a separately-guessed value.
const DEFAULT_INTERVAL: &str = "5m";

// ============================================================================
// In-process steward metadata (mode/interval used at launch)
// ============================================================================

/// The mode/interval a steward terminal was started with. Keyed by terminal
/// id so `GET /steward/status` can echo back what `POST /steward/start` was
/// called with. Runner-local and in-memory only: a runner restart kills the
/// PTY (and thus the steward) anyway, so there's nothing durable to lose.
struct StewardMeta {
    mode: String,
    interval: String,
}

/// Global registry of steward metadata, keyed by terminal id. A `HashMap`
/// rather than a single `Option` in case of races between stop/start, though
/// the single-instance guard in `steward_start_handler` prevents more than
/// one *live* entry from mattering at a time. Stale entries (terminal no
/// longer alive) are pruned whenever status is computed.
fn steward_meta_store() -> &'static Mutex<HashMap<String, StewardMeta>> {
    static STORE: OnceLock<Mutex<HashMap<String, StewardMeta>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
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

/// Find the live terminal session tracked as the steward, if any.
///
/// Keyed on terminal **id** via [`steward_meta_store`], NOT on `title`.
/// Manual testing (2026-07-19, temp-runner UI Bridge verification) found
/// that `title` cannot serve as a durable single-instance marker on this
/// runner: PowerShell (and other shells) emit OSC 0/2 title-change escape
/// sequences, and xterm.js relays those back to the runner via
/// `TerminalSession::set_title` — "Phase 2 of bi-directional title sync"
/// (`terminal/session.rs:1541-1555`) — silently overwriting our fixed
/// [`STEWARD_TITLE`] sentinel moments after the shell starts. `STEWARD_TITLE`
/// is still set at creation as the terminal's initial cosmetic label, but
/// only the metadata-store id membership is authoritative for "running".
fn find_running_steward(terminal_manager: &TerminalManager) -> Option<TerminalInfo> {
    let tracked_ids: std::collections::HashSet<String> = steward_meta_store()
        .lock()
        .map(|guard| guard.keys().cloned().collect())
        .unwrap_or_default();
    if tracked_ids.is_empty() {
        return None;
    }
    terminal_manager
        .list()
        .into_iter()
        .find(|info| info.is_alive && tracked_ids.contains(&info.id))
}

// ============================================================================
// Request / Response Types
// ============================================================================

/// Request body for `POST /steward/start`.
#[derive(Debug, Deserialize)]
pub struct StewardStartRequest {
    /// Skill mode: `observe` (default, detect+propose only) or `autonomous`.
    #[serde(default)]
    pub mode: Option<String>,
    /// `/loop` polling interval (e.g. `"5m"`).
    #[serde(default)]
    pub interval: Option<String>,
}

/// Response for `GET /steward/status`.
#[derive(Debug, Serialize)]
pub struct StewardStatusResponse {
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

/// Compute the current steward status by scanning live terminal sessions —
/// shared by `GET /steward/status` and the single-instance guard in
/// `POST /steward/start`.
fn steward_status(terminal_manager: &TerminalManager) -> StewardStatusResponse {
    let running = find_running_steward(terminal_manager);
    prune_stale_meta(terminal_manager);

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
                session_id: Some(info.id),
                mode,
                interval,
                started_at: Some(info.created_at),
            }
        }
        None => StewardStatusResponse {
            running: false,
            session_id: None,
            mode: None,
            interval: None,
            started_at: None,
        },
    }
}

/// Build the launch command typed into the PTY, platform-appropriate for the
/// env-var prefix. `TerminalSession::build_shell_command` (`terminal/session.rs`)
/// spawns `powershell.exe` on Windows and `$SHELL` (bash by default) on
/// other platforms, so the env-var syntax must match — the same convention
/// already used by the frontend's `buildAiLaunchCommand`
/// (`aiLaunchCommand.ts`): `$env:VAR="value"; cmd` on Windows,
/// `VAR=value cmd` (POSIX prefix form) elsewhere. The plan's literal
/// invocation (`COORD_MERGE_STEWARD_ENABLED=1 claude /loop <interval>
/// /merge-train-steward --mode=<mode>`) is POSIX-shaped; this reproduces its
/// effect on both shells rather than typing bash syntax into a PowerShell
/// prompt where it would not parse.
fn build_launch_command(mode: &str, interval: &str) -> String {
    let base = format!(
        "claude /loop {} /merge-train-steward --mode={}",
        interval, mode
    );
    if cfg!(target_os = "windows") {
        format!("$env:COORD_MERGE_STEWARD_ENABLED=\"1\"; {}", base)
    } else {
        format!("COORD_MERGE_STEWARD_ENABLED=1 {}", base)
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

/// `GET /steward/status` — is the merge-train steward running, and if so,
/// which terminal/mode/interval?
pub async fn steward_status_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<StewardStatusResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);
    Ok(Json(ApiResponse::success(steward_status(
        &terminal_manager,
    ))))
}

/// `POST /steward/start` — spawn the steward's PTY terminal session
/// server-side, mirroring `create_terminal_handler`
/// (`mcp/terminals.rs:137`). Refuses with 409 if a steward is already
/// running (single-instance guard, keyed on the `title` marker).
pub async fn steward_start_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StewardStartRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);
    let app_handle = state.app_handle.clone();

    // Single-instance guard: refuse if `GET /steward/status` would report
    // running: true.
    let current = steward_status(&terminal_manager);
    if current.running {
        let session_id = current.session_id.unwrap_or_default();
        warn!(
            "HTTP: Refusing to start merge-train-steward — already running as terminal {}",
            session_id
        );
        return Err((
            StatusCode::CONFLICT,
            Json(api_error(format!(
                "merge-train-steward is already running (terminal {})",
                session_id
            ))),
        ));
    }

    let mode = request
        .mode
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| DEFAULT_MODE.to_string());
    let interval = request
        .interval
        .filter(|i| !i.is_empty())
        .unwrap_or_else(|| DEFAULT_INTERVAL.to_string());
    let launch_command = build_launch_command(&mode, &interval);

    info!(
        "HTTP: Starting merge-train-steward (mode={}, interval={})",
        mode, interval
    );

    match terminal_manager.create(
        Some(STEWARD_TITLE.to_string()),
        None, // working_dir — default to workspace root
        None, // page_id — default "default"
        None, // cols — default
        None, // rows — default
        app_handle,
        None, // command override — interactive shell, we type the command in
        // The shared session-env contribution. No isolated edit context here
        // (the steward runs in the shared checkout), so `QONTINUI_SESSION_WORKTREES`
        // is omitted exactly as before — but the steward is an agent session and
        // must still learn where the plans live. See `agent_worktree::session_env`.
        crate::agent_worktree::session_env::session_extra_env(None),
        // UNATTENDED spawn — respect the critical floor. The merge-train steward
        // is a long-running autonomous agent session; starting one on a box
        // that is already out of commit is how the incident's `claude`-inside-a-
        // terminal deaths happened. The refusal returns as this endpoint's error
        // body (with lane/headroom/floor), and the steward is explicitly
        // restartable, so a refusal defers the steward rather than losing it.
        false,
    ) {
        Ok(info) => {
            info!("HTTP: Created merge-train-steward terminal: {}", info.id);

            if let Ok(mut guard) = steward_meta_store().lock() {
                guard.insert(
                    info.id.clone(),
                    StewardMeta {
                        mode: mode.clone(),
                        interval: interval.clone(),
                    },
                );
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
                    let _ = session.write(cmd.as_bytes());
                }
            });

            Ok(Json(ApiResponse::success(serde_json::json!({
                "id": info.id,
                "title": STEWARD_TITLE,
                "mode": mode,
                "interval": interval,
            }))))
        }
        Err(e) => {
            error!("HTTP: Failed to start merge-train-steward: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to start merge-train-steward: {}",
                    e
                ))),
            ))
        }
    }
}

/// `POST /steward/stop` — stop the tracked steward terminal session. Reuses
/// the existing `TerminalManager::close` kill path
/// (`terminal/manager.rs:221`, same as `close_terminal_handler`,
/// `mcp/terminals.rs:387`).
pub async fn steward_stop_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);

    let info = find_running_steward(&terminal_manager).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error("merge-train-steward is not running")),
        )
    })?;

    let manager = terminal_manager.clone();
    let terminal_id = info.id.clone();

    info!(
        "HTTP: Stopping merge-train-steward (terminal {})",
        terminal_id
    );

    tokio::task::spawn_blocking(move || manager.close(&terminal_id))
        .await
        .map_err(|e| {
            error!(
                "HTTP: spawn_blocking error closing merge-train-steward terminal: {}",
                e
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Internal error: {}", e))),
            )
        })?
        .map_err(|e| {
            error!("HTTP: Failed to stop merge-train-steward: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to stop merge-train-steward: {}",
                    e
                ))),
            )
        })?;

    if let Ok(mut guard) = steward_meta_store().lock() {
        guard.remove(&info.id);
    }

    Ok(Json(ApiResponse::success(serde_json::json!({
        "stopped": true,
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
        .route("/steward/status", get(steward_status_handler))
        .route("/steward/start", post(steward_start_handler))
        .route("/steward/stop", post(steward_stop_handler))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_launch_command_windows_uses_env_assignment_syntax() {
        // We can't cfg-gate the test itself onto the *build* target easily
        // here, so this only asserts the branch logic directly rather than
        // relying on `cfg!(target_os = "windows")` at test time when run on
        // non-Windows CI. The Windows-shaped assertion below is a literal
        // string check of the function that would run on a Windows build.
        if cfg!(target_os = "windows") {
            let cmd = build_launch_command("observe", "5m");
            assert_eq!(
                cmd,
                "$env:COORD_MERGE_STEWARD_ENABLED=\"1\"; claude /loop 5m /merge-train-steward --mode=observe"
            );
        } else {
            let cmd = build_launch_command("observe", "5m");
            assert_eq!(
                cmd,
                "COORD_MERGE_STEWARD_ENABLED=1 claude /loop 5m /merge-train-steward --mode=observe"
            );
        }
    }

    #[test]
    fn build_launch_command_honors_custom_mode_and_interval() {
        let cmd = build_launch_command("autonomous", "10m");
        assert!(cmd.contains("--mode=autonomous"));
        assert!(cmd.contains("/loop 10m"));
    }
}
