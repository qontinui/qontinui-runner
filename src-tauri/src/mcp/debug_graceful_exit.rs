//! Debug-profile door onto `TerminalManager::graceful_exit` (plan
//! `2026-09-13-drained-runner-never-reaches-idle`, Phase 1 falsification step
//! 2): `POST /__debug/terminals/{id}/graceful-exit?deadline_s=N` runs the
//! graceful-exit protocol on that pane and answers the
//! [`GracefulExitOutcome`].
//!
//! Nothing in the runner calls graceful exit on its own in Phase 1, so this is
//! the one way to exercise the primitive against a real pane.
//!
//! # Where it exists
//!
//! Declared `#[cfg(debug_assertions)]` in `mcp/mod.rs` and merged under the
//! same gate in `mcp_api`, following `mcp::debug_wedge`. That keeps it out of
//! release builds, but it is NOT test-only: the supervisor builds runners in
//! the dev profile, so every supervisor-built runner, temporary ones included,
//! serves this route. It adds no capability beyond what that runner's loopback
//! API already offers unauthenticated — `POST /terminals/{id}/write` can type
//! anything into a pane and `DELETE /terminals/{id}` closes one with a kill —
//! and it is strictly more conservative than either: it refuses unless the
//! pane sits at an empty prompt, and it never kills a live `claude`.
//!
//! # Since R0a it closes no live session
//!
//! Plan `2026-10-28-one-session-lifecycle-authority`, Phase 14: the primitive
//! now requires a close authority once it finds a live `claude`, and this door
//! has none to present, because only the lifecycle Closer will mint one. On a
//! pane holding a live `claude` it therefore answers `no_close_authority` with
//! nothing typed. On a pane with none it answers `no_live_claude`, as before.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use tauri::Manager;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::terminal::graceful_exit::{GracefulExitOutcome, DEFAULT_DEADLINE};
use crate::terminal::TerminalManager;

/// Ceiling on a requested deadline, so a typo cannot hold a request open for
/// an hour.
pub const MAX_DEADLINE: Duration = Duration::from_secs(300);

#[derive(Debug, Deserialize)]
pub struct GracefulExitQuery {
    /// Seconds to wait for `claude` to leave. Default
    /// [`DEFAULT_DEADLINE`]; clamped to `1..=`[`MAX_DEADLINE`].
    pub deadline_s: Option<u64>,
}

/// Resolve the requested deadline.
pub fn resolve_deadline(raw: Option<u64>) -> Duration {
    raw.map(|secs| Duration::from_secs(secs.clamp(1, MAX_DEADLINE.as_secs())))
        .unwrap_or(DEFAULT_DEADLINE)
}

type HandlerError = (StatusCode, Json<ApiResponse<()>>);

/// `POST /__debug/terminals/{id}/graceful-exit` — see the module docs.
pub async fn graceful_exit_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<GracefulExitQuery>,
) -> Result<Json<ApiResponse<GracefulExitOutcome>>, HandlerError> {
    let manager = state
        .app_handle
        .try_state::<Arc<TerminalManager>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(api_error("TerminalManager is not available")),
            )
        })?;
    graceful_exit_door(&manager, &id, query.deadline_s).await
}

/// The door's body, once the handler holds the manager. It runs
/// `TerminalManager::graceful_exit` on pane `id` with the resolved deadline,
/// presenting NO close authority (see the module docs, "Since R0a"), and
/// answers the outcome unchanged: a `no_close_authority` refusal is a 200 whose
/// body says so, never an error. An unknown pane is a 404.
///
/// Split out so the refusal is testable without an `AppHandle`.
pub(crate) async fn graceful_exit_door(
    manager: &Arc<TerminalManager>,
    id: &str,
    deadline_s: Option<u64>,
) -> Result<Json<ApiResponse<GracefulExitOutcome>>, HandlerError> {
    let outcome = manager
        .graceful_exit(id, resolve_deadline(deadline_s), None)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, Json(api_error(e))))?;
    Ok(Json(ApiResponse::success(outcome)))
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    axum::Router::new().route(
        "/__debug/terminals/{id}/graceful-exit",
        axum::routing::post(graceful_exit_handler),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_defaults_and_clamps() {
        assert_eq!(resolve_deadline(None), DEFAULT_DEADLINE);
        assert_eq!(resolve_deadline(Some(0)), Duration::from_secs(1));
        assert_eq!(resolve_deadline(Some(45)), Duration::from_secs(45));
        assert_eq!(resolve_deadline(Some(86_400)), MAX_DEADLINE);
    }

    /// R0a for the debug door, entered at the door's own body: a pane holding
    /// ONE live `claude` with ZERO children, at an empty prompt that would
    /// accept `/exit` (a coord-finished session left idle), is refused
    /// `no_close_authority`. The door answers 200 with that outcome in its
    /// body. Nothing is typed, nothing is killed, the pane stays registered.
    #[tokio::test(start_paused = true)]
    async fn the_debug_door_is_refused_no_close_authority_on_a_live_claude() {
        let pane = crate::terminal::session::tests::live_claude_pane();
        let tm = Arc::new(TerminalManager::new());
        let id = "debug-door-r0a";
        tm.insert_for_test(id, pane.session.clone());

        let Json(reply) = graceful_exit_door(&tm, id, None)
            .await
            .expect("a refusal is an answer, not an error");

        let body = serde_json::to_value(&reply).expect("serializes");
        assert_eq!(body["data"]["outcome"], "no_close_authority", "{body}");
        assert_eq!(
            body["data"]["claude_pids"],
            serde_json::json!(pane.claude_pids)
        );
        assert!(pane.typed().is_empty(), "typed into a live claude");
        assert_eq!(pane.kills(), 0);
        assert!(
            tm.get(id).is_some(),
            "a refused exit must leave the pane registered"
        );
    }

    #[tokio::test]
    async fn an_unknown_pane_is_a_404() {
        let tm = Arc::new(TerminalManager::new());
        let err = graceful_exit_door(&tm, "no-such-pane", None)
            .await
            .expect_err("no pane");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }
}
