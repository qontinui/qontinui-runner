//! HTTP adapter for `qontinui_spec_check::evaluate`/`evaluate_batch`.
//!
//! Pure shim layer. No matching logic; no policy logic. Loads specs via
//! `spec_api::storage`, fetches snapshots via `mcp::sdk_client`, then forwards
//! to the crate. Maps `SnapshotFetchError` to HTTP status per §5.9 and
//! un-spec'd pages to `404 SpecError { reason: "spec-not-found" }` per §5.13.
//!
//! Plan: `qontinui-dev-notes/spec-check-v1/03-adapters.md` Steps 1-3.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::stream;
use serde::Deserialize;
use serde_json::json;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::mcp::types::ApiState;
use crate::spec_api::events;
use crate::spec_api::responses::SpecError;
use crate::spec_api::storage;
use qontinui_spec_check as spec_check; // crate name = "qontinui-spec-check"
use qontinui_types::spec_api_events::SpecApiEvent;
use qontinui_types::spec_check::{
    MatchOutcome, PolicyEvaluation, SpecCheckPolicy, SpecCheckResult,
};
use qontinui_types::ui_bridge::UIBridgeSnapshot; // capital UI; lives in ui_bridge

/// Wall-clock at evaluator entry → end. Carried into `SpecCheckCompleted`'s
/// `total_duration_ms` field. Sync-only — the evaluator itself is sync.
fn invoke_emit(snapshot_id: &str, page_ids: Vec<String>, invoked_via: &str) {
    events::emit(SpecApiEvent::SpecCheckInvoked {
        snapshot_id: snapshot_id.to_string(),
        page_ids,
        invoked_via: invoked_via.to_string(),
        at_ms: events::now_ms(),
    });
}

/// Roll up a single `SpecCheckResult` (or many) into the `SpecCheckCompleted`
/// counts. `page_count` is the input count — `eval_error_count` is the
/// difference between input and successfully-evaluated results (only meaningful
/// for the batch path).
fn completion_counts(
    results: &[&SpecCheckResult],
    input_page_count: usize,
) -> (usize, usize, usize, usize, f32) {
    let mut perfect = 0;
    let mut partial = 0;
    let mut no_match = 0;
    let mut rate_sum = 0f32;
    for r in results {
        match r.summary.match_outcome {
            MatchOutcome::FullMatch => perfect += 1,
            MatchOutcome::PartialMatch => partial += 1,
            MatchOutcome::NoMatch => no_match += 1,
        }
        rate_sum += r.summary.overall_match_rate;
    }
    let eval_error = input_page_count.saturating_sub(results.len());
    let overall = if results.is_empty() {
        0.0
    } else {
        rate_sum / results.len() as f32
    };
    (perfect, partial, no_match, eval_error, overall)
}

// ===========================================================================
// Shared snapshot fetch + error mapping
// ===========================================================================

/// Fetch a fresh UI Bridge snapshot via the active SDK connection.
///
/// Resolves app identity from the active SDK connection (`SdkAppInfo`) before
/// dispatch; if nothing is connected we return `NotConnected` before touching
/// the dispatcher. Calls `dispatch_app_request("getControlSnapshot", …)` and
/// wraps the returned `Value` into a `UIBridgeSnapshot` via
/// `spec_check::wrap_snapshot` (which mints `snapshot_id = "scs_" + UUIDv7()`
/// plus a content sha256 and a `BridgeFingerprint`).
///
/// Return: `(snapshot, fingerprint, snapshot_id, content_sha256)`.
pub(crate) async fn fetch_fresh_snapshot(
    state: &Arc<ApiState>,
) -> Result<
    (
        UIBridgeSnapshot,
        qontinui_types::spec_check::BridgeFingerprint,
        String,
        String,
    ),
    spec_check::SnapshotFetchError,
> {
    // Resolve app identity from the active SDK connection before dispatch.
    //
    // `SdkAppInfo` carries `app_id` + optional `version` + optional
    // `framework`. There is no explicit `route` on the connection, and the
    // bridge version is surfaced via `framework` when present. Map what we
    // have; the rest is `None` (all optional on `BridgeFingerprint`).
    let (app_id, app_version, route, bridge_version) = {
        let guard = state.sdk_connection.lock().await;
        match guard.active_connection() {
            Some(c) => (
                c.app_info.app_id.clone(),
                c.app_info.version.clone(),
                None,
                c.app_info.framework.clone(),
            ),
            None => return Err(spec_check::SnapshotFetchError::NotConnected),
        }
    };
    match crate::mcp::sdk_client::dispatch_app_request(
        state,
        "getControlSnapshot",
        json!({}),
        reqwest::Method::GET,
        "/control/snapshot",
        None,
    )
    .await
    {
        Ok(value) => spec_check::wrap_snapshot(value, app_id, app_version, route, bridge_version),
        Err(e) => Err(classify_sdk_error(&e)),
    }
}

/// Classify the stringly-typed error from `dispatch_app_request` into a
/// `SnapshotFetchError`. `dispatch_app_request` returns `Err(String)` with no
/// structured shape; we prefix-match the canonical phrasings emitted by
/// `mcp::app_dispatch`.
fn classify_sdk_error(msg: &str) -> spec_check::SnapshotFetchError {
    use spec_check::SnapshotFetchError as E;
    let lower = msg.to_ascii_lowercase();
    if lower.contains("not connected")
        || lower.contains("not-connected")
        || lower.contains("no sdk")
    {
        E::NotConnected
    } else if lower.contains("timed out") || lower.contains("timeout") {
        E::Timeout { elapsed_ms: 5000 }
    } else if lower.contains("forbidden") || lower.contains("403") {
        E::Forbidden { status: 403 }
    } else {
        E::Network(msg.to_string())
    }
}

/// Map a `SnapshotFetchError` to the canonical HTTP `SpecError` envelope per
/// the §5.9 table. `Stale` is never mapped here — it is folded into success
/// warnings, never an error response.
fn snapshot_error_to_response(err: spec_check::SnapshotFetchError) -> Response {
    use spec_check::SnapshotFetchError as E;
    let (status, detail) = match &err {
        E::NotConnected => (
            StatusCode::FAILED_DEPENDENCY,
            json!({ "cause": "not-connected" }),
        ),
        E::Timeout { elapsed_ms } => (
            StatusCode::GATEWAY_TIMEOUT,
            json!({ "cause": "timeout", "elapsedMs": elapsed_ms }),
        ),
        E::Partial { from, to } => (
            StatusCode::CONFLICT,
            json!({ "cause": "partial-fetch", "from": from, "to": to }),
        ),
        E::Stale { .. } => {
            unreachable!("Stale is folded into success warnings, never mapped to a response")
        }
        E::Network(_) => (StatusCode::BAD_GATEWAY, json!({ "cause": "network" })),
        E::Forbidden { .. } => (
            StatusCode::BAD_GATEWAY,
            json!({ "cause": "upstream-forbidden" }),
        ),
        E::Malformed(_) => (StatusCode::BAD_GATEWAY, json!({ "cause": "malformed" })),
    };
    (
        status,
        Json(SpecError::with_detail("snapshot-unavailable", detail)),
    )
        .into_response()
}

/// Reject a `page_id` that is empty or contains path-traversal characters.
fn invalid_page_id(page_id: &str) -> bool {
    page_id.is_empty() || page_id.contains("..") || page_id.contains('/') || page_id.contains('\\')
}

// ===========================================================================
// Step 2 — POST /spec-check
// ===========================================================================

#[derive(Debug, Deserialize)]
pub struct SpecCheckRequest {
    pub page_id: String,
    #[serde(default)]
    pub snapshot: Option<UIBridgeSnapshot>,
}

/// `POST /spec-check` — evaluate one page spec against a fresh (or supplied)
/// UI Bridge snapshot. Returns the full `SpecCheckResult` on success, or the
/// canonical `SpecError` envelope on failure (§5.13 / §5.9 tables).
pub async fn post_spec_check(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<SpecCheckRequest>,
) -> Response {
    if invalid_page_id(&req.page_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(SpecError::with_detail(
                "invalid-page-id",
                json!({ "pageId": req.page_id }),
            )),
        )
            .into_response();
    }

    // Load IR via the existing helper — adapters do not re-implement
    // spec-loading. The IR doc is directly consumable by `evaluate`
    // (Plan 01 unified the top-level on `IrPageSpec`; no projection).
    let root = storage::resolve_specs_root();
    let ir = match storage::read_ir(&root, &req.page_id) {
        Ok(Some(doc)) => doc,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(SpecError::with_detail(
                    "spec-not-found",
                    json!({ "pageId": req.page_id }),
                )),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpecError::with_detail(
                    "spec-malformed",
                    json!({ "pageId": req.page_id, "error": e }),
                )),
            )
                .into_response();
        }
    };

    // Fetch snapshot (fresh or supplied).
    let (snapshot, _fingerprint, snapshot_id, _content_hash) = match req.snapshot {
        Some(s) => {
            // Caller-supplied path: skip wrap_snapshot, synthesize the
            // fingerprint locally. Snapshot identity for replay is the
            // caller's responsibility.
            let fp = qontinui_types::spec_check::BridgeFingerprint {
                app_id: "supplied".to_string(),
                app_version: None,
                route: None,
                bridge_version: None,
                snapshot_timestamp: String::new(),
                element_count: s.elements.len() as u32,
            };
            (
                s,
                fp,
                format!("scs_supplied_{}", uuid::Uuid::now_v7()),
                String::new(),
            )
        }
        None => match fetch_fresh_snapshot(&state).await {
            Ok(tuple) => tuple,
            Err(err) => return snapshot_error_to_response(err),
        },
    };

    // Plan 06: emit Invoked before the (potentially slow) evaluator runs so
    // subscribers can correlate `snapshot_id` against tracing spans.
    invoke_emit(&snapshot_id, vec![req.page_id.clone()], "http");
    let started_ms = events::now_ms();

    // Evaluate — pure crate call, no logic here.
    let result = spec_check::evaluate(&snapshot, &ir);

    info!(
        "spec_check page_id={} snapshot_id={} match_outcome={:?} match_rate={:.3}",
        req.page_id, snapshot_id, result.summary.match_outcome, result.summary.overall_match_rate
    );

    // Plan 06: emit Completed after the evaluator returns. The full result
    // body stays out of the broadcast (subscribers join by snapshot_id).
    let (perfect, partial, no_match, eval_error, overall_rate) = completion_counts(&[&result], 1);
    let completed_ms = events::now_ms();
    events::emit(SpecApiEvent::SpecCheckCompleted {
        snapshot_id: snapshot_id.clone(),
        page_count: 1,
        overall_match_rate: overall_rate,
        perfect_match_count: perfect,
        partial_match_count: partial,
        no_match_count: no_match,
        eval_error_count: eval_error,
        total_duration_ms: completed_ms.saturating_sub(started_ms),
        at_ms: completed_ms,
    });

    (StatusCode::OK, Json(result)).into_response()
}

// ===========================================================================
// Step 3 — POST /spec-check/batch
// ===========================================================================

#[derive(Debug, Deserialize)]
pub struct BatchRequest {
    pub snapshot: BatchSnapshot,
    pub pages: Vec<BatchPageEntry>,
    #[serde(default, rename = "sharedPolicy")]
    pub shared_policy: Option<SpecCheckPolicy>,
    #[serde(default)]
    pub stream: bool,
}

#[derive(Debug, Deserialize)]
pub struct BatchSnapshot {
    pub source: String, // "fresh" | "supplied"
    #[serde(default)]
    pub blob: Option<UIBridgeSnapshot>,
}

#[derive(Debug, Deserialize)]
pub struct BatchPageEntry {
    #[serde(rename = "pageId")]
    pub page_id: String,
    #[serde(default, rename = "policyOverride")]
    pub policy_override: Option<SpecCheckPolicy>,
}

const MAX_BATCH_SIZE: usize = 256;

/// `POST /spec-check/batch` — evaluate ≤ 256 pages against a single snapshot.
/// Partial-success per element; non-stream returns one JSON document, stream
/// returns NDJSON (`application/x-ndjson`).
pub async fn post_spec_check_batch(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<BatchRequest>,
) -> Response {
    if req.pages.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(SpecError::new("empty-batch"))).into_response();
    }
    if req.pages.len() > MAX_BATCH_SIZE {
        return (
            StatusCode::BAD_REQUEST,
            Json(SpecError::with_detail(
                "batch-too-large",
                json!({ "max": MAX_BATCH_SIZE, "supplied": req.pages.len() }),
            )),
        )
            .into_response();
    }

    // Resolve snapshot once. The batch handler Arc-shares the snapshot
    // itself and forwards `snapshot_id` as the response field/header.
    let (snapshot, snapshot_id) = match req.snapshot.source.as_str() {
        "fresh" => match fetch_fresh_snapshot(&state).await {
            Ok((s, _fp, id, _hash)) => (Arc::new(s), id),
            Err(err) => return snapshot_error_to_response(err),
        },
        "supplied" => match req.snapshot.blob {
            Some(s) => (
                Arc::new(s),
                format!("scs_supplied_{}", uuid::Uuid::now_v7()),
            ),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(SpecError::with_detail(
                        "invalid-batch-request",
                        json!({ "cause": "supplied-snapshot-missing-blob" }),
                    )),
                )
                    .into_response();
            }
        },
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(SpecError::with_detail(
                    "invalid-batch-request",
                    json!({ "cause": "unknown-snapshot-source", "value": other }),
                )),
            )
                .into_response();
        }
    };

    let root = Arc::new(storage::resolve_specs_root());

    info!(
        "spec_check batch pages={} stream={} snapshot_id={}",
        req.pages.len(),
        req.stream,
        snapshot_id
    );

    // Plan 06: emit Invoked once for the batch. Both the streaming and
    // collected paths reach the evaluator below.
    let input_page_count = req.pages.len();
    let page_ids: Vec<String> = req.pages.iter().map(|p| p.page_id.clone()).collect();
    invoke_emit(&snapshot_id, page_ids, "http");
    let batch_started_ms = events::now_ms();

    if req.stream {
        return stream_batch_results(snapshot, snapshot_id, root, req.pages, req.shared_policy)
            .await;
    }

    // Concurrent eval — spawn all N (N ≤ 256), snapshot is Arc-shared.
    let mut set: JoinSet<BatchElementResult> = JoinSet::new();
    for entry in req.pages {
        let snapshot = snapshot.clone();
        let root = root.clone();
        let shared_policy = req.shared_policy.clone();
        set.spawn(
            async move { evaluate_one(&root, &entry, &snapshot, shared_policy.as_ref()).await },
        );
    }
    let mut results = Vec::with_capacity(set.len());
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(r) => results.push(r),
            Err(e) => results.push(BatchElementResult::eval_error(
                "<join-failed>",
                e.to_string(),
            )),
        }
    }
    // Stable order by input pageId for determinism.
    results.sort_by(|a, b| a.page_id.cmp(&b.page_id));

    // Plan 06: emit Completed once after all per-element evaluations have
    // landed. The `eval_error_count` derives from input vs successfully-evaluated.
    let ok_results: Vec<&SpecCheckResult> = results
        .iter()
        .filter(|r| r.status == "ok")
        .filter_map(|r| r.result.as_ref())
        .collect();
    let (perfect, partial, no_match, eval_error, overall_rate) =
        completion_counts(&ok_results, input_page_count);
    let completed_ms = events::now_ms();
    events::emit(SpecApiEvent::SpecCheckCompleted {
        snapshot_id: snapshot_id.clone(),
        page_count: input_page_count,
        overall_match_rate: overall_rate,
        perfect_match_count: perfect,
        partial_match_count: partial,
        no_match_count: no_match,
        eval_error_count: eval_error,
        total_duration_ms: completed_ms.saturating_sub(batch_started_ms),
        at_ms: completed_ms,
    });

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "snapshot_id": snapshot_id,
            "results": results,
        })),
    )
        .into_response()
}

#[derive(Debug, serde::Serialize)]
pub struct BatchElementResult {
    #[serde(rename = "pageId")]
    pub page_id: String,
    pub status: &'static str, // "ok" | "spec-not-found" | "spec-malformed" | "eval-error"
    pub result: Option<SpecCheckResult>,
    pub policy_evaluation: Option<PolicyEvaluation>,
    pub error: Option<String>,
}

impl BatchElementResult {
    fn ok(
        page_id: String,
        result: SpecCheckResult,
        policy_evaluation: Option<PolicyEvaluation>,
    ) -> Self {
        Self {
            page_id,
            status: "ok",
            result: Some(result),
            policy_evaluation,
            error: None,
        }
    }

    fn spec_not_found(page_id: impl Into<String>) -> Self {
        Self {
            page_id: page_id.into(),
            status: "spec-not-found",
            result: None,
            policy_evaluation: None,
            error: None,
        }
    }

    fn spec_malformed(page_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            page_id: page_id.into(),
            status: "spec-malformed",
            result: None,
            policy_evaluation: None,
            error: Some(error.into()),
        }
    }

    fn invalid_page_id(page_id: impl Into<String>) -> Self {
        Self {
            page_id: page_id.into(),
            status: "eval-error",
            result: None,
            policy_evaluation: None,
            error: Some("invalid-page-id".to_string()),
        }
    }

    fn eval_error(page_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            page_id: page_id.into(),
            status: "eval-error",
            result: None,
            policy_evaluation: None,
            error: Some(error.into()),
        }
    }
}

/// Evaluate one batch entry. Identical to the single-shot handler except
/// errors become per-element statuses, not HTTP responses (§5.14).
async fn evaluate_one(
    root: &std::path::Path,
    entry: &BatchPageEntry,
    snapshot: &Arc<UIBridgeSnapshot>,
    shared_policy: Option<&SpecCheckPolicy>,
) -> BatchElementResult {
    if invalid_page_id(&entry.page_id) {
        return BatchElementResult::invalid_page_id(entry.page_id.clone());
    }

    let ir = match storage::read_ir(root, &entry.page_id) {
        Ok(Some(doc)) => doc,
        Ok(None) => return BatchElementResult::spec_not_found(entry.page_id.clone()),
        Err(e) => return BatchElementResult::spec_malformed(entry.page_id.clone(), e),
    };

    // Pure crate call. `evaluate` is sync + rayon-internal; for v1 N ≤ 256
    // we run it inline on the spawned task (see Risks §"contention").
    let result = spec_check::evaluate(snapshot, &ir);

    // Per-entry override wins over the shared policy.
    let policy = entry.policy_override.as_ref().or(shared_policy);
    let policy_eval = policy.map(|p| spec_check::apply_policy(&result, p));

    BatchElementResult::ok(entry.page_id.clone(), result, policy_eval)
}

/// NDJSON streaming variant: one `BatchElementResult` JSON object per line,
/// `application/x-ndjson`. SSE (`text/event-stream`) is wrong here — this is
/// raw NDJSON via a streamed body.
async fn stream_batch_results(
    snapshot: Arc<UIBridgeSnapshot>,
    snapshot_id: String,
    root: Arc<std::path::PathBuf>,
    pages: Vec<BatchPageEntry>,
    shared_policy: Option<SpecCheckPolicy>,
) -> Response {
    // Plan 06: stream-path Completed emission. Accumulate counts as we
    // walk each item, then emit on stream end (when the inner iterator is
    // exhausted). The `started_ms` is captured outside the unfold so it's
    // consistent with the Invoked emit in the caller.
    let input_page_count = pages.len();
    let started_ms = events::now_ms();
    let snapshot_id_for_completion = snapshot_id.clone();
    struct StreamState {
        iter: std::vec::IntoIter<BatchPageEntry>,
        perfect: usize,
        partial: usize,
        no_match: usize,
        eval_error: usize,
        rate_sum: f32,
        ok_count: usize,
        emitted: bool,
    }
    let initial = StreamState {
        iter: pages.into_iter(),
        perfect: 0,
        partial: 0,
        no_match: 0,
        eval_error: 0,
        rate_sum: 0.0,
        ok_count: 0,
        emitted: false,
    };
    let body_stream = stream::unfold(initial, move |mut st| {
        let snapshot = snapshot.clone();
        let root = root.clone();
        let shared_policy = shared_policy.clone();
        let snap_id = snapshot_id_for_completion.clone();
        async move {
            let entry = match st.iter.next() {
                Some(e) => e,
                None => {
                    if !st.emitted {
                        let overall = if st.ok_count == 0 {
                            0.0
                        } else {
                            st.rate_sum / st.ok_count as f32
                        };
                        let completed_ms = events::now_ms();
                        events::emit(SpecApiEvent::SpecCheckCompleted {
                            snapshot_id: snap_id,
                            page_count: input_page_count,
                            overall_match_rate: overall,
                            perfect_match_count: st.perfect,
                            partial_match_count: st.partial,
                            no_match_count: st.no_match,
                            eval_error_count: st.eval_error,
                            total_duration_ms: completed_ms.saturating_sub(started_ms),
                            at_ms: completed_ms,
                        });
                        st.emitted = true;
                    }
                    return None;
                }
            };
            let r = evaluate_one(&root, &entry, &snapshot, shared_policy.as_ref()).await;
            match (r.status, r.result.as_ref()) {
                ("ok", Some(res)) => {
                    st.ok_count += 1;
                    st.rate_sum += res.summary.overall_match_rate;
                    match res.summary.match_outcome {
                        MatchOutcome::FullMatch => st.perfect += 1,
                        MatchOutcome::PartialMatch => st.partial += 1,
                        MatchOutcome::NoMatch => st.no_match += 1,
                    }
                }
                _ => st.eval_error += 1,
            }
            let mut line = match serde_json::to_vec(&r) {
                Ok(v) => v,
                Err(e) => {
                    warn!("spec_check batch stream serialize failed: {e}");
                    return None;
                }
            };
            line.push(b'\n');
            Some((Ok::<_, std::io::Error>(line), st))
        }
    });
    match Response::builder()
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header("X-Snapshot-Id", snapshot_id)
        .body(axum::body::Body::from_stream(body_stream))
    {
        Ok(resp) => resp,
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SpecError::with_detail(
                "stream-init-failed",
                json!({ "error": e.to_string() }),
            )),
        )
            .into_response(),
    }
}

// ===========================================================================
// Step 10 — Rust test modules (HTTP smoke + helper round-trip)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_api::storage;
    use serde_json::json;

    fn minimal_snapshot_json() -> serde_json::Value {
        json!({
            "timestamp": 1_700_000_000_000_i64,
            "elements": [],
            "components": []
        })
    }

    #[test]
    fn invalid_page_id_rejects_traversal_and_empty() {
        assert!(invalid_page_id(""));
        assert!(invalid_page_id("../etc/passwd"));
        assert!(invalid_page_id("a/b"));
        assert!(invalid_page_id("a\\b"));
        assert!(!invalid_page_id("settings-general"));
        assert!(!invalid_page_id("active-runs"));
    }

    #[test]
    fn classify_sdk_error_prefix_matches() {
        use spec_check::SnapshotFetchError as E;
        assert!(matches!(
            classify_sdk_error("no SDK app connected"),
            E::NotConnected
        ));
        assert!(matches!(
            classify_sdk_error("request timed out after 5000ms"),
            E::Timeout { .. }
        ));
        assert!(matches!(
            classify_sdk_error("upstream returned forbidden 403"),
            E::Forbidden { .. }
        ));
        assert!(matches!(
            classify_sdk_error("connection reset by peer"),
            E::Network(_)
        ));
    }

    #[test]
    fn snapshot_error_maps_to_expected_status() {
        use spec_check::SnapshotFetchError as E;

        let resp = snapshot_error_to_response(E::NotConnected);
        assert_eq!(resp.status(), StatusCode::FAILED_DEPENDENCY);

        let resp = snapshot_error_to_response(E::Timeout { elapsed_ms: 9 });
        assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);

        let resp = snapshot_error_to_response(E::Partial { from: 1, to: 2 });
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        let resp = snapshot_error_to_response(E::Network("x".into()));
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);

        let resp = snapshot_error_to_response(E::Forbidden { status: 403 });
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);

        let resp = snapshot_error_to_response(E::Malformed("x".into()));
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    #[should_panic(expected = "Stale is folded into success warnings")]
    fn snapshot_error_stale_is_unreachable() {
        let _ = snapshot_error_to_response(spec_check::SnapshotFetchError::Stale { age_ms: 1 });
    }

    #[tokio::test]
    async fn evaluate_one_returns_spec_not_found_for_unknown_page() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let snapshot: UIBridgeSnapshot = serde_json::from_value(minimal_snapshot_json()).unwrap();
        let snapshot = Arc::new(snapshot);
        let entry = BatchPageEntry {
            page_id: "no-such-page".to_string(),
            policy_override: None,
        };
        let r = evaluate_one(&root, &entry, &snapshot, None).await;
        assert_eq!(r.status, "spec-not-found");
        assert!(r.result.is_none());
        assert_eq!(r.page_id, "no-such-page");
    }

    #[tokio::test]
    async fn evaluate_one_rejects_invalid_page_id() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let snapshot: UIBridgeSnapshot = serde_json::from_value(minimal_snapshot_json()).unwrap();
        let snapshot = Arc::new(snapshot);
        let entry = BatchPageEntry {
            page_id: "../escape".to_string(),
            policy_override: None,
        };
        let r = evaluate_one(&root, &entry, &snapshot, None).await;
        assert_eq!(r.status, "eval-error");
        assert_eq!(r.error.as_deref(), Some("invalid-page-id"));
    }

    fn minimal_ir_doc(id: &str) -> qontinui_types::ir::IrPageSpec {
        let raw = json!({
            "version": "1.0",
            "id": id,
            "name": "Batch Page",
            "description": "Minimal IR for batch handler tests",
            "metadata": { "tags": [] },
            "states": [
                {
                    "id": "running",
                    "name": "Running",
                    "description": "Workflow executing",
                    "assertions": [
                        {
                            "id": "running-elem-0",
                            "description": "Required element 0 for state Running",
                            "category": "element-presence",
                            "severity": "critical",
                            "assertionType": "exists",
                            "target": {
                                "type": "search",
                                "criteria": { "role": "button", "textContent": "Stop" },
                                "label": "Required element for Running"
                            },
                            "source": "ai-generated",
                            "reviewed": false,
                            "enabled": true
                        }
                    ],
                    "isInitial": false
                }
            ],
            "transitions": []
        });
        serde_json::from_value(raw).expect("minimal IR doc must deserialize")
    }

    #[tokio::test]
    async fn evaluate_one_evaluates_known_page() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Seed a minimal IR page on disk so read_ir finds it.
        let doc = minimal_ir_doc("batch-page");
        storage::write_ir_and_regenerate(root, &doc).unwrap();

        let snapshot: UIBridgeSnapshot = serde_json::from_value(minimal_snapshot_json()).unwrap();
        let snapshot = Arc::new(snapshot);
        let entry = BatchPageEntry {
            page_id: "batch-page".to_string(),
            policy_override: None,
        };
        let r = evaluate_one(root, &entry, &snapshot, None).await;
        assert_eq!(r.status, "ok");
        let result = r.result.expect("ok status carries a result");
        assert_eq!(result.page_id, "batch-page");
        assert_eq!(result.result_schema_version, 1);
    }

    #[test]
    fn batch_request_deserializes_camel_case_fields() {
        let body = json!({
            "snapshot": { "source": "supplied", "blob": minimal_snapshot_json() },
            "pages": [
                { "pageId": "p1" },
                { "pageId": "p2", "policyOverride": { "conjuncts": [] } }
            ],
            "stream": false
        });
        let req: BatchRequest = serde_json::from_value(body).unwrap();
        assert_eq!(req.snapshot.source, "supplied");
        assert!(req.snapshot.blob.is_some());
        assert_eq!(req.pages.len(), 2);
        assert_eq!(req.pages[0].page_id, "p1");
        assert!(req.pages[1].policy_override.is_some());
        assert!(!req.stream);
    }

    #[test]
    fn spec_check_request_snapshot_is_optional() {
        let req: SpecCheckRequest =
            serde_json::from_value(json!({ "page_id": "settings-general" })).unwrap();
        assert_eq!(req.page_id, "settings-general");
        assert!(req.snapshot.is_none());
    }

    // ------------------------------------------------------------------------
    // Plan 06 — emit-callsite helpers
    // ------------------------------------------------------------------------

    /// Drain any pre-existing events on the broadcast (cross-test
    /// contamination) and recv up to `max_attempts` items, returning the
    /// first one whose `snapshot_id` matches the supplied marker.
    /// Defensive against parallel-test interference on the singleton.
    fn recv_matching_invoked(
        rx: &mut tokio::sync::broadcast::Receiver<SpecApiEvent>,
        marker: &str,
        max_attempts: usize,
    ) -> Option<SpecApiEvent> {
        for _ in 0..max_attempts {
            match rx.try_recv() {
                Ok(ev) => match &ev {
                    SpecApiEvent::SpecCheckInvoked { snapshot_id, .. }
                    | SpecApiEvent::SpecCheckCompleted { snapshot_id, .. }
                        if snapshot_id == marker =>
                    {
                        return Some(ev)
                    }
                    _ => continue,
                },
                Err(_) => return None,
            }
        }
        None
    }

    #[test]
    fn invoke_emit_publishes_spec_check_invoked() {
        let mut rx = events::subscribe();
        let marker = "scs_test_invoke_emit_1";
        invoke_emit(marker, vec!["settings-general".into()], "http");
        let event = recv_matching_invoked(&mut rx, marker, 32)
            .expect("SpecCheckInvoked with marker must arrive");
        match event {
            SpecApiEvent::SpecCheckInvoked {
                page_ids,
                invoked_via,
                ..
            } => {
                assert_eq!(page_ids, vec!["settings-general".to_string()]);
                assert_eq!(invoked_via, "http");
            }
            other => panic!("expected SpecCheckInvoked, got {other:?}"),
        }
    }

    #[test]
    fn completion_counts_rolls_up_match_outcomes() {
        // Hand-construct three SpecCheckResults with deterministic outcomes
        // to exercise the rate-mean + outcome-bucket math.
        let mk = |outcome: MatchOutcome, rate: f32| SpecCheckResult {
            result_schema_version: 1,
            snapshot_id: String::new(),
            spec_content_hash: String::new(),
            spec_version: "1.0".into(),
            page_id: "p".into(),
            state_results: vec![],
            summary: qontinui_types::spec_check::SpecCheckSummary {
                match_outcome: outcome,
                overall_match_rate: rate,
                severity_counts: Default::default(),
                recommended_state: None,
                recommendation_reason: None,
            },
            bridge_fingerprint: qontinui_types::spec_check::BridgeFingerprint {
                app_id: "test".into(),
                app_version: None,
                route: None,
                bridge_version: None,
                snapshot_timestamp: String::new(),
                element_count: 0,
            },
            evaluated_at: String::new(),
            warnings: vec![],
        };
        let a = mk(MatchOutcome::FullMatch, 1.0);
        let b = mk(MatchOutcome::PartialMatch, 0.5);
        let c = mk(MatchOutcome::NoMatch, 0.0);
        let results = vec![&a, &b, &c];
        let (perfect, partial, no_match, eval_error, overall) = completion_counts(&results, 4); // 4 inputs, 3 evaluated → 1 eval_error
        assert_eq!(perfect, 1);
        assert_eq!(partial, 1);
        assert_eq!(no_match, 1);
        assert_eq!(eval_error, 1);
        assert!((overall - 0.5).abs() < f32::EPSILON);

        // Empty results bucket: zero across the board, overall = 0.0.
        let (p, q, n, e, o) = completion_counts(&[], 0);
        assert_eq!((p, q, n, e), (0, 0, 0, 0));
        assert_eq!(o, 0.0);
    }
}
