//! Runner MCP tool — `orchestration_report_subtask`.
//!
//! Approach-D Conductor/Engine Phase 2 §3 (RESULT). A worker AI-session calls
//! this tool when it finishes a subtask, handing back a structured
//! [`CompletionReport`] keyed by `(run_id, task_id)`. The tool is mounted as an
//! HTTP route alongside the other runner MCP route modules (see
//! `mcp_api.rs`), which is
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
//!   a failed `serde_json::from_value::<CompletionReport>`. → return a
//!   structured 400 carrying serde's detail AND the JSON Schema the report must
//!   match ([`completion_report_schema`]). The artifact is NOT written and the
//!   subtask is NOT touched: a malformed report is a recoverable client error,
//!   and the worker retries.
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
use crate::database::pg::PgDb;
use crate::mcp::types::ApiState;

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

/// The JSON Schema a `completion_report` must match, generated from
/// [`CompletionReport`] itself so it cannot drift from what the route accepts.
/// Returned inside every schema-violation refusal, and the source of the
/// `/coord-mcp` tool's input schema.
pub(crate) fn completion_report_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(CompletionReport)).unwrap_or(serde_json::Value::Null)
}

/// The 400 body for a `completion_report` that does not deserialize: the
/// token, serde's detail, and the schema to correct it against.
pub(crate) fn schema_violation_body(detail: &str) -> serde_json::Value {
    serde_json::json!({
        "error": "schema_violation",
        "detail": detail,
        "retryable": true,
        "hint": "the subtask was not changed; correct completion_report to match expectedSchema and report again",
        "expectedSchema": completion_report_schema(),
    })
}

/// Shared implementation behind the HTTP handler and the runner-local
/// `orchestration_report_subtask` tool on the `/coord-mcp` proxy. Performs the
/// structural-vs-prose validation split and persists the artifact on accept.
/// Returns the response body on accept, or `(StatusCode, message)` on reject;
/// a reject never changes the subtask.
pub(crate) async fn report_subtask_inner(
    state: &Arc<ApiState>,
    body: ReportSubtaskBody,
) -> Result<ReportSubtaskResponse, (StatusCode, String)> {
    let resp = record_report(&state.app_state.pg_db, body).await?;

    // Notify the frontend so the run panel can re-render without polling.
    if let Err(e) = state.app_handle.emit(
        "orchestration-subtask-reported",
        serde_json::json!({
            "runId": resp.run_id,
            "taskId": resp.task_id,
            "warnings": resp.warnings,
        }),
    ) {
        warn!("emit orchestration-subtask-reported failed: {}", e);
    }

    Ok(resp)
}

/// The ledger half of [`report_subtask_inner`]: validate, then persist the
/// artifact on accept. Split out so it runs against a bare [`PgDb`].
pub(crate) async fn record_report(
    pg: &PgDb,
    body: ReportSubtaskBody,
) -> Result<ReportSubtaskResponse, (StatusCode, String)> {
    // Structural gate — a `data`-part schema violation (wrong types / missing
    // required fields) fails to deserialize into the typed CompletionReport.
    // It is a RECOVERABLE client error: the subtask is left exactly where it
    // is and the 400 carries the schema the report must match, so the worker
    // corrects it and calls again. It used to mark the subtask `Failed`, which
    // — once a failed row fails its dependents (plan
    // `2026-09-23-conductor-e2e-phase1-defects`, Phase 3) — let one malformed
    // `followUps` entry fail every row downstream of it. A worker that never
    // gets it right is still bounded: the §5 guard needs `ReadyIdle` AND an
    // artifact, and the post-Ready recovery re-prompts once then fails it.
    let report: CompletionReport = match serde_json::from_value(body.completion_report.clone()) {
        Ok(r) => r,
        Err(e) => {
            let err_body = schema_violation_body(&e.to_string());
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

    /// Phase 3 (P1-4): a schema violation hands the worker what it needs to
    /// correct the report — serde's detail, the schema (with the `FollowUp`
    /// element shape a guess most often gets wrong), and that it may retry.
    #[test]
    fn schema_violation_body_carries_the_expected_schema() {
        let err = serde_json::from_value::<CompletionReport>(serde_json::json!({
            "summaryMd": "x",
            "deliverables": [],
            "breakingChanges": [],
            "followUps": ["write the docs"]
        }))
        .expect_err("a bare-string followUp is a structural violation");
        let body = schema_violation_body(&err.to_string());
        assert_eq!(body["error"], "schema_violation");
        assert_eq!(body["retryable"], true);
        assert!(!body["detail"].as_str().unwrap_or("").is_empty());
        let schema = body["expectedSchema"].to_string();
        for field in [
            "summaryMd",
            "followUps",
            "blockingForDependents",
            "migrationStepsMd",
        ] {
            assert!(schema.contains(field), "expectedSchema must name {field}");
        }
        assert!(
            body.get("subtask_state").is_none(),
            "a refusal no longer reports (or causes) a state change"
        );
    }

    /// Phase 3: against a real row, a malformed report is a 400 carrying the
    /// schema and leaves the subtask `working` with no artifact; the corrected
    /// report is then accepted and written. This is the path the `/coord-mcp`
    /// `orchestration_report_subtask` tool and the HTTP route both take.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn a_malformed_report_leaves_the_row_working_and_a_retry_lands() {
        use crate::orchestration_loop::ledger::{Subtask, SubtaskState};

        let pg = PgDb::new_for_test().await;
        let run_id = Uuid::new_v4();
        pg.create_or_get_run(run_id, "g", None, &[], "running", Some("test"), None)
            .await
            .expect("create");
        let now = chrono::Utc::now();
        pg.upsert_subtask(&Subtask {
            task_id: "T1".to_string(),
            run_id,
            idx: 0,
            title: "t".to_string(),
            brief: "b".to_string(),
            phase: "implement".to_string(),
            repo: None,
            depends_on: vec![],
            expected_output: "a report".to_string(),
            emits_subtasks: false,
            state: SubtaskState::Working,
            task_run_id: Some(Uuid::new_v4()),
            artifact: None,
            produced_by: None,
            gate_id: None,
            gate_status: None,
            state_reason: None,
            created_at: now,
            updated_at: now,
        })
        .await
        .expect("upsert");

        let mut bad = valid_report_value();
        bad["followUps"] = serde_json::json!(["a bare string"]);
        let (status, body) = record_report(
            &pg,
            ReportSubtaskBody {
                run_id,
                task_id: "T1".to_string(),
                completion_report: bad,
            },
        )
        .await
        .expect_err("a malformed report is refused");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("expectedSchema"));
        let row = pg.list_subtasks(run_id).await.expect("list").remove(0);
        assert_eq!(row.state, SubtaskState::Working, "the row is untouched");
        assert!(row.artifact.is_none());

        let ok = record_report(
            &pg,
            ReportSubtaskBody {
                run_id,
                task_id: "T1".to_string(),
                completion_report: valid_report_value(),
            },
        )
        .await
        .expect("the corrected report lands");
        assert!(ok.accepted);
        let row = pg.list_subtasks(run_id).await.expect("list").remove(0);
        assert_eq!(
            row.state,
            SubtaskState::Working,
            "completion is the reconciler's"
        );
        assert!(row.artifact.is_some());

        let conn = pg.pool().get().await.expect("conn");
        let _ = conn
            .execute(
                "DELETE FROM orchestration.runs WHERE run_id = $1",
                &[&run_id],
            )
            .await;
    }
}
