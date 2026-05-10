//! File Registry MCP API
//!
//! HTTP endpoints for the advisory file registry. Sessions (workflows and
//! AI terminal sessions) use these to register files they're working on,
//! check for conflicts, and release registrations.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::database::pg::session_touched_files::{HotFileRow, HotSessionRow};
use crate::executor::file_registry::{FileConflict, FileLockInfo, FileRegistryInfo};
use crate::mcp::types::ApiState;

// =============================================================================
// Request / Response Types
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct RegisterFilesRequest {
    /// Files being actively developed.
    pub file_paths: Vec<String>,
    /// Task run ID of the registering session.
    pub task_run_id: String,
    /// Human-readable session/workflow name.
    pub holder_name: String,
    /// Optional worktree ID — registrations are scoped per-worktree.
    /// `None` (default) means the main tree (historical behavior).
    #[serde(default)]
    pub worktree_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegisterFilesResponse {
    /// Number of files registered.
    pub registered: usize,
    /// Files that are also being worked on by other sessions.
    pub conflicts: Vec<FileConflict>,
}

#[derive(Debug, Deserialize)]
pub struct UnregisterFilesRequest {
    /// Files to unregister.
    pub file_paths: Vec<String>,
    /// Task run ID of the session releasing the files.
    pub task_run_id: String,
    /// Optional worktree ID — unregistration is scoped per-worktree.
    /// `None` (default) means the main tree.
    #[serde(default)]
    pub worktree_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReleaseAllRequest {
    /// Task run ID of the session releasing all files.
    pub task_run_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CheckConflictsRequest {
    /// Task run ID of the querying session (excluded from conflicts).
    pub task_run_id: String,
    /// Optional: only check these specific files. If empty, checks all.
    #[serde(default)]
    pub file_paths: Vec<String>,
    /// Optional worktree ID — when `file_paths` is non-empty, conflicts are
    /// scoped to this worktree. Ignored when `file_paths` is empty (the
    /// global `check_conflicts` query is inherently cross-worktree).
    #[serde(default)]
    pub worktree_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CheckConflictsResponse {
    /// Files under active development by other sessions.
    pub conflicts: Vec<FileConflict>,
}

#[derive(Debug, Deserialize)]
pub struct ProbeConflictsRequest {
    /// Free-form text (the launch prompt) to extract candidate paths from.
    /// `None` skips extraction; only `live_holdings` is populated.
    pub prompt: Option<String>,
    /// Working directory of the session being launched. Used to resolve
    /// relative tokens in `prompt` so candidates can be compared
    /// symmetrically against the registry's normalized keys. Pass an empty
    /// string to disable resolution.
    pub cwd: String,
}

/// A historical (non-live) editor of a probed file, derived from
/// `coord.session_touched_files`. Surfaces "the last session that touched
/// this file is X" hints when no live registration exists.
#[derive(Debug, Serialize)]
pub struct RecentEditor {
    /// File path (already normalized by the upstream record).
    pub file_path: String,
    /// Task run ID of the prior editor.
    pub task_run_id: String,
}

#[derive(Debug, Serialize)]
pub struct ConflictReport {
    /// Live snapshot of every file currently held in the registry,
    /// regardless of prompt. Drives the launch menu's "Currently editing"
    /// panel.
    pub live_holdings: Vec<FileRegistryInfo>,
    /// Subset of live registrations whose path matches a prompt-extracted
    /// candidate. Drives the predictive yellow warning.
    pub predicted_collisions: Vec<FileConflict>,
    /// For each prompt-extracted candidate, the most recent prior editor
    /// (from `session_touched_files`). May be empty for a stale repo or
    /// when no candidates were extracted.
    pub recent_editors: Vec<RecentEditor>,
    /// What the extractor produced — exposed so the frontend can render a
    /// dev-only tooltip when the warning looks wrong.
    pub extracted_candidates: Vec<String>,
}

// =============================================================================
// Handlers
// =============================================================================

/// POST /file-registry/register
///
/// Register files as under active development. Returns any conflicts with
/// other sessions. Registration always succeeds (advisory, not exclusive).
async fn register_files(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<RegisterFilesRequest>,
) -> Result<Json<RegisterFilesResponse>, (StatusCode, String)> {
    if req.file_paths.is_empty() {
        return Ok(Json(RegisterFilesResponse {
            registered: 0,
            conflicts: vec![],
        }));
    }

    let conflicts = state
        .app_state
        .file_registry_manager
        .register(
            &req.file_paths,
            &req.task_run_id,
            &req.holder_name,
            req.worktree_id.clone(),
        )
        .await;

    Ok(Json(RegisterFilesResponse {
        registered: req.file_paths.len(),
        conflicts,
    }))
}

/// POST /file-registry/unregister
///
/// Unregister specific files for a session.
async fn unregister_files(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<UnregisterFilesRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .app_state
        .file_registry_manager
        .unregister(&req.file_paths, &req.task_run_id, req.worktree_id.clone())
        .await;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// POST /file-registry/release-all
///
/// Release all file registrations for a session. Called when a session ends.
async fn release_all(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ReleaseAllRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .app_state
        .file_registry_manager
        .release_all(&req.task_run_id)
        .await;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// POST /file-registry/check-conflicts
///
/// Check which files are under active development by other sessions.
/// If `file_paths` is provided, only checks those files. Otherwise checks all.
async fn check_conflicts(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CheckConflictsRequest>,
) -> Result<Json<CheckConflictsResponse>, (StatusCode, String)> {
    let conflicts = if req.file_paths.is_empty() {
        state
            .app_state
            .file_registry_manager
            .check_conflicts(&req.task_run_id)
            .await
    } else {
        state
            .app_state
            .file_registry_manager
            .check_conflicts_for_files(&req.file_paths, &req.task_run_id, req.worktree_id.clone())
            .await
    };

    Ok(Json(CheckConflictsResponse { conflicts }))
}

/// GET /file-registry/info
///
/// Get a snapshot of all current file registrations across all sessions.
async fn get_info(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<FileRegistryInfo>>, (StatusCode, String)> {
    let info = state.app_state.file_registry_manager.info().await;
    Ok(Json(info))
}

// =============================================================================
// File Activity Heatmap
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct HeatmapQuery {
    /// Time window for the windowed aggregates, in seconds. Defaults to
    /// 3600 (1 hour). Clamped to a sane upper bound below.
    #[serde(default)]
    pub window_secs: Option<i64>,
    /// Cap on the number of hot rows returned in each list. Defaults to
    /// 25. Clamped to 100 to keep responses bounded.
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct HeatmapResponse {
    /// Live `FileRegistryManager.info()` snapshot, pass-through. Always
    /// reflects the current process; not affected by `window_secs`.
    pub live: Vec<FileRegistryInfo>,
    /// Top files by distinct-toucher count in the window.
    pub hot_files: Vec<HotFileRow>,
    /// Top sessions by distinct-file count in the window.
    pub hot_sessions: Vec<HotSessionRow>,
    /// Echo of the resolved window for the UI's selector.
    pub window_secs: i64,
    /// Echo of the resolved limit.
    pub limit: i64,
}

/// GET /file-activity/heatmap?window_secs=3600&limit=25
///
/// Composes the live `FileRegistryManager.info()` snapshot with two
/// windowed aggregates over `coord.session_touched_files`:
///   - top files by distinct-toucher count
///   - top sessions by distinct-file count
///
/// The aggregates use a >= NOW() - INTERVAL filter that exploits
/// `idx_session_touched_files_recorded_at`. PG returns empty lists when
/// no rows fall in the window — caller renders the empty-state copy.
async fn get_heatmap(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<HeatmapQuery>,
) -> Result<Json<HeatmapResponse>, (StatusCode, String)> {
    let window_secs = q.window_secs.unwrap_or(3600).clamp(60, 86_400);
    let limit = q.limit.unwrap_or(25).clamp(1, 100);

    let live = state.app_state.file_registry_manager.info().await;

    let hot_files = state
        .app_state
        .pg_db
        .hot_files(window_secs, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("hot_files: {e}")))?;
    let hot_sessions = state
        .app_state
        .pg_db
        .hot_sessions(window_secs, limit)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("hot_sessions: {e}"),
            )
        })?;

    Ok(Json(HeatmapResponse {
        live,
        hot_files,
        hot_sessions,
        window_secs,
        limit,
    }))
}

/// GET /file-locks/info
///
/// Get a snapshot of all currently held exclusive file locks.
async fn get_lock_info(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<FileLockInfo>>, (StatusCode, String)> {
    let info = state.app_state.file_lock_manager.info().await;
    Ok(Json(info))
}

/// POST /file-registry/probe-conflicts
///
/// Pre-launch predictive-conflict probe. Given a launch prompt and cwd,
/// returns:
///   - `live_holdings`: every file currently registered (always — drives
///     the ambient "Currently editing" panel).
///   - `predicted_collisions`: live holdings whose path matches a
///     prompt-extracted candidate (the yellow warning).
///   - `recent_editors`: prior editors of the candidates, from
///     `session_touched_files` (the "X knows this file" hint).
///   - `extracted_candidates`: what the path extractor produced (for
///     dev-tooltip debugging).
///
/// Designed to be debounced on every keystroke in the launch menu —
/// `info()` is in-memory, `check_conflicts_for_files` is in-memory, and
/// `get_sessions_for_files` is one indexed PG query.
async fn probe_conflicts(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ProbeConflictsRequest>,
) -> Result<Json<ConflictReport>, (StatusCode, String)> {
    let live_holdings = state.app_state.file_registry_manager.info().await;

    let candidates =
        crate::util::path_extraction::extract_candidate_paths(req.prompt.as_deref(), &req.cwd);

    let predicted_collisions = if candidates.is_empty() {
        Vec::new()
    } else {
        // `<probe>` is a sentinel task_run_id — distinct from any real
        // session UUID, so `check_conflicts_for_files` returns every
        // holder of every candidate (no self-exclusion).
        state
            .app_state
            .file_registry_manager
            .check_conflicts_for_files(&candidates, "<probe>", None)
            .await
    };

    let recent_editors = if candidates.is_empty() {
        Vec::new()
    } else {
        state
            .app_state
            .pg_db
            .get_sessions_for_files(&candidates)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(file_path, task_run_id)| RecentEditor {
                file_path,
                task_run_id,
            })
            .collect()
    };

    Ok(Json(ConflictReport {
        live_holdings,
        predicted_collisions,
        recent_editors,
        extracted_candidates: candidates,
    }))
}

// =============================================================================
// Routes
// =============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/file-registry/register", post(register_files))
        .route("/file-registry/unregister", post(unregister_files))
        .route("/file-registry/release-all", post(release_all))
        .route("/file-registry/check-conflicts", post(check_conflicts))
        .route("/file-registry/info", get(get_info))
        .route("/file-registry/probe-conflicts", post(probe_conflicts))
        .route("/file-locks/info", get(get_lock_info))
        .route("/file-activity/heatmap", get(get_heatmap))
}

// =============================================================================
// Tests
// =============================================================================
//
// `probe_conflicts` is the composition of three independently-testable
// pieces: `FileRegistryManager.info()`, `extract_candidate_paths`, and
// `PgDb.get_sessions_for_files()`. The handler itself is a fan-in — its
// `Arc<ApiState>` argument is impractical to construct in a unit test
// (30+ fields including `tauri::AppHandle`). These integration tests
// instead drive the same composition the handler does, against a real
// `FileRegistryManager` and a real `PgDb`. The result is the same
// `ConflictReport` the handler would produce; gating on
// `#[ignore = "requires PG via DATABASE_URL"]` matches the convention
// established in `database/pg/session_touched_files.rs::tests`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::pg::PgDb;
    use crate::executor::file_registry::FileRegistryManager;

    /// Build the same `ConflictReport` the HTTP handler would build, using
    /// real backing collaborators. Mirrors `probe_conflicts` exactly so any
    /// drift would surface here first.
    async fn build_report(
        registry: &FileRegistryManager,
        pg_db: &PgDb,
        prompt: Option<&str>,
        cwd: &str,
    ) -> ConflictReport {
        let live_holdings = registry.info().await;

        let candidates = crate::util::path_extraction::extract_candidate_paths(prompt, cwd);

        let predicted_collisions = if candidates.is_empty() {
            Vec::new()
        } else {
            registry
                .check_conflicts_for_files(&candidates, "<probe>", None)
                .await
        };

        let recent_editors = if candidates.is_empty() {
            Vec::new()
        } else {
            pg_db
                .get_sessions_for_files(&candidates)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(file_path, task_run_id)| RecentEditor {
                    file_path,
                    task_run_id,
                })
                .collect()
        };

        ConflictReport {
            live_holdings,
            predicted_collisions,
            recent_editors,
            extracted_candidates: candidates,
        }
    }

    fn unique_task_run_id(label: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!(
            "test-probe-{}-{}-{:?}",
            label,
            nanos,
            std::thread::current().id()
        )
    }

    /// Connect to the test PG instance from inside an async test. We can't
    /// reuse `PgDb::new_blocking_for_test()` here because it calls
    /// `rt.block_on(...)` and `#[tokio::test]` is already inside a runtime,
    /// which `tokio::Runtime::new().block_on(...)` rejects with
    /// "Cannot start a runtime from within a runtime."
    async fn pg_for_test() -> std::sync::Arc<PgDb> {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://localhost:5432/qontinui_test".to_string());
        std::sync::Arc::new(PgDb::new(&url).await.expect("PgDb connection for test"))
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn probe_conflicts_empty_prompt_empty_registry() {
        let registry = FileRegistryManager::new();
        let pg_db = pg_for_test().await;

        let report = build_report(&registry, &pg_db, None, "/repo").await;

        assert!(report.live_holdings.is_empty());
        assert!(report.predicted_collisions.is_empty());
        assert!(report.recent_editors.is_empty());
        assert!(report.extracted_candidates.is_empty());
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn probe_conflicts_predicts_live_collision() {
        use crate::executor::file_registry::normalize_path;

        let registry = FileRegistryManager::new();
        let pg_db = pg_for_test().await;

        let holder_task = unique_task_run_id("holder");
        let probed_path = "/repo/src/lib.rs";

        // One session holds the file under the registry. The probe should
        // surface it under `predicted_collisions` once the prompt mentions
        // the matching path.
        let conflicts_at_register = registry
            .register(
                &[probed_path.to_string()],
                &holder_task,
                "Holder Workflow",
                None,
            )
            .await;
        assert!(
            conflicts_at_register.is_empty(),
            "first registration should be conflict-free"
        );

        let report = build_report(
            &registry,
            &pg_db,
            Some("please edit /repo/src/lib.rs to add logging"),
            "/repo",
        )
        .await;

        let normalized = normalize_path(probed_path);

        assert!(
            report.extracted_candidates.contains(&normalized),
            "extractor should have produced {:?}, got {:?}",
            normalized,
            report.extracted_candidates
        );
        assert_eq!(
            report.predicted_collisions.len(),
            1,
            "expected one predicted collision, got {:?}",
            report.predicted_collisions
        );
        assert_eq!(report.predicted_collisions[0].file_path, normalized);
        assert_eq!(
            report.predicted_collisions[0].other_holders.len(),
            1,
            "expected one other holder"
        );
        assert_eq!(
            report.predicted_collisions[0].other_holders[0].task_run_id,
            holder_task
        );

        // Cleanup so reruns are deterministic.
        registry.release_all(&holder_task).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn probe_conflicts_surfaces_recent_editor_when_no_live_holder() {
        use crate::executor::file_registry::normalize_path;

        let registry = FileRegistryManager::new();
        let pg_db = pg_for_test().await;

        // Pick a path that's unique to this test run so we don't collide
        // with leftover rows in `coord.session_touched_files`.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let probed_path = format!("/repo/src/feature_{}.rs", nanos);
        let normalized = normalize_path(&probed_path);

        let prior_editor = unique_task_run_id("prior-editor");

        // Stage a row in session_touched_files but *do not* register the
        // file in the live registry. The probe should report this under
        // `recent_editors` while `predicted_collisions` stays empty.
        pg_db
            .record_file_touched(&prior_editor, &probed_path, None)
            .await
            .expect("record_file_touched");

        // Avoid a Some(_) check that would also match unrelated rows: ask
        // the probe with the unique path in the prompt.
        let prompt = format!("please touch {} again", probed_path);
        let report = build_report(&registry, &pg_db, Some(&prompt), "").await;

        assert!(
            report.predicted_collisions.is_empty(),
            "no live holder → no predicted collisions, got {:?}",
            report.predicted_collisions
        );
        assert!(
            report
                .recent_editors
                .iter()
                .any(|e| e.file_path == normalized && e.task_run_id == prior_editor),
            "expected recent_editor for {} with task_run_id {}, got {:?}",
            normalized,
            prior_editor,
            report.recent_editors
        );

        // Cleanup so reruns are deterministic.
        let _ = pg_db.clear_files_touched(&prior_editor).await;
    }
}
