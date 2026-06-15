//! Runner MCP tool — `orchestration_report_subtask`.
//!
//! Approach-D Conductor/Engine Phase 2 §3 (RESULT). A worker AI-session calls
//! this tool when it finishes a subtask, handing back a structured
//! [`CompletionReport`] keyed by `(run_id, task_id)`. The tool is mounted as an
//! HTTP route alongside the other runner MCP route modules (see
//! `mcp_api.rs` — merged right after `completion_reports::routes()`), which is
//! how every "runner MCP tool" in this codebase is surfaced (each module
//! exposes a `routes()` axum `Router`).
//!
//! Endpoint: `POST /orchestration/report-subtask`
//!   body: `{ "run_id": "<uuid>", "task_id": "<string>",
//!            "completion_report": { …CompletionReport… } }`
//!
//! ## Validation — reject-vs-warn split (contract resolved-Q1)
//!
//! The `completion_report` arrives as a `serde_json::Value` so the tool can
//! distinguish two failure classes:
//!
//! - **`data`-part schema violation (structural)** — wrong types / missing
//!   required fields, e.g. `deliverables` is a string not an array. Detected by
//!   a failed `serde_json::from_value::<CompletionReport>`. → set the subtask
//!   `Failed` and return a structured 400 error. The artifact is NOT written.
//!
//! - **prose-only / soft issues** — the payload deserializes into a valid
//!   `CompletionReport` but a soft field is weak (e.g. empty `summaryMd`,
//!   empty deliverables). → accept-and-warn: persist the artifact, return 200
//!   with a `warnings` array. The reconciler still gates completion on the
//!   artifact's PRESENCE, not its prose quality.
//!
//! On accept the tool calls `write_subtask_artifact(run_id, task_id, &report)`.
//! It does NOT flip the subtask to `Completed` — that is the Phase-3
//! reconciler's job (it needs BOTH the FSM `Ready` signal AND the artifact;
//! see `orchestration_loop::ai_session_executor::can_complete`).

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::Emitter;
use tracing::warn;
use uuid::Uuid;

use crate::database::pg::completion_reports::CompletionReport;
use crate::mcp::types::ApiState;
use crate::orchestration_loop::ledger::SubtaskState;

/// Request body for `orchestration_report_subtask`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ReportSubtaskBody {
    /// The run uuid the worker was dispatched under (resolves the ledger run).
    pub run_id: Uuid,
    /// The subtask's stable DAG-node id within the run.
    pub task_id: String,
    /// The worker-written completion report, untyped at the wire boundary so
    /// the tool can distinguish a structural violation (deserialize failure)
    /// from a prose-soft one (deserializes, weak fields).
    pub completion_report: serde_json::Value,
}

/// Success response — the artifact was persisted. `warnings` is non-empty when
/// the report was accepted-with-warnings (prose-soft issues).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportSubtaskResponse {
    pub run_id: Uuid,
    pub task_id: String,
    pub accepted: bool,
    /// Empty when the report was clean; otherwise the prose-soft notes.
    pub warnings: Vec<String>,
}

/// Inspect a successfully-deserialized report for prose-only / soft issues.
/// These NEVER reject — they are surfaced as warnings so the artifact is still
/// written. Mirrors the contract's "accept-and-warn" branch.
fn soft_warnings(report: &CompletionReport) -> Vec<String> {
    let mut w = Vec::new();
    if report.summary_md.trim().is_empty() {
        w.push("summaryMd is empty — the downstream brief will have no narrative".to_string());
    }
    if report.deliverables.is_empty() {
        w.push("deliverables is empty — no concrete output was pointed at".to_string());
    }
    w
}

/// Shared implementation behind the HTTP handler (and any future Tauri
/// sibling). Performs the structural-vs-prose validation split, persists the
/// artifact on accept, and flips the subtask to `Failed` on a structural
/// violation. Returns the response body on accept, or `(StatusCode, message)`
/// on reject.
pub(crate) async fn report_subtask_inner(
    state: &Arc<ApiState>,
    body: ReportSubtaskBody,
) -> Result<ReportSubtaskResponse, (StatusCode, String)> {
    let pg = &state.app_state.pg_db;

    // Structural gate — a `data`-part schema violation (wrong types / missing
    // required fields) fails to deserialize into the typed CompletionReport.
    // On that failure: mark the subtask Failed and return a structured 400.
    let report: CompletionReport = match serde_json::from_value(body.completion_report.clone()) {
        Ok(r) => r,
        Err(e) => {
            // Best-effort transition to Failed — the structural violation means
            // the worker's report is unusable. A missing row is itself an error
            // worth surfacing (we still return the structural error to caller).
            if let Err(set_err) = pg
                .set_subtask_state(body.run_id, &body.task_id, SubtaskState::Failed)
                .await
            {
                warn!(
                    "orchestration_report_subtask: could not mark {}/{} Failed after schema violation: {}",
                    body.run_id, body.task_id, set_err
                );
            }
            let err_body = serde_json::json!({
                "error": "schema_violation",
                "detail": e.to_string(),
                "subtask_state": SubtaskState::Failed.as_str(),
            });
            return Err((StatusCode::BAD_REQUEST, err_body.to_string()));
        }
    };

    // Prose-soft check — deserialized fine; weak fields produce warnings only.
    let warnings = soft_warnings(&report);

    // Persist the artifact. We do NOT flip to Completed — the reconciler owns
    // that (it needs BOTH Ready + artifact). A missing (run_id, task_id) row is
    // a real error (worker reporting against an unknown subtask).
    pg.write_subtask_artifact(body.run_id, &body.task_id, &report)
        .await
        .map_err(|e| {
            if e.contains("no subtask") {
                (StatusCode::NOT_FOUND, e)
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e)
            }
        })?;

    // Notify the frontend so the run panel can re-render without polling.
    if let Err(e) = state.app_handle.emit(
        "orchestration-subtask-reported",
        serde_json::json!({
            "runId": body.run_id,
            "taskId": body.task_id,
            "warnings": warnings,
        }),
    ) {
        warn!("emit orchestration-subtask-reported failed: {}", e);
    }

    Ok(ReportSubtaskResponse {
        run_id: body.run_id,
        task_id: body.task_id,
        accepted: true,
        warnings,
    })
}

async fn report_subtask(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<ReportSubtaskBody>,
) -> Result<Json<ReportSubtaskResponse>, (StatusCode, String)> {
    let resp = report_subtask_inner(&state, body).await?;
    Ok(Json(resp))
}

/// Routes for the `orchestration_report_subtask` MCP tool. Merged in
/// `mcp_api.rs` alongside the other runner MCP route modules.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/orchestration/report-subtask", post(report_subtask))
}

// ============================================================================
// Tests — validation split is unit-testable WITHOUT a live runner. The accept
// path (which touches PG) is exercised via the deferred temp-runner test; here
// we cover the structural-vs-prose classification that decides reject vs warn.
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// A structurally-valid completion_report value (deserializes cleanly).
    fn valid_report_value() -> serde_json::Value {
        serde_json::json!({
            "summaryMd": "Implemented the thing and opened a PR.",
            "deliverables": [
                {"kind": "pr", "reference": "https://github.com/x/y/pull/1", "description": "the PR"}
            ],
            "breakingChanges": [],
            "followUps": []
        })
    }

    #[test]
    fn well_formed_report_deserializes_and_would_persist() {
        let v = valid_report_value();
        let report: CompletionReport =
            serde_json::from_value(v).expect("well-formed report must deserialize");
        // No soft warnings on a clean report → accept WITHOUT warnings.
        assert!(
            soft_warnings(&report).is_empty(),
            "clean report must produce no warnings"
        );
    }

    #[test]
    fn structurally_broken_report_is_rejected() {
        // `deliverables` is a STRING, not an array → structural schema
        // violation → must FAIL to deserialize (the reject branch).
        let v = serde_json::json!({
            "summaryMd": "did stuff",
            "deliverables": "not-an-array",
            "breakingChanges": [],
            "followUps": []
        });
        let result: Result<CompletionReport, _> = serde_json::from_value(v);
        assert!(
            result.is_err(),
            "deliverables-as-string must be a structural (reject) violation"
        );
    }

    #[test]
    fn missing_required_field_is_rejected() {
        // `summaryMd` (required String) is absent → structural violation.
        let v = serde_json::json!({
            "deliverables": [],
            "breakingChanges": [],
            "followUps": []
        });
        let result: Result<CompletionReport, _> = serde_json::from_value(v);
        assert!(
            result.is_err(),
            "a missing required field must be a structural (reject) violation"
        );
    }

    #[test]
    fn prose_soft_empty_summary_is_accept_with_warning() {
        // Empty summaryMd deserializes fine (it's a valid String) → this is the
        // accept-and-warn branch, NOT a reject.
        let v = serde_json::json!({
            "summaryMd": "",
            "deliverables": [
                {"kind": "pr", "reference": "x", "description": "y"}
            ],
            "breakingChanges": [],
            "followUps": []
        });
        let report: CompletionReport =
            serde_json::from_value(v).expect("empty-summary report still deserializes");
        let warnings = soft_warnings(&report);
        assert!(
            warnings.iter().any(|w| w.contains("summaryMd is empty")),
            "empty summary must produce a warning (accept-with-warning), got: {warnings:?}"
        );
    }

    #[test]
    fn prose_soft_empty_deliverables_warns_but_accepts() {
        let v = serde_json::json!({
            "summaryMd": "did the work but pointed at nothing concrete",
            "deliverables": [],
            "breakingChanges": [],
            "followUps": []
        });
        let report: CompletionReport = serde_json::from_value(v).expect("deserializes");
        let warnings = soft_warnings(&report);
        assert!(
            warnings.iter().any(|w| w.contains("deliverables is empty")),
            "empty deliverables must warn, got: {warnings:?}"
        );
    }

    #[test]
    fn report_body_deserializes_snake_case() {
        let body: ReportSubtaskBody = serde_json::from_value(serde_json::json!({
            "run_id": "11111111-2222-3333-4444-555555555555",
            "task_id": "T1",
            "completion_report": valid_report_value(),
        }))
        .expect("body must deserialize");
        assert_eq!(body.task_id, "T1");
        assert_eq!(
            body.run_id.to_string(),
            "11111111-2222-3333-4444-555555555555"
        );
    }
}
