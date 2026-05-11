//! AI-prefixed handlers.
//!
//! Covers:
//!   - `/ai/search`, `/ai/find`, `/ai/execute`, `/ai/assert`, `/ai/assert-batch`,
//!     `/ai/snapshot`, `/ai/summary`
//!   - `/control/ai/search`, `/control/ai/find` aliases
//!   - Structured action-plan execution: `/control/action-plan` plus the
//!     `/control/action-plan/cache` GET endpoints
//!
//! The action-plan types (`ActionPlanElementTarget`, `PlannedAction`,
//! `ActionPlanRequest`, `PlannedActionResult`, `ActionPlanResponse`,
//! `ActionPlanCacheLookupQuery`, `AiSnapshotQuery`) stay public so external
//! callers importing them from `crate::mcp::ui_bridge::*` still resolve via
//! the `pub use ai::*` re-export in `mod.rs`.

use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::helpers::extract_first_element_id;
use super::request::{ui_bridge_request_sync, wrap_ipc_result};

// ============================================================================
// AI search / find / execute / assert / snapshot / summary
// ============================================================================

/// AI-powered element search.
pub async fn ui_bridge_ai_search_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI search");

    let payload = serde_json::json!({ "params": body });

    wrap_ipc_result(ui_bridge_request_sync(&state, "ai_search", payload).await)
}

/// Request body for `/ai/find` and `/control/ai/find`.
///
/// Captured here only to give the optional `includeHidden` flag a typed
/// home — every other field is still forwarded as-is via the underlying
/// `serde_json::Value` body, so adding new query knobs (e.g. `context`,
/// `confidenceThreshold`) doesn't require a struct edit. `include_hidden`
/// defaults to `true`: callers that omit it match against every
/// registered element regardless of `state.visible`, which is the
/// historical front-end behaviour (the IPC handler hardcoded
/// `SearchEngine({ includeHidden: true })`). Callers that pass
/// `includeHidden: false` opt into the visibility filter.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiFindRequest {
    /// Default true: skip the visibility filter — match elements regardless
    /// of `state.visible`. Pass `false` to apply the visibility filter.
    #[serde(default)]
    pub include_hidden: Option<bool>,
}

/// Query-string params for `/ai/find` and `/control/ai/find`. The body
/// continues to carry the query / context / minConfidence — this struct
/// exists only to give the new `?strict=true` knob a typed home so axum
/// can extract it via `Query<AiFindQuery>`. The body's `"strict": true`
/// is honoured in parallel; either form works.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiFindQuery {
    /// B2 — literal-match-only mode. When `true`, the SDK matcher
    /// suppresses fuzzy candidates; returns `found: false` if no element
    /// matches exactly. Default `false` keeps fuzzy behaviour PLUS
    /// literal-match precedence (exact matches outrank fuzzy candidates
    /// even when `strict` is omitted).
    #[serde(default)]
    pub strict: Option<bool>,
}

/// Natural language element find.
///
/// Callers can pass `"minConfidence": 0.3` in the request body to override
/// the default threshold. The default (0.5) balances precision and recall
/// for common queries like "text input" or "search box".
///
/// Callers can also pass `"includeHidden": false` to apply the visibility
/// filter. Default `true` matches the historical front-end behaviour and
/// is what callers need when driving collapsed sidebars or tabs whose
/// targets are off-screen but still in the registry.
///
/// B2 — `?strict=true` (query string) or `"strict": true` (body) enables
/// literal-match-only mode. The SDK returns only candidates that have a
/// case-insensitive exact match against id / labelText / ariaLabel /
/// textContent / title / placeholder / value / name. No fuzzy fallback.
/// If no element matches literally, the response is `found: false`.
pub async fn ui_bridge_ai_find_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AiFindQuery>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI find");

    // Parse out the include_hidden flag (defaults to false). We keep
    // forwarding the original `body` as `params` so every other field
    // (query, minConfidence, context, confidenceThreshold, …) flows
    // through unchanged; we just normalise `includeHidden` into the
    // forwarded params so downstream handlers always see a concrete
    // boolean rather than "missing" vs "explicit false".
    let parsed: AiFindRequest = serde_json::from_value(body.clone()).unwrap_or_default();
    let include_hidden = parsed.include_hidden.unwrap_or(true);

    // B2 — `strict` can arrive via two channels:
    //   - query string: `?strict=true`
    //   - JSON body:    `{"strict": true}`
    // We OR them together so either form works, then normalise it into the
    // forwarded params so the SDK handler always sees a concrete boolean.
    let strict = query.strict.unwrap_or(false)
        || body
            .get("strict")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

    let mut forwarded_body = body.clone();
    if let Some(obj) = forwarded_body.as_object_mut() {
        obj.insert(
            "includeHidden".to_string(),
            serde_json::Value::Bool(include_hidden),
        );
        obj.insert("strict".to_string(), serde_json::Value::Bool(strict));
    }

    let payload = serde_json::json!({ "params": forwarded_body });

    // Post-process the IPC response to apply the confidence gate before
    // delegating to wrap_ipc_result for the outer envelope (so a frontend
    // soft-failure still flattens to HTTP 400 like every other handler).
    let processed = ui_bridge_request_sync(&state, "ai_find", payload)
        .await
        .map(|mut data| {
            // Confidence gate: if the best match is below threshold, return
            // element: null so callers don't act on a wrong match.  The raw
            // confidence and alternatives are preserved for debugging.
            //
            // Default 0.5 was chosen empirically: exact-match queries score
            // 1.0, good partial matches score 0.7-0.9, and reasonable fuzzy
            // matches (e.g., "search box" → input element) score ~0.5.
            // Callers can override via "minConfidence" in the request body.
            let min_confidence = body
                .get("minConfidence")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5)
                .clamp(0.0, 1.0);
            let min_confidence = if min_confidence == 0.0 {
                0.5
            } else {
                min_confidence
            };
            let inner = data.get("data").unwrap_or(&data);
            let conf = inner
                .get("confidence")
                .and_then(|c| c.as_f64())
                .unwrap_or(0.0);
            if conf < min_confidence && conf > 0.0 {
                info!(
                    "UI Bridge API: ai/find below confidence threshold ({:.2} < {:.2}) — returning null element",
                    conf, min_confidence
                );
                if let Some(obj) = data.as_object_mut() {
                    obj.insert("element".to_string(), serde_json::Value::Null);
                    obj.insert(
                        "belowThreshold".to_string(),
                        serde_json::json!({
                            "originalConfidence": conf,
                            "threshold": min_confidence,
                        }),
                    );
                }
                if let Some(inner_obj) = data.get_mut("data").and_then(|d| d.as_object_mut()) {
                    inner_obj.insert("element".to_string(), serde_json::Value::Null);
                    inner_obj.insert(
                        "belowThreshold".to_string(),
                        serde_json::json!({
                            "originalConfidence": conf,
                            "threshold": min_confidence,
                        }),
                    );
                }
            }
            data
        });
    wrap_ipc_result(processed)
}

/// Natural language action execution.
pub async fn ui_bridge_ai_execute_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI execute");
    let payload = serde_json::json!({ "params": body });
    wrap_ipc_result(ui_bridge_request_sync(&state, "ai_execute", payload).await)
}

/// AI assertion evaluation.
pub async fn ui_bridge_ai_assert_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI assert");
    let payload = serde_json::json!({ "params": body });
    wrap_ipc_result(ui_bridge_request_sync(&state, "ai_assert", payload).await)
}

/// Batch AI assertion evaluation.
pub async fn ui_bridge_ai_assert_batch_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI assert batch");
    let payload = serde_json::json!({ "params": body });
    wrap_ipc_result(ui_bridge_request_sync(&state, "ai_assert_batch", payload).await)
}

/// Query parameters for the AI snapshot endpoint.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AiSnapshotQuery {
    /// Maximum token budget for the snapshot (0 = unlimited).
    /// When set, elements are pruned by region priority.
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

/// Semantic AI snapshot of the page.
pub async fn ui_bridge_ai_snapshot_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AiSnapshotQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: AI snapshot (maxTokens={:?})",
        query.max_tokens
    );
    let mut payload = serde_json::json!({});
    if let Some(max_tokens) = query.max_tokens {
        payload["maxTokens"] = serde_json::json!(max_tokens);
    }
    wrap_ipc_result(ui_bridge_request_sync(&state, "ai_snapshot", payload).await)
}

/// Natural language page summary.
pub async fn ui_bridge_ai_summary_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI summary");
    wrap_ipc_result(ui_bridge_request_sync(&state, "ai_summary", serde_json::json!({})).await)
}

// ============================================================================
// Structured Action Plan Execution
// ============================================================================

/// A target specification for finding an element.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPlanElementTarget {
    /// Direct element ID from a prior snapshot
    #[serde(default)]
    pub element_id: Option<String>,
    /// data-testid attribute value
    #[serde(default)]
    pub test_id: Option<String>,
    /// Natural language description for fuzzy search
    #[serde(default)]
    pub search_text: Option<String>,
    /// Element type hint (e.g., "button", "input")
    #[serde(default)]
    pub element_type: Option<String>,
    /// CSS selector
    #[serde(default)]
    pub selector: Option<String>,
}

/// A single planned action in an action plan.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedAction {
    /// Action type to execute (click, type, select, etc.)
    pub action: String,
    /// How to find the target element
    pub target: ActionPlanElementTarget,
    /// LLM's reasoning for this action (audit trail)
    #[serde(default)]
    pub reasoning: Option<String>,
    /// LLM's confidence (0.0–1.0)
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    /// Action-specific parameters (text, value, direction, etc.)
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    /// Separates generic intent from specific data (for caching).
    /// Example: "What email should be entered?"
    #[serde(default)]
    pub user_detail_query: Option<String>,
    /// The specific data for the query.
    /// Example: "test@example.com"
    #[serde(default)]
    pub user_detail_answer: Option<String>,
}

fn default_confidence() -> f64 {
    1.0
}

/// Request to execute a structured action plan.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPlanRequest {
    /// Ordered list of actions to execute
    pub actions: Vec<PlannedAction>,
    /// High-level goal this plan achieves
    #[serde(default)]
    pub goal: Option<String>,
    /// Minimum confidence threshold — actions below are skipped (default: 0.5)
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,
    /// Whether to stop on first action failure (default: true)
    #[serde(default = "default_stop_on_failure")]
    pub stop_on_failure: bool,
    /// Page URL for cache keying (when provided, enables plan caching)
    #[serde(default)]
    pub page_url: Option<String>,
    /// Element snapshot for cache fingerprinting (array of {id, type, role, label})
    #[serde(default)]
    pub element_snapshot: Option<serde_json::Value>,
}

fn default_confidence_threshold() -> f64 {
    0.5
}

fn default_stop_on_failure() -> bool {
    true
}

/// Result of a single planned action execution.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedActionResult {
    pub index: usize,
    pub success: bool,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_element_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub skipped_low_confidence: bool,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element_state: Option<serde_json::Value>,
}

/// Aggregated result of executing a full action plan.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPlanResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    pub results: Vec<PlannedActionResult>,
    pub executed_count: usize,
    pub skipped_count: usize,
    pub failed_count: usize,
    pub total_duration_ms: u64,
    /// Whether this plan was stored in the cache for future reuse
    #[serde(default)]
    pub cached: bool,
}

/// Execute a structured action plan: an ordered sequence of typed UI actions.
pub async fn ui_bridge_execute_action_plan_handler(
    State(state): State<Arc<ApiState>>,
    Json(plan): Json<ActionPlanRequest>,
) -> Result<Json<ApiResponse<ActionPlanResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let total_start = Instant::now();
    let action_count = plan.actions.len();

    if action_count == 0 {
        return Ok(Json(ApiResponse::success(ActionPlanResponse {
            success: true,
            goal: plan.goal,
            results: vec![],
            executed_count: 0,
            skipped_count: 0,
            failed_count: 0,
            total_duration_ms: 0,
            cached: false,
        })));
    }

    if action_count > 50 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "Action plan has {} actions, maximum is 50",
                action_count
            ))),
        ));
    }

    info!(
        "UI Bridge API: Executing action plan with {} actions (goal: {:?})",
        action_count, plan.goal
    );

    let mut results = Vec::with_capacity(action_count);
    let mut executed_count = 0usize;
    let mut skipped_count = 0usize;
    let mut failed_count = 0usize;
    let mut all_success = true;

    for (i, planned) in plan.actions.iter().enumerate() {
        let action_start = Instant::now();

        // Skip low-confidence actions
        if planned.confidence < plan.confidence_threshold {
            info!(
                "UI Bridge action plan: Skipping action {} ({}) — confidence {:.2} < threshold {:.2}",
                i, planned.action, planned.confidence, plan.confidence_threshold
            );
            skipped_count += 1;
            results.push(PlannedActionResult {
                index: i,
                success: true,
                action: planned.action.clone(),
                resolved_element_id: None,
                error: None,
                skipped_low_confidence: true,
                duration_ms: 0,
                element_state: None,
            });
            continue;
        }

        // Handle non-element actions (navigate, wait)
        if planned.action == "navigate" {
            let url = planned
                .params
                .as_ref()
                .and_then(|p| p.get("url"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let result =
                ui_bridge_request_sync(&state, "navigate", serde_json::json!({ "url": url })).await;
            let duration = action_start.elapsed().as_millis() as u64;
            let (success, error) = match result {
                Ok(_) => (true, None),
                Err(e) => (false, Some(e)),
            };
            if !success {
                all_success = false;
                failed_count += 1;
            } else {
                executed_count += 1;
            }
            results.push(PlannedActionResult {
                index: i,
                success,
                action: "navigate".into(),
                resolved_element_id: None,
                error,
                skipped_low_confidence: false,
                duration_ms: duration,
                element_state: None,
            });
            if !success && plan.stop_on_failure {
                break;
            }
            continue;
        }

        if planned.action == "wait" {
            let ms = planned
                .params
                .as_ref()
                .and_then(|p| p.get("ms"))
                .and_then(|v| v.as_u64())
                .unwrap_or(1000)
                .min(30000); // Cap at 30s
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            executed_count += 1;
            results.push(PlannedActionResult {
                index: i,
                success: true,
                action: "wait".into(),
                resolved_element_id: None,
                error: None,
                skipped_low_confidence: false,
                duration_ms: ms,
                element_state: None,
            });
            continue;
        }

        // Resolve element ID from target specification
        let resolved_id = resolve_action_plan_target(&state, &planned.target).await;

        let resolved_id = match resolved_id {
            Ok(id) => id,
            Err(e) => {
                let duration = action_start.elapsed().as_millis() as u64;
                all_success = false;
                failed_count += 1;
                results.push(PlannedActionResult {
                    index: i,
                    success: false,
                    action: planned.action.clone(),
                    resolved_element_id: None,
                    error: Some(format!("Element resolution failed: {}", e)),
                    skipped_low_confidence: false,
                    duration_ms: duration,
                    element_state: None,
                });
                if plan.stop_on_failure {
                    break;
                }
                continue;
            }
        };

        // Execute the action via standard IPC
        let action_payload = serde_json::json!({
            "elementId": resolved_id,
            "action": {
                "action": planned.action,
                "params": planned.params,
            }
        });

        let result = ui_bridge_request_sync(&state, "execute_action", action_payload).await;
        let duration = action_start.elapsed().as_millis() as u64;

        match result {
            Ok(data) => {
                let action_success = data
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let element_state = data.get("elementState").cloned();
                let action_error = if action_success {
                    None
                } else {
                    data.get("error")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                };

                if !action_success {
                    all_success = false;
                    failed_count += 1;
                } else {
                    executed_count += 1;
                }

                results.push(PlannedActionResult {
                    index: i,
                    success: action_success,
                    action: planned.action.clone(),
                    resolved_element_id: Some(resolved_id.clone()),
                    error: action_error,
                    skipped_low_confidence: false,
                    duration_ms: duration,
                    element_state,
                });

                if !action_success && plan.stop_on_failure {
                    break;
                }
            }
            Err(e) => {
                all_success = false;
                failed_count += 1;
                results.push(PlannedActionResult {
                    index: i,
                    success: false,
                    action: planned.action.clone(),
                    resolved_element_id: Some(resolved_id.clone()),
                    error: Some(e),
                    skipped_low_confidence: false,
                    duration_ms: duration,
                    element_state: None,
                });
                if plan.stop_on_failure {
                    break;
                }
            }
        }
    }

    let total_duration = total_start.elapsed().as_millis() as u64;
    info!(
        "UI Bridge action plan complete: {}/{} succeeded, {} skipped, {} failed ({}ms)",
        executed_count, action_count, skipped_count, failed_count, total_duration
    );

    // Cache successful plans for future reuse
    let mut was_cached = false;
    if all_success {
        if let (Some(ref url), Some(ref snapshot)) = (&plan.page_url, &plan.element_snapshot) {
            if let Some((norm_url, fingerprint)) =
                crate::mcp::action_plan_cache::ActionPlanCache::build_key(url, snapshot)
            {
                let plan_json = serde_json::to_value(&plan.actions).unwrap_or_default();
                crate::mcp::action_plan_cache::global_action_plan_cache().put(
                    &norm_url,
                    &fingerprint,
                    plan_json,
                    plan.goal.clone(),
                );
                was_cached = true;
            }
        }
    } else if let (Some(ref url), Some(ref snapshot)) = (&plan.page_url, &plan.element_snapshot) {
        // Mark failed plans so they're not reused
        if let Some((norm_url, fingerprint)) =
            crate::mcp::action_plan_cache::ActionPlanCache::build_key(url, snapshot)
        {
            crate::mcp::action_plan_cache::global_action_plan_cache()
                .mark_failed(&norm_url, &fingerprint);
        }
    }

    Ok(Json(ApiResponse::success(ActionPlanResponse {
        success: all_success,
        goal: plan.goal,
        results,
        executed_count,
        skipped_count,
        failed_count,
        total_duration_ms: total_duration,
        cached: was_cached,
    })))
}

/// Resolve an element target to a concrete element ID.
pub(super) async fn resolve_action_plan_target(
    state: &Arc<ApiState>,
    target: &ActionPlanElementTarget,
) -> Result<String, String> {
    // 1. Direct element ID
    if let Some(ref id) = target.element_id {
        return Ok(id.clone());
    }

    // 2. Find by data-testid attribute
    if let Some(ref test_id) = target.test_id {
        let find_payload = serde_json::json!({
            "testId": test_id
        });
        if let Ok(data) = ui_bridge_request_sync(state, "find", find_payload).await {
            if let Some(id) = extract_first_element_id(&data) {
                return Ok(id);
            }
        }
    }

    // 3. Find by CSS selector
    if let Some(ref selector) = target.selector {
        let find_payload = serde_json::json!({
            "selector": selector
        });
        if let Ok(data) = ui_bridge_request_sync(state, "find", find_payload).await {
            if let Some(id) = extract_first_element_id(&data) {
                return Ok(id);
            }
        }
    }

    // 4. Fuzzy search by text + element type
    if let Some(ref text) = target.search_text {
        let mut find_payload = serde_json::json!({
            "text": text,
            "fuzzy": true
        });
        if let Some(ref el_type) = target.element_type {
            find_payload["element_type"] = serde_json::json!(el_type);
        }
        if let Ok(data) = ui_bridge_request_sync(state, "find", find_payload).await {
            if let Some(id) = extract_first_element_id(&data) {
                return Ok(id);
            }
        }
    }

    Err("No element target specified or element not found".to_string())
}

// ============================================================================
// Action Plan Cache Endpoints
// ============================================================================

/// Query params for action plan cache lookup.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPlanCacheLookupQuery {
    /// Page URL to look up
    pub url: String,
    /// Element snapshot JSON (array of {id, type, role, label})
    pub elements: String,
}

/// Look up a cached action plan by page URL and element fingerprint.
pub async fn ui_bridge_action_plan_cache_lookup_handler(
    Query(query): Query<ActionPlanCacheLookupQuery>,
) -> Json<ApiResponse<serde_json::Value>> {
    let elements: serde_json::Value = match serde_json::from_str(&query.elements) {
        Ok(v) => v,
        Err(_) => {
            return Json(ApiResponse::error("Invalid elements JSON".to_string()));
        }
    };

    if let Some((norm_url, fingerprint)) =
        crate::mcp::action_plan_cache::ActionPlanCache::build_key(&query.url, &elements)
    {
        if let Some(cached) =
            crate::mcp::action_plan_cache::global_action_plan_cache().get(&norm_url, &fingerprint)
        {
            return Json(ApiResponse::success(serde_json::json!({
                "hit": true,
                "plan": cached.plan,
                "goal": cached.goal,
                "hitCount": cached.hit_count,
                "lastSuccess": cached.last_success,
            })));
        }
    }

    Json(ApiResponse::success(serde_json::json!({ "hit": false })))
}

/// Get action plan cache statistics.
pub async fn ui_bridge_action_plan_cache_stats_handler() -> Json<ApiResponse<serde_json::Value>> {
    let stats = crate::mcp::action_plan_cache::global_action_plan_cache().stats();
    Json(ApiResponse::success(stats))
}

// ============================================================================
// Routes + manifest
// ============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        // AI search & find (control + ai)
        .route(
            "/ui-bridge/control/ai/search",
            post(ui_bridge_ai_search_handler),
        )
        .route(
            "/ui-bridge/control/ai/find",
            post(ui_bridge_ai_find_handler),
        )
        .route("/ui-bridge/ai/search", post(ui_bridge_ai_search_handler))
        .route("/ui-bridge/ai/find", post(ui_bridge_ai_find_handler))
        .route("/ui-bridge/ai/execute", post(ui_bridge_ai_execute_handler))
        .route("/ui-bridge/ai/assert", post(ui_bridge_ai_assert_handler))
        .route(
            "/ui-bridge/ai/assert-batch",
            post(ui_bridge_ai_assert_batch_handler),
        )
        // SDK declares the slash-form /ai/assert/batch — same handler as the
        // hyphen-form above, mounted as an alias for symmetry with the contract.
        .route(
            "/ui-bridge/ai/assert/batch",
            post(ui_bridge_ai_assert_batch_handler),
        )
        .route("/ui-bridge/ai/snapshot", get(ui_bridge_ai_snapshot_handler))
        .route("/ui-bridge/ai/summary", get(ui_bridge_ai_summary_handler))
        // Action plan execution & caching
        .route(
            "/ui-bridge/control/action-plan",
            post(ui_bridge_execute_action_plan_handler),
        )
        .route(
            "/ui-bridge/control/action-plan/cache",
            get(ui_bridge_action_plan_cache_lookup_handler),
        )
        .route(
            "/ui-bridge/control/action-plan/cache/stats",
            get(ui_bridge_action_plan_cache_stats_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("POST", "/ui-bridge/control/ai/search"),
        ("POST", "/ui-bridge/control/ai/find"),
        ("POST", "/ui-bridge/ai/search"),
        ("POST", "/ui-bridge/ai/find"),
        ("POST", "/ui-bridge/ai/execute"),
        ("POST", "/ui-bridge/ai/assert"),
        ("POST", "/ui-bridge/ai/assert-batch"),
        ("POST", "/ui-bridge/ai/assert/batch"),
        ("GET", "/ui-bridge/ai/snapshot"),
        ("GET", "/ui-bridge/ai/summary"),
        ("POST", "/ui-bridge/control/action-plan"),
        ("GET", "/ui-bridge/control/action-plan/cache"),
        ("GET", "/ui-bridge/control/action-plan/cache/stats"),
    ]
}
