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
    /// Optional explicit file hints from callers that already know the
    /// paths (drag-and-drop into the launch sheet, plan-derived launches).
    /// These bypass the extractor and are unioned into the candidate set
    /// at confidence 1.0 (after `normalize_path` so they compare
    /// symmetrically against registry keys).
    #[serde(default)]
    pub hinted_files: Option<Vec<String>>,
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

/// A predicted collision between an inbound launch prompt's candidate
/// paths and a live registration. Wraps `FileConflict` with a `confidence`
/// score so the frontend can bracket warnings (1.0 / 0.7 / 0.4):
///   - `1.0` — an extracted candidate (or a `hinted_files` entry) matches
///     the conflict path literally.
///   - `0.7` — a candidate matches the conflict's basename, or a candidate
///     path is a substring of the conflict path (or vice versa).
///   - `0.4` — only directory-level overlap (a candidate's parent dir is a
///     prefix of the conflict path, but the basenames don't match).
#[derive(Debug, Serialize)]
pub struct PredictedCollision {
    /// Flatten so the JSON shape is `FileConflict`'s fields plus
    /// `confidence`, preserving wire compatibility on the existing fields.
    #[serde(flatten)]
    pub conflict: FileConflict,
    /// 0..1 confidence score; see struct docs for the rubric.
    pub confidence: f32,
}

#[derive(Debug, Serialize)]
pub struct ConflictReport {
    /// Live snapshot of every file currently held in the registry,
    /// regardless of prompt. Drives the launch menu's "Currently editing"
    /// panel.
    pub live_holdings: Vec<FileRegistryInfo>,
    /// Subset of live registrations whose path matches a prompt-extracted
    /// candidate (or a `hinted_files` entry). Each row carries a
    /// `confidence` score (see `PredictedCollision`). Drives the
    /// predictive yellow warning.
    pub predicted_collisions: Vec<PredictedCollision>,
    /// For each prompt-extracted candidate, the most recent prior editor
    /// (from `session_touched_files`). May be empty for a stale repo or
    /// when no candidates were extracted.
    pub recent_editors: Vec<RecentEditor>,
    /// What the extractor produced (plus any `hinted_files` entries,
    /// normalized) — exposed so the frontend can render a dev-only
    /// tooltip when the warning looks wrong.
    pub extracted_candidates: Vec<String>,
}

/// Compute the `PredictedCollision.confidence` for a single conflict.
///
/// Compares `conflict_path` against every candidate in `candidates`
/// (already normalized) and `hinted_set` (already normalized). Hits in
/// `hinted_set` are always treated as 1.0 matches when the path matches
/// literally; otherwise the same scoring rubric as extracted candidates
/// applies (basename / substring / parent-dir overlap).
///
/// Returns the highest confidence found across all candidates.
fn confidence_for_conflict(
    conflict_path: &str,
    candidates: &[String],
    hinted_set: &std::collections::HashSet<String>,
) -> f32 {
    use std::path::Path;

    // Fast path: literal match against any hinted entry or candidate.
    if hinted_set.contains(conflict_path) || candidates.iter().any(|c| c == conflict_path) {
        return 1.0;
    }

    let conflict_p = Path::new(conflict_path);
    let conflict_basename = conflict_p
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let conflict_parent = conflict_p.parent().and_then(|p| p.to_str()).unwrap_or("");

    let mut best: f32 = 0.0;

    for candidate in candidates {
        // Already covered the literal-match case above; here we look for
        // basename / substring / directory-overlap matches.
        let candidate_p = Path::new(candidate);
        let candidate_basename = candidate_p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        // 0.7 — basename match (e.g. candidate "foo.rs" or
        // "src/foo.rs", conflict "/repo/src/foo.rs").
        if !candidate_basename.is_empty() && candidate_basename == conflict_basename {
            best = best.max(0.7);
            continue;
        }

        // 0.7 — substring match either direction (e.g. candidate
        // "src/foo.rs", conflict "/repo/src/foo.rs").
        if !candidate.is_empty()
            && (conflict_path.contains(candidate.as_str()) || candidate.contains(conflict_path))
        {
            best = best.max(0.7);
            continue;
        }

        // 0.4 — directory-level overlap (candidate's parent dir is a
        // prefix of the conflict path) but basenames did not match.
        let candidate_parent = candidate_p.parent().and_then(|p| p.to_str()).unwrap_or("");
        if !candidate_parent.is_empty()
            && (conflict_path.starts_with(candidate_parent)
                || conflict_parent.starts_with(candidate_parent))
        {
            best = best.max(0.4);
        }
    }

    best
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

    let extracted =
        crate::util::path_extraction::extract_candidate_paths(req.prompt.as_deref(), &req.cwd);

    // Normalize hinted_files via the same path normalization the registry
    // uses, so they compare symmetrically against registry keys. Build a
    // HashSet for O(1) membership checks during confidence scoring, and a
    // Vec preserving the union (extracted ∪ hints) for the candidate
    // list / `extracted_candidates` field.
    let hinted_normalized: Vec<String> = req
        .hinted_files
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|p| crate::executor::file_registry::normalize_path(p))
        .collect();
    let hinted_set: std::collections::HashSet<String> = hinted_normalized.iter().cloned().collect();

    // Union extracted ∪ hinted (preserving extracted-first order, then
    // appending any hints not already present).
    let mut candidates: Vec<String> = extracted.clone();
    let extracted_set: std::collections::HashSet<&String> = extracted.iter().collect();
    for h in &hinted_normalized {
        if !extracted_set.contains(h) {
            candidates.push(h.clone());
        }
    }

    let raw_collisions = if candidates.is_empty() {
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

    // Score each conflict against the candidate set (extracted + hints).
    let predicted_collisions: Vec<PredictedCollision> = raw_collisions
        .into_iter()
        .map(|conflict| {
            let confidence = confidence_for_conflict(&conflict.file_path, &candidates, &hinted_set);
            PredictedCollision {
                conflict,
                confidence,
            }
        })
        .collect();

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
        hinted_files: Option<&[String]>,
    ) -> ConflictReport {
        let live_holdings = registry.info().await;

        let extracted = crate::util::path_extraction::extract_candidate_paths(prompt, cwd);

        let hinted_normalized: Vec<String> = hinted_files
            .unwrap_or(&[])
            .iter()
            .map(|p| crate::executor::file_registry::normalize_path(p))
            .collect();
        let hinted_set: std::collections::HashSet<String> =
            hinted_normalized.iter().cloned().collect();

        let mut candidates: Vec<String> = extracted.clone();
        let extracted_set: std::collections::HashSet<&String> = extracted.iter().collect();
        for h in &hinted_normalized {
            if !extracted_set.contains(h) {
                candidates.push(h.clone());
            }
        }

        let raw_collisions = if candidates.is_empty() {
            Vec::new()
        } else {
            registry
                .check_conflicts_for_files(&candidates, "<probe>", None)
                .await
        };

        let predicted_collisions: Vec<PredictedCollision> = raw_collisions
            .into_iter()
            .map(|conflict| {
                let confidence =
                    confidence_for_conflict(&conflict.file_path, &candidates, &hinted_set);
                PredictedCollision {
                    conflict,
                    confidence,
                }
            })
            .collect();

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

        let report = build_report(&registry, &pg_db, None, "/repo", None).await;

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
            None,
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
        assert_eq!(
            report.predicted_collisions[0].conflict.file_path,
            normalized
        );
        assert_eq!(
            report.predicted_collisions[0].conflict.other_holders.len(),
            1,
            "expected one other holder"
        );
        assert_eq!(
            report.predicted_collisions[0].conflict.other_holders[0].task_run_id,
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
        let report = build_report(&registry, &pg_db, Some(&prompt), "", None).await;

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

    // =========================================================================
    // Phase 1 deltas: confidence scoring + hinted_files (PG-gated)
    // =========================================================================

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn probe_conflicts_confidence_exact_match() {
        use crate::executor::file_registry::normalize_path;

        let registry = FileRegistryManager::new();
        let pg_db = pg_for_test().await;

        let holder_task = unique_task_run_id("conf-exact");
        let probed_path = "/repo/src/lib.rs";

        registry
            .register(
                &[probed_path.to_string()],
                &holder_task,
                "Holder Workflow",
                None,
            )
            .await;

        // Prompt mentions the path literally — extractor should produce
        // the same normalized path the registry stores, giving 1.0.
        let report = build_report(
            &registry,
            &pg_db,
            Some("please edit /repo/src/lib.rs"),
            "/repo",
            None,
        )
        .await;

        let normalized = normalize_path(probed_path);
        let row = report
            .predicted_collisions
            .iter()
            .find(|c| c.conflict.file_path == normalized)
            .expect("expected a predicted collision for the literal path");
        assert!(
            (row.confidence - 1.0).abs() < f32::EPSILON,
            "literal-match confidence should be 1.0, got {}",
            row.confidence
        );

        registry.release_all(&holder_task).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn probe_conflicts_confidence_basename_match() {
        use crate::executor::file_registry::normalize_path;

        let registry = FileRegistryManager::new();
        let pg_db = pg_for_test().await;

        let holder_task = unique_task_run_id("conf-base");
        let probed_path = "/repo/src/lib.rs";

        registry
            .register(
                &[probed_path.to_string()],
                &holder_task,
                "Holder Workflow",
                None,
            )
            .await;

        // Prompt mentions just the basename (no /repo/src/ prefix).
        // path_extraction may not pick up "lib.rs" alone — drive the
        // basename match through hinted_files instead, which goes through
        // the normalize_path pipeline but bypasses the extractor heuristic.
        let hinted = vec!["lib.rs".to_string()];
        let report = build_report(
            &registry,
            &pg_db,
            Some("touch the file"),
            "/repo",
            Some(&hinted),
        )
        .await;

        let normalized = normalize_path(probed_path);
        let row = report
            .predicted_collisions
            .iter()
            .find(|c| c.conflict.file_path == normalized)
            .expect("expected a predicted collision for the basename match");
        assert!(
            (row.confidence - 0.7).abs() < f32::EPSILON,
            "basename-match confidence should be 0.7, got {} (collision path {:?})",
            row.confidence,
            row.conflict.file_path
        );

        registry.release_all(&holder_task).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn probe_conflicts_confidence_directory_overlap() {
        use crate::executor::file_registry::normalize_path;

        let registry = FileRegistryManager::new();
        let pg_db = pg_for_test().await;

        let holder_task = unique_task_run_id("conf-dir");
        let probed_path = "/repo/src/lib.rs";

        registry
            .register(
                &[probed_path.to_string()],
                &holder_task,
                "Holder Workflow",
                None,
            )
            .await;

        // Hint a sibling file under the same parent dir. Basename
        // doesn't match (other.rs vs lib.rs) and neither path is a
        // substring of the other, but they share parent /repo/src.
        let hinted = vec!["/repo/src/other.rs".to_string()];
        let report = build_report(
            &registry,
            &pg_db,
            Some("touch the sibling"),
            "/repo",
            Some(&hinted),
        )
        .await;

        let normalized = normalize_path(probed_path);
        let row = report
            .predicted_collisions
            .iter()
            .find(|c| c.conflict.file_path == normalized)
            .expect("expected a predicted collision for the directory overlap");
        assert!(
            (row.confidence - 0.4).abs() < f32::EPSILON,
            "directory-overlap confidence should be 0.4, got {} (collision path {:?})",
            row.confidence,
            row.conflict.file_path
        );

        registry.release_all(&holder_task).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn probe_conflicts_hinted_files_unioned() {
        use crate::executor::file_registry::normalize_path;

        let registry = FileRegistryManager::new();
        let pg_db = pg_for_test().await;

        let holder_extracted = unique_task_run_id("hint-extracted");
        let holder_hinted = unique_task_run_id("hint-hinted");
        let extracted_path = "/repo/src/extracted.rs";
        let hinted_path = "/repo/src/hinted.rs";

        registry
            .register(
                &[extracted_path.to_string()],
                &holder_extracted,
                "Extracted Holder",
                None,
            )
            .await;
        registry
            .register(
                &[hinted_path.to_string()],
                &holder_hinted,
                "Hinted Holder",
                None,
            )
            .await;

        // Prompt mentions only the extracted path; hint covers the other.
        let prompt = format!("edit {}", extracted_path);
        let hinted = vec![hinted_path.to_string()];
        let report = build_report(&registry, &pg_db, Some(&prompt), "/repo", Some(&hinted)).await;

        let normalized_extracted = normalize_path(extracted_path);
        let normalized_hinted = normalize_path(hinted_path);

        // extracted_candidates should now contain both.
        assert!(
            report
                .extracted_candidates
                .iter()
                .any(|c| c == &normalized_extracted),
            "expected extracted path in extracted_candidates, got {:?}",
            report.extracted_candidates
        );
        assert!(
            report
                .extracted_candidates
                .iter()
                .any(|c| c == &normalized_hinted),
            "expected hinted path in extracted_candidates, got {:?}",
            report.extracted_candidates
        );

        // predicted_collisions should cover both holders, both at 1.0.
        let extracted_row = report
            .predicted_collisions
            .iter()
            .find(|c| c.conflict.file_path == normalized_extracted)
            .expect("expected predicted collision for extracted path");
        assert!(
            (extracted_row.confidence - 1.0).abs() < f32::EPSILON,
            "extracted-path confidence should be 1.0, got {}",
            extracted_row.confidence
        );

        let hinted_row = report
            .predicted_collisions
            .iter()
            .find(|c| c.conflict.file_path == normalized_hinted)
            .expect("expected predicted collision for hinted path");
        assert!(
            (hinted_row.confidence - 1.0).abs() < f32::EPSILON,
            "hinted-path confidence should be 1.0, got {}",
            hinted_row.confidence
        );

        registry.release_all(&holder_extracted).await;
        registry.release_all(&holder_hinted).await;
    }
}
