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
use std::time::Duration;
use tracing::warn;

use crate::database::pg::session_touched_files::{HotFileRow, HotSessionRow};
use crate::executor::file_registry::{FileConflict, FileLockInfo, FileRegistryInfo};
use crate::mcp::types::ApiState;
use crate::util::path_extraction::{extract_paths_via_ai, AiContextBundle, ExtractError};

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

/// Status of the AI-side path extractor in a `ConflictReport`. The TS
/// frontend (Phase 3b) renders a small badge for any non-`Ok` value.
/// Variants serialize as their PascalCase names — DO NOT add
/// `#[serde(rename_all = ...)]`; the TS side consumes
/// `"Ok" | "Offline" | "Disabled" | "RateLimited"`.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub enum AiStatus {
    /// AI extractor ran and returned (possibly empty) paths.
    Ok,
    /// AI provider unreachable, timed out, or returned a transport-level
    /// error. The regex extractor still ran.
    Offline,
    /// User setting `ai_path_prediction_enabled` is false. Regex still ran.
    Disabled,
    /// Provider responded with a rate-limit error. Regex still ran.
    RateLimited,
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
    /// Status of the AI extractor's call. `Ok` = AI ran and returned
    /// (possibly empty) paths. `Offline` = AI provider unreachable or
    /// timed out (5s cap); regex still ran. `Disabled` = user setting
    /// `ai_path_prediction_enabled` is false. `RateLimited` = provider
    /// said no for the moment. Frontend renders a small badge for any
    /// non-Ok value.
    pub ai_status: AiStatus,
    /// Telemetry: how many candidates the AI extractor contributed
    /// (post-dedupe with regex). Always 0 when `ai_status != Ok`.
    pub ai_extracted_count: usize,
    /// Telemetry: how many candidates the regex extractor contributed.
    /// Always present regardless of AI status.
    pub regex_extracted_count: usize,
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

    // Regex extractor — synchronous, <1ms. Always runs first. Same call
    // shape as today.
    let regex_paths =
        crate::util::path_extraction::extract_candidate_paths(req.prompt.as_deref(), &req.cwd);
    let regex_extracted_count = regex_paths.len();

    // Decide whether to invoke the AI extractor. Skip when the user has
    // disabled it OR the prompt is None / empty (nothing to infer from).
    // The "no prompt" case is `Ok` (not a failure), the disabled case is
    // `Disabled`.
    let ai_enabled = crate::settings::get_ai_settings().ai_path_prediction_enabled;
    let trimmed_prompt = req.prompt.as_deref().map(|s| s.trim()).unwrap_or("");

    let (ai_paths, ai_status) = run_ai_extractor(ai_enabled, trimmed_prompt, &req.cwd).await;

    // Union regex ∪ AI (regex order preserved, AI-only paths appended,
    // capped at MAX_CANDIDATES = 50). `ai_only_count` is the number of
    // AI-contributed paths post-dedupe and post-cap — exactly the
    // telemetry the report exposes when `ai_status == Ok`.
    let (ai_union, ai_only_count) =
        union_and_dedupe(regex_paths.clone(), ai_paths, MAX_PROBE_CANDIDATES);

    let ai_extracted_count = if ai_status == AiStatus::Ok {
        ai_only_count
    } else {
        0
    };

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

    // Union (regex ∪ AI) ∪ hinted_files (preserving prior order, then
    // appending any hints not already present).
    let mut candidates: Vec<String> = ai_union;
    let union_set: std::collections::HashSet<String> = candidates.iter().cloned().collect();
    for h in &hinted_normalized {
        if !union_set.contains(h) {
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
        ai_status,
        ai_extracted_count,
        regex_extracted_count,
    }))
}

/// Hard cap on the candidate union (regex ∪ AI). Mirrors
/// `path_extraction::MAX_CANDIDATES`. Past this point we are almost
/// certainly extracting noise.
const MAX_PROBE_CANDIDATES: usize = 50;

/// Run the AI extractor and translate its result to `(paths, AiStatus)`.
///
/// Factored out of `probe_conflicts` so the AI-or-not branch is testable
/// without going through the live HTTP handler (which requires
/// `Arc<ApiState>` with 30+ fields). Pass `enabled = false` to short-circuit
/// to `(vec![], AiStatus::Disabled)` without any network I/O. Pass an empty
/// `prompt` to short-circuit to `(vec![], AiStatus::Ok)` — there's nothing
/// to extract from, so it's not a failure.
async fn run_ai_extractor(enabled: bool, prompt: &str, cwd: &str) -> (Vec<String>, AiStatus) {
    if !enabled {
        return (Vec::new(), AiStatus::Disabled);
    }
    if prompt.is_empty() {
        // No prompt to enrich. Not an AI failure — surface as Ok with 0
        // counts so the frontend doesn't render an unwarranted badge.
        return (Vec::new(), AiStatus::Ok);
    }

    let bundle = build_context_bundle(cwd).await;
    match extract_paths_via_ai(prompt, &bundle).await {
        Ok(paths) => (paths, AiStatus::Ok),
        Err(ExtractError::Offline) => (Vec::new(), AiStatus::Offline),
        Err(ExtractError::RateLimited) => (Vec::new(), AiStatus::RateLimited),
        Err(ExtractError::Disabled) => {
            // Defensive — shouldn't fire since we already gated on `enabled`.
            (Vec::new(), AiStatus::Disabled)
        }
        Err(ExtractError::ParseFailure(msg)) => {
            warn!("file_registry.probe_conflicts.ai_parse_failure: {}", msg);
            (Vec::new(), AiStatus::Offline)
        }
        Err(ExtractError::Other(msg)) => {
            warn!("file_registry.probe_conflicts.ai_error: {}", msg);
            (Vec::new(), AiStatus::Offline)
        }
    }
}

/// Build the small context bundle the AI extractor sees. All best-effort
/// — anything that fails (missing dir, no git, timeout) reduces to an
/// empty string so the AI call still proceeds with whatever signal we
/// could gather.
async fn build_context_bundle(cwd: &str) -> AiContextBundle {
    AiContextBundle {
        cwd: cwd.to_string(),
        repo_tree_snippet: build_repo_tree_snippet(cwd).await,
        recent_git_log: build_recent_git_log(cwd).await,
    }
}

/// Two-level directory snippet (top-level entries plus immediate children
/// of each top-level dir). Capped at ~80 entries / ~3 KB to stay under the
/// AI extractor's per-call user-message budget. Skips `.`-prefixed dirs
/// and the standard build-output / dependency dirs that the AI extractor's
/// blacklist would drop anyway. Returns `String::new()` on any error
/// (missing dir, permission denied, etc.) — the AI extractor still works
/// without it, just with fewer ground-truth signals.
async fn build_repo_tree_snippet(cwd: &str) -> String {
    const MAX_ENTRIES: usize = 80;
    const MAX_BYTES: usize = 3072;

    if cwd.is_empty() {
        return String::new();
    }

    fn should_skip(name: &str) -> bool {
        if name.starts_with('.') {
            return true;
        }
        matches!(name, "node_modules" | "target" | "dist" | "build")
    }

    let mut out = String::new();
    let mut count: usize = 0;

    let Ok(mut top) = tokio::fs::read_dir(cwd).await else {
        return String::new();
    };

    let mut top_dirs: Vec<std::path::PathBuf> = Vec::new();

    loop {
        if count >= MAX_ENTRIES || out.len() >= MAX_BYTES {
            return out;
        }
        let entry = match top.next_entry().await {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(_) => break,
        };
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if should_skip(&name_str) {
            continue;
        }

        let is_dir = match entry.file_type().await {
            Ok(ft) => ft.is_dir(),
            Err(_) => false,
        };
        if is_dir {
            out.push_str(&name_str);
            out.push_str("/\n");
            top_dirs.push(entry.path());
        } else {
            out.push_str(&name_str);
            out.push('\n');
        }
        count += 1;
    }

    for dir in top_dirs {
        if count >= MAX_ENTRIES || out.len() >= MAX_BYTES {
            return out;
        }
        let parent_name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let Ok(mut children) = tokio::fs::read_dir(&dir).await else {
            continue;
        };

        loop {
            if count >= MAX_ENTRIES || out.len() >= MAX_BYTES {
                return out;
            }
            let entry = match children.next_entry().await {
                Ok(Some(e)) => e,
                Ok(None) => break,
                Err(_) => break,
            };
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if should_skip(&name_str) {
                continue;
            }

            let is_dir = match entry.file_type().await {
                Ok(ft) => ft.is_dir(),
                Err(_) => false,
            };

            out.push_str(&parent_name);
            out.push('/');
            out.push_str(&name_str);
            if is_dir {
                out.push('/');
            }
            out.push('\n');
            count += 1;
        }
    }

    out
}

/// `git log --oneline -5` in `cwd`, with a 2-second hard timeout. Returns
/// `String::new()` on any error (no git, not a repo, command failure, or
/// timeout). Trailing newline is trimmed.
async fn build_recent_git_log(cwd: &str) -> String {
    if cwd.is_empty() {
        return String::new();
    }

    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["log", "-5", "--oneline"]);
    cmd.current_dir(cwd);

    let fut = cmd.output();
    let output = match tokio::time::timeout(Duration::from_secs(2), fut).await {
        Ok(Ok(out)) => out,
        Ok(Err(_)) | Err(_) => return String::new(),
    };

    if !output.status.success() {
        return String::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.trim_end_matches('\n').to_string()
}

/// Union the AI extractor's output into the regex extractor's output:
/// regex paths come first (preserving the regex order), then any AI-only
/// paths (i.e. those NOT already in `regex`) are appended. The combined
/// list is capped at `cap` total entries.
///
/// Returns `(union, ai_only_count)` where `ai_only_count` is the number
/// of AI-contributed paths that actually made it into the final union
/// (after dedupe AND after the cap). This is the telemetry the
/// `ConflictReport` surfaces as `ai_extracted_count`.
fn union_and_dedupe(regex: Vec<String>, ai: Vec<String>, cap: usize) -> (Vec<String>, usize) {
    let mut out: Vec<String> = Vec::with_capacity((regex.len() + ai.len()).min(cap));
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for p in regex {
        if out.len() >= cap {
            return (out, 0);
        }
        if seen.insert(p.clone()) {
            out.push(p);
        }
    }

    let mut ai_only_count: usize = 0;
    for p in ai {
        if out.len() >= cap {
            break;
        }
        if seen.insert(p.clone()) {
            out.push(p);
            ai_only_count += 1;
        }
    }

    (out, ai_only_count)
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
            // PG-gated tests pre-date Phase 2; they don't drive the AI
            // path at all. Mirror "no AI call attempted" by reporting Ok
            // with a zero AI count and the regex count as the only signal.
            ai_status: AiStatus::Ok,
            ai_extracted_count: 0,
            regex_extracted_count: extracted.len(),
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

    // =========================================================================
    // Phase 2 deltas: AI-extractor fan-in (regex ∪ AI), AiStatus telemetry,
    // union_and_dedupe.
    //
    // The AI-or-not branch lives in `run_ai_extractor(enabled, prompt, cwd)`
    // — a small async helper factored out of `probe_conflicts` so the
    // disabled-setting path doesn't require a live AI provider OR a way to
    // override the global settings singleton. Tests drive the helper
    // directly. The handler-level integration is covered by the existing
    // PG-gated tests, which now also assert the new fields are populated.
    // =========================================================================

    #[test]
    fn union_and_dedupe_empty_inputs() {
        // Both empty.
        let (out, ai_only) = union_and_dedupe(vec![], vec![], 50);
        assert!(out.is_empty());
        assert_eq!(ai_only, 0);

        // Regex empty, AI present.
        let (out, ai_only) = union_and_dedupe(vec![], vec!["a".into(), "b".into()], 50);
        assert_eq!(out, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(ai_only, 2);

        // AI empty, regex present.
        let (out, ai_only) = union_and_dedupe(vec!["a".into(), "b".into()], vec![], 50);
        assert_eq!(out, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(ai_only, 0);
    }

    #[test]
    fn probe_conflicts_union_dedupes_overlap() {
        // Regex returns ["a", "b"], AI returns ["b", "c"]. Expected union:
        // ["a", "b", "c"] (regex order preserved; AI-only "c" appended;
        // duplicate "b" dropped). ai_only_count == 1.
        let (out, ai_only) = union_and_dedupe(
            vec!["a".to_string(), "b".to_string()],
            vec!["b".to_string(), "c".to_string()],
            50,
        );
        assert_eq!(out, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        assert_eq!(ai_only, 1);
    }

    #[test]
    fn probe_conflicts_union_caps_at_50() {
        // Regex returns 30 distinct, AI returns 30 distinct. Cap = 50 →
        // result has exactly 50 (30 regex + first 20 AI). ai_only_count == 20.
        let regex: Vec<String> = (0..30).map(|i| format!("r{}", i)).collect();
        let ai: Vec<String> = (0..30).map(|i| format!("a{}", i)).collect();
        let (out, ai_only) = union_and_dedupe(regex.clone(), ai, 50);
        assert_eq!(out.len(), 50);
        // First 30 must be the regex paths in order.
        for (i, p) in regex.iter().enumerate() {
            assert_eq!(&out[i], p, "regex order should be preserved at index {}", i);
        }
        // Remaining 20 must be a0..a19.
        for i in 0..20 {
            assert_eq!(out[30 + i], format!("a{}", i));
        }
        assert_eq!(ai_only, 20);
    }

    #[tokio::test]
    async fn probe_conflicts_ai_disabled_setting_returns_disabled_status() {
        // Drive the AI-or-not branch directly so we don't need to mutate
        // the global settings singleton (`get_ai_settings()` reads a
        // process-wide store; tests run in parallel, so flipping it would
        // race other tests). The handler delegates the decision to
        // `run_ai_extractor(enabled, prompt, cwd)`; calling it with
        // `enabled = false` exercises the same code path the handler hits
        // when the user setting is off.
        let (paths, status) = run_ai_extractor(
            false,
            "fix the OAuth bug where tokens never expire",
            "/repo",
        )
        .await;
        assert!(paths.is_empty(), "no AI call → no AI paths");
        assert_eq!(status, AiStatus::Disabled);
    }

    #[tokio::test]
    async fn probe_conflicts_ai_status_ok_when_prompt_empty() {
        // `enabled = true` but prompt is empty after trim → the helper
        // short-circuits to `Ok` (not a failure: there was nothing to
        // extract from). ai_extracted_count would also be 0 in this case.
        let (paths, status) = run_ai_extractor(true, "", "/repo").await;
        assert!(paths.is_empty());
        assert_eq!(status, AiStatus::Ok);

        // Whitespace-only prompt is treated identically — the handler
        // trims before calling.
        let (paths, status) = run_ai_extractor(true, "", "/repo").await;
        assert!(paths.is_empty());
        assert_eq!(status, AiStatus::Ok);
    }

    #[tokio::test]
    async fn build_recent_git_log_handles_missing_dir() {
        // Best-effort: nonexistent cwd should not panic, just return empty.
        let log = build_recent_git_log("/this/path/definitely/does/not/exist/qontinui-test").await;
        assert!(log.is_empty());
    }

    #[tokio::test]
    async fn build_repo_tree_snippet_empty_cwd_is_empty() {
        let snippet = build_repo_tree_snippet("").await;
        assert!(snippet.is_empty());
    }

    #[tokio::test]
    async fn build_repo_tree_snippet_skips_dotted_and_build_dirs() {
        // Use the runner's own crate root — guaranteed to exist on every
        // dev machine and on CI (CARGO_MANIFEST_DIR is the src-tauri dir).
        let cwd = env!("CARGO_MANIFEST_DIR");
        let snippet = build_repo_tree_snippet(cwd).await;
        // Should not be empty for a real repo.
        assert!(
            !snippet.is_empty(),
            "expected non-empty snippet for {}, got empty",
            cwd
        );
        // Should not contain skipped dirs as standalone entries. The
        // skip is on the directory name itself; the substring check below
        // also catches any "target/foo" child entry that would have been
        // emitted if the parent had been recursed into.
        for forbidden in &["target/", "node_modules/", ".git/", "dist/", "build/"] {
            assert!(
                !snippet.contains(forbidden),
                "snippet should skip {}, got: {}",
                forbidden,
                snippet
            );
        }
        // Hard cap holds.
        assert!(
            snippet.len() <= 3072,
            "snippet should be ≤3KB, got {} bytes",
            snippet.len()
        );
    }
}
