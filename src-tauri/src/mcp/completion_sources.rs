//! HTTP surface for external completion-source adapters.
//!
//! Phase 3 of `productivity-coordinator-completion-reports.md` (§3 "External
//! trigger sources" + §6 "External-trigger extensibility"). Translates
//! external events (PR merge webhook, manual user fire, future CI completion)
//! into structured `CompletionReport` writes and force-flips the target task
//! to `done`. Source-specific knowledge is confined to this module — Rule B
//! / Rule E / dashboard never branch on source.
//!
//! Endpoints:
//!
//! - `POST /completion-sources/github-merge` — translate a merged PR into a
//!   completion report. Resolves the target task either by the explicit
//!   `target_task_id` body field or by JSONB containment scan over
//!   `coord.tasks.completion_report->'deliverables'` for a `kind="pr",
//!   reference=<pr_url>` entry. Parses `## Breaking changes` / `## Follow-ups`
//!   sections from the PR body.
//! - `POST /completion-sources/manual-user-fire` — dashboard-issued path. The
//!   user supplies a fully-formed `completion_report`. Audited via
//!   `coordinator_decisions` so we know who fired what.
//! - `POST /completion-sources/ci-pipeline` — stub returning 501 until a real
//!   CI consumer materialises (per plan: "premature design otherwise").
//!
//! UUIDs are validated at the endpoint boundary with the
//! `Uuid::parse_str` map_err pattern from §9.5 hygiene rules.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::Emitter;
use tracing::warn;
use uuid::Uuid;

use crate::database::pg::completion_reports::{
    BreakingChange, CompletionReport, CompletionSource, Deliverable, FollowUp,
};
use crate::database::pg::coordinator_decisions::InsertCoordinatorDecisionInput;
use crate::database::pg::tasks::TaskRow;
use crate::mcp::types::ApiState;

// ============================================================================
// UUID parsing helper — same shape as mcp::completion_reports::parse_uuid.
// ============================================================================

fn parse_uuid(field_label: &str, value: &str) -> Result<Uuid, (StatusCode, String)> {
    Uuid::parse_str(value).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid {}_uuid: {}", field_label, e),
        )
    })
}

// ============================================================================
// POST /completion-sources/github-merge
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubMergeBody {
    pub pr_url: String,
    pub pr_title: String,
    #[serde(default)]
    pub pr_body: Option<String>,
    pub merged_at: String,
    pub merge_sha: String,
    #[serde(default)]
    pub head_sha: Option<String>,
    /// Explicit target task. Optional — when omitted, the server resolves by
    /// scanning `coord.tasks.completion_report->'deliverables'` for an entry
    /// with `kind="pr"` AND `reference == pr_url`.
    #[serde(default)]
    pub target_task_id: Option<String>,
}

/// Resolve the target task for a github-merge event.
///
/// Logic:
/// 1. If `target_task_id` is `Some`, return that task (404 if missing).
/// 2. Otherwise containment-scan `coord.tasks.completion_report->'deliverables'`
///    for `[{kind:"pr", reference:<pr_url>}]`. 0 hits → 404. >1 hits → 409.
async fn resolve_target_task(
    state: &Arc<ApiState>,
    pr_url: &str,
    target_task_id: Option<&str>,
) -> Result<TaskRow, (StatusCode, String)> {
    let pg = &state.app_state.pg_db;

    if let Some(tid) = target_task_id {
        return pg
            .get_task_by_id(tid)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("get_task: {}", e),
                )
            })?
            .ok_or((StatusCode::NOT_FOUND, format!("task {} not found", tid)));
    }

    // JSONB containment query — find tasks whose `completion_report.deliverables`
    // array contains an entry matching `{kind: "pr", reference: <pr_url>}`.
    // Uses the GIN(jsonb_path_ops) index added in cr01a2b3c4d5_completion_reports
    // for efficient containment.
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("PG pool: {}", e)))?;

    let containment = serde_json::json!([{"kind": "pr", "reference": pr_url}]);
    let rows = conn
        .query(
            r#"
            SELECT id::text
            FROM coord.tasks
            WHERE completion_report->'deliverables' @> $1::jsonb
            "#,
            &[&containment],
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("resolve_target_task: {}", e),
            )
        })?;

    let ids: Vec<String> = rows.iter().map(|r| r.get(0)).collect();
    drop(conn);

    match ids.len() {
        0 => Err((
            StatusCode::NOT_FOUND,
            format!("no task found with PR deliverable matching {}", pr_url),
        )),
        1 => pg
            .get_task_by_id(&ids[0])
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("get_task: {}", e),
                )
            })?
            .ok_or((
                StatusCode::INTERNAL_SERVER_ERROR,
                "task vanished after containment match".to_string(),
            )),
        _ => Err((
            StatusCode::CONFLICT,
            serde_json::json!({
                "error": "ambiguous",
                "candidates": ids,
            })
            .to_string(),
        )),
    }
}

async fn github_merge(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<GithubMergeBody>,
) -> Result<Json<TaskRow>, (StatusCode, String)> {
    if let Some(t) = body.target_task_id.as_deref() {
        let _ = parse_uuid("target_task_id", t)?;
    }

    let task = resolve_target_task(&state, &body.pr_url, body.target_task_id.as_deref()).await?;

    // Build the synthesized report. summary_md is `format!("PR merged: {}\n\n{}", pr_title, pr_body)`,
    // truncated at MAX_SUMMARY_MD (4000 chars — matches the validator cap so
    // the write_completion_report call doesn't reject it).
    let body_text = body.pr_body.clone().unwrap_or_default();
    let mut summary_md = format!("PR merged: {}\n\n{}", body.pr_title, body_text);
    if summary_md.chars().count() > 4000 {
        // Char-aligned truncation — same convention as productivity::review::tail_chars
        let truncated: String = summary_md.chars().take(4000).collect();
        summary_md = truncated;
    }

    let breaking_changes = parse_breaking_changes_from_pr_body(&body_text);
    let follow_ups = parse_follow_ups_from_pr_body(&body_text);

    let mut artifacts: HashMap<String, serde_json::Value> = HashMap::new();
    artifacts.insert(
        "github".to_string(),
        serde_json::json!({
            "prUrl": body.pr_url,
            "mergedAt": body.merged_at,
            "mergeSha": body.merge_sha,
            "headSha": body.head_sha,
        }),
    );

    let report = CompletionReport {
        summary_md,
        deliverables: vec![
            Deliverable {
                kind: "pr".to_string(),
                reference: body.pr_url.clone(),
                description: body.pr_title.clone(),
            },
            Deliverable {
                kind: "commit".to_string(),
                reference: body.merge_sha.clone(),
                description: format!("Merge of {}", body.pr_url),
            },
        ],
        breaking_changes,
        follow_ups,
        artifacts,
    };

    let pg = &state.app_state.pg_db;

    pg.write_completion_report(&task.id, &report, CompletionSource::GithubMerge)
        .await
        .map_err(|e| {
            if e.starts_with("validation:") {
                (StatusCode::BAD_REQUEST, e)
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e)
            }
        })?;

    pg.force_task_status_to_done(&task.id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Dashboard's task-detail listener subscribes to
    // `completion-report-written` (Phase 1 contract); emit it AND the
    // `coordinator-wakeup` so the panel re-fetches the task and the
    // scheduler observes the state change without waiting for its tick.
    if let Err(e) = state.app_handle.emit(
        "completion-report-written",
        serde_json::json!({
            "taskId": task.id,
            "source": CompletionSource::GithubMerge.as_str(),
        }),
    ) {
        warn!(
            "emit completion-report-written (github-merge) failed: {}",
            e
        );
    }
    if let Err(e) = state.app_handle.emit(
        "coordinator-wakeup",
        serde_json::json!({
            "reason": "external-completion",
            "source": CompletionSource::GithubMerge.as_str(),
            "taskId": task.id,
        }),
    ) {
        warn!("emit coordinator-wakeup (github-merge) failed: {}", e);
    }

    let reloaded = pg
        .get_task_by_id(&task.id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("get_task: {}", e),
            )
        })?
        .ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "task disappeared after force-done".to_string(),
        ))?;

    Ok(Json(reloaded))
}

// ============================================================================
// POST /completion-sources/manual-user-fire
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualUserFireBody {
    pub task_id: String,
    pub completion_report: CompletionReport,
}

async fn manual_user_fire(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<ManualUserFireBody>,
) -> Result<Json<TaskRow>, (StatusCode, String)> {
    let _ = parse_uuid("task_id", &body.task_id)?;

    let pg = &state.app_state.pg_db;

    let task = pg
        .get_task_by_id(&body.task_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("get_task: {}", e),
            )
        })?
        .ok_or((
            StatusCode::NOT_FOUND,
            format!("task {} not found", body.task_id),
        ))?;

    pg.write_completion_report(
        &task.id,
        &body.completion_report,
        CompletionSource::ManualUserFire,
    )
    .await
    .map_err(|e| {
        if e.starts_with("validation:") {
            (StatusCode::BAD_REQUEST, e)
        } else {
            (StatusCode::INTERNAL_SERVER_ERROR, e)
        }
    })?;

    // Audit: coordinator_decisions row tagging this as an external trigger.
    // Reuses the same shape Phase 1's WORKER_ADDED_DEPENDENCY row used.
    let reasoning_tail: String = body
        .completion_report
        .summary_md
        .chars()
        .take(200)
        .collect();
    let session_id_for_audit = task
        .assigned_session_id
        .clone()
        .unwrap_or_else(|| "external".to_string());
    if let Err(e) = pg
        .insert_coordinator_decision(&InsertCoordinatorDecisionInput {
            session_id: &session_id_for_audit,
            iteration: 0,
            rule: "EXTERNAL_TRIGGER",
            action: "manual-user-fire",
            target_id: Some(&task.id),
            reasoning: &reasoning_tail,
            auto_acted: false,
        })
        .await
    {
        warn!(
            "manual_user_fire: insert_coordinator_decision failed for task {}: {}",
            task.id, e
        );
    }

    pg.force_task_status_to_done(&task.id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Dashboard's task-detail listener subscribes to
    // `completion-report-written` (Phase 1 contract); emit it AND the
    // `coordinator-wakeup` so the panel re-fetches the task and the
    // scheduler observes the state change without waiting for its tick.
    if let Err(e) = state.app_handle.emit(
        "completion-report-written",
        serde_json::json!({
            "taskId": task.id,
            "source": CompletionSource::ManualUserFire.as_str(),
        }),
    ) {
        warn!(
            "emit completion-report-written (manual-user-fire) failed: {}",
            e
        );
    }
    if let Err(e) = state.app_handle.emit(
        "coordinator-wakeup",
        serde_json::json!({
            "reason": "external-completion",
            "source": CompletionSource::ManualUserFire.as_str(),
            "taskId": task.id,
        }),
    ) {
        warn!("emit coordinator-wakeup (manual-user-fire) failed: {}", e);
    }

    let reloaded = pg
        .get_task_by_id(&task.id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("get_task: {}", e),
            )
        })?
        .ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "task disappeared after force-done".to_string(),
        ))?;

    Ok(Json(reloaded))
}

// ============================================================================
// POST /completion-sources/ci-pipeline — stub
// ============================================================================

#[derive(Debug, Clone, Serialize)]
struct CiPipelineStub {
    error: &'static str,
}

async fn ci_pipeline_stub() -> (StatusCode, Json<CiPipelineStub>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(CiPipelineStub {
            error: "CI pipeline source not yet wired; submit via /completion-sources/manual-user-fire instead",
        }),
    )
}

// ============================================================================
// PR-body parsers
// ============================================================================

/// Match a markdown heading line `## <name>` (allowing trailing whitespace
/// only). Case-insensitive on the heading text.
fn is_h2_heading(line: &str, candidates: &[&str]) -> bool {
    let trimmed = line.trim_end();
    if !trimmed.starts_with("## ") {
        return false;
    }
    let body = trimmed[3..].trim_end().to_ascii_lowercase();
    candidates.iter().any(|c| body == c.to_ascii_lowercase())
}

/// Any `## ...` heading marks the end of the previous section's bullets.
fn is_any_h2_heading(line: &str) -> bool {
    line.trim_end().starts_with("## ")
}

/// Strip a leading bullet marker (`- ` / `* `) and return the body, or `None`
/// when the line is not a bullet (in which case the caller should ignore the
/// line — per plan: malformed bullets are silently dropped).
fn strip_bullet_prefix(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("- ") {
        Some(rest)
    } else if let Some(rest) = trimmed.strip_prefix("* ") {
        Some(rest)
    } else {
        None
    }
}

/// Iterate lines under the matching `## <heading>` until the next `##` (or EOF).
/// Returns the bullet-body slices, in order. Empty Vec when no matching
/// section exists.
fn collect_section_bullets<'a>(body: &'a str, headings: &[&str]) -> Vec<&'a str> {
    let mut bullets: Vec<&'a str> = Vec::new();
    let mut in_section = false;
    for line in body.lines() {
        if is_h2_heading(line, headings) {
            in_section = true;
            continue;
        }
        if in_section {
            if is_any_h2_heading(line) {
                // End of our section — stop. We don't restart on a later
                // heading even if it matches: the convention is one section
                // per kind per PR body.
                break;
            }
            if let Some(b) = strip_bullet_prefix(line) {
                let b = b.trim();
                if !b.is_empty() {
                    bullets.push(b);
                }
            }
            // Non-bullet, non-heading lines (continuations, blank lines) are
            // silently skipped per plan §6 ("malformed bullets … silently dropped").
        }
    }
    bullets
}

/// Parse the `## Breaking changes` (case-insensitive) section from a PR body.
/// Each bullet maps to a `BreakingChange`:
/// - If the bullet contains a `:`, split on the first `:` — area = before,
///   description = after (trimmed).
/// - Otherwise area = "general", description = bullet body.
/// - migration_steps_md is always empty (sub-bullets not parsed in v1).
pub fn parse_breaking_changes_from_pr_body(body: &str) -> Vec<BreakingChange> {
    let bullets = collect_section_bullets(body, &["Breaking changes", "Breaking Changes"]);
    bullets
        .into_iter()
        .map(|b| {
            if let Some((area, desc)) = b.split_once(':') {
                BreakingChange {
                    area: area.trim().to_string(),
                    description: desc.trim().to_string(),
                    migration_steps_md: String::new(),
                }
            } else {
                BreakingChange {
                    area: "general".to_string(),
                    description: b.to_string(),
                    migration_steps_md: String::new(),
                }
            }
        })
        .collect()
}

/// Parse the `## Follow-ups` (or `Follow ups` / `Followups`, case-insensitive)
/// section from a PR body.
///
/// Per plan §3:
/// - Each bullet → `FollowUp { description, priority: "important", blocking_for_dependents: false }`.
/// - If the bullet starts with `[blocking]` (case-insensitive), set
///   `blocking_for_dependents = true` and strip the prefix.
/// - If the bullet starts with `[critical]`, `[important]`, or
///   `[nice-to-have]`, capture priority and strip the prefix.
/// - Both prefixes can stack — `[blocking][critical] foo` works.
pub fn parse_follow_ups_from_pr_body(body: &str) -> Vec<FollowUp> {
    let bullets = collect_section_bullets(body, &["Follow-ups", "Follow ups", "Followups"]);
    bullets
        .into_iter()
        .map(|raw| {
            let mut text = raw.to_string();
            let mut priority = "important".to_string();
            let mut blocking = false;

            // Repeat the prefix-strip loop so `[blocking][critical] foo` and
            // `[critical][blocking] foo` both work.
            loop {
                let lo = text.trim_start().to_ascii_lowercase();
                if let Some(rest) = lo.strip_prefix("[blocking]") {
                    let original_skip = text.len() - rest.len();
                    text = text[original_skip..].trim_start().to_string();
                    blocking = true;
                    continue;
                }
                if let Some(rest) = lo.strip_prefix("[critical]") {
                    let original_skip = text.len() - rest.len();
                    text = text[original_skip..].trim_start().to_string();
                    priority = "critical".to_string();
                    continue;
                }
                if let Some(rest) = lo.strip_prefix("[important]") {
                    let original_skip = text.len() - rest.len();
                    text = text[original_skip..].trim_start().to_string();
                    priority = "important".to_string();
                    continue;
                }
                if let Some(rest) = lo.strip_prefix("[nice-to-have]") {
                    let original_skip = text.len() - rest.len();
                    text = text[original_skip..].trim_start().to_string();
                    priority = "nice-to-have".to_string();
                    continue;
                }
                break;
            }

            FollowUp {
                description: text.trim().to_string(),
                priority,
                blocking_for_dependents: blocking,
            }
        })
        .filter(|f| !f.description.is_empty())
        .collect()
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/completion-sources/github-merge", post(github_merge))
        .route(
            "/completion-sources/manual-user-fire",
            post(manual_user_fire),
        )
        .route("/completion-sources/ci-pipeline", post(ci_pipeline_stub))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_breaking_changes_from_pr_body -------------------------------

    #[test]
    fn breaking_changes_no_section_returns_empty() {
        let body = "Some PR body without the heading.";
        assert!(parse_breaking_changes_from_pr_body(body).is_empty());
    }

    #[test]
    fn breaking_changes_empty_section_returns_empty() {
        let body = "## Breaking changes\n\n## Follow-ups\n- whatever\n";
        assert!(parse_breaking_changes_from_pr_body(body).is_empty());
    }

    #[test]
    fn breaking_changes_single_bullet_with_colon() {
        let body = "## Breaking changes\n- api: removed endpoint /old\n";
        let out = parse_breaking_changes_from_pr_body(body);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].area, "api");
        assert_eq!(out[0].description, "removed endpoint /old");
        assert_eq!(out[0].migration_steps_md, "");
    }

    #[test]
    fn breaking_changes_bullet_without_colon() {
        let body = "## Breaking changes\n- something just changed\n";
        let out = parse_breaking_changes_from_pr_body(body);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].area, "general");
        assert_eq!(out[0].description, "something just changed");
    }

    #[test]
    fn breaking_changes_multiple_bullets() {
        let body = "## Breaking changes\n- a: change A\n- b: change B\n- standalone\n";
        let out = parse_breaking_changes_from_pr_body(body);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].area, "a");
        assert_eq!(out[1].area, "b");
        assert_eq!(out[2].area, "general");
        assert_eq!(out[2].description, "standalone");
    }

    #[test]
    fn breaking_changes_stops_at_next_h2() {
        let body = "## Breaking changes\n- in section\n## Other\n- not in section\n";
        let out = parse_breaking_changes_from_pr_body(body);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].description, "in section");
    }

    #[test]
    fn breaking_changes_case_insensitive_heading() {
        let body = "## breaking changes\n- thing: fact\n";
        let out = parse_breaking_changes_from_pr_body(body);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].area, "thing");
    }

    #[test]
    fn breaking_changes_skips_malformed_lines() {
        let body = "## Breaking changes\nNot a bullet\n- real: bullet\nrandom prose\n";
        let out = parse_breaking_changes_from_pr_body(body);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].area, "real");
    }

    #[test]
    fn breaking_changes_accepts_star_bullets() {
        let body = "## Breaking changes\n* api: removed thing\n";
        let out = parse_breaking_changes_from_pr_body(body);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].area, "api");
    }

    // ---- parse_follow_ups_from_pr_body -------------------------------------

    #[test]
    fn follow_ups_no_section_returns_empty() {
        let body = "Body with no follow-ups heading.";
        assert!(parse_follow_ups_from_pr_body(body).is_empty());
    }

    #[test]
    fn follow_ups_empty_section_returns_empty() {
        let body = "## Follow-ups\n\n## next\n";
        assert!(parse_follow_ups_from_pr_body(body).is_empty());
    }

    #[test]
    fn follow_ups_default_priority_important() {
        let body = "## Follow-ups\n- do the thing\n";
        let out = parse_follow_ups_from_pr_body(body);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].description, "do the thing");
        assert_eq!(out[0].priority, "important");
        assert!(!out[0].blocking_for_dependents);
    }

    #[test]
    fn follow_ups_blocking_prefix_strips() {
        let body = "## Follow-ups\n- [blocking] critical work pending\n";
        let out = parse_follow_ups_from_pr_body(body);
        assert_eq!(out.len(), 1);
        assert!(out[0].blocking_for_dependents);
        assert_eq!(out[0].description, "critical work pending");
        assert_eq!(out[0].priority, "important");
    }

    #[test]
    fn follow_ups_priority_prefix_critical() {
        let body = "## Follow-ups\n- [critical] do this\n";
        let out = parse_follow_ups_from_pr_body(body);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].priority, "critical");
        assert_eq!(out[0].description, "do this");
    }

    #[test]
    fn follow_ups_priority_prefix_nice_to_have() {
        let body = "## Follow-ups\n- [nice-to-have] minor cleanup\n";
        let out = parse_follow_ups_from_pr_body(body);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].priority, "nice-to-have");
        assert_eq!(out[0].description, "minor cleanup");
    }

    #[test]
    fn follow_ups_blocking_and_priority_stack() {
        let body =
            "## Follow-ups\n- [blocking][critical] urgent\n- [critical][blocking] also urgent\n";
        let out = parse_follow_ups_from_pr_body(body);
        assert_eq!(out.len(), 2);
        assert!(out[0].blocking_for_dependents);
        assert_eq!(out[0].priority, "critical");
        assert_eq!(out[0].description, "urgent");
        assert!(out[1].blocking_for_dependents);
        assert_eq!(out[1].priority, "critical");
        assert_eq!(out[1].description, "also urgent");
    }

    #[test]
    fn follow_ups_alternate_heading_spellings() {
        for heading in ["## Follow-ups", "## Follow ups", "## Followups"] {
            let body = format!("{}\n- a thing\n", heading);
            let out = parse_follow_ups_from_pr_body(&body);
            assert_eq!(out.len(), 1, "heading: {}", heading);
        }
    }

    #[test]
    fn follow_ups_stops_at_next_h2() {
        let body = "## Follow-ups\n- a\n## breaking\n- not me\n";
        let out = parse_follow_ups_from_pr_body(body);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].description, "a");
    }

    #[test]
    fn follow_ups_case_insensitive_prefix() {
        let body = "## Follow-ups\n- [BLOCKING][Critical] yelling\n";
        let out = parse_follow_ups_from_pr_body(body);
        assert_eq!(out.len(), 1);
        assert!(out[0].blocking_for_dependents);
        assert_eq!(out[0].priority, "critical");
        assert_eq!(out[0].description, "yelling");
    }

    #[test]
    fn follow_ups_skips_malformed_lines() {
        let body = "## Follow-ups\nrandom\n- valid\nmore random\n";
        let out = parse_follow_ups_from_pr_body(body);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].description, "valid");
    }

    #[test]
    fn follow_ups_drops_empty_after_prefix_strip() {
        // `[blocking]` alone with nothing else collapses to empty body and is dropped.
        let body = "## Follow-ups\n- [blocking]\n- real\n";
        let out = parse_follow_ups_from_pr_body(body);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].description, "real");
    }

    // ---- UUID validation surface ------------------------------------------

    #[test]
    fn parse_uuid_rejects_garbage() {
        let err = parse_uuid("task_id", "not-a-uuid").expect_err("must reject");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.starts_with("invalid task_id_uuid:"));
    }

    #[test]
    fn parse_uuid_accepts_canonical() {
        let u = parse_uuid("target_task_id", "00000000-0000-0000-0000-000000000000").unwrap();
        assert_eq!(u.to_string(), "00000000-0000-0000-0000-000000000000");
    }

    // ---- ci-pipeline stub --------------------------------------------------

    #[tokio::test]
    async fn ci_pipeline_stub_returns_501() {
        let (code, body) = ci_pipeline_stub().await;
        assert_eq!(code, StatusCode::NOT_IMPLEMENTED);
        assert!(body.0.error.contains("not yet wired"));
    }

    // ---- github-merge resolution path (PG fixture, ignored) ---------------
    //
    // Validates the JSONB containment query end-to-end. Marked `#[ignore]`
    // because it requires a running PG with the cr01a2b3c4d5 alembic
    // revision applied. Run via:
    //   cargo test -p qontinui-runner completion_sources::tests::github_merge_resolves -- --ignored

    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL with cr01a2b3c4d5 applied)"]
    async fn github_merge_resolves_via_jsonb_containment() {
        use crate::database::pg::completion_reports::{CompletionSource, Deliverable};
        use crate::database::pg::tasks::InsertTaskInput;
        use crate::database::pg::PgDb;

        let pg = PgDb::new_blocking_for_test();
        let conn = pg.pool().get().await.expect("conn");

        let plan_row = conn
            .query_one(
                r#"
                INSERT INTO coord.plans (markdown_path, plan_version_hash, status)
                VALUES ($1, $2, $3)
                RETURNING id::text, plan_version_hash
                "#,
                &[&"/tmp/gh-merge-test.md", &"cafefeed", &"decomposed"],
            )
            .await
            .expect("insert plan");
        let plan_id: String = plan_row.get(0);
        let plan_version_hash: String = plan_row.get(1);

        let pr_url = "https://github.com/example/repo/pull/42";
        let task = pg
            .insert_task(&InsertTaskInput {
                plan_id: &plan_id,
                plan_version_hash: &plan_version_hash,
                phase_name: "P",
                sequence_in_phase: 1,
                description: "task with declared PR deliverable",
                expected_file_claims: &[],
                expected_dirs: &[],
                depends_on: &[],
                status: "running",
                notes: None,
            })
            .await
            .expect("insert task");

        // Worker pre-declares the PR they're about to ship in a self-report.
        let pre_report = CompletionReport {
            summary_md: "draft".to_string(),
            deliverables: vec![Deliverable {
                kind: "pr".to_string(),
                reference: pr_url.to_string(),
                description: "the upcoming PR".to_string(),
            }],
            breaking_changes: vec![],
            follow_ups: vec![],
            artifacts: HashMap::new(),
        };
        pg.write_completion_report(&task.id, &pre_report, CompletionSource::SessionSelfReport)
            .await
            .expect("write pre-report");

        drop(conn);

        // Build a minimal ApiState — TODO: replace with proper test-state
        // helper if/when one lands. For now, asserting only the resolver
        // path works directly without ApiState.
        let resolved = pg
            .pool()
            .get()
            .await
            .expect("conn")
            .query(
                r#"
                SELECT id::text
                FROM coord.tasks
                WHERE completion_report->'deliverables' @> $1::jsonb
                "#,
                &[&serde_json::json!([{"kind": "pr", "reference": pr_url}])],
            )
            .await
            .expect("containment");
        let ids: Vec<String> = resolved.iter().map(|r| r.get(0)).collect();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], task.id);

        // Cleanup.
        let plan_cleanup_uuid = Uuid::parse_str(&plan_id).expect("plan_id must be uuid");
        let _ = pg
            .pool()
            .get()
            .await
            .expect("conn")
            .execute(
                "DELETE FROM coord.plans WHERE id = $1::uuid",
                &[&plan_cleanup_uuid],
            )
            .await;
    }
}
