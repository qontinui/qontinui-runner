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
//! The same tool is also served in-process on the `/coord-mcp` proxy
//! (`mcp_api::coord_mcp_runner_local_tool_call`), which every worker's
//! `.mcp.json` already points at, with [`report_tool_input_schema`] as its
//! input schema. Both doors end in [`report_subtask_inner`].
//!
//! ## Validation — reject-vs-warn split (contract resolved-Q1)
//!
//! The `completion_report` arrives as a `serde_json::Value` so the tool can
//! distinguish two failure classes:
//!
//! - **`data`-part schema violation (structural)** — wrong types / missing
//!   required fields, e.g. `deliverables` is a string not an array. Detected by
//!   a failed `serde_json::from_value::<CompletionReport>`. → a 400 carrying
//!   serde's detail AND the expected schema ([`schema_violation_body`]). The
//!   artifact is NOT written and the subtask is NOT touched: a malformed
//!   report is a recoverable client error, and the worker may call again.
//!   (It used to mark the subtask `Failed`, so one mistyped `followUps`
//!   element killed the subtask — and, once a failed dependency fails its
//!   dependents, every row downstream of it. A worker that never gets the
//!   shape right is still bounded: it goes `ReadyIdle` with no artifact and
//!   the conductor's §5 re-prompt-then-fail recovery reaches it.)
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
use crate::orchestration_loop::ai_session_executor::{REPORT_ROUTE, REPORT_TOOL_NAME};

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

/// The expected shape of a `completion_report`, generated from
/// [`CompletionReport`] itself so it can never drift from what the route
/// deserializes.
pub fn completion_report_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(CompletionReport))
        .unwrap_or_else(|e| serde_json::json!({ "error": format!("schema unavailable: {e}") }))
}

/// The 400 body for a structurally invalid report: serde's `detail`, the
/// `expectedSchema`, and `retryable: true` — the subtask was left as it was,
/// so the worker fixes the payload and calls again.
pub fn schema_violation_body(detail: &str) -> serde_json::Value {
    serde_json::json!({
        "error": "schema_violation",
        "detail": detail,
        "retryable": true,
        "subtaskUnchanged": true,
        "hint": "completion_report did not match the expected schema. The subtask is \
                 unchanged: fix the payload and call the tool again.",
        "expectedSchema": completion_report_schema(),
    })
}

/// The structural gate: the typed report, or the 400 body. Pure — no write
/// happens on either arm, which is what keeps a malformed report from
/// touching the subtask.
pub fn parse_completion_report(
    value: &serde_json::Value,
) -> Result<CompletionReport, serde_json::Value> {
    serde_json::from_value(value.clone()).map_err(|e| schema_violation_body(&e.to_string()))
}

/// The MCP `inputSchema` of [`REPORT_TOOL_NAME`]: the [`ReportSubtaskBody`]
/// envelope, with `completion_report` generated from [`CompletionReport`].
/// The generated schema's definitions are hoisted to the input schema's root,
/// because its `$ref`s are written against the root of the document they
/// live in.
pub fn report_tool_input_schema() -> serde_json::Value {
    let mut report = completion_report_schema();
    let mut defs = serde_json::Map::new();
    if let Some(obj) = report.as_object_mut() {
        obj.remove("$schema");
        for key in ["$defs", "definitions"] {
            if let Some(serde_json::Value::Object(d)) = obj.remove(key) {
                defs.extend(d);
            }
        }
    }
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": {
            "run_id": {
                "type": "string",
                "format": "uuid",
                "description": "The run uuid this worker was dispatched under.",
            },
            "task_id": {
                "type": "string",
                "description": "This subtask's task_id within the run.",
            },
            "completion_report": report,
        },
        "required": ["run_id", "task_id", "completion_report"],
    });
    if !defs.is_empty() {
        schema["$defs"] = serde_json::Value::Object(defs);
    }
    schema
}

/// The MCP `tools/list` entry for [`REPORT_TOOL_NAME`].
pub fn report_tool_descriptor() -> serde_json::Value {
    serde_json::json!({
        "name": REPORT_TOOL_NAME,
        "description": format!(
            "Report this orchestration worker's finished subtask: a CompletionReport \
             for (run_id, task_id). Served by the local runner (never forwarded to \
             coord); the same body can be POSTed to {REPORT_ROUTE}. A malformed report \
             is rejected with the expected schema and leaves the subtask unchanged, so \
             fix it and call again."
        ),
        "inputSchema": report_tool_input_schema(),
    })
}

/// Validate and persist a report against the ledger. Performs the
/// structural-vs-prose validation split and writes the artifact on accept.
/// A structural violation writes NOTHING (see the module docs). Returns the
/// response body on accept, or `(StatusCode, body)` on reject.
pub(crate) async fn report_subtask_to_ledger(
    pg: &PgDb,
    body: &ReportSubtaskBody,
) -> Result<ReportSubtaskResponse, (StatusCode, String)> {
    let report = parse_completion_report(&body.completion_report).map_err(|err_body| {
        warn!(
            "orchestration_report_subtask: {}/{} sent a malformed report; rejected with the \
             schema, subtask unchanged: {}",
            body.run_id,
            body.task_id,
            err_body["detail"].as_str().unwrap_or("")
        );
        (StatusCode::BAD_REQUEST, err_body.to_string())
    })?;

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
        task_id: body.task_id.clone(),
        accepted: true,
        warnings,
    })
}

/// Shared implementation behind the HTTP handler and the `/coord-mcp`
/// runner-local tool: [`report_subtask_to_ledger`], then the frontend
/// notification on accept.
pub(crate) async fn report_subtask_inner(
    state: &Arc<ApiState>,
    body: ReportSubtaskBody,
) -> Result<ReportSubtaskResponse, (StatusCode, String)> {
    let resp = report_subtask_to_ledger(&state.app_state.pg_db, &body).await?;

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
    Router::new().route(REPORT_ROUTE, post(report_subtask))
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
    fn a_schema_violation_answers_400_material_with_the_expected_schema() {
        let v = serde_json::json!({
            "summaryMd": "did stuff",
            "deliverables": [],
            "breakingChanges": [],
            // A follow-up missing its required `priority`.
            "followUps": [{"description": "loose end"}]
        });
        let body = parse_completion_report(&v).expect_err("missing priority is structural");
        assert_eq!(body["error"], "schema_violation");
        assert_eq!(body["retryable"], true);
        assert_eq!(body["subtaskUnchanged"], true);
        assert!(
            body["detail"].as_str().unwrap().contains("priority"),
            "serde's detail names the field: {body}"
        );
        let schema = body["expectedSchema"].to_string();
        for field in ["summaryMd", "followUps", "blockingForDependents", "migrationStepsMd"] {
            assert!(schema.contains(field), "schema names {field}: {schema}");
        }
        // No `subtask_state: failed` any more — the row is not touched.
        assert!(body.get("subtask_state").is_none(), "{body}");
    }

    #[test]
    fn the_tool_input_schema_wraps_the_generated_report_schema() {
        let schema = report_tool_input_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(
            schema["required"],
            serde_json::json!(["run_id", "task_id", "completion_report"])
        );
        let report = &schema["properties"]["completion_report"];
        assert!(report["properties"]["summaryMd"].is_object(), "{schema}");
        assert!(report.get("$schema").is_none(), "hoisted: {schema}");
        assert!(report.get("$defs").is_none(), "hoisted: {schema}");
        // Every `$ref` resolves against the input schema's own root.
        let text = schema.to_string();
        for (i, _) in text.match_indices("\"$ref\":\"#/$defs/") {
            let name: String = text
                .get(i + "\"$ref\":\"#/$defs/".len()..)
                .unwrap_or("")
                .chars()
                .take_while(|c| *c != '"')
                .collect();
            assert!(schema["$defs"][&name].is_object(), "$ref {name} resolves: {schema}");
        }
        assert!(schema["$defs"]["FollowUp"].is_object(), "{schema}");
        let d = report_tool_descriptor();
        assert_eq!(d["name"], REPORT_TOOL_NAME);
        assert_eq!(d["inputSchema"], schema);
    }

    /// The PG half of "a malformed report returns 400 and leaves the row
    /// `Working`": the route writes nothing on a schema violation.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn a_malformed_report_is_400_and_leaves_the_row_working() {
        use crate::orchestration_loop::ledger::{Subtask, SubtaskState};
        let pg = PgDb::new_for_test().await;
        let run_id = Uuid::new_v4();
        pg.create_run(run_id, "malformed report", None, &[], "running")
            .await
            .expect("create_run");
        let now = chrono::Utc::now();
        pg.upsert_subtask(&Subtask {
            task_id: "W".to_string(),
            run_id,
            idx: 0,
            title: "worker".to_string(),
            brief: "b".to_string(),
            phase: "implement".to_string(),
            repo: None,
            depends_on: vec![],
            expected_output: "x".to_string(),
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

        let bad = ReportSubtaskBody {
            run_id,
            task_id: "W".to_string(),
            completion_report: serde_json::json!({"summaryMd": "x", "deliverables": "nope"}),
        };
        let (status, body) = report_subtask_to_ledger(&pg, &bad)
            .await
            .expect_err("malformed");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let body: serde_json::Value = serde_json::from_str(&body).expect("json body");
        assert!(body["expectedSchema"].is_object(), "{body}");
        let row = pg.list_subtasks(run_id).await.expect("list").remove(0);
        assert_eq!(row.state, SubtaskState::Working, "the row is not failed");
        assert!(row.artifact.is_none(), "and nothing was written");

        // The retry with a well-formed report lands.
        let good = ReportSubtaskBody {
            completion_report: valid_report_value(),
            ..bad
        };
        report_subtask_to_ledger(&pg, &good).await.expect("accepted");
        let row = pg.list_subtasks(run_id).await.expect("list").remove(0);
        assert!(row.artifact.is_some());

        let conn = pg.pool().get().await.expect("conn");
        conn.execute("DELETE FROM orchestration.runs WHERE run_id = $1", &[&run_id])
            .await
            .expect("cleanup");
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
