//! Spec proposals — Stream E (Flywheel) coverage-growth queue.
//!
//! Four per-app endpoints, mounted under `/apps/{app_id}/spec/proposals/...`
//! when the `spec-authoring` feature is enabled (it is in `default`), plus the
//! process-wide `GET /spec/proposals/metrics`. Every row belongs to the app
//! whose scan queued it (`spec_proposals.app_id`), and each endpoint reads and
//! writes only its own app's rows:
//!
//! - `POST .../scan` — discovers eligible `fullPage` candidates
//!   (un-spec'd pathnames with ≥ N of THIS app's observations in the lookback
//!   window) and `patch` candidates (drift-flagged specs), inserts them into
//!   `atlas_managed.spec_proposals` with `status = 'queued'`, and first puts
//!   back in the queue any proposal whose execute died holding it
//!   (`requeuedStale`), stopping the task run that execute recorded
//!   (`stoppedStaleRuns`).
//! - `POST .../{id}/execute` — drives one queued proposal through
//!   `spec_authoring::author_candidate` then `validator::validate_candidate`
//!   (Step 8 — distinctness → round-trip → coverage → b-green) and lands
//!   the candidate in `_pending/` via `storage::write_pending_ir` (Step 9).
//!   Authoring runs an AI meta-workflow on the user's account, so only a
//!   `queued` proposal is executed (`queued → inFlight` is one
//!   compare-and-set that writes a claim token; anything else answers 409
//!   `proposal-not-queued`), and the run is authorized first: when coord's
//!   device drain defers it, or the agent registry refuses it, nothing is
//!   spawned, the proposal goes back to `queued` and the answer is
//!   `outcome: "skipped"`. Every write the execute makes to the row matches
//!   its claim token. An execute whose caller goes away before its outcome is
//!   written also puts the proposal back in the queue; one that died with its
//!   runner is re-queued by the next scan once its claim has outlived the
//!   claimant's own bound, and the task run it recorded is stopped. On
//!   validator rejection
//!   the structured `ValidatorError` reason + detail is persisted to
//!   `spec_proposals.last_error` and returned as `outcome:
//!   "validator-rejected"`. Authoring failures stay distinct and surface as
//!   `outcome: "authoring-failed"`.
//! - `POST .../sweep-pending` — Step 9's 2-green sweep. Re-runs
//!   gate-3 (B-green) against every `pendingPromotion` row; on Ok bumps
//!   `consecutive_greens` and promotes at ≥ 2; on `BExecutionRed` demotes
//!   immediately back to `queued`; on infra error (snapshot fetch failed
//!   etc.) only `last_attempt_at` moves.
//! - `GET /apps/{app_id}/spec/proposals` — paginated list of the app's rows.
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
use crate::coord_drain_state::SpawnOrigin;
use crate::database::pg::spec_proposals::SpecProposalRow;
use crate::database::pg::PgDb;
use crate::mcp::types::ApiState;
use crate::spec_api::apps::AppErrorExt;
use crate::spec_api::events;
use crate::spec_api::responses::SpecError;
// The single spelling of the index page's spec id. Owned by `spec_api::slug`
// alongside both directions of the pathname ↔ spec-id mapping: the copy that
// lived here recognised only `index` for the root page while the slugger emits
// `root`, so the two ids shadowed each other on disk.
use crate::spec_api::slug::INDEX_SPEC_ID;
use crate::spec_api::storage;
use qontinui_types::spec_api_events::SpecApiEvent;

// =============================================================================
// Flywheel Metrics — observability for the spec proposal lifecycle
// =============================================================================

/// Global counters for flywheel metrics. Uses atomic increments for thread-safe
/// observation across the runtime lifecycle.
use std::sync::atomic::{AtomicU64, Ordering};

pub static FLYWHEEL_PROPOSALS_SCANNED_TOTAL: AtomicU64 = AtomicU64::new(0);
pub static FLYWHEEL_PROPOSALS_EXECUTED_TOTAL: AtomicU64 = AtomicU64::new(0);
pub static FLYWHEEL_CONSECUTIVE_GREENS_MAX: AtomicU64 = AtomicU64::new(0);

/// Increment the total proposals scanned.
#[inline]
fn metric_scanned_inc() {
    FLYWHEEL_PROPOSALS_SCANNED_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// Increment the total proposals executed with the given outcome.
#[inline]
fn metric_executed_inc(_outcome: &str) {
    FLYWHEEL_PROPOSALS_EXECUTED_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// Update the max consecutive greens if the new value is higher.
#[inline]
fn metric_consecutive_greens_max(value: i32) {
    if value > 0 {
        let val_u64 = value as u64;
        let mut current = FLYWHEEL_CONSECUTIVE_GREENS_MAX.load(Ordering::Relaxed);
        while val_u64 > current {
            match FLYWHEEL_CONSECUTIVE_GREENS_MAX.compare_exchange(
                current,
                val_u64,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }
}

// =============================================================================
// Scan endpoint — POST /apps/{app_id}/spec/proposals/scan
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

    // ---- Executes that died -----------------------------------------------
    // An execute that died with its runner leaves its proposal `inFlight`,
    // and nothing else would ever pick that row up: the dedup index keeps the
    // target from being queued again. A claim records its claimant's own
    // bound on how long it may hold the row (`stale_in_flight_after` of THAT
    // process's execute timeout), so a row held longer than that goes back in
    // the queue here, at the queue's intake, and the task run its execute
    // recorded is stopped. This process's bound applies only to a row whose
    // claim recorded none. `None` when the re-queue failed: how many it would
    // have taken is unknown, not zero.
    let fallback_stale_after = stale_in_flight_after(
        crate::workflow_generation::meta_workflow_run::configured_execute_timeout(),
    );
    let stale = match pg_db
        .requeue_stale_in_flight_proposals(
            &app_id,
            fallback_stale_after,
            STALE_EXECUTE_ERROR,
            STALE_RUN_STOP_REASON,
        )
        .await
    {
        Ok(stale) => Some(stale),
        Err(e) => {
            warn!("post_scan: requeue_stale_in_flight_proposals failed: {}", e);
            None
        }
    };
    let requeued_stale = stale.map(|s| s.requeued);
    let stopped_stale_runs = stale.map(|s| s.runs_stopped);

    // ---- Full-page branch -------------------------------------------------
    let full_page_rows =
        match query_eligible_pathnames(&pg_db, &app_id, lookback, min_obs, limit).await {
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

    // Compare page labels to spec ids directly. Both sides are now the same
    // slug space (`storage::list_pages` returns spec ids; the scan groups by
    // the `spec_id` label capture writes), so no pathname round-trip is
    // needed — and none is possible: a label like `dag-workflow-editor` maps
    // to the pathname `/dag/workflow/editor`, which exists nowhere.
    let on_disk_labels: HashSet<String> = storage::list_pages(&root, &app_id)
        .unwrap_or_default()
        .into_iter()
        .collect();

    let mut queued: Vec<QueuedProposalSummary> = Vec::new();
    let scanned_pathnames = full_page_rows.len();
    for pathname in full_page_rows {
        if on_disk_labels.contains(&pathname) {
            debug!("post_scan: page {} already has a spec; skipping", pathname);
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
                &app_id,
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
                        &app_id,
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

    // Increment metrics for scanned proposals
    for _ in &queued {
        metric_scanned_inc();
    }

    info!(
        "post_scan: scanned_pathnames={} scanned_drift_reports={} queued={} requeued_stale={:?} stopped_stale_runs={:?}",
        scanned_pathnames,
        scanned_drift_reports,
        queued.len(),
        requeued_stale,
        stopped_stale_runs
    );

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "scannedPathnames": scanned_pathnames,
            "scannedDriftReports": scanned_drift_reports,
            "queuedProposals": queued,
            "requeuedStale": requeued_stale,
            "stoppedStaleRuns": stopped_stale_runs,
            "reason": reason,
        })),
    )
        .into_response()
}

/// Whether `co_occurrence_observations` has the `app_id` column capture writes
/// (qontinui-web migration `appid_01_co_occurrence_app_id`). Asked per scan, a
/// catalog read: a scan is rare, and an answer cached per process would pin
/// whichever database a test process reached first.
const OBSERVATION_APP_ID_COLUMN_SQL: &str = r#"SELECT EXISTS (
    SELECT 1 FROM pg_attribute
    WHERE attrelid = to_regclass('co_occurrence_observations')
      AND attname = 'app_id'
      AND attnum > 0
      AND NOT attisdropped)"#;

/// The page labels `app_id`'s scan may queue: labels its own observations
/// carry at least `$2` times in the lookback window.
///
/// An observation belongs to the app capture attributed it to. One capture
/// could not attribute (`app_id` NULL, an observation older than attribution
/// or one whose app was not registered) belongs to the bootstrap app,
/// [`storage::RUNNER_APP_ID`], as a proposal from before `spec_proposals`
/// carried an app is filed under it (`PgDb::verify_and_provision`). `$5` says
/// whether the scanning app is that one.
const ELIGIBLE_PAGE_LABELS_SQL: &str = r#"
            SELECT
                spec_id AS page_label,
                COUNT(*) AS obs
            FROM co_occurrence_observations
            WHERE captured_at >= now() - ($1::int || ' days')::interval
              AND invalidated_at IS NULL
              AND spec_id IS NOT NULL
              AND (app_id = $4 OR (app_id IS NULL AND $5::bool))
            GROUP BY spec_id
            HAVING COUNT(*) >= $2
            ORDER BY obs DESC
            LIMIT $3
            "#;

/// [`ELIGIBLE_PAGE_LABELS_SQL`] for a database whose observations carry no
/// `app_id` column. Every observation there is unattributed, so it is the
/// bootstrap app's alone, and only that app's scan runs this.
const ELIGIBLE_PAGE_LABELS_UNATTRIBUTED_SQL: &str = r#"
            SELECT
                spec_id AS page_label,
                COUNT(*) AS obs
            FROM co_occurrence_observations
            WHERE captured_at >= now() - ($1::int || ' days')::interval
              AND invalidated_at IS NULL
              AND spec_id IS NOT NULL
            GROUP BY spec_id
            HAVING COUNT(*) >= $2
            ORDER BY obs DESC
            LIMIT $3
            "#;

/// Find pages with enough observations to be worth authoring a spec for.
/// Returns page labels sorted by observation count desc, capped at `limit`.
///
/// Groups by the `spec_id` label `state_discovery::capture` writes, NOT by
/// `snapshot_metadata->>'pathname'` as `05-flywheel.md` § Step 7 originally
/// specified. The pathname is structurally `/` for every view of a desktop
/// SPA, so grouping by it yielded exactly one candidate whose label resolved
/// to `root` — a page capture never labels. Producer and consumer disagreed
/// on the key, and every proposal died with no active states. Grouping by the
/// same column the selection side reads keeps the two ends in one namespace.
///
/// `app_id`'s scan reads only `app_id`'s observations
/// ([`ELIGIBLE_PAGE_LABELS_SQL`]). Every queued proposal starts a paid
/// authoring run, and the dedup index is per app, so reading every app's
/// observations would queue each label once per registered app.
pub(crate) async fn query_eligible_pathnames(
    pg_db: &crate::database::pg::PgDb,
    app_id: &str,
    lookback_days: i32,
    min_observations: i32,
    limit: i64,
) -> Result<Vec<String>, String> {
    let conn = pg_db
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;
    let bootstrap_app = app_id == storage::RUNNER_APP_ID;
    let attributed: bool = conn
        .query_one(OBSERVATION_APP_ID_COLUMN_SQL, &[])
        .await
        .and_then(|row| row.try_get(0))
        .map_err(|e| format!("PG query_eligible_pathnames (app_id column probe): {}", e))?;
    let min_observations = min_observations as i64;
    let rows = if attributed {
        conn.query(
            ELIGIBLE_PAGE_LABELS_SQL,
            &[
                &lookback_days,
                &min_observations,
                &limit,
                &app_id,
                &bootstrap_app,
            ],
        )
        .await
    } else if bootstrap_app {
        conn.query(
            ELIGIBLE_PAGE_LABELS_UNATTRIBUTED_SQL,
            &[&lookback_days, &min_observations, &limit],
        )
        .await
    } else {
        return Ok(Vec::new());
    }
    .map_err(|e| format!("PG query_eligible_pathnames: {}", e))?;

    rows.iter()
        .map(|r| r.try_get::<usize, String>(0))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("PG query_eligible_pathnames (page_label): {}", e))
}

// =============================================================================
// Execute endpoint — POST /apps/{app_id}/spec/proposals/{id}/execute
// =============================================================================

/// `POST /apps/{app_id}/spec/proposals/{id}/execute`. An HTTP caller — a
/// script, an agent, a UI-Bridge-driven call — is autonomous (`unknown`) under
/// coord's device drain, as at every other spawn door.
pub async fn post_execute(
    AxPath((app_id, id)): AxPath<(String, String)>,
    State(state): State<Arc<ApiState>>,
) -> Response {
    execute_proposal(state, app_id, id, SpawnOrigin::Unknown).await
}

/// Execute `app_id`'s proposal `id` on behalf of `origin`. The AI meta-workflow
/// that authoring runs is spawned under `origin`: the device drain and the
/// agent registry judge it there, and a spawn they do not admit leaves the
/// proposal `queued` (see [`execute_skip`]).
pub async fn execute_proposal(
    state: Arc<ApiState>,
    app_id: String,
    id: String,
    origin: SpawnOrigin,
) -> Response {
    let pg_db = state.app_state.pg_db.clone();
    let app_state = state.app_state.clone();
    let root = match storage::resolve_specs_root(&pg_db, &app_id).await {
        Ok(p) => p,
        Err(e) => return e.into_app_response(),
    };

    // Look up the proposal.
    let row = match pg_db.find_proposal_by_id(&app_id, &id).await {
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

    // Claim it: `queued → inFlight` as one compare-and-set, which writes a
    // claim token and this process's bound on how long it may hold the row.
    // Authoring starts a paid AI run, so a retry, a concurrent execute, or an
    // execute of a proposal already past the queue must not start another.
    let stale_after = stale_in_flight_after(
        crate::workflow_generation::meta_workflow_run::configured_execute_timeout(),
    );
    let token = match pg_db
        .claim_proposal_for_execute(&app_id, &row.id, stale_after)
        .await
    {
        Ok(Some(token)) => token,
        Ok(None) => {
            return (
                StatusCode::CONFLICT,
                Json(SpecError::with_detail(
                    "proposal-not-queued",
                    json!({ "proposalId": row.id }),
                )),
            )
                .into_response();
        }
        Err(e) => {
            warn!("post_execute: claim_proposal_for_execute failed: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpecError::with_detail(
                    "proposal-claim-failed",
                    json!({ "proposalId": row.id, "error": e }),
                )),
            )
                .into_response();
        }
    };
    let claim = HeldClaim {
        pg_db: pg_db.clone(),
        proposal_id: row.id.clone(),
        token,
    };

    // Every return of `execute_claimed` writes the proposal's next status. If
    // this future is dropped first (the HTTP client hung up during the run),
    // the guard puts the proposal back in the queue rather than leave it
    // `inFlight`, where nothing would pick it up.
    let requeue = RequeueOnDrop::armed(
        pg_db.clone(),
        claim.proposal_id.clone(),
        claim.token.clone(),
    );
    let response = execute_claimed(pg_db, app_state, root, app_id, row, origin, &claim).await;
    requeue.disarm();
    response
}

/// The claim an execute holds on its proposal
/// (`PgDb::claim_proposal_for_execute`). Every write the execute makes to the
/// row goes through it, and lands only while the claim is still held: once a
/// scan has taken the row back as stale, and another execute may hold it, this
/// execute's writes change nothing.
struct HeldClaim {
    pg_db: Arc<PgDb>,
    proposal_id: String,
    token: String,
}

impl HeldClaim {
    /// The claim as the executor records its run on it.
    fn run_claim(&self) -> crate::workflow_generation::spec_authoring::RunClaim {
        crate::workflow_generation::spec_authoring::RunClaim {
            proposal_id: self.proposal_id.clone(),
            token: self.token.clone(),
        }
    }

    /// Settle the proposal `failed` with `last_error`.
    async fn fail(&self, last_error: &str) {
        let written = self
            .pg_db
            .fail_claimed_proposal(&self.proposal_id, &self.token, last_error)
            .await;
        report_settle(&self.proposal_id, "failed", written);
    }

    /// Settle the proposal `pendingPromotion` with its authored candidate.
    /// `Ok(false)` means the claim is no longer held, and nothing was written:
    /// the execute's caller is told, because its outcome was not recorded.
    async fn stage(&self, candidate_ir: &serde_json::Value) -> Result<bool, String> {
        self.pg_db
            .stage_claimed_proposal(&self.proposal_id, &self.token, candidate_ir)
            .await
    }

    /// Put the proposal back in the queue, keeping its `last_error`.
    async fn requeue(&self) {
        let written = self
            .pg_db
            .requeue_claimed_proposal(&self.proposal_id, &self.token, None)
            .await;
        report_settle(&self.proposal_id, "queued", written);
    }
}

/// Log a claimed write that did not land.
fn report_settle(proposal_id: &str, status: &str, written: Result<bool, String>) {
    match written {
        Ok(true) => {}
        Ok(false) => warn!(
            "post_execute: proposal {} was not set {}: its claim is no longer held \
             (a scan took it back as stale)",
            proposal_id, status
        ),
        Err(e) => warn!(
            "post_execute: setting proposal {} {} failed: {}",
            proposal_id, status, e
        ),
    }
}

/// The execute after its claim: author the candidate, validate it, and stage
/// it. Every return writes the proposal's next status through `claim`.
async fn execute_claimed(
    pg_db: Arc<PgDb>,
    app_state: Arc<crate::commands::AppState>,
    root: std::path::PathBuf,
    app_id: String,
    row: SpecProposalRow,
    origin: SpawnOrigin,
    claim: &HeldClaim,
) -> Response {
    // Build the authoring mode from the row shape.
    let mode = match row.kind.as_str() {
        "fullPage" => {
            let pathname = match row.pathname.clone() {
                Some(p) => p,
                None => {
                    let msg = "fullPage proposal missing pathname".to_string();
                    claim.fail(&msg).await;
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
                    claim.fail(&msg).await;
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
                        claim.fail(&e).await;
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
            claim.fail(&msg).await;
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
        origin,
        Some(claim.run_claim()),
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            if let Some((status, reason, code)) = execute_skip(&e) {
                // Nothing was spawned: the drain deferred the run or the
                // registry refused it. The proposal is not failed; it goes
                // back to the queue for a later execute.
                claim.requeue().await;
                info!(
                    "post_execute: proposal {} skipped ({}): {}",
                    row.id,
                    reason,
                    authoring_error_detail(&e)
                );
                return (
                    status,
                    Json(json!({
                        "ok": false,
                        "proposalId": row.id,
                        "outcome": "skipped",
                        "reason": reason,
                        "code": code,
                        "detail": authoring_error_detail(&e),
                    })),
                )
                    .into_response();
            }
            let reason = authoring_error_reason(&e);
            let detail = authoring_error_detail(&e);
            claim.fail(&detail).await;
            metric_executed_inc("authoring-failed");
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
        claim.fail(&detail).await;
        metric_executed_inc("validator-rejected");
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
                claim.fail(&e).await;
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
                claim.fail(&e).await;
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
            claim.fail(&msg).await;
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
    match claim.stage(&candidate_json).await {
        Ok(true) => {}
        Ok(false) => {
            // A scan took the proposal back as stale while this execute was
            // still working: the row is the next execute's now, so this
            // outcome is not recorded, and is not counted.
            warn!(
                "post_execute: proposal {} lost its claim before its candidate was recorded",
                row.id
            );
            return (
                StatusCode::OK,
                Json(json!({
                    "ok": false,
                    "proposalId": row.id,
                    "outcome": "claim-lost",
                    "stagedPath": staged_path,
                    "detail": "the proposal's claim was taken back as stale before its \
                               candidate was recorded; the staged file is not its outcome",
                })),
            )
                .into_response();
        }
        Err(e) => {
            warn!(
                "post_execute: recording proposal {}'s candidate failed: {}",
                row.id, e
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpecError::with_detail(
                    "proposal-outcome-not-recorded",
                    json!({ "proposalId": row.id, "stagedPath": staged_path, "error": e }),
                )),
            )
                .into_response();
        }
    }

    metric_executed_inc("promoted");

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

/// `last_error` for a proposal whose execute went away before writing its
/// outcome.
pub(crate) const ABANDONED_EXECUTE_ERROR: &str =
    "abandoned: the execute's caller went away before its outcome was written";

/// `last_error` for a proposal whose claim outlived its claimant's own bound.
pub(crate) const STALE_EXECUTE_ERROR: &str =
    "abandoned: inFlight longer than its execute's own bound (its runner likely died)";

/// The `stopped` reason written on the task run a stale claim recorded.
pub(crate) const STALE_RUN_STOP_REASON: &str =
    "spec execute: its proposal claim went stale, nobody waiting";

/// How much longer than its run's deadline an execute may take: authorizing,
/// starting the run, then validating and staging its candidate.
const STALE_IN_FLIGHT_MARGIN: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// PURE: how long an execute may hold its proposal `inFlight` before a scan
/// takes it for an execute that died: the run deadline plus
/// [`STALE_IN_FLIGHT_MARGIN`]. The claim records it, so it is the claimant's
/// own bound that decides, whichever process scans.
pub(crate) fn stale_in_flight_after(execute_timeout: std::time::Duration) -> std::time::Duration {
    execute_timeout + STALE_IN_FLIGHT_MARGIN
}

/// Where an abandoned execute's proposal goes back to the queue: [`PgDb`] in
/// production, a recording queue in tests.
#[async_trait::async_trait]
pub(crate) trait ProposalRequeue: Send + Sync {
    /// Re-queue `id` with `last_error` while the claim with `token` is still
    /// held.
    async fn requeue_claimed(
        &self,
        id: &str,
        token: &str,
        last_error: &str,
    ) -> Result<bool, String>;
}

#[async_trait::async_trait]
impl ProposalRequeue for PgDb {
    async fn requeue_claimed(
        &self,
        id: &str,
        token: &str,
        last_error: &str,
    ) -> Result<bool, String> {
        self.requeue_claimed_proposal(id, token, Some(last_error))
            .await
    }
}

/// Puts a claimed proposal back in the queue if the execute holding it is
/// dropped before it writes the proposal's outcome. Disarmed once it has.
struct RequeueOnDrop {
    queue: Arc<dyn ProposalRequeue>,
    proposal_id: String,
    token: String,
    armed: bool,
}

impl RequeueOnDrop {
    fn armed(queue: Arc<dyn ProposalRequeue>, proposal_id: String, token: String) -> Self {
        Self {
            queue,
            proposal_id,
            token,
            armed: true,
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for RequeueOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let proposal_id = std::mem::take(&mut self.proposal_id);
        let token = std::mem::take(&mut self.token);
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            warn!(
                "post_execute: proposal {} abandoned outside a tokio runtime; it stays inFlight \
                 until a scan finds it stale",
                proposal_id
            );
            return;
        };
        let queue = self.queue.clone();
        handle.spawn(async move {
            match queue
                .requeue_claimed(&proposal_id, &token, ABANDONED_EXECUTE_ERROR)
                .await
            {
                Ok(true) => info!(
                    "post_execute: proposal {} abandoned; re-queued",
                    proposal_id
                ),
                Ok(false) => info!(
                    "post_execute: proposal {} abandoned after its claim was released; left \
                     as it was",
                    proposal_id
                ),
                Err(e) => warn!(
                    "post_execute: proposal {} abandoned; re-queueing it failed (a scan will, \
                     once it is stale): {}",
                    proposal_id, e
                ),
            }
        });
    }
}

fn authoring_error_reason(
    e: &crate::workflow_generation::spec_authoring::AuthoringError,
) -> &'static str {
    use crate::workflow_generation::spec_authoring::AuthoringError::*;
    match e {
        NoDiscoveryArtifact { .. } => "no-discovery-artifact",
        EmptyArtifact => "empty-artifact",
        NoPageObservations { .. } => "no-page-observations",
        PageObservationsUnattributed { .. } => "page-observations-unattributed",
        NoActiveStates { .. } => "no-active-states",
        ObservedButUnclustered { .. } => "observed-but-unclustered",
        MalformedAiOutput(_) => "malformed-ai-output",
        AiDispatchFailed(_) => "ai-dispatch-failed",
        SpawnDeferred { .. } => "spawn-deferred",
        SpawnNotAuthorized(_) => "spawn-not-authorized",
        ExistingSpecMissing(_) => "existing-spec-missing",
        Database(_) => "database-error",
    }
}

/// PURE: an execute that was never admitted — `(HTTP status, outcome reason,
/// drain code)` — or `None` for every real authoring outcome. The device
/// drain's deferral answers 409 with coord's drain code, as every spawn door
/// does under a drain; a registry refusal answers 403. The reasons are the
/// flywheel's `skipped:` vocabulary (`drained`, `not-authorized`).
fn execute_skip(
    e: &crate::workflow_generation::spec_authoring::AuthoringError,
) -> Option<(StatusCode, &'static str, Option<&'static str>)> {
    use crate::workflow_generation::spec_authoring::AuthoringError::*;
    match e {
        SpawnDeferred { code, .. } => Some((StatusCode::CONFLICT, "drained", Some(*code))),
        SpawnNotAuthorized(_) => Some((StatusCode::FORBIDDEN, "not-authorized", None)),
        _ => None,
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
        NoPageObservations { page_label } => format!(
            "no observations labelled for page '{}' — the page has never been \
             visited, or capture is not labelling it",
            page_label
        ),
        PageObservationsUnattributed { page_label, app_id } => format!(
            "page '{}' HAS labelled observations, but none of them carries app_id='{}' — \
             visiting the page will not help. Either the observation app_id backfill has \
             not been applied on this database (every historical row still reads NULL), or \
             the capture producer that stamps app_id is not deployed yet. Apply the \
             backfill / deploy the producer; only if both are already true does this page \
             genuinely belong to a different app",
            page_label, app_id
        ),
        NoActiveStates {
            page_label,
            total_states,
        } => format!(
            "page '{}' has been observed since the newest discovery artifact was derived, \
             and none of its observations is in that artifact's render-set, so none of its \
             {} states can be shown active here — re-derive to pick the page's newer visits up",
            page_label, total_states
        ),
        ObservedButUnclustered {
            page_label,
            total_states,
        } => format!(
            "page '{}' has observations, all of them older than the newest discovery \
             artifact, and that derivation clustered none of them into its {} states — \
             re-deriving will not change this; the observations most likely carry no \
             fingerprints, so they can never enter an artifact",
            page_label, total_states
        ),
        MalformedAiOutput(s) => format!("malformed AI output: {}", s),
        AiDispatchFailed(s) => format!("AI dispatch failed: {}", s),
        SpawnDeferred { code, reason } => format!(
            "the meta-workflow spawn was deferred by coord's device drain ({}): {}",
            code, reason
        ),
        SpawnNotAuthorized(s) => format!(
            "the agent registry did not authorize the meta-workflow spawn: {}",
            s
        ),
        ExistingSpecMissing(s) => format!("existing spec missing: {}", s),
        Database(s) => format!("database error: {}", s),
    }
}

// =============================================================================
// Sweep-pending endpoint — POST /apps/{app_id}/spec/proposals/sweep-pending
// =============================================================================
//
// Drives the 2-green promotion (and single-red demotion) of every one of the
// app's proposals in `status = 'pendingPromotion'` — only that app's rows,
// against that app's specs root and page locks. Nothing schedules it today:
// qontinui-supervisor#297 deleted the supervisor's nightly cron, and a
// runner-side schedule is owed by plan
// `2026-11-09-the-spec-flywheel-authors-a-spec-candidate-before-the-runner-schedules-it`,
// Phase 4. Usable as a manual call. Per Step 9 of `05-flywheel.md`:
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
        .list_proposals_by_status(&app_id, "pendingPromotion", SWEEP_BATCH_LIMIT)
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
                metric_consecutive_greens_max(new_greens);
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
// List endpoint — GET /apps/{app_id}/spec/proposals
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

    match pg_db.list_proposals(&app_id, limit, offset).await {
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
// Metrics endpoint — GET /spec/proposals/metrics
// =============================================================================

/// Returns a JSON object with current flywheel metrics. Useful for observability
/// and testing.
pub async fn get_metrics() -> Response {
    let scanned = FLYWHEEL_PROPOSALS_SCANNED_TOTAL.load(Ordering::Relaxed);
    let executed = FLYWHEEL_PROPOSALS_EXECUTED_TOTAL.load(Ordering::Relaxed);
    let max_greens = FLYWHEEL_CONSECUTIVE_GREENS_MAX.load(Ordering::Relaxed);

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "metrics": {
                "flywheel_proposals_scanned_total": scanned,
                "flywheel_proposals_executed_total": executed,
                "flywheel_consecutive_greens_max": max_greens,
            }
        })),
    )
        .into_response()
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
/// segments with `-`. A bare `index` at the root becomes
/// [`INDEX_SPEC_ID`] (`root`), matching what
/// [`crate::spec_api::slug::pathname_to_spec_id`] emits for `/`. Files
/// outside these prefixes return `None` (the caller drops them — patch
/// routing is best-effort).
///
/// Sibling-module symmetry: keeps the slug shape identical to the
/// `pathname_to_spec_id` helper owned by `workflow_generation::spec_authoring`.
/// If the sibling makes that helper `pub(crate)`, this should be deleted in
/// favor of calling it directly.
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
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
    // `account/billing`). Index-at-root maps to `INDEX_SPEC_ID` (`root`) —
    // the one spelling `spec_api::slug` recognises for the `/` pathname.
    let trimmed: Vec<String> = if segments.last().map(|s| s.as_str()) == Some("index") {
        let n = segments.len();
        if n == 1 {
            return Some(INDEX_SPEC_ID.to_string());
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

    /// An execute whose spawn was never admitted is `skipped` (the proposal is
    /// re-queued, nothing was spawned), with the status every spawn door uses
    /// for the same refusal. Every real authoring outcome is not a skip.
    #[test]
    fn a_spawn_that_was_never_admitted_is_skipped_not_failed() {
        use crate::workflow_generation::spec_authoring::AuthoringError;
        for code in ["device_drained", "drain_unreadable"] {
            assert_eq!(
                execute_skip(&AuthoringError::SpawnDeferred {
                    code,
                    reason: "drained".into(),
                }),
                Some((StatusCode::CONFLICT, "drained", Some(code)))
            );
        }
        assert_eq!(
            execute_skip(&AuthoringError::SpawnNotAuthorized("deny: off".into())),
            Some((StatusCode::FORBIDDEN, "not-authorized", None))
        );
        for authored in [
            AuthoringError::AiDispatchFailed("timed out".into()),
            AuthoringError::MalformedAiOutput("not an IR".into()),
            AuthoringError::EmptyArtifact,
            AuthoringError::Database("down".into()),
        ] {
            assert_eq!(execute_skip(&authored), None, "{authored}");
        }
    }

    /// The scan takes a proposal for a dead execute's only after any execute
    /// would have finished: the run's whole deadline, plus a margin for the
    /// work around the run.
    #[test]
    fn a_proposal_is_stale_only_after_any_execute_would_have_finished() {
        let hour = std::time::Duration::from_secs(3600);
        assert_eq!(
            stale_in_flight_after(hour),
            hour + std::time::Duration::from_secs(15 * 60)
        );
        assert!(stale_in_flight_after(std::time::Duration::ZERO) > std::time::Duration::ZERO);
    }

    /// Records every re-queue it is asked for.
    #[derive(Default)]
    struct RecordingQueue {
        requeued: std::sync::Mutex<Vec<(String, String, String)>>,
    }

    #[async_trait::async_trait]
    impl ProposalRequeue for RecordingQueue {
        async fn requeue_claimed(
            &self,
            id: &str,
            token: &str,
            last_error: &str,
        ) -> Result<bool, String> {
            self.requeued.lock().unwrap().push((
                id.to_string(),
                token.to_string(),
                last_error.to_string(),
            ));
            Ok(true)
        }
    }

    /// An execute dropped while it holds its claim puts the proposal back in
    /// the queue under that claim's token; one that finished (and disarmed the
    /// guard) does not.
    #[tokio::test]
    async fn an_execute_dropped_before_its_outcome_requeues_its_proposal() {
        let queue = Arc::new(RecordingQueue::default());
        drop(RequeueOnDrop::armed(
            queue.clone(),
            "prop_dropped".into(),
            "claim-1".into(),
        ));
        RequeueOnDrop::armed(queue.clone(), "prop_finished".into(), "claim-2".into()).disarm();
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            *queue.requeued.lock().unwrap(),
            vec![(
                "prop_dropped".to_string(),
                "claim-1".to_string(),
                ABANDONED_EXECUTE_ERROR.to_string()
            )]
        );
    }

    /// The body of the fn whose signature starts with `name`, read from this
    /// file's own source: the execute handlers take an `ApiState`, which holds
    /// a `tauri::AppHandle` no test can build, so their wiring is pinned by
    /// reading it.
    fn fn_body(name: &str) -> &'static str {
        let src: &'static str = include_str!("proposals.rs");
        let (_, rest) = src
            .split_once(name)
            .unwrap_or_else(|| panic!("{name} is defined"));
        rest.split_once("\n}\n").expect("the fn body ends").0
    }

    fn find_in(body: &str, needle: &str, name: &str) -> usize {
        body.find(needle)
            .unwrap_or_else(|| panic!("`{needle}` is missing from {name}"))
    }

    /// The execute claims before it can spawn, arms the guard on the claim,
    /// and disarms it only once the claimed work has returned. The PG tests
    /// prove each step; this proves the order, which they cannot see.
    #[test]
    fn the_execute_claims_then_guards_then_authors() {
        let execute = fn_body("pub async fn execute_proposal(");
        let at = |needle: &str| find_in(execute, needle, "execute_proposal");
        let claim = at("claim_proposal_for_execute(");
        let arm = at("RequeueOnDrop::armed(");
        let work = at("execute_claimed(");
        let disarm = at("requeue.disarm()");
        assert!(
            claim < arm && arm < work && work < disarm,
            "order must be claim -> arm -> execute_claimed -> disarm \
             (found {claim}, {arm}, {work}, {disarm})"
        );
        assert!(
            !execute.contains("author_candidate("),
            "authoring happens only in execute_claimed, after the claim"
        );
        assert!(fn_body("async fn execute_claimed(").contains("author_candidate("));
    }

    /// Everything `execute_claimed` writes to its proposal goes through the
    /// claim it holds, and the run it starts is recorded on that claim. An
    /// unfenced write would let an execute whose claim a scan took back
    /// overwrite the next execute's state.
    #[test]
    fn the_claimed_execute_writes_only_through_its_claim() {
        let claimed = fn_body("async fn execute_claimed(");
        assert!(
            claimed.contains("Some(claim.run_claim())"),
            "the executor records its run on the execute's claim"
        );
        for unfenced in [
            "update_proposal_status(",
            "update_proposal_status_with_error(",
            "pg_db.update_",
        ] {
            assert!(
                !claimed.contains(unfenced),
                "execute_claimed writes its proposal only through its claim; found `{unfenced}`"
            );
        }
    }

    /// A candidate whose claim was taken back before it was recorded answers
    /// `claim-lost`, and only a recorded candidate is counted as promoted.
    #[test]
    fn a_candidate_whose_claim_was_lost_is_not_counted() {
        let claimed = fn_body("async fn execute_claimed(");
        let at = |needle: &str| find_in(claimed, needle, "execute_claimed");
        let staged = at("claim.stage(");
        let lost = at("\"claim-lost\"");
        let counted = at("metric_executed_inc(\"promoted\")");
        assert!(
            staged < lost && lost < counted,
            "a candidate whose claim was lost answers claim-lost before anything counts it \
             promoted (found {staged}, {lost}, {counted})"
        );
        // The arm must also RETURN its answer: without the `return`, it would
        // fall through to the count and answer `landed-in-pending`.
        let arm = staged + find_in(&claimed[staged..], "Ok(false) =>", "the stage match");
        let arm_end = arm + find_in(&claimed[arm..], "Err(", "the stage match");
        let lost_arm = &claimed[arm..arm_end];
        assert!(
            lost_arm.contains("\"claim-lost\"") && lost_arm.contains("return ("),
            "the claim-lost arm returns its answer:\n{lost_arm}"
        );
    }

    #[test]
    fn mint_proposal_id_shape() {
        let id = mint_proposal_id();
        assert!(id.starts_with("prop_"), "expected prop_ prefix: {}", id);
        // UUIDv7 string is 36 chars (8-4-4-4-12 plus 4 dashes); plus the
        // 5-char prefix.
        assert_eq!(id.len(), 5 + 36, "unexpected proposal id length: {}", id);
    }

    // `spec_id_to_pathname` behaviour is owned and tested by
    // `spec_api::slug`, together with its inverse.

    /// Only the variant that actually means "your visits postdate the
    /// derivation" may prescribe a re-derive.
    ///
    /// Regression guard for a diagnosis that asserted a cause it had not
    /// measured: an empty page/artifact intersection used to map to
    /// `NoActiveStates` unconditionally, so a page whose observations carry no
    /// fingerprints — skipped by `derive::load_observations`, and therefore
    /// unable to enter ANY artifact — was told to re-derive forever.
    #[test]
    fn only_the_stale_artifact_diagnosis_prescribes_a_re_derive() {
        use crate::workflow_generation::spec_authoring::AuthoringError;

        let stale = AuthoringError::NoActiveStates {
            page_label: "account-billing".into(),
            total_states: 3,
        };
        let unclustered = AuthoringError::ObservedButUnclustered {
            page_label: "account-billing".into(),
            total_states: 3,
        };

        assert_ne!(
            authoring_error_reason(&stale),
            authoring_error_reason(&unclustered),
            "the two causes must be machine-distinguishable, not just worded differently"
        );

        let stale_detail = authoring_error_detail(&stale);
        assert!(
            stale_detail.contains("re-derive"),
            "the stale-artifact case is the one a re-derive fixes: {stale_detail}"
        );

        let unclustered_detail = authoring_error_detail(&unclustered);
        assert!(
            unclustered_detail.contains("re-deriving will not change this"),
            "the unclustered case must say re-deriving won't help: {unclustered_detail}"
        );
    }

    /// Only the variant that actually means "this page was never observed" may
    /// prescribe visiting it.
    ///
    /// Regression guard for the second-order cost of app-scoping the failure
    /// diagnostic: against an un-backfilled or un-attributed corpus every
    /// app-scoped read is empty, so without the unscoped probe behind
    /// `PageObservationsUnattributed` this path would tell the operator to go
    /// click around the UI — for every page at once, and it would never help.
    #[test]
    fn only_the_never_observed_diagnosis_prescribes_visiting_the_page() {
        use crate::workflow_generation::spec_authoring::AuthoringError;

        let never = AuthoringError::NoPageObservations {
            page_label: "specs".into(),
        };
        let unattributed = AuthoringError::PageObservationsUnattributed {
            page_label: "specs".into(),
            app_id: "qontinui-runner".into(),
        };

        assert_ne!(
            authoring_error_reason(&never),
            authoring_error_reason(&unattributed),
            "the two causes must be machine-distinguishable, not just worded differently"
        );

        let never_detail = authoring_error_detail(&never);
        assert!(
            never_detail.contains("never been visited"),
            "the never-observed case is the one visiting fixes: {never_detail}"
        );

        let unattributed_detail = authoring_error_detail(&unattributed);
        assert!(
            unattributed_detail.contains("visiting the page will not help"),
            "the unattributed case must explicitly deny the visit remedy, or it reads as a \
             wordier NoPageObservations: {unattributed_detail}"
        );
        assert!(
            unattributed_detail.contains("backfill")
                && unattributed_detail.contains("qontinui-runner"),
            "the unattributed case must name the actual remedies and the app that cannot \
             see the corpus: {unattributed_detail}"
        );
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
        // Must be the slugger's spelling of `/` (`root`), not the literal
        // `index` leaf — the two used to be accepted interchangeably and so
        // shadowed each other under `specs/pages/`.
        assert_eq!(
            infer_target_spec_id("qontinui-runner", &elem).as_deref(),
            Some(INDEX_SPEC_ID)
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
            api_assertions: None,
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
    /// shared dev DB is the only place the `atlas_managed.spec_proposals` table
    /// exists (Atlas-managed). Unit tests above cover the pure helpers
    /// (slug, JSON shapes, placeholder writer).
    ///
    /// To run: `cargo test --features spec-authoring,pg_integration_tests -- proposals::tests::pg`
    #[cfg(feature = "pg_integration_tests")]
    mod pg {
        use super::*;
        use crate::database::pg::PgDb;

        async fn fresh_db() -> (tokio::sync::MutexGuard<'static, ()>, std::sync::Arc<PgDb>) {
            let guard = crate::database::pg::spec_proposals::SPEC_PROPOSALS_TEST_LOCK
                .lock()
                .await;
            // The integration DB must already have spec_proposals (Atlas).
            let db = PgDb::new_for_test().await;
            // Wipe any prior test rows so dedup behaves deterministically.
            let conn = db.pool().get().await.expect("pg conn");
            conn.execute("TRUNCATE TABLE atlas_managed.spec_proposals", &[])
                .await
                .expect("truncate spec_proposals");
            (guard, db)
        }

        async fn queue(db: &PgDb, id: &str, app_id: &str, pathname: &str) -> bool {
            db.insert_proposal(
                id,
                app_id,
                "fullPage",
                Some(pathname),
                None,
                "queued",
                &serde_json::json!({}),
            )
            .await
            .expect("insert_proposal")
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn insert_dedupes_on_kind_and_target() {
            let (_guard, db) = fresh_db().await;
            assert!(
                queue(&db, "prop_test_one", "app-a", "/foo").await,
                "first insert"
            );
            assert!(
                !queue(&db, "prop_test_two", "app-a", "/foo").await,
                "the same app queuing the same target again is deduped"
            );
        }

        /// The dedup index carries `app_id`: two apps may each queue the same
        /// page label, and each still dedupes against itself.
        #[tokio::test(flavor = "multi_thread")]
        async fn the_same_target_queues_once_per_app() {
            let (_guard, db) = fresh_db().await;
            assert!(queue(&db, "prop_a", "app-a", "/same").await);
            assert!(
                queue(&db, "prop_b", "app-b", "/same").await,
                "another app's row must not dedupe this app's proposal"
            );
            assert!(!queue(&db, "prop_b2", "app-b", "/same").await);
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn list_round_trips_inserted_rows() {
            let (_guard, db) = fresh_db().await;
            assert!(queue(&db, "prop_test_list", "app-a", "/bar").await);
            let rows = db.list_proposals("app-a", 10, 0).await.expect("list");
            assert!(rows
                .iter()
                .any(|r| r.id == "prop_test_list" && r.status == "queued" && r.app_id == "app-a"));
        }

        /// list, list-by-status (the sweep's reader) and find (the execute
        /// handler's lookup) see only the asking app's rows.
        #[tokio::test(flavor = "multi_thread")]
        async fn every_read_is_scoped_to_the_asking_app() {
            let (_guard, db) = fresh_db().await;
            assert!(queue(&db, "prop_mine", "app-a", "/mine").await);
            assert!(queue(&db, "prop_theirs", "app-b", "/theirs").await);
            for id in ["prop_mine", "prop_theirs"] {
                db.update_proposal_status(id, "pendingPromotion")
                    .await
                    .expect("status");
            }

            let listed: Vec<String> = db
                .list_proposals("app-a", 50, 0)
                .await
                .expect("list")
                .into_iter()
                .map(|r| r.id)
                .collect();
            assert_eq!(listed, vec!["prop_mine".to_string()]);

            let pending: Vec<String> = db
                .list_proposals_by_status("app-a", "pendingPromotion", 50)
                .await
                .expect("list_by_status")
                .into_iter()
                .map(|r| r.id)
                .collect();
            assert_eq!(pending, vec!["prop_mine".to_string()]);

            assert!(db
                .find_proposal_by_id("app-a", "prop_mine")
                .await
                .expect("find")
                .is_some());
            assert!(
                db.find_proposal_by_id("app-a", "prop_theirs")
                    .await
                    .expect("find")
                    .is_none(),
                "another app's proposal is not this app's to execute"
            );
        }

        async fn status_of(db: &PgDb, app_id: &str, id: &str) -> (String, Option<String>) {
            let row = db
                .find_proposal_by_id(app_id, id)
                .await
                .expect("find")
                .expect("the row exists");
            (row.status, row.last_error)
        }

        const HOUR: std::time::Duration = std::time::Duration::from_secs(3600);

        async fn claim(
            db: &PgDb,
            app_id: &str,
            id: &str,
            bound: std::time::Duration,
        ) -> Option<String> {
            db.claim_proposal_for_execute(app_id, id, bound)
                .await
                .expect("claim")
        }

        async fn row_of(db: &PgDb, app_id: &str, id: &str) -> SpecProposalRow {
            db.find_proposal_by_id(app_id, id)
                .await
                .expect("find")
                .expect("the row exists")
        }

        /// Backdate the claims on `ids` by two hours.
        async fn age_claims(db: &PgDb, ids: &[&str]) {
            let conn = db.pool().get().await.expect("pg conn");
            let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
            conn.execute(
                "UPDATE atlas_managed.spec_proposals \
                 SET last_attempt_at = now() - interval '2 hours' \
                 WHERE id = ANY($1)",
                &[&ids],
            )
            .await
            .expect("age the claims");
        }

        /// The execute's claim is one compare-and-set: only a queued row of
        /// the asking app moves to `inFlight`, and only once. It records its
        /// token and its claimant's own bound in `metadata.execute`.
        #[tokio::test(flavor = "multi_thread")]
        async fn an_execute_claims_only_a_queued_proposal_of_its_own_app() {
            let (_guard, db) = fresh_db().await;
            assert!(queue(&db, "prop_claim", "app-a", "/claim").await);
            assert!(
                claim(&db, "app-b", "prop_claim", HOUR).await.is_none(),
                "another app cannot claim this app's proposal"
            );
            assert_eq!(status_of(&db, "app-a", "prop_claim").await.0, "queued");

            let token = claim(&db, "app-a", "prop_claim", HOUR)
                .await
                .expect("a queued proposal is claimed");
            let row = row_of(&db, "app-a", "prop_claim").await;
            assert_eq!(row.status, "inFlight");
            assert!(
                row.last_attempt_at.is_some(),
                "the claim stamps the attempt"
            );
            assert_eq!(
                row.metadata["execute"]["claim"].as_str(),
                Some(token.as_str()),
                "the claim records its token"
            );
            assert_eq!(
                row.metadata["execute"]["staleAfterSecs"].as_f64(),
                Some(3600.0),
                "and its claimant's own bound"
            );

            assert!(
                claim(&db, "app-a", "prop_claim", HOUR).await.is_none(),
                "a second execute of an inFlight proposal starts no second run"
            );
            db.update_proposal_status("prop_claim", "pendingPromotion")
                .await
                .expect("status");
            assert!(
                claim(&db, "app-a", "prop_claim", HOUR).await.is_none(),
                "a proposal past the queue is not executed again"
            );
        }

        /// Every write an execute makes to its proposal lands only while its
        /// claim is held: a write under any other token changes nothing, the
        /// abandoned-execute re-queue releases the claim, a released claim's
        /// late outcome is not written over the next execute's claim, and a
        /// written outcome is never undone.
        #[tokio::test(flavor = "multi_thread")]
        async fn a_claimed_write_lands_only_while_its_claim_is_held() {
            let (_guard, db) = fresh_db().await;
            assert!(queue(&db, "prop_gone", "app-a", "/gone").await);
            let first = claim(&db, "app-a", "prop_gone", HOUR).await.expect("claim");

            let candidate = serde_json::json!({ "id": "page" });
            assert!(!db
                .requeue_claimed_proposal(
                    "prop_gone",
                    "not-the-claim",
                    Some(ABANDONED_EXECUTE_ERROR)
                )
                .await
                .expect("requeue"));
            assert!(!db
                .fail_claimed_proposal("prop_gone", "not-the-claim", "x")
                .await
                .expect("fail"));
            assert!(!db
                .stage_claimed_proposal("prop_gone", "not-the-claim", &candidate)
                .await
                .expect("stage"));
            assert_eq!(
                status_of(&db, "app-a", "prop_gone").await,
                ("inFlight".to_string(), None),
                "a write under another token changes nothing"
            );

            assert!(db
                .requeue_claimed_proposal("prop_gone", &first, Some(ABANDONED_EXECUTE_ERROR))
                .await
                .expect("requeue"));
            let row = row_of(&db, "app-a", "prop_gone").await;
            assert_eq!(
                (row.status.as_str(), row.last_error.as_deref()),
                ("queued", Some(ABANDONED_EXECUTE_ERROR))
            );
            assert!(
                row.metadata.get("execute").is_none(),
                "the re-queue releases the claim: {}",
                row.metadata
            );

            let next = claim(&db, "app-a", "prop_gone", HOUR)
                .await
                .expect("claimed again");
            assert!(
                !db.fail_claimed_proposal("prop_gone", &first, "late outcome of the first execute")
                    .await
                    .expect("fail"),
                "a released claim's late outcome is not written over the next claim"
            );
            assert_eq!(status_of(&db, "app-a", "prop_gone").await.0, "inFlight");
            assert!(db
                .fail_claimed_proposal("prop_gone", &next, "builder step failed")
                .await
                .expect("fail"));
            assert_eq!(
                status_of(&db, "app-a", "prop_gone").await,
                (
                    "failed".to_string(),
                    Some("builder step failed".to_string())
                )
            );
            assert!(
                !db.requeue_claimed_proposal("prop_gone", &next, Some(ABANDONED_EXECUTE_ERROR))
                    .await
                    .expect("requeue"),
                "a written outcome is never undone"
            );

            assert!(queue(&db, "prop_staged", "app-a", "/staged").await);
            let staged = claim(&db, "app-a", "prop_staged", HOUR)
                .await
                .expect("claim");
            assert!(db
                .stage_claimed_proposal("prop_staged", &staged, &candidate)
                .await
                .expect("stage"));
            let row = row_of(&db, "app-a", "prop_staged").await;
            assert_eq!(row.status, "pendingPromotion");
            assert_eq!(row.candidate_ir, Some(candidate));
            assert!(row.metadata.get("execute").is_none());
        }

        /// The scan re-queues the asking app's claims that have outlived
        /// their CLAIMANT's own bound, and nothing else. This scanner's
        /// bound (one hour) applies only to a claim that recorded none.
        #[tokio::test(flavor = "multi_thread")]
        async fn the_scan_requeues_only_its_apps_stale_claims() {
            let (_guard, db) = fresh_db().await;
            for (id, app, target, bound) in [
                ("prop_stale", "app-a", "/stale", HOUR),
                ("prop_long", "app-a", "/long", 3 * HOUR),
                ("prop_fresh", "app-a", "/fresh", HOUR),
                ("prop_legacy", "app-a", "/legacy", HOUR),
                ("prop_other", "app-b", "/other", HOUR),
            ] {
                assert!(queue(&db, id, app, target).await);
                assert!(claim(&db, app, id, bound).await.is_some());
            }
            age_claims(
                &db,
                &["prop_stale", "prop_long", "prop_legacy", "prop_other"],
            )
            .await;
            let conn = db.pool().get().await.expect("pg conn");
            conn.execute(
                "UPDATE atlas_managed.spec_proposals SET metadata = metadata - 'execute' \
                 WHERE id = 'prop_legacy'",
                &[],
            )
            .await
            .expect("a claim from before claims recorded a bound");
            drop(conn);

            let stale = db
                .requeue_stale_in_flight_proposals(
                    "app-a",
                    HOUR,
                    STALE_EXECUTE_ERROR,
                    STALE_RUN_STOP_REASON,
                )
                .await
                .expect("requeue stale");
            assert_eq!(stale.requeued, 2, "{stale:?}");
            assert_eq!(
                status_of(&db, "app-a", "prop_stale").await,
                ("queued".to_string(), Some(STALE_EXECUTE_ERROR.to_string()))
            );
            assert_eq!(
                status_of(&db, "app-a", "prop_legacy").await.0,
                "queued",
                "a claim with no recorded bound is judged by the scanner's"
            );
            assert_eq!(
                status_of(&db, "app-a", "prop_long").await.0,
                "inFlight",
                "a claimant's own longer bound governs, whichever process scans"
            );
            assert_eq!(
                status_of(&db, "app-a", "prop_fresh").await.0,
                "inFlight",
                "an execute still within its bound keeps its claim"
            );
            assert_eq!(
                status_of(&db, "app-b", "prop_other").await.0,
                "inFlight",
                "another app's stale row is that app's scan's to re-queue"
            );
        }

        /// A run is recorded on a claim only while the claim is held, and the
        /// scan that finds that claim stale stops the run unless it has
        /// finished, then releases the claim.
        #[tokio::test(flavor = "multi_thread")]
        async fn the_scan_stops_the_run_a_stale_claim_recorded() {
            use crate::database::types::CreateTaskRunInput;
            let (_guard, db) = fresh_db().await;
            let going = format!("spec-run-going-{}", uuid::Uuid::new_v4());
            let done = format!("spec-run-done-{}", uuid::Uuid::new_v4());
            for id in [&going, &done] {
                db.create_task_run(&CreateTaskRunInput::new(id.clone(), "spec execute run"))
                    .await
                    .expect("create_task_run");
            }
            db.complete_task_run(&done).await.expect("complete");

            assert!(queue(&db, "prop_run_going", "app-a", "/going").await);
            assert!(queue(&db, "prop_run_done", "app-a", "/done").await);
            let g = claim(&db, "app-a", "prop_run_going", HOUR)
                .await
                .expect("claim");
            let d = claim(&db, "app-a", "prop_run_done", HOUR)
                .await
                .expect("claim");
            assert!(
                !db.record_execute_run("prop_run_going", "not-the-claim", &going)
                    .await
                    .expect("record"),
                "a run is recorded only under the held claim"
            );
            assert!(db
                .record_execute_run("prop_run_going", &g, &going)
                .await
                .expect("record"));
            assert!(db
                .record_execute_run("prop_run_done", &d, &done)
                .await
                .expect("record"));
            assert_eq!(
                row_of(&db, "app-a", "prop_run_going").await.metadata["execute"]["taskRunId"]
                    .as_str(),
                Some(going.as_str())
            );

            age_claims(&db, &["prop_run_going", "prop_run_done"]).await;
            let stale = db
                .requeue_stale_in_flight_proposals(
                    "app-a",
                    HOUR,
                    STALE_EXECUTE_ERROR,
                    STALE_RUN_STOP_REASON,
                )
                .await
                .expect("requeue stale");
            assert_eq!(
                stale,
                crate::database::pg::spec_proposals::StaleRequeue {
                    requeued: 2,
                    runs_stopped: 1
                }
            );
            assert_eq!(
                db.get_task_run_status(&going).await.expect("status"),
                Some((
                    "stopped".to_string(),
                    Some(STALE_RUN_STOP_REASON.to_string())
                ))
            );
            assert_eq!(
                db.get_task_run_status(&done).await.expect("status"),
                Some(("complete".to_string(), None)),
                "a finished run is left as it was"
            );
            assert!(
                !db.fail_claimed_proposal("prop_run_going", &g, "late")
                    .await
                    .expect("fail"),
                "the dead execute's claim is released"
            );
            for id in [&going, &done] {
                db.delete_task_run(id).await.expect("cleanup");
            }
        }

        /// A scan reads only its own app's observations. Unattributed ones
        /// (`app_id` NULL) are the bootstrap app's, as legacy proposals are.
        #[tokio::test(flavor = "multi_thread")]
        async fn the_scan_reads_only_its_apps_observations() {
            let (_guard, db) = fresh_db().await;
            let tag = uuid::Uuid::new_v4().simple().to_string();
            let mine = format!("scan-mine-{tag}");
            let theirs = format!("scan-theirs-{tag}");
            let unattributed = format!("scan-unattributed-{tag}");
            let conn = db.pool().get().await.expect("pg conn");
            for (label, app) in [
                (&mine, Some("app-a")),
                (&theirs, Some("app-b")),
                (&unattributed, None),
            ] {
                for _ in 0..2 {
                    conn.execute(
                        "INSERT INTO co_occurrence_observations (id, spec_id, fingerprints, app_id) \
                         VALUES (gen_random_uuid(), $1, '[]'::jsonb, $2)",
                        &[label, &app],
                    )
                    .await
                    .expect("observation");
                }
            }

            let a = query_eligible_pathnames(&db, "app-a", 1, 2, 100_000)
                .await
                .expect("app-a's scan");
            assert!(a.contains(&mine), "{a:?}");
            assert!(
                !a.contains(&theirs),
                "another app's page is not queued: {a:?}"
            );
            assert!(!a.contains(&unattributed), "{a:?}");
            let bootstrap = query_eligible_pathnames(&db, storage::RUNNER_APP_ID, 1, 2, 100_000)
                .await
                .expect("the bootstrap app's scan");
            assert!(bootstrap.contains(&unattributed), "{bootstrap:?}");
            assert!(
                !bootstrap.contains(&mine) && !bootstrap.contains(&theirs),
                "{bootstrap:?}"
            );

            let labels = vec![mine, theirs, unattributed];
            conn.execute(
                "DELETE FROM co_occurrence_observations WHERE spec_id = ANY($1)",
                &[&labels],
            )
            .await
            .expect("cleanup");
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
            pg.insert_app(&RegisterAppRequest::new(
                app_id.clone(),
                tmp.path().to_string_lossy(),
                format!("http://localhost:0/{}", app_id),
                format!("Test {}", app_id),
            ))
            .await
            .expect("insert_app");

            // Insert a proposal so the event has something to reference.
            let proposal_id = unique_id("e9-prop");
            pg.insert_proposal(
                &proposal_id,
                &app_id,
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
            pg.insert_app(&RegisterAppRequest::new(
                app_id.clone(),
                tmp.path().to_string_lossy(),
                format!("http://localhost:0/{}", app_id),
                format!("Test {}", app_id),
            ))
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
                api_assertions: None,
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
