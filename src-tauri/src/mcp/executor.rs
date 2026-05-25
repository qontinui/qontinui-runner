//! Executor lifecycle HTTP handlers for MCP API.
//!
//! Provides endpoints for managing the Python executor subprocess:
//! - `POST /executor/restart` — stop the current Python process (if any) and
//!   respawn a fresh one. Useful for recovering from transient startup
//!   failures (e.g., a cv2 DLL crash) without restarting the runner itself.
//!
//! Design notes
//! ------------
//! * The Python bridge is owned by `BridgeManager` which stores bridges in a
//!   `std::sync::RwLock<HashMap<BridgeId, ManagedBridge>>`. We never hold that
//!   lock across `.await`; `stop_default_bridge()` / `start_default_bridge()`
//!   already handle the remove-drop-op-reinsert dance internally.
//! * After a successful stop, the lifecycle is in `Shutdown`. The `ready()`
//!   transition is only valid from `Initializing`, so we explicitly reset the
//!   lifecycle to `Initializing` before spawning (mirrors the pattern used in
//!   `executor::extraction_executor::ensure_started`).
//! * A process-wide `RESTART_LOCK` (tokio `Mutex`) serialises restarts so
//!   callers can't hammer the endpoint into racing `start`/`stop` pairs.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Serialize;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::{sleep, Instant};
use tracing::{error, info, warn};

use crate::executor::state::ExecutorState;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Success payload for `POST /executor/restart`.
#[derive(Debug, Serialize)]
pub struct RestartSuccess {
    /// Resulting executor state name (e.g. "Ready").
    pub state: String,
    /// PID of the freshly-spawned Python process, if observable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// Error payload (sent inside the `data` field of an error `ApiResponse`).
#[derive(Debug, Serialize)]
pub struct RestartFailure {
    /// Resulting executor state name (e.g. "Failed", "Initializing").
    pub state: String,
    /// Last known error message from the bridge / lifecycle, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

// ---------------------------------------------------------------------------
// Concurrency gate
// ---------------------------------------------------------------------------

/// Serialises concurrent calls to `/executor/restart` so we never race
/// stop/start pairs against each other.
fn restart_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

// ---------------------------------------------------------------------------
// Timeouts
// ---------------------------------------------------------------------------

/// Maximum time to wait for the previous Python process to gracefully exit
/// before we rely on the bridge's internal kill path. The underlying
/// `BridgeManager::stop_default_bridge` already issues a graceful stop + 500ms
/// grace + forced kill, so this value is a safety ceiling on the whole stop
/// operation, not just the grace period.
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

/// Maximum time to wait for the freshly-spawned executor to reach `Ready`
/// after `start_default_bridge` returns.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// How often to poll the bridge state while waiting for `Ready`.
const READY_POLL_INTERVAL: Duration = Duration::from_millis(100);

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// `POST /executor/restart` — restart the Python executor subprocess.
///
/// Idempotent: calling on an already-running executor performs a clean
/// stop + respawn. Calling on a dead/failed executor simply respawns.
///
/// Returns:
/// * `200 OK` with `{success: true, data: {state: "Ready", pid: ...}}`
/// * `500 Internal Server Error` with `{success: false, error: "...",
///   data: {state, last_error}}` on timeout or failure.
pub async fn restart_executor(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<RestartSuccess>>, (StatusCode, Json<ApiResponse<RestartFailure>>)> {
    // Serialise restarts so a caller spamming this endpoint doesn't create
    // interleaved stop/start sequences.
    let _guard = restart_lock().lock().await;
    info!("Executor API: restart requested");

    let app_state = state.app_state.clone();

    // -----------------------------------------------------------------------
    // 1. Grab the bridge manager (once).
    // -----------------------------------------------------------------------
    let manager = {
        let guard = app_state.bridge_manager.lock().await;
        match &*guard {
            Some(m) => m.clone(),
            None => {
                error!("Executor API: bridge manager not initialised");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiResponse {
                        success: false,
                        data: Some(RestartFailure {
                            state: "uninitialized".to_string(),
                            last_error: Some("Bridge manager not initialised".to_string()),
                        }),
                        error: Some("Bridge manager not initialised".to_string()),
                        error_detail: None,
                        hint: None,
                        code: None,
                        suggestions: None,
                    }),
                ));
            }
        }
    };

    // -----------------------------------------------------------------------
    // 2. If an existing bridge is mid-startup (Initializing), give it a
    //    brief window to resolve one way or the other before we rip it out.
    //    This avoids aborting a healthy startup that just needs one more
    //    second, without blocking long if the startup is stuck.
    // -----------------------------------------------------------------------
    wait_for_startup_to_settle(&manager, Duration::from_secs(2)).await;

    // -----------------------------------------------------------------------
    // 3. Stop the current bridge (if any). We tolerate failures here: the
    //    bridge may already be dead (Failed/Shutdown), in which case there's
    //    nothing to stop and we should just move on to respawn.
    // -----------------------------------------------------------------------
    let stop_start = Instant::now();
    let stop_result = tokio::time::timeout(STOP_TIMEOUT, manager.stop_default_bridge()).await;

    match stop_result {
        Ok(Ok(())) => {
            info!(
                "Executor API: previous bridge stopped in {:?}",
                stop_start.elapsed()
            );
        }
        Ok(Err(e)) => {
            // Most common benign case: "No default bridge available" when
            // there was never one in the first place.
            warn!(
                "Executor API: stop_default_bridge returned error (continuing to respawn): {}",
                e
            );
        }
        Err(_) => {
            warn!(
                "Executor API: stop_default_bridge timed out after {:?}, continuing to respawn",
                STOP_TIMEOUT
            );
        }
    }

    // -----------------------------------------------------------------------
    // 4. Reset the default bridge's lifecycle to Initializing so that the
    //    incoming READY signal can legally transition to Ready. Without this
    //    step, a bridge that was just Shutdown stays Shutdown and the
    //    Ready transition fails with InvalidStateTransition.
    //
    //    If no default bridge exists yet, this is a no-op — `start_default_
    //    bridge` returns Ok(false) below and we fall through to
    //    `get_or_create_default_bridge`, which spawns a fresh bridge with
    //    a fresh (Initializing) lifecycle.
    // -----------------------------------------------------------------------
    reset_default_lifecycle_to_initializing(&manager).await;

    // -----------------------------------------------------------------------
    // 5. Start (or create) the default bridge. `start_default_bridge`
    //    returns Ok(true) if it started an existing but-stopped bridge,
    //    Ok(false) if there's no default bridge to start — in that case we
    //    fall back to creating one from scratch.
    // -----------------------------------------------------------------------
    let spawn_result = match manager.start_default_bridge().await {
        Ok(true) => Ok(()),
        Ok(false) => {
            info!("Executor API: no existing default bridge to start, creating one");
            manager.get_or_create_default_bridge().await.map(|_| ())
        }
        Err(e) => Err(e),
    };

    if let Err(e) = spawn_result {
        error!("Executor API: failed to spawn Python executor: {}", e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse {
                success: false,
                data: Some(RestartFailure {
                    state: manager
                        .get_default_bridge_state_async()
                        .await
                        .map(|s| s.name().to_string())
                        .unwrap_or_else(|| "Unknown".to_string()),
                    last_error: Some(e.clone()),
                }),
                error: Some(format!("Failed to spawn Python executor: {}", e)),
                error_detail: None,
                hint: None,
                code: None,
                suggestions: None,
            }),
        ));
    }

    // -----------------------------------------------------------------------
    // 6. Wait up to READY_TIMEOUT for the new process to reach Ready. If it
    //    settles into Failed (e.g. cv2 DLL crash), bail out immediately —
    //    there's no point waiting the full 10 seconds.
    // -----------------------------------------------------------------------
    let wait_start = Instant::now();
    let final_state = loop {
        let current = manager
            .get_default_bridge_state_async()
            .await
            .unwrap_or_else(|| ExecutorState::failed("No default bridge after spawn".to_string()));

        match &current {
            ExecutorState::Ready { .. } | ExecutorState::Running { .. } => {
                break current;
            }
            ExecutorState::Failed { .. } => {
                warn!(
                    "Executor API: bridge reached Failed state after {:?}",
                    wait_start.elapsed()
                );
                break current;
            }
            _ => {}
        }

        if wait_start.elapsed() >= READY_TIMEOUT {
            warn!(
                "Executor API: bridge did not reach Ready within {:?} (state: {})",
                READY_TIMEOUT,
                current.name()
            );
            break current;
        }

        sleep(READY_POLL_INTERVAL).await;
    };

    // -----------------------------------------------------------------------
    // 7. Build the response.
    // -----------------------------------------------------------------------
    match final_state {
        ExecutorState::Ready { .. } | ExecutorState::Running { .. } => {
            let pid = extract_default_bridge_pid(&manager);
            info!(
                "Executor API: restart succeeded (state={}, pid={:?})",
                final_state.name(),
                pid
            );
            Ok(Json(ApiResponse::success(RestartSuccess {
                state: final_state.name().to_string(),
                pid,
            })))
        }
        other => {
            let (state_name, last_error) = match &other {
                ExecutorState::Failed { error, .. } => ("Failed".to_string(), Some(error.clone())),
                _ => (
                    other.name().to_string(),
                    Some(format!(
                        "Executor did not reach Ready within {:?}",
                        READY_TIMEOUT
                    )),
                ),
            };
            let message = format!(
                "Executor restart did not reach Ready (final state: {})",
                state_name
            );
            error!("Executor API: {}", message);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    success: false,
                    data: Some(RestartFailure {
                        state: state_name,
                        last_error,
                    }),
                    error: Some(message),
                    error_detail: None,
                    hint: None,
                    code: None,
                    suggestions: None,
                }),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// If the default bridge is currently `Initializing`, wait up to `max_wait`
/// for it to transition to a non-initializing state. This avoids stomping on
/// a healthy startup that just needs a moment, while capping the added
/// latency before we proceed to stop.
async fn wait_for_startup_to_settle(
    manager: &Arc<crate::executor::BridgeManager>,
    max_wait: Duration,
) {
    let start = Instant::now();
    loop {
        let s = manager.get_default_bridge_state_async().await;
        match s {
            Some(ExecutorState::Initializing { .. }) => {}
            _ => return,
        }
        if start.elapsed() >= max_wait {
            return;
        }
        sleep(Duration::from_millis(50)).await;
    }
}

/// Reset the default bridge's lifecycle to `Initializing` so the next `start`
/// can legally transition to `Ready`. Best-effort — if there is no default
/// bridge, this is a no-op.
async fn reset_default_lifecycle_to_initializing(manager: &Arc<crate::executor::BridgeManager>) {
    // Grab the lifecycle handle under the sync bridges lock, then drop the
    // guard before touching async.
    let lifecycle = {
        let bridges = manager.bridges.read().unwrap();
        let first = bridges.values().next();
        first.map(|m| m.bridge.get_lifecycle())
    };

    if let Some(lc) = lifecycle {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let res = lc
            .read()
            .await
            .set_state(ExecutorState::Initializing { started_at: ts })
            .await;
        match res {
            Ok(()) => info!("Executor API: lifecycle reset to Initializing"),
            Err(e) => {
                // Not fatal — if the state is already Initializing or we
                // can't legally transition (rare race), the start attempt
                // below will either succeed (state is already Initializing)
                // or surface the error.
                warn!(
                    "Executor API: could not reset lifecycle to Initializing (continuing): {}",
                    e
                );
            }
        }
    }
}

/// Best-effort extraction of the default bridge's Python process PID. The
/// process handle lives inside `PythonBridge::process` and is not currently
/// exposed via any method, so we return `None` and rely on state alone for
/// success reporting. Kept as a stub so the response shape matches the task
/// spec and we can wire in a real PID later without breaking callers.
fn extract_default_bridge_pid(_manager: &Arc<crate::executor::BridgeManager>) -> Option<u32> {
    None
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::post;
    axum::Router::new().route("/executor/restart", post(restart_executor))
}
