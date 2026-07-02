//! Spec proposals — Stream E (Flywheel) coverage-growth queue.
//!
//! Four endpoints, mounted under `/spec/proposals/...` when the
//! `spec-authoring` feature is enabled:
//!
//! - `POST /spec/proposals/scan` — discovers eligible `fullPage` candidates
//!   (un-spec'd pathnames with ≥ N observations in the lookback window) and
//!   `patch` candidates (drift-flagged specs), inserts them into
//!   `project.spec_proposals` with `status = 'queued'`.
//! - `POST /spec/proposals/{id}/execute` — drives one queued proposal through
//!   `spec_authoring::author_candidate` then `validator::validate_candidate`
//!   (Step 8 — distinctness → round-trip → coverage → b-green) and lands
//!   the candidate in `_pending/` via `storage::write_pending_ir` (Step 9).
//!   On validator rejection the structured `ValidatorError` reason + detail
//!   is persisted to `spec_proposals.last_error` and returned as `outcome:
//!   "validator-rejected"`. Authoring failures stay distinct and surface as
//!   `outcome: "authoring-failed"`.
//! - `POST /spec/proposals/sweep-pending` — Step 9's 2-green sweep. Re-runs
//!   gate-3 (B-green) against every `pendingPromotion` row; on Ok bumps
//!   `consecutive_greens` and promotes at ≥ 2; on `BExecutionRed` demotes
//!   immediately back to `queued`; on infra error (snapshot fetch failed
//!   etc.) only `last_attempt_at` moves.
//! - `GET /spec/proposals` — paginated list of all proposal rows.
//!
//! See `qontinui-dev-notes/spec-check-v1/05-flywheel.md` Steps 6–9 for the
//! design context; the SQL trigger query is verbatim from the plan
//! (§ Step 7 — "Full-page branch").

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{debug, info, warn};

use crate::commands::spec_drift::RegisteredElement;
use crate::database::pg::spec_proposals::SpecProposalRow;
use crate::mcp::types::ApiState;
use crate::spec_api::apps::AppErrorExt;
use crate::spec_api::events;
use crate::spec_api::responses::SpecError;
use crate::spec_api::storage;
use qontinui_types::spec_api_events::SpecApiEvent;

// =============================================================================
// Scan endpoint — POST /spec/proposals/scan
// =============================================================================

/// Default knobs for `/spec/proposals/scan`. All tunable via the request
/// body. Defaults come from `05-flywheel.md` § Step 7 ("`N` (default 50) and
/// 90-day window are baked into the SQL").
const DEFAULT_MIN_OBSERVATIONS: i32 = 50;
const DEFAULT_LOOKBACK_DAYS: i32 = 90;
const DEFAULT_SCAN_LIMIT: i64 = 100;

/// Request body for `/spec/proposals/scan`. All fields optional.
#[derive(Debug, Deserialize, Default)]
pub struct ScanRequest {
    #[serde(rename = "minObservations")]
    pub min_observations: Option<i32>,
    #[serde(rename = "lookbackDays")]
    pub lookback_days: Option<i32>,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
struct QueuedProposalSummary {
    #[serde(rename = "proposalId")]
    proposal_id: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pathname: Option<String>,
    #[serde(rename = "specId", skip_serializing_if = "Option::is_none")]
    spec_id: Option<String>,
}

pub async fn post_scan(
    AxPath(app_id): AxPath<String>,
    State(state): State<Arc<ApiState>>,
    body: Option<Json<ScanRequest>>,
) -> Response {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let min_obs = req.min_observations.unwrap_or(DEFAULT_MIN_OBSERVATIONS);
    let lookback = req.lookback_days.unwrap_or(DEFAULT_LOOKBACK_DAYS);
    let limit = req.limit.unwrap_or(DEFAULT_SCAN_LIMIT);

    let pg_db = state.app_state.pg_db.clone();
    let root = match storage::resolve_specs_root(&pg_db, &app_id).await {
        Ok(p) => p,
        Err(e) => return e.into_app_response(),
    };

    // ---- Full-page branch -------------------------------------------------
    let full_page_rows = match query_eligible_pathnames(&pg_db, lookback, min_obs, limit).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!("post_scan: query_eligible_pathnames failed: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpecError::with_detail(
                    "scan-query-failed",
                    json!({ "branch": "fullPage", "error": e }),
                )),
            )
                .into_response();
        }
    };

    let on_disk = storage::list_pages(&root, &app_id).unwrap_or_default();
    let on_disk_pathnames: HashSet<String> = on_disk
        .iter()
        .filter_map(|id| spec_id_to_pathname(id))
        .collect();

    let mut queued: Vec<QueuedProposalSummary> = Vec::new();
    let scanned_pathnames = full_page_rows.len();
    for pathname in full_page_rows {
        if on_disk_pathnames.contains(&pathname) {
            debug!(
                "post_scan: pathname {} already has a spec; skipping",
                pathname
            );
            continue;
        }
        let proposal_id = mint_proposal_id();
        let metadata = json!({
            "minObservations": min_obs,
            "lookbackDays": lookback,
        });
        match pg_db
            .insert_proposal(
                &proposal_id,
                "fullPage",
                Some(&pathname),
                None,
                "queued",
                &metadata,
            )
            .await
        {
            Ok(true) => {
                queued.push(QueuedProposalSummary {
                    proposal_id,
                    kind: "fullPage".to_string(),
                    pathname: Some(pathname),
                    spec_id: None,
                });
            }
            Ok(false) => {
                // Dedup hit — already queued.
                debug!(
                    "post_scan: fullPage pathname {} already queued (dedup)",
                    pathname
                );
            }
            Err(e) => {
                warn!("post_scan: insert_proposal (fullPage) failed: {}", e);
            }
        }
    }

    // ---- Patch branch -----------------------------------------------------
    let project_root = std::env::var("QONTINUI_PROJECT_ROOT").unwrap_or_else(|_| ".".into());
    let mut scanned_drift_reports = 0usize;
    match crate::commands::spec_drift::scan_spec_drift(app_id.clone(), project_root.clone()).await {
        Ok(drift) => {
            scanned_drift_reports = 1;
            let mut per_spec: std::collections::BTreeMap<String, Vec<&RegisteredElement>> =
                Default::default();
            for elem in &drift.missing_from_spec {
                if let Some(target_id) = infer_target_spec_id(&app_id, elem) {
                    per_spec.entry(target_id).or_default().push(elem);
                }
            }
            for (target_spec_id, elems) in per_spec {
                let proposal_id = mint_proposal_id();
                let metadata = json!({
                    "missingElementIds": elems.iter().map(|e| &e.id).collect::<Vec<_>>(),
                    "missingCount": elems.len(),
                });
                match pg_db
                    .insert_proposal(
                        &proposal_id,
                        "patch",
                        None,
                        Some(&target_spec_id),
                        "queued",
                        &metadata,
                    )
                    .await
                {
                    Ok(true) => {
                        queued.push(QueuedProposalSummary {
                            proposal_id,
                            kind: "patch".to_string(),
                            pathname: None,
                            spec_id: Some(target_spec_id),
                        });
                    }
                    Ok(false) => {
                        debug!(
                            "post_scan: patch spec_id {} already queued (dedup)",
                            target_spec_id
                        );
                    }
                    Err(e) => {
                        warn!("post_scan: insert_proposal (patch) failed: {}", e);
                    }
                }
            }
        }
        Err(e) => {
            // Soft failure — drift scan can fail (e.g. project_root missing)
            // without breaking the full-page branch. Surface in the response
            // but don't 500.
            warn!("post_scan: scan_spec_drift failed: {}", e);
        }
    }

    let reason = if queued.is_empty() {
        "no-eligible-pathnames".to_string()
    } else {
        format!("queued-{}-proposals", queued.len())
    };

    info!(
        "post_scan: scanned_pathnames={} scanned_drift_reports={} queued={}",
        scanned_pathnames,
        scanned_drift_reports,
        queued.len()
    );

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "scannedPathnames": scanned_pathnames,
            "scannedDriftReports": scanned_drift_reports,
            "queuedProposals": queued,
            "reason": reason,
        })),
    )
        .into_response()
}

/// Run the verbatim SQL from `05-flywheel.md` § Step 7 against
/// `co_occurrence_observations`. Returns pathnames sorted by observation
/// count desc, capped at `limit`.
async fn query_eligible_pathnames(
    pg_db: &crate::database::pg::PgDb,
    lookback_days: i32,
    min_observations: i32,
    limit: i64,
) -> Result<Vec<String>, String> {
    let conn = pg_db
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;
    let rows = conn
        .query(
            r#"
            SELECT
                snapshot_metadata->>'pathname' AS pathname,
                COUNT(*) AS obs
            FROM co_occurrence_observations
            WHERE captured_at >= now() - ($1::int || ' days')::interval
              AND invalidated_at IS NULL
              AND (snapshot_metadata->>'pathname') IS NOT NULL
            GROUP BY pathname
            HAVING COUNT(*) >= $2
            ORDER BY obs DESC
            LIMIT $3
            "#,
            &[&lookback_days, &(min_observations as i64), &limit],
        )
        .await
        .map_err(|e| format!("PG query_eligible_pathnames: {}", e))?;

    Ok(rows.iter().map(|r| r.get::<usize, String>(0)).collect())
}

// =============================================================================
// Execute endpoint — POST /spec/proposals/{id}/execute
// =============================================================================

pub async fn post_execute(
    AxPath((app_id, id)): AxPath<(String, String)>,
    State(state): State<Arc<ApiState>>,
) -> Response {
    let pg_db = state.app_state.pg_db.clone();
    let app_state = state.app_state.clone();
    let root = match storage::resolve_specs_root(&pg_db, &app_id).await {
        Ok(p) => p,
        Err(e) => return e.into_app_response(),
    };

    // Look up the proposal.
    let row = match pg_db.find_proposal_by_id(&id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(SpecError::with_detail(
                    "proposal-not-found",
                    json!({ "proposalId": id }),
                )),
            )
                .into_response();
        }
        Err(e) => {
            warn!("post_execute: find_proposal_by_id failed: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpecError::with_detail(
                    "proposal-lookup-failed",
                    json!({ "proposalId": id, "error": e }),
                )),
            )
                .into_response();
        }
    };

    // Transition to inFlight (best-effort; ignore errors so we still try to
    // author rather than wedging the queue).
    if let Err(e) = pg_db.update_proposal_status(&row.id, "inFlight").await {
        warn!(
            "post_execute: update_proposal_status (inFlight) failed: {}",
            e
        );
    }
    let _ = pg_db.update_proposal_attempt_only(&row.id).await;

    // Build the authoring mode from the row shape.
    let mode = match row.kind.as_str() {
        "fullPage" => {
            let pathname = match row.pathname.clone() {
                Some(p) => p,
                None => {
                    let msg = "fullPage proposal missing pathname".to_string();
                    let _ = pg_db
                        .update_proposal_status_with_error(&row.id, "failed", &msg)
                        .await;
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(SpecError::with_detail(
                            "proposal-shape-invalid",
                            json!({ "proposalId": row.id, "reason": msg }),
                        )),
                    )
                        .into_response();
                }
            };
            crate::workflow_generation::spec_authoring::AuthoringMode::FullPage { pathname }
        }
        "patch" => {
            let existing_spec_id = match row.spec_id.clone() {
                Some(s) => s,
                None => {
                    let msg = "patch proposal missing spec_id".to_string();
                    let _ = pg_db
                        .update_proposal_status_with_error(&row.id, "failed", &msg)
                        .await;
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(SpecError::with_detail(
                            "proposal-shape-invalid",
                            json!({ "proposalId": row.id, "reason": msg }),
                        )),
                    )
                        .into_response();
                }
            };
            // Patch mode requires a fresh drift report — re-scan to get the
            // current delta (the queued row only carries the missing element
            // ids in `metadata`, not the full report shape).
            let project_root =
                std::env::var("QONTINUI_PROJECT_ROOT").unwrap_or_else(|_| ".".into());
            let drift =
                match crate::commands::spec_drift::scan_spec_drift(app_id.clone(), project_root)
                    .await
                {
                    Ok(d) => d,
                    Err(e) => {
                        let _ = pg_db
                            .update_proposal_status_with_error(&row.id, "failed", &e)
                            .await;
                        return (
                            StatusCode::OK,
                            Json(json!({
                                "ok": false,
                                "proposalId": row.id,
                                "outcome": "authoring-failed",
                                "reason": "drift-scan-failed",
                                "detail": e,
                            })),
                        )
                            .into_response();
                    }
                };
            crate::workflow_generation::spec_authoring::AuthoringMode::Patch {
                existing_spec_id,
                drift,
            }
        }
        other => {
            let msg = format!("unknown proposal kind: {}", other);
            let _ = pg_db
                .update_proposal_status_with_error(&row.id, "failed", &msg)
                .await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpecError::with_detail(
                    "proposal-shape-invalid",
                    json!({ "proposalId": row.id, "reason": msg }),
                )),
            )
                .into_response();
        }
    };

    // Dispatch to spec_authoring. The sibling agent owns this module — at
    // integration time, an unresolved import here means the sibling's PR
    // hasn't landed yet.
    let app_state_for_validator = app_state.clone();
    let outcome = match crate::workflow_generation::spec_authoring::author_candidate(
        pg_db.clone(),
        app_state,
        &app_id,
        mode,
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            let reason = authoring_error_reason(&e);
            let detail = authoring_error_detail(&e);
            let _ = pg_db
                .update_proposal_status_with_error(&row.id, "failed", &detail)
                .await;
            return (
                StatusCode::OK,
                Json(json!({
                    "ok": false,
                    "proposalId": row.id,
                    "outcome": "authoring-failed",
                    "reason": reason,
                    "detail": detail,
                })),
            )
                .into_response();
        }
    };

    // Step 8 — three-gate validator (distinctness → round-trip → coverage →
    // b-green). On Err, surface as `validator-rejected` + persist the
    // reason in `spec_proposals.last_error`. Authoring failures stay
    // distinct (`authoring-failed` codepath above). `root` resolved above.
    let pathname_for_gate = match row.kind.as_str() {
        "fullPage" => row.pathname.clone().unwrap_or_default(),
        "patch" => row.spec_id.clone().unwrap_or_default(),
        _ => String::new(),
    };
    if let Err(verr) = crate::spec_api::validator::validate_candidate(
        app_state_for_validator,
        &outcome.candidate,
        &pathname_for_gate,
        &root,
    )
    .await
    {
        let reason = crate::spec_api::validator::validator_error_reason(&verr);
        let detail = crate::spec_api::validator::validator_error_detail(&verr);
        let _ = pg_db
            .update_proposal_status_with_error(&row.id, "failed", &detail)
            .await;
        warn!(
            "post_execute: validator rejected proposal {}: {} — {}",
            row.id, reason, detail
        );
        return (
            StatusCode::OK,
            Json(json!({
                "ok": false,
                "proposalId": row.id,
                "outcome": "validator-rejected",
                "reason": reason,
                "detail": detail,
            })),
        )
            .into_response();
    }

    // Step 9 — stage the candidate into `<root>/pages/<id>/_pending/`. The
    // helper stamps `provenance.status = Pending` and regenerates the
    // projection sibling, both atomically via temp-rename. The page-level
    // lock guards against a concurrent sweep promoting / demoting the same
    // page while we write.
    let staged_path = {
        let _guard = match storage::page_lock(&root, &outcome.candidate.id) {
            Ok(g) => g,
            Err(e) => {
                let _ = pg_db
                    .update_proposal_status_with_error(&row.id, "failed", &e)
                    .await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(SpecError::with_detail(
                        "stage-pending-locked",
                        json!({ "proposalId": row.id, "error": e }),
                    )),
                )
                    .into_response();
            }
        };
        match storage::write_pending_ir(&root, &app_id, &outcome.candidate) {
            Ok(p) => p.display().to_string(),
            Err(e) => {
                let _ = pg_db
                    .update_proposal_status_with_error(&row.id, "failed", &e)
                    .await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(SpecError::with_detail(
                        "stage-pending-failed",
                        json!({ "proposalId": row.id, "error": e }),
                    )),
                )
                    .into_response();
            }
        }
    };

    // Persist the candidate IR + flip to pendingPromotion (skipping the
    // pendingValidation hop that Step 8 will introduce).
    let candidate_json = match serde_json::to_value(&outcome.candidate) {
        Ok(v) => v,
        Err(e) => {
            warn!("post_execute: serialize candidate failed: {}", e);
            let msg = format!("serialize candidate: {}", e);
            let _ = pg_db
                .update_proposal_status_with_error(&row.id, "failed", &msg)
                .await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpecError::with_detail(
                    "serialize-candidate-failed",
                    json!({ "proposalId": row.id, "error": msg }),
                )),
            )
                .into_response();
        }
    };
    if let Err(e) = pg_db
        .update_proposal_candidate_ir(&row.id, "pendingPromotion", &candidate_json)
        .await
    {
        warn!("post_execute: update_proposal_candidate_ir failed: {}", e);
    }

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "proposalId": row.id,
            "outcome": "landed-in-pending",
            "stagedPath": staged_path,
        })),
    )
        .into_response()
}

fn authoring_error_reason(
    e: &crate::workflow_generation::spec_authoring::AuthoringError,
) -> &'static str {
    use crate::workflow_generation::spec_authoring::AuthoringError::*;
    match e {
        NoDiscoveryArtifact { .. } => "no-discovery-artifact",
        EmptyArtifact => "empty-artifact",
        MalformedAiOutput(_) => "malformed-ai-output",
        AiDispatchFailed(_) => "ai-dispatch-failed",
        ExistingSpecMissing(_) => "existing-spec-missing",
        Database(_) => "database-error",
    }
}

fn authoring_error_detail(
    e: &crate::workflow_generation::spec_authoring::AuthoringError,
) -> String {
    use crate::workflow_generation::spec_authoring::AuthoringError::*;
    match e {
        NoDiscoveryArtifact { pathname } => {
            format!("no discovery artifact for pathname={}", pathname)
        }
        EmptyArtifact => "discovery artifact has zero states".to_string(),
        MalformedAiOutput(s) => format!("malformed AI output: {}", s),
        AiDispatchFailed(s) => format!("AI dispatch failed: {}", s),
        ExistingSpecMissing(s) => format!("existing spec missing: {}", s),
        Database(s) => format!("database error: {}", s),
    }
}

// =============================================================================
// Sweep-pending endpoint — POST /spec/proposals/sweep-pending
// =============================================================================
//
// Drives the 2-green promotion (and single-red demotion) of every proposal in
// `status = 'pendingPromotion'`. Invoked by the supervisor cron (Step 10) on
// the nightly cadence; also usable as a manual ergonomic. Per Step 9 of
// `05-flywheel.md`:
//
//   - Re-runs ONLY gate-3 (B-execution-green). Gates 1 + 2 are pure
//     properties of the candidate IR and don't change between sweeps.
//   - Ok ⇒ greens += 1; if ≥ 2, call `storage::promote_pending` + status =
//     "promoted". Else just bump the greens counter.
//   - `Err(BExecutionRed)` ⇒ `storage::demote_pending` + reset greens to 0 +
//     status = "queued" + `last_error = "single-red: <summary>"`.
//   - `Err(other)` (e.g. SnapshotFetchFailed) ⇒ infra error; only
//     `last_attempt_at` moves. No state change.
//
// Per-proposal critical section is wrapped in `storage::page_lock` so a
// manual `/execute` overlap can't race the sweep on the same page id.

const SWEEP_BATCH_LIMIT: i64 = 100;

pub async fn post_sweep_pending(
    AxPath(app_id): AxPath<String>,
    State(state): State<Arc<ApiState>>,
) -> Response {
    let pg_db = state.app_state.pg_db.clone();
    let app_state = state.app_state.clone();
    let root = match storage::resolve_specs_root(&pg_db, &app_id).await {
        Ok(p) => p,
        Err(e) => return e.into_app_response(),
    };

    let rows = match pg_db
        .list_proposals_by_status("pendingPromotion", SWEEP_BATCH_LIMIT)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!("post_sweep_pending: list_proposals_by_status failed: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpecError::with_detail(
                    "sweep-list-failed",
                    json!({ "error": e }),
                )),
            )
                .into_response();
        }
    };

    let total = rows.len();
    let mut promoted = 0usize;
    let mut demoted = 0usize;
    let mut infra_errors = 0usize;

    for prop in rows {
        // Use spec_id when present (canonical page id); fall back to pathname
        // for fullPage rows that haven't been canonicalized yet. The same
        // value is used both as the page id for storage helpers AND as the
        // pathname forwarded to `gate_b_execution_green` (the SDK treats it
        // as a `route` hint on the fingerprint — non-load-bearing in v1.0).
        let spec_id = prop
            .spec_id
            .clone()
            .or_else(|| prop.pathname.clone())
            .unwrap_or_default();
        if spec_id.is_empty() {
            warn!(
                "post_sweep_pending: skip {} — no spec_id or pathname",
                prop.id
            );
            infra_errors += 1;
            let _ = pg_db.update_proposal_attempt_only(&prop.id).await;
            continue;
        }

        // Acquire the per-page lock for the duration of the read-and-gate +
        // promote/demote critical section.
        let _guard = match storage::page_lock(&root, &spec_id) {
            Ok(g) => g,
            Err(e) => {
                // Treat lock contention as an infra error — the next sweep
                // tick will retry. Bump last_attempt_at and move on.
                warn!(
                    "post_sweep_pending: page_lock({}) failed: {} — treating as infra error",
                    spec_id, e
                );
                infra_errors += 1;
                let _ = pg_db.update_proposal_attempt_only(&prop.id).await;
                continue;
            }
        };

        let pending_ir = match storage::read_pending_ir(&root, &spec_id) {
            Ok(Some(ir)) => ir,
            Ok(None) => {
                // The _pending/ artefact has been swept out from under us —
                // the row is stale. Demote the row in PG so the queue moves
                // on, but don't increment `demoted` (no green/red verdict
                // was reached).
                warn!(
                    "post_sweep_pending: pending IR missing on disk for {}; resetting",
                    prop.id
                );
                let _ = pg_db
                    .update_proposal_status_with_error(
                        &prop.id,
                        "queued",
                        "pending IR missing on disk during sweep",
                    )
                    .await;
                let _ = pg_db.update_proposal_greens(&prop.id, 0).await;
                infra_errors += 1;
                continue;
            }
            Err(e) => {
                warn!(
                    "post_sweep_pending: read_pending_ir({}) failed: {}",
                    spec_id, e
                );
                infra_errors += 1;
                let _ = pg_db.update_proposal_attempt_only(&prop.id).await;
                continue;
            }
        };

        let gate_result = crate::spec_api::validator::gate_b_execution_green(
            app_state.clone(),
            &pending_ir,
            &spec_id,
        )
        .await;

        match gate_result {
            Ok(green_result) => {
                let new_greens = prop.consecutive_greens.saturating_add(1);
                if new_greens >= 2 {
                    // Promote.
                    match storage::promote_pending(&root, &app_id, &spec_id) {
                        Ok(()) => {
                            let _ = pg_db.update_proposal_status(&prop.id, "promoted").await;
                            let _ = pg_db.update_proposal_attempt_only(&prop.id).await;
                            promoted += 1;
                            // Plan 06: Promoted emit — joins via snapshot_id
                            // against the green run that triggered it.
                            events::emit(SpecApiEvent::FlywheelProposalPromoted {
                                app_id: app_id.clone(),
                                proposal_id: prop.id.clone(),
                                page_id: spec_id.clone(),
                                consecutive_greens: new_greens.max(0) as u32,
                                snapshot_id: green_result.snapshot_id.clone(),
                                at_ms: events::now_ms(),
                            });
                            info!(
                                "post_sweep_pending: promoted {} (spec_id={})",
                                prop.id, spec_id
                            );
                        }
                        Err(e) => {
                            // Promote-time IO error — leave greens at +1
                            // count so the next sweep retries.
                            warn!(
                                "post_sweep_pending: promote_pending({}) failed: {}",
                                spec_id, e
                            );
                            let _ = pg_db.update_proposal_greens(&prop.id, new_greens).await;
                            let _ = pg_db.update_proposal_attempt_only(&prop.id).await;
                            infra_errors += 1;
                        }
                    }
                } else {
                    let _ = pg_db.update_proposal_greens(&prop.id, new_greens).await;
                    let _ = pg_db.update_proposal_attempt_only(&prop.id).await;
                    info!(
                        "post_sweep_pending: {} greens advanced to {}",
                        prop.id, new_greens
                    );
                }
            }
            Err(crate::spec_api::validator::ValidatorError::BExecutionRed {
                result_summary,
                snapshot_id: red_snapshot_id,
                failing_assertion,
                ..
            }) => {
                let detail = format!(
                    "single-red: critical={} error={} warning={} info={}",
                    result_summary.severity_counts.critical,
                    result_summary.severity_counts.error,
                    result_summary.severity_counts.warning,
                    result_summary.severity_counts.info,
                );
                // Demote — rm -rf _pending/, reset greens, requeue.
                if let Err(e) = storage::demote_pending(&root, &spec_id) {
                    warn!(
                        "post_sweep_pending: demote_pending({}) failed: {}",
                        spec_id, e
                    );
                    // Continue — PG-side demotion is still useful even if
                    // the FS cleanup fails (the row will re-run next sweep
                    // and the FS layer is idempotent).
                }
                let _ = pg_db
                    .update_proposal_status_with_error(&prop.id, "queued", &detail)
                    .await;
                let _ = pg_db.update_proposal_greens(&prop.id, 0).await;
                demoted += 1;
                // Plan 06: Demoted emit. `failing_assertion` is `None` only
                // when the red came from policy `Indeterminate` over an
                // empty-in-scope assertion set — emit empty-string locator
                // in that case so the wire shape stays uniform.
                let (failing_state_id, failing_assertion_id) =
                    failing_assertion.unwrap_or_default();
                events::emit(SpecApiEvent::FlywheelProposalDemoted {
                    app_id: app_id.clone(),
                    proposal_id: prop.id.clone(),
                    page_id: spec_id.clone(),
                    failing_assertion_id,
                    failing_state_id,
                    snapshot_id: red_snapshot_id,
                    at_ms: events::now_ms(),
                });
                info!(
                    "post_sweep_pending: demoted {} (spec_id={}): {}",
                    prop.id, spec_id, detail
                );
            }
            Err(other) => {
                // SnapshotFetchFailed et al. — infra error, no state change
                // beyond last_attempt_at.
                let _ = pg_db.update_proposal_attempt_only(&prop.id).await;
                infra_errors += 1;
                warn!(
                    "post_sweep_pending: infra error for {} (spec_id={}): {:?}",
                    prop.id, spec_id, other
                );
            }
        }
    }

    info!(
        "post_sweep_pending: swept={} promoted={} demoted={} infra_errors={}",
        total, promoted, demoted, infra_errors
    );

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "swept": total,
            "promoted": promoted,
            "demoted": demoted,
            "infraErrors": infra_errors,
        })),
    )
        .into_response()
}

// =============================================================================
// List endpoint — GET /spec/proposals
// =============================================================================

#[derive(Debug, Deserialize, Default)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Serialize)]
struct ProposalListItem {
    #[serde(rename = "proposalId")]
    proposal_id: String,
    kind: String,
    pathname: Option<String>,
    #[serde(rename = "specId")]
    spec_id: Option<String>,
    status: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "lastAttemptAt")]
    last_attempt_at: Option<String>,
    #[serde(rename = "consecutiveGreens")]
    consecutive_greens: i32,
    #[serde(rename = "lastError")]
    last_error: Option<String>,
}

impl From<SpecProposalRow> for ProposalListItem {
    fn from(r: SpecProposalRow) -> Self {
        Self {
            proposal_id: r.id,
            kind: r.kind,
            pathname: r.pathname,
            spec_id: r.spec_id,
            status: r.status,
            created_at: r.created_at,
            last_attempt_at: r.last_attempt_at,
            consecutive_greens: r.consecutive_greens,
            last_error: r.last_error,
        }
    }
}

pub async fn get_list(
    AxPath(app_id): AxPath<String>,
    State(state): State<Arc<ApiState>>,
    Query(q): Query<ListQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(50);
    let offset = q.offset.unwrap_or(0);
    let pg_db = state.app_state.pg_db.clone();
    // Resolve so unregistered apps return 404 before we query proposals.
    if let Err(e) = storage::resolve_specs_root(&pg_db, &app_id).await {
        return e.into_app_response();
    }

    match pg_db.list_proposals(limit, offset).await {
        Ok(rows) => {
            let items: Vec<ProposalListItem> = rows.into_iter().map(Into::into).collect();
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "proposals": items,
                })),
            )
                .into_response()
        }
        Err(e) => {
            warn!("get_list: list_proposals failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpecError::with_detail(
                    "list-proposals-failed",
                    json!({ "error": e }),
                )),
            )
                .into_response()
        }
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Generate a new proposal id of the form `prop_<UUIDv7>`.
///
/// Plan reference: `05-flywheel.md` § Step 6 ("ULID: there's no `ulid` crate.
/// Use `format!("prop_{{}}", uuid::Uuid::now_v7())`"). UUIDv7 carries a
/// millisecond timestamp prefix so the queue lists in roughly creation order
/// even without `ORDER BY created_at`.
fn mint_proposal_id() -> String {
    format!("prop_{}", uuid::Uuid::now_v7())
}

/// Convert an on-disk page id (slug form, e.g. `account-billing`) back to
/// its pathname (e.g. `/account/billing`). Inverse of `pathname_to_spec_id`
/// owned by the sibling `workflow_generation::spec_authoring` module.
///
/// v1.0 slug shape: pathname segments joined with `-`, lowercased, leading
/// slash dropped. Index pathnames (empty / `/`) map to `index`. Anything
/// outside that shape returns `None`.
fn spec_id_to_pathname(spec_id: &str) -> Option<String> {
    if spec_id.is_empty() {
        return None;
    }
    if spec_id == "index" {
        return Some("/".to_string());
    }
    // Reject anything that wouldn't round-trip cleanly through the slug
    // shape — e.g. uppercase, double-dashes (which would collapse on
    // re-slug), leading/trailing dashes.
    if spec_id.chars().any(|c| c.is_uppercase())
        || spec_id.contains("--")
        || spec_id.starts_with('-')
        || spec_id.ends_with('-')
    {
        return None;
    }
    Some(format!("/{}", spec_id.replace('-', "/")))
}

/// Map a `RegisteredElement` (from `commands::spec_drift::DriftReport
/// .missing_from_spec`) to a target `spec_id` within `app_id`'s spec
/// corpus, using `elem.file` as the pathname-routing input.
///
/// spec-multi-app Stream E.5: the result is scoped to a single app — the
/// returned slug is interpreted by callers as a page id within
/// `<app.repo_root>/specs/pages/`, never as a cross-app global id. The
/// `app_id` parameter is reserved for future routing rules (e.g. app-specific
/// source-tree layout overrides); v1.0 still uses the universal Next.js /
/// Vite prefixes regardless of which app we're scanning.
///
/// Heuristic v1.0: strip the conventional source-tree prefix
/// (`src/pages/...`, `src/app/...`, `app/...`), drop the file extension and
/// the `index` leaf, lowercase, and slugify by joining the remaining
/// segments with `-`. Files outside these prefixes return `None` (the
/// caller drops them — patch routing is best-effort).
///
/// Sibling-module symmetry: keeps the slug shape identical to the
/// `pathname_to_spec_id` helper owned by `workflow_generation::spec_authoring`.
/// If the sibling makes that helper `pub(crate)`, this should be deleted in
/// favor of calling it directly.
fn infer_target_spec_id(_app_id: &str, elem: &RegisteredElement) -> Option<String> {
    let file = elem.file.replace('\\', "/");

    // Strip the leading source-tree prefix; preserve order, longest first.
    let candidates = [
        "src/pages/",
        "src/app/",
        "pages/",
        "app/",
        "src/views/",
        "src/routes/",
    ];
    let mut tail: Option<&str> = None;
    for prefix in &candidates {
        if let Some(idx) = file.find(prefix) {
            tail = Some(&file[idx + prefix.len()..]);
            break;
        }
    }
    let tail = tail?;

    // Drop extension and trim.
    let no_ext = match tail.rsplit_once('.') {
        Some((stem, _)) => stem,
        None => tail,
    };

    let segments: Vec<String> = no_ext
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect();

    if segments.is_empty() {
        return None;
    }

    // Drop trailing `index` (e.g. `account/billing/index.tsx` →
    // `account/billing`). Index-at-root maps to "index".
    let trimmed: Vec<String> = if segments.last().map(|s| s.as_str()) == Some("index") {
        let n = segments.len();
        if n == 1 {
            return Some("index".to_string());
        }
        segments.into_iter().take(n - 1).collect()
    } else {
        segments
    };

    if trimmed.is_empty() {
        return None;
    }

    Some(trimmed.join("-"))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_proposal_id_shape() {
        let id = mint_proposal_id();
        assert!(id.starts_with("prop_"), "expected prop_ prefix: {}", id);
        // UUIDv7 string is 36 chars (8-4-4-4-12 plus 4 dashes); plus the
        // 5-char prefix.
        assert_eq!(id.len(), 5 + 36, "unexpected proposal id length: {}", id);
    }

    #[test]
    fn spec_id_to_pathname_simple() {
        assert_eq!(
            spec_id_to_pathname("account-billing").as_deref(),
            Some("/account/billing")
        );
        assert_eq!(spec_id_to_pathname("home").as_deref(), Some("/home"));
        assert_eq!(spec_id_to_pathname("index").as_deref(), Some("/"));
        assert_eq!(spec_id_to_pathname(""), None);
        // Reject non-roundtrippable shapes.
        assert_eq!(spec_id_to_pathname("Account-Billing"), None);
        assert_eq!(spec_id_to_pathname("-leading"), None);
        assert_eq!(spec_id_to_pathname("trailing-"), None);
        assert_eq!(spec_id_to_pathname("double--dash"), None);
    }

    #[test]
    fn infer_target_spec_id_strips_src_pages() {
        let elem = RegisteredElement {
            id: "btn-save".into(),
            label: None,
            file: "src/pages/account/billing.tsx".into(),
            line: 12,
            suggested_assertion: String::new(),
        };
        assert_eq!(
            infer_target_spec_id("qontinui-runner", &elem).as_deref(),
            Some("account-billing")
        );
    }

    #[test]
    fn infer_target_spec_id_strips_app_dir() {
        let elem = RegisteredElement {
            id: "btn-save".into(),
            label: None,
            file: "src/app/settings/general/page.tsx".into(),
            line: 12,
            suggested_assertion: String::new(),
        };
        // `page.tsx` is not the literal `index`, so it stays as a segment.
        assert_eq!(
            infer_target_spec_id("qontinui-runner", &elem).as_deref(),
            Some("settings-general-page")
        );
    }

    #[test]
    fn infer_target_spec_id_drops_index_leaf() {
        let elem = RegisteredElement {
            id: "btn-save".into(),
            label: None,
            file: "src/pages/account/billing/index.tsx".into(),
            line: 12,
            suggested_assertion: String::new(),
        };
        assert_eq!(
            infer_target_spec_id("qontinui-runner", &elem).as_deref(),
            Some("account-billing")
        );
    }

    #[test]
    fn infer_target_spec_id_root_index() {
        let elem = RegisteredElement {
            id: "btn-save".into(),
            label: None,
            file: "src/pages/index.tsx".into(),
            line: 12,
            suggested_assertion: String::new(),
        };
        assert_eq!(
            infer_target_spec_id("qontinui-runner", &elem).as_deref(),
            Some("index")
        );
    }

    #[test]
    fn infer_target_spec_id_outside_known_prefixes() {
        let elem = RegisteredElement {
            id: "btn-save".into(),
            label: None,
            file: "src-tauri/src/main.rs".into(),
            line: 12,
            suggested_assertion: String::new(),
        };
        assert_eq!(infer_target_spec_id("qontinui-runner", &elem), None);
    }

    #[test]
    fn infer_target_spec_id_tolerates_backslashes() {
        let elem = RegisteredElement {
            id: "btn-save".into(),
            label: None,
            file: "src\\pages\\account\\billing.tsx".into(),
            line: 12,
            suggested_assertion: String::new(),
        };
        assert_eq!(
            infer_target_spec_id("qontinui-runner", &elem).as_deref(),
            Some("account-billing")
        );
    }

    #[test]
    fn write_pending_ir_round_trips_through_proposals_path() {
        // Smoke-test that the canonical Step 9 staging helper writes a
        // candidate to `<root>/pages/<id>/_pending/state-machine.derived.json`
        // with the expected on-disk shape. The deeper provenance-stamping +
        // projection-sibling behavior is covered by storage::pending_tests.
        use crate::spec_api::types::IrPageSpec;
        let tmp = tempfile::tempdir().unwrap();
        let candidate = IrPageSpec {
            version: "1.0".into(),
            id: "demo-page".into(),
            name: "Demo Page".into(),
            description: None,
            metadata: None,
            provenance: None,
            states: Vec::new(),
            transitions: Vec::new(),
            synthesized_groups: None,
            initial_state: None,
        };
        let path = storage::write_pending_ir(tmp.path(), "qontinui-runner", &candidate).unwrap();
        let path_str = path.display().to_string();
        assert!(path_str.contains("_pending"));
        let target = tmp
            .path()
            .join("pages")
            .join("demo-page")
            .join("_pending")
            .join("state-machine.derived.json");
        assert!(
            target.exists(),
            "target file should exist: {}",
            target.display()
        );
        let body = std::fs::read_to_string(&target).unwrap();
        assert!(body.contains("\"id\""));
        assert!(body.contains("demo-page"));
        // Projection sibling is written too.
        let projection = tmp
            .path()
            .join("pages")
            .join("demo-page")
            .join("_pending")
            .join("spec.uibridge.json");
        assert!(
            projection.exists(),
            "projection sibling missing: {}",
            projection.display()
        );
    }

    #[test]
    fn scan_request_defaults() {
        let req: ScanRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(
            req.min_observations.unwrap_or(DEFAULT_MIN_OBSERVATIONS),
            DEFAULT_MIN_OBSERVATIONS
        );
        assert_eq!(
            req.lookback_days.unwrap_or(DEFAULT_LOOKBACK_DAYS),
            DEFAULT_LOOKBACK_DAYS
        );
        assert_eq!(req.limit.unwrap_or(DEFAULT_SCAN_LIMIT), DEFAULT_SCAN_LIMIT);
    }

    #[test]
    fn scan_request_parses_full_payload() {
        let req: ScanRequest =
            serde_json::from_str(r#"{ "minObservations": 10, "lookbackDays": 30, "limit": 5 }"#)
                .unwrap();
        assert_eq!(req.min_observations, Some(10));
        assert_eq!(req.lookback_days, Some(30));
        assert_eq!(req.limit, Some(5));
    }

    /// PG-bound tests are gated behind `pg_integration_tests` because the
    /// shared dev DB is the only place the `project.spec_proposals` table
    /// exists (Atlas-managed). Unit tests above cover the pure helpers
    /// (slug, JSON shapes, placeholder writer).
    ///
    /// To run: `cargo test --features spec-authoring,pg_integration_tests -- proposals::tests::pg`
    #[cfg(feature = "pg_integration_tests")]
    mod pg {
        use super::*;
        use crate::database::pg::PgDb;

        async fn fresh_db() -> std::sync::Arc<PgDb> {
            // The integration DB must already have spec_proposals (Atlas).
            let db = PgDb::new_blocking_for_test();
            // Wipe any prior test rows so dedup behaves deterministically.
            let conn = db.pool().get().await.expect("pg conn");
            conn.execute("TRUNCATE TABLE spec_proposals", &[])
                .await
                .expect("truncate spec_proposals");
            db
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn insert_dedupes_on_kind_and_target() {
            let db = fresh_db().await;
            let first = db
                .insert_proposal(
                    "prop_test_one",
                    "fullPage",
                    Some("/foo"),
                    None,
                    "queued",
                    &serde_json::json!({}),
                )
                .await
                .expect("first insert");
            assert!(first, "first insert should succeed");
            let second = db
                .insert_proposal(
                    "prop_test_two",
                    "fullPage",
                    Some("/foo"),
                    None,
                    "queued",
                    &serde_json::json!({}),
                )
                .await
                .expect("second insert");
            assert!(!second, "second insert should be deduped");
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn list_round_trips_inserted_rows() {
            let db = fresh_db().await;
            db.insert_proposal(
                "prop_test_list",
                "fullPage",
                Some("/bar"),
                None,
                "queued",
                &serde_json::json!({}),
            )
            .await
            .expect("insert");
            let rows = db.list_proposals(10, 0).await.expect("list");
            assert!(rows
                .iter()
                .any(|r| r.id == "prop_test_list" && r.status == "queued"));
        }
    }

    // -----------------------------------------------------------------
    // spec-multi-app Stream E.9 — multi-tenant proposal-sweep tests
    // -----------------------------------------------------------------
    //
    // Each test exercises the underlying primitives (`storage::resolve_specs_root`,
    // `pg_db.insert_proposal_event`, `storage::promote_pending`) directly
    // rather than the HTTP handler — the handler is a thin
    // `Path((app_id,)) -> resolve_specs_root -> ...` wrapper, so testing
    // the primitives transitively covers the route shape. Gated on
    // DATABASE_URL because they touch live PG.

    #[cfg(feature = "spec-authoring")]
    mod stream_e {
        use super::super::storage;
        use crate::database::pg::PgDb;
        use qontinui_types::apps::{AppError, RegisterAppRequest};

        async fn pg() -> PgDb {
            let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
                "postgres://qontinui_user:qontinui_password@localhost:5433/qontinui_db".to_string()
            });
            PgDb::new(&url).await.expect("PgDb for Stream E tests")
        }

        fn unique_id(prefix: &str) -> String {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("{}-{}", prefix, nanos)
        }

        async fn cleanup_app(pg: &PgDb, app_id: &str) {
            let _ = pg.delete_app(app_id).await;
        }

        /// Stream E.9 test #1 — `scan_for_unknown_app_returns_404`.
        /// The handler calls `storage::resolve_specs_root(&pg, &app_id)`
        /// first; on unknown app it returns `AppError::NotRegistered`,
        /// which the handler converts to a 404 via `AppErrorExt::into_app_response`.
        #[tokio::test(flavor = "multi_thread")]
        #[ignore = "requires PG via DATABASE_URL"]
        async fn scan_for_unknown_app_returns_404() {
            let pg = pg().await;
            let ghost = unique_id("e9-ghost");
            let err = storage::resolve_specs_root(&pg, &ghost)
                .await
                .expect_err("unknown app must error");
            match err {
                AppError::NotRegistered { app_id } => {
                    assert_eq!(app_id, ghost, "NotRegistered must echo the requested id");
                }
                other => panic!("expected NotRegistered; got {:?}", other),
            }
        }

        /// Stream E.9 test #2 — `scan_for_known_app_writes_proposal_events_with_app_id`.
        /// Insert an event under one app and confirm the round-trip carries
        /// the `app_id` correctly (via the per-app reader added by E.1).
        #[tokio::test(flavor = "multi_thread")]
        #[ignore = "requires PG via DATABASE_URL"]
        async fn scan_for_known_app_writes_proposal_events_with_app_id() {
            let pg = pg().await;
            let app_id = unique_id("e9-app-write");
            let tmp = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(tmp.path().join("specs")).unwrap();
            pg.insert_app(&RegisterAppRequest {
                app_id: app_id.clone(),
                repo_root: tmp.path().to_string_lossy().to_string(),
                ui_bridge_url: format!("http://localhost:0/{}", app_id),
                display_name: format!("Test {}", app_id),
                auth_required: false,
                red_threshold: 0.5,
                yellow_threshold: 0.8,
            })
            .await
            .expect("insert_app");

            // Insert a proposal so the event has something to reference.
            let proposal_id = unique_id("e9-prop");
            pg.insert_proposal(
                &proposal_id,
                "fullPage",
                Some("/e9-test"),
                None,
                "queued",
                &serde_json::json!({}),
            )
            .await
            .expect("insert_proposal");

            // Write an event under this app's id.
            let _event_id = pg
                .insert_proposal_event(&app_id, &proposal_id, "scanned", None, None)
                .await
                .expect("insert_proposal_event");

            // Read back — the per-app reader must return at least our row.
            let rows = pg
                .list_recent_proposal_events_for_app(&app_id, 50)
                .await
                .expect("list_recent_proposal_events_for_app");
            assert!(
                rows.iter().any(|r| r.proposal_id == proposal_id
                    && r.event_type == "scanned"
                    && r.app_id == app_id),
                "event must round-trip with the right app_id; rows={:?}",
                rows
            );

            cleanup_app(&pg, &app_id).await;
        }

        /// Stream E.9 test #3 — `promote_pending_under_app_root_succeeds`.
        /// Stage a candidate via `write_pending_ir` under the app's
        /// app-scoped specs root, then promote — the canonical IR + projection
        /// land under `<repo_root>/specs/pages/<id>/` and `_pending/` is gone.
        #[tokio::test(flavor = "multi_thread")]
        #[ignore = "requires PG via DATABASE_URL"]
        async fn promote_pending_under_app_root_succeeds() {
            let pg = pg().await;
            let app_id = unique_id("e9-app-promote");
            let tmp = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(tmp.path().join("specs")).unwrap();
            pg.insert_app(&RegisterAppRequest {
                app_id: app_id.clone(),
                repo_root: tmp.path().to_string_lossy().to_string(),
                ui_bridge_url: format!("http://localhost:0/{}", app_id),
                display_name: format!("Test {}", app_id),
                auth_required: false,
                red_threshold: 0.5,
                yellow_threshold: 0.8,
            })
            .await
            .expect("insert_app");

            let root = storage::resolve_specs_root(&pg, &app_id)
                .await
                .expect("resolve_specs_root for registered app");

            let candidate = crate::spec_api::types::IrPageSpec {
                version: "1.0".into(),
                id: "e9-promote-page".into(),
                name: "E9 Promote".into(),
                description: None,
                metadata: None,
                provenance: None,
                states: Vec::new(),
                transitions: Vec::new(),
                synthesized_groups: None,
                initial_state: None,
            };
            let staged =
                storage::write_pending_ir(&root, &app_id, &candidate).expect("write_pending_ir");
            assert!(
                staged.to_string_lossy().contains("_pending"),
                "staged path should land under _pending/: {}",
                staged.display()
            );

            storage::promote_pending(&root, &app_id, &candidate.id).expect("promote_pending");

            let canon_ir = root
                .join("pages")
                .join(&candidate.id)
                .join("state-machine.derived.json");
            assert!(
                canon_ir.exists(),
                "canonical IR missing: {}",
                canon_ir.display()
            );
            let pending_dir = root.join("pages").join(&candidate.id).join("_pending");
            assert!(
                !pending_dir.exists(),
                "_pending/ should be gone after promote: {}",
                pending_dir.display()
            );

            cleanup_app(&pg, &app_id).await;
        }
    }
}
