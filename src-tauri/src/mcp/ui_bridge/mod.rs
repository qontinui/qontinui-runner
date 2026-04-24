//! UI Bridge handlers for MCP API
//!
//! Provides HTTP handlers for UI Bridge control (React UI automation)
//! and UI Bridge exploration (qontinui library via Python bridge).
//!
//! This module was extracted from a single 12.7kLOC file. Foundation
//! pieces (request types, error taxonomy, circuit breaker, core IPC
//! request machinery) live in dedicated submodules; the per-endpoint
//! HTTP handlers and `routes()` composer are still inline in this file
//! and will be extracted in subsequent passes.

pub mod ai;
pub mod ai_analyze;
pub mod analytics;
pub mod bookmarks;
pub mod capabilities;
pub mod circuit_breaker;
pub mod design;
pub mod design_eval;
pub mod elements;
pub mod errors;
pub mod exploration;
pub mod forms;
pub mod helpers;
pub mod history;
pub mod intents;
pub mod intents_registry;
pub mod misc;
pub mod network;
pub mod page;
pub mod request;
pub mod screenshots;
pub mod state_machine;
pub mod stubs;
pub mod types;

// Re-export all public symbols so every currently-used path like
// `crate::mcp::ui_bridge::UiBridgeError`, `::ui_bridge_request_sync`,
// `::handle_ui_bridge_response`, `::UiBridgeCircuitBreaker` still resolves.
pub use ai::{
    ui_bridge_action_plan_cache_lookup_handler, ui_bridge_action_plan_cache_stats_handler,
    ui_bridge_ai_assert_batch_handler, ui_bridge_ai_assert_handler, ui_bridge_ai_execute_handler,
    ui_bridge_ai_find_handler, ui_bridge_ai_search_handler, ui_bridge_ai_snapshot_handler,
    ui_bridge_ai_summary_handler, ui_bridge_execute_action_plan_handler,
    ActionPlanCacheLookupQuery, ActionPlanElementTarget, ActionPlanRequest, ActionPlanResponse,
    AiSnapshotQuery, PlannedAction, PlannedActionResult,
};
pub use analytics::{
    analytics_action_baselines_handler, analytics_annotation_gaps_handler,
    analytics_decay_curve_handler, analytics_failure_taxonomy_handler,
    analytics_fragility_heatmap_handler, analytics_health_score_handler,
    analytics_intervention_handler, analytics_recommendations_handler,
    analytics_regressions_handler, analytics_stall_frequency_handler,
    analytics_state_coverage_handler, AnalyticsDaysQuery, AnnotationGapQuery, DecayCurveQuery,
    StateCoverageQuery,
};
pub use bookmarks::{
    ui_bridge_categorize_last_diff_handler, ui_bridge_delete_bookmark_handler,
    ui_bridge_diff_from_bookmark_handler, ui_bridge_disable_change_buffer_handler,
    ui_bridge_drain_change_buffer_handler, ui_bridge_enable_change_buffer_handler,
    ui_bridge_execute_with_diff_handler, ui_bridge_get_bookmark_handler,
    ui_bridge_get_change_buffer_size_handler, ui_bridge_list_bookmarks_handler,
    ui_bridge_save_bookmark_handler, ui_bridge_scoped_diff_handler,
    ui_bridge_structured_changes_handler, ui_bridge_summarize_diff_handler,
    ui_bridge_wait_for_change_handler, ui_bridge_with_diff_handler,
};
pub use capabilities::{
    ui_bridge_append_render_log_handler, ui_bridge_batch_handler, ui_bridge_capabilities_handler,
    ui_bridge_control_batch_execute_handler, ui_bridge_control_batch_handler,
    ui_bridge_expect_text_handler, ui_bridge_get_action_history_handler,
    ui_bridge_get_element_state_handler, ui_bridge_get_interaction_metrics_handler,
    ui_bridge_get_keyboard_shortcuts_handler, ui_bridge_get_render_log_handler,
    ui_bridge_get_workflow_status_handler, ui_bridge_get_workflows_handler,
    ui_bridge_ipc_response_handler, ui_bridge_pong_handler, ui_bridge_routes_manifest_handler,
    ui_bridge_run_workflow_handler, ui_bridge_structured_assert_handler, AssertResult,
    BatchOperation, BatchOperationResult, BatchRequest, BatchResponse, StructuredAssertRequest,
};
pub use circuit_breaker::{CircuitBreakerState, UiBridgeCircuitBreaker};
pub use design::{
    ui_bridge_design_audit_handler, ui_bridge_design_clear_style_guide_handler,
    ui_bridge_design_element_styles_handler, ui_bridge_design_get_style_guide_handler,
    ui_bridge_design_load_style_guide_handler, ui_bridge_design_responsive_handler,
    ui_bridge_design_snapshot_handler, ui_bridge_design_state_styles_handler,
};
pub use elements::{
    ui_bridge_assert_element_handler, ui_bridge_batch_actions_handler,
    ui_bridge_click_by_selector_handler, ui_bridge_click_by_text_handler,
    ui_bridge_discover_handler, ui_bridge_execute_action_handler,
    ui_bridge_execute_component_action_handler, ui_bridge_find_by_text_handler,
    ui_bridge_find_handler, ui_bridge_get_component_handler, ui_bridge_get_components_handler,
    ui_bridge_get_element_handler, ui_bridge_get_elements_handler,
    ui_bridge_get_last_discovered_handler, ui_bridge_get_snapshot_handler,
    ui_bridge_read_value_handler, ui_bridge_type_into_handler,
    ui_bridge_wait_for_element_handler, ActionQueryParams,
};
pub use errors::{
    ui_bridge_capture_error_baseline_handler, ui_bridge_circuit_breaker_reset_handler,
    ui_bridge_compare_error_baseline_handler, ui_bridge_diagnostics_handler,
    ui_bridge_end_error_session_handler, ui_bridge_get_error_report_handler,
    ui_bridge_get_error_sessions_handler, ui_bridge_get_error_snapshots_handler,
    ui_bridge_get_health_report_handler, ui_bridge_get_idle_signal_handler,
    ui_bridge_get_idle_status_handler, ui_bridge_health_signals_handler,
    ui_bridge_readiness_handler, ui_bridge_start_error_session_handler, ErrorBaselineRequest,
    ErrorSessionStartRequest, ErrorSnapshotsQuery, UiBridgeHealthSignals,
};
pub use exploration::{
    discover_states_from_renders, get_ui_bridge_exploration_results,
    get_ui_bridge_exploration_status, start_ui_bridge_exploration, stop_ui_bridge_exploration,
    ui_bridge_list_windows_handler, WindowInfo,
};
pub use forms::{
    ui_bridge_clipboard_read_handler, ui_bridge_clipboard_write_handler,
    ui_bridge_diff_forms_handler, ui_bridge_fill_form_handler, ui_bridge_get_forms_handler,
    ui_bridge_snapshot_forms_handler,
};
pub use history::{
    ui_bridge_element_reliability_handler, ui_bridge_history_element_handler,
    ui_bridge_history_elements_handler, ui_bridge_history_flaky_handler, ElementReliabilityQuery,
    FlakyElementsQuery, HistoryElementQuery, HistoryElementsQuery,
};
pub use intents::{
    ui_bridge_wait_for_element_condition_handler, ui_bridge_wait_for_element_stable_handler,
    ui_bridge_wait_for_element_state_handler, ui_bridge_wait_for_idle_handler,
    ui_bridge_wait_for_idle_signal_handler, ui_bridge_wait_for_navigation_handler,
    ui_bridge_wait_for_route_handler, ui_bridge_wait_for_targets_handler,
};
pub use network::{
    ui_bridge_clear_console_errors_handler, ui_bridge_get_browser_events_handler,
    ui_bridge_get_console_errors_handler, ui_bridge_get_network_chains_handler,
    ui_bridge_get_network_request_handler, ui_bridge_get_network_requests_handler,
    ui_bridge_get_network_requests_in_flight_handler, ui_bridge_get_timeline_handler,
    ui_bridge_wait_for_network_request_handler, BrowserEventsQuery, ConsoleErrorsQuery,
    NetworkChainsQuery, NetworkRequestsQuery, TimelineQuery,
};
pub use page::{
    ui_bridge_activate_tab_handler, ui_bridge_navigate_and_wait_handler,
    ui_bridge_page_close_request_handler, ui_bridge_page_evaluate_batch_handler,
    ui_bridge_page_evaluate_handler, ui_bridge_page_evaluate_raw_handler,
    ui_bridge_page_evaluate_safe_handler, ui_bridge_page_go_back_handler,
    ui_bridge_page_go_forward_handler, ui_bridge_page_hard_refresh_handler,
    ui_bridge_page_navigate_handler, ui_bridge_page_refresh_handler,
    ui_bridge_page_set_tab_handler, ui_bridge_page_summary_handler,
    ui_bridge_query_selector_handler, ui_bridge_tab_activate_handler,
    ui_bridge_tabs_list_handler, BatchEvaluateRequest, BatchExpression, BatchExpressionResult,
    NavigateAndWaitRequest, PageEvaluateRequest, PageNavigateRequest, QuerySelectorRequest,
    SetTabRequest, SetTabResponse, TabActivateRequest,
};
pub use request::{handle_ui_bridge_response, ui_bridge_request_sync};
pub use screenshots::{
    capture_runner_window_base64, ui_bridge_annotated_screenshot_handler,
    ui_bridge_annotations_coverage_handler, ui_bridge_annotations_create_handler,
    ui_bridge_annotations_delete_handler, ui_bridge_annotations_export_handler,
    ui_bridge_annotations_get_handler, ui_bridge_annotations_list_handler,
    ui_bridge_annotations_update_handler, ui_bridge_capture_element_images_handler,
    ui_bridge_diagnose_stuck_screen_handler, ui_bridge_element_screenshot_handler,
    ui_bridge_get_element_images_handler, ui_bridge_media_analyze_handler,
    ui_bridge_media_audit_handler, ui_bridge_media_find_handler,
    ui_bridge_media_snapshot_handler, ui_bridge_page_health_handler,
    AnnotatedScreenshotData, AnnotatedScreenshotQuery, PageHealthRequest,
};
pub use types::{
    classify_assertion_failure, classify_transport_error, recovery_hint_for, ClipboardWriteRequest,
    DiscoverStatesRequest, RecoveryHint, StartUIBridgeExplorationRequest, UIBridgeActionRequest,
    UIBridgeComponentActionRequest, UIBridgeDiscoveryRequest, UIBridgeExplorationStatusRequest,
    UiBridgeError, UiBridgeErrorCode,
};

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, info, warn};

use tauri::Emitter;

use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, api_error_detailed, ApiResponse, ApiState};
use crate::screen;

// Internal re-imports for inline handlers that still live in this file.
// `request::get_ui_bridge_timeout_ms` and `request::wrap_ipc_result` are
// `pub(super)` helpers the inline handlers depend on.
use request::{gather_readiness_diagnostics, get_ui_bridge_timeout_ms, wrap_ipc_result};

// Shared helpers (JS evaluation, snapshot diffing, response extractors)
// live in `helpers.rs` so per-family handler extractions can pull them in
// without dragging the rest of `mod.rs` along.
use helpers::{
    compute_snapshot_diff, count_elements_in_discover_payload, direct_webview_evaluate_with_result,
    evaluate_js_expression, extract_ai_find_match, extract_first_element_id,
    extract_get_element_match, filter_element_fields, glob_match, safe_evaluate,
    snapshot_signature,
};

/// Maximum age of the last-discovered cache served to callers, in seconds.
/// Past this age the endpoint reports the entry as stale but still returns
/// it (callers can decide what to do based on `age_ms`).
const LAST_DISCOVERED_FRESH_SECS: u64 = 60;

// Undo/redo, specs, component-state, scroll, performance-entries,
// annotations-import, debug element-tree/highlight, navigate-tab, and
// clear-storage handlers live in `misc::routes()` — see `misc.rs`.

// ============================================================================
// Page Navigation Handlers
// ============================================================================


// ============================================================================
// Direct tab navigation moved to `page::routes()` — see `page.rs`.
// ============================================================================


// ============================================================================
// Exploration Handlers
// ============================================================================


// ============================================================================
// Idle Detection Handlers
// ============================================================================

// ============================================================================
// State machine routes (IPC to webview SDK)
// ============================================================================

macro_rules! ipc_handler_get {
    ($fn_name:ident, $ipc_type:expr) => {
        pub async fn $fn_name(
            State(state): State<Arc<ApiState>>,
        ) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
            match ui_bridge_request_sync(&state, $ipc_type, serde_json::json!({})).await {
                Ok(data) => Ok(Json(ApiResponse::success(data))),
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
            }
        }
    };
}

macro_rules! ipc_handler_post {
    ($fn_name:ident, $ipc_type:expr) => {
        pub async fn $fn_name(
            State(state): State<Arc<ApiState>>,
            Json(body): Json<serde_json::Value>,
        ) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
            let payload = serde_json::json!({ "params": body });
            match ui_bridge_request_sync(&state, $ipc_type, payload).await {
                Ok(data) => Ok(Json(ApiResponse::success(data))),
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
            }
        }
    };
}

macro_rules! ipc_handler_path_get {
    ($fn_name:ident, $ipc_type:expr, $param_name:expr) => {
        pub async fn $fn_name(
            State(state): State<Arc<ApiState>>,
            axum::extract::Path(id): axum::extract::Path<String>,
        ) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
            let payload = serde_json::json!({ "params": { $param_name: id } });
            match ui_bridge_request_sync(&state, $ipc_type, payload).await {
                Ok(data) => Ok(Json(ApiResponse::success(data))),
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
            }
        }
    };
}

macro_rules! ipc_handler_path_post {
    ($fn_name:ident, $ipc_type:expr, $param_name:expr) => {
        pub async fn $fn_name(
            State(state): State<Arc<ApiState>>,
            axum::extract::Path(id): axum::extract::Path<String>,
            Json(body): Json<serde_json::Value>,
        ) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
            let mut params = body;
            if let Some(obj) = params.as_object_mut() {
                obj.insert($param_name.to_string(), serde_json::json!(id));
            } else {
                // Body wasn't an object; create one with just the path param
                params = serde_json::json!({ $param_name: id });
            }
            let payload = serde_json::json!({ "params": params });
            match ui_bridge_request_sync(&state, $ipc_type, payload).await {
                Ok(data) => Ok(Json(ApiResponse::success(data))),
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
            }
        }
    };
}

// Re-export macros so per-family submodules can import them via
// `use super::{ipc_handler_get, ipc_handler_post, ...};`. The macros expand
// to code referencing `State`, `Arc<ApiState>`, `Json`, `StatusCode`,
// `ApiResponse`, `api_error`, and `ui_bridge_request_sync` — callers must
// have those symbols in scope at the call site.
pub(crate) use ipc_handler_get;
pub(crate) use ipc_handler_path_get;
pub(crate) use ipc_handler_path_post;
pub(crate) use ipc_handler_post;

// Per-family submodules host the rest of the macro-generated handlers:
//   - state_machine.rs   (states/, state/, state-groups/, transitions/)
//   - ai_analyze.rs      (ai/analyze/*, ai/semantic-search, ai/diff, media-compare, image-diff)
//   - design_eval.rs     (control/design/evaluate/*)
//   - intents_registry.rs (control/intents/*)
//   - misc.rs            (undo/redo, specs, component-state, scroll,
//                         performance-entries, annotations-import, debug,
//                         navigate-tab, clear-storage)

// =========================================================================
// Convenience endpoints — app-agnostic DOM interaction helpers
// =========================================================================

/// Static manifest of every UI Bridge route registered by `routes()`. Kept
/// in sync by hand — adding a new `.route(...)` call below should be paired
/// with a new `(method, path)` entry here. The `_routes` endpoint reads from
/// this list.
///
/// When a path supports multiple methods (e.g. GET+POST on the same URL),
/// list each method as a separate tuple.
///
/// Returns the in-file entries concatenated with `bookmarks::route_entries()`
/// (and any future per-family extraction). The OnceLock keeps the resulting
/// slice `'static` so existing callers that pass it around without cloning
/// continue to compile.
pub(super) fn route_manifest() -> &'static [(&'static str, &'static str)] {
    use std::sync::OnceLock;
    static MANIFEST: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        let mut all: Vec<(&'static str, &'static str)> = Vec::new();
        all.extend_from_slice(local_route_entries());
        all.extend_from_slice(ai::route_entries());
        all.extend_from_slice(ai_analyze::route_entries());
        all.extend_from_slice(bookmarks::route_entries());
        all.extend_from_slice(capabilities::route_entries());
        all.extend_from_slice(design::route_entries());
        all.extend_from_slice(design_eval::route_entries());
        all.extend_from_slice(elements::route_entries());
        all.extend_from_slice(errors::route_entries());
        all.extend_from_slice(exploration::route_entries());
        all.extend_from_slice(forms::route_entries());
        all.extend_from_slice(intents::route_entries());
        all.extend_from_slice(intents_registry::route_entries());
        all.extend_from_slice(misc::route_entries());
        all.extend_from_slice(network::route_entries());
        all.extend_from_slice(page::route_entries());
        all.extend_from_slice(screenshots::route_entries());
        all.extend_from_slice(state_machine::route_entries());
        all.extend_from_slice(stubs::route_entries());
        all
    })
}

/// Routes that are still defined inline in `mod.rs::routes()`.
/// Per-family extractions append their own `route_entries()` via
/// `route_manifest()` above; do not list those entries here.
fn local_route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        // Persisted interaction history (cross-run analysis)
        ("GET", "/ui-bridge/history/elements"),
        ("GET", "/ui-bridge/history/element/{id}"),
        ("GET", "/ui-bridge/history/flaky"),
        ("GET", "/ui-bridge/graph/element-reliability"),
        // Analytics
        ("GET", "/ui-bridge/analytics/decay-curve"),
        ("GET", "/ui-bridge/analytics/action-baselines"),
        ("GET", "/ui-bridge/analytics/failure-taxonomy"),
        ("GET", "/ui-bridge/analytics/fragility-heatmap"),
        ("GET", "/ui-bridge/analytics/regressions"),
        ("GET", "/ui-bridge/analytics/stall-frequency"),
        ("GET", "/ui-bridge/analytics/intervention-effectiveness"),
        ("GET", "/ui-bridge/analytics/state-coverage"),
        ("GET", "/ui-bridge/analytics/annotation-gaps"),
        ("GET", "/ui-bridge/analytics/health-score"),
        ("GET", "/ui-bridge/analytics/recommendations"),
        // Phase 3I.1 + 3I.2 — UI Bridge invoke proxy
        ("GET", "/ui-bridge/commands"),
        ("POST", "/ui-bridge/invoke/{command_name}"),
    ]
}

pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        // Endpoint discovery (`/_routes`, `/_help`) lives in
        // `capabilities::routes()` — see `capabilities.rs`.
        // Primary element operations (get/get-by-id/assert/execute-action/
        // batch-actions), components, discovery/snapshot, wait-for-element
        // (control + ai aliases), find, and the DOM-convenience family
        // (find-by-text / click-by-text / click-by-selector / read-value /
        // type-into) live in `elements::routes()` — see `elements.rs`.
        // `/control/batch` (step-level diff batch) lives in
        // `capabilities::routes()`.
        // `/control/annotated-screenshot` lives in `screenshots::routes()`.
        // Undo/redo + specs handlers (extracted to misc.rs) merged below.
        // Form state awareness, /ai/* form aliases, and clipboard live in
        // `forms::routes()` — see `forms.rs`.
        // /ai/design-audit lives in `design::routes()` — see `design.rs`.
        // Network request monitoring, console errors, browser events, and
        // timeline live in `network::routes()` — see `network.rs`.
        // Page navigation, evaluation, tab switching, navigate-and-wait, and
        // page summary live in `page::routes()` — see `page.rs`.
        // Convenience DOM interaction endpoints (find-by-text / click-by-text /
        // click-by-selector / read-value / type-into) plus `/control/find` and
        // the `/control/wait-for-element` + `/ai/wait-for-element` pair live
        // in `elements::routes()` — see `elements.rs`.
        // `/control/assert` (structured assert) lives in
        // `capabilities::routes()`.
        // Design Review (manual handlers + /ai/design-audit alias) live in
        // `design::routes()` — see `design.rs`. Macro-generated
        // `design/evaluate*` handlers stay in this file.
        // Change tracking + bookmarks + with-diff (control + ai aliases) live
        // in `bookmarks::routes()` — see `bookmarks.rs`.
        // Keyboard shortcuts live in `capabilities::routes()`.
        // Wait-for handlers (navigation, idle, element-stable / condition /
        // state, route, targets, idle signal) live in `intents::routes()`.
        // `/control/batch-execute` (mixed action/wait/snapshot) lives in
        // `capabilities::routes()`.
        // `/ai/expect` lives in `capabilities::routes()`.
        // `/control/diagnose-stuck`, `/control/page-health`,
        // `/control/capture-element-images`, `/control/get-element-images`
        // live in `screenshots::routes()`.
        // `/control/ai/search`, `/control/ai/find` live in `ai::routes()`.
        // Workflows, element-state, render log, pong, ipc-response, batch
        // execution and action plan cache live in `capabilities::routes()`
        // and `ai::routes()`.
        // Exploration + window listing live in `exploration::routes()` —
        // see `exploration.rs`.
        // /ai/* bookmark aliases live in `bookmarks::routes()` —
        // see `bookmarks.rs`.
        // AI search/find/execute/assert/snapshot/summary live in
        // `ai::routes()` — see `ai.rs`.
        // `/capabilities`, `/control/action-history`, `/control/metrics` live
        // in `capabilities::routes()`.
        // Phase 4: Idle sub-signal wait (get moved to errors::routes(),
        // post moved to intents::routes()).
        // Annotations CRUD and media routes live in `screenshots::routes()`.
        // State machine routes (extracted to state_machine.rs) merged below.
        // AI semantic search / diff / analyze / recovery / media-compare /
        // image-diff handlers (extracted to ai_analyze.rs) merged below.
        // Intent registry handlers (extracted to intents_registry.rs) merged below.
        // Design evaluation handlers (extracted to design_eval.rs) merged below.
        // Element-screenshot routes live in `screenshots::routes()`.
        // Component state, page scroll, performance entries, annotations
        // import, and debug routes (extracted to misc.rs) merged below.
        // Persisted interaction history (cross-run analysis)
        .route(
            "/ui-bridge/history/elements",
            get(ui_bridge_history_elements_handler),
        )
        .route(
            "/ui-bridge/history/element/{id}",
            get(ui_bridge_history_element_handler),
        )
        .route(
            "/ui-bridge/history/flaky",
            get(ui_bridge_history_flaky_handler),
        )
        .route(
            "/ui-bridge/graph/element-reliability",
            get(ui_bridge_element_reliability_handler),
        )
        // Analytics endpoints (Phase 1 + 2)
        .route(
            "/ui-bridge/analytics/decay-curve",
            get(analytics_decay_curve_handler),
        )
        .route(
            "/ui-bridge/analytics/action-baselines",
            get(analytics_action_baselines_handler),
        )
        .route(
            "/ui-bridge/analytics/failure-taxonomy",
            get(analytics_failure_taxonomy_handler),
        )
        .route(
            "/ui-bridge/analytics/fragility-heatmap",
            get(analytics_fragility_heatmap_handler),
        )
        .route(
            "/ui-bridge/analytics/regressions",
            get(analytics_regressions_handler),
        )
        .route(
            "/ui-bridge/analytics/stall-frequency",
            get(analytics_stall_frequency_handler),
        )
        .route(
            "/ui-bridge/analytics/intervention-effectiveness",
            get(analytics_intervention_handler),
        )
        .route(
            "/ui-bridge/analytics/state-coverage",
            get(analytics_state_coverage_handler),
        )
        .route(
            "/ui-bridge/analytics/annotation-gaps",
            get(analytics_annotation_gaps_handler),
        )
        .route(
            "/ui-bridge/analytics/health-score",
            get(analytics_health_score_handler),
        )
        .route(
            "/ui-bridge/analytics/recommendations",
            get(analytics_recommendations_handler),
        )
        // Runner-specific navigate-tab + clear-storage (extracted to misc.rs).
        // Bookmark, change-tracking and with-diff handlers (extracted to bookmarks.rs)
        .merge(bookmarks::routes())
        // Design Review handlers (extracted to design.rs)
        .merge(design::routes())
        // Elements + components + discovery/snapshot + wait-for-element +
        // find + DOM-convenience (find/click/read/type) handlers
        // (extracted to elements.rs)
        .merge(elements::routes())
        // Health, error snapshots/sessions/baselines, diagnostics, idle status
        // (extracted to errors.rs)
        .merge(errors::routes())
        // Form state + clipboard handlers (extracted to forms.rs)
        .merge(forms::routes())
        // Wait-for / intent polling handlers (extracted to intents.rs)
        .merge(intents::routes())
        // Console errors, browser events, timeline, network requests
        // (extracted to network.rs)
        .merge(network::routes())
        // F2 — Network stub registry (extracted to stubs.rs)
        .merge(stubs::routes())
        // State machine handlers (extracted to state_machine.rs)
        .merge(state_machine::routes())
        // AI analyze / semantic search / image-diff handlers (extracted to ai_analyze.rs)
        .merge(ai_analyze::routes())
        // Design evaluate handlers (extracted to design_eval.rs)
        .merge(design_eval::routes())
        // Intent registry handlers (extracted to intents_registry.rs)
        .merge(intents_registry::routes())
        // Undo/redo, specs, component-state, scroll, performance-entries,
        // annotations-import, debug, navigate-tab, clear-storage
        // (extracted to misc.rs)
        .merge(misc::routes())
        // UI Bridge exploration + window listing (extracted to exploration.rs)
        .merge(exploration::routes())
        // Page navigation / evaluation / tab switching (extracted to page.rs)
        .merge(page::routes())
        // Screenshot capture, annotations CRUD, media routes, page-health,
        // diagnose-stuck (extracted to screenshots.rs)
        .merge(screenshots::routes())
        // AI search/find/execute/assert + action plan execution
        // (extracted to ai.rs)
        .merge(ai::routes())
        // Capabilities, keyboard shortcuts, action history, metrics,
        // structured assert, expect text, render log, pong, IPC response,
        // batch execution, control batch, routes manifest, workflow run/
        // status, element state (extracted to capabilities.rs)
        .merge(capabilities::routes())
        // Tier 2.1 — safelisted Tauri command proxy
        .merge(crate::mcp::tauri_proxy::routes())
        // Phase 3I.1 + 3I.2 — UI Bridge invoke proxy (HTTP → Tauri invoke round-trip)
        .route(
            "/ui-bridge/commands",
            get(crate::mcp::ui_bridge_invoke_handlers::ui_bridge_commands_handler),
        )
        .route(
            "/ui-bridge/invoke/{command_name}",
            post(crate::mcp::ui_bridge_invoke_handlers::ui_bridge_invoke_handler),
        )
}

#[cfg(test)]
mod manifest_drift_tests {
    use super::route_manifest;
    use std::collections::HashSet;
    use std::path::PathBuf;

    /// Verify `route_manifest()` matches the actual `.route(...)` calls in
    /// this file. Catches the common drift bug where someone adds a new
    /// endpoint but forgets to register it in the manifest (or the reverse).
    ///
    /// This is a stop-gap for the manual maintenance burden — axum 0.8
    /// doesn't expose `Router::routes()`, so we can't introspect the live
    /// router at runtime. Re-evaluate if axum adds router introspection.
    #[test]
    fn manifest_matches_route_calls() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let src_path = PathBuf::from(manifest_dir).join("src/mcp/ui_bridge.rs");
        let src = std::fs::read_to_string(&src_path)
            .unwrap_or_else(|e| panic!("read {}: {}", src_path.display(), e));

        // For each route registration, extract every HTTP method present
        // (axum allows chaining like get(h).delete(h2) on the same route).
        // Two passes: find each .route call, then within its body grep for
        // method-constructor calls.
        let route_open_re =
            regex::Regex::new(r#"(?s)\.route\(\s*"(/ui-bridge/[^"]+)"\s*,"#).unwrap();
        let method_re = regex::Regex::new(r#"\b(get|post|put|delete|patch)\("#).unwrap();

        let mut source_routes: HashSet<(String, String)> = HashSet::new();
        for cap in route_open_re.captures_iter(&src) {
            let path = cap[1].to_string();
            // Body starts at end of the matched prefix; scan forward up to
            // 400 bytes (largest known route body has 3 chained methods).
            let body_start = cap.get(0).unwrap().end();
            let body_end = (body_start + 400).min(src.len());
            let body = &src[body_start..body_end];
            // Stop at first balanced ")" — for our purposes, the next ".route("
            // delimiter is a safe upper bound, and we only care about methods
            // before the first newline followed by ".route(" or "}\n".
            let scan_end = body
                .find("\n        .route(")
                .or_else(|| body.find("\n    }"))
                .unwrap_or(body.len());
            let scan = &body[..scan_end];

            for m in method_re.captures_iter(scan) {
                source_routes.insert((m[1].to_uppercase(), path.clone()));
            }
        }

        let manifest_routes: HashSet<(String, String)> = route_manifest()
            .iter()
            .map(|(m, p)| ((*m).to_string(), (*p).to_string()))
            .collect();

        let registered_but_missing: Vec<&(String, String)> =
            source_routes.difference(&manifest_routes).collect();
        let in_manifest_but_unregistered: Vec<&(String, String)> =
            manifest_routes.difference(&source_routes).collect();

        if !registered_but_missing.is_empty() || !in_manifest_but_unregistered.is_empty() {
            panic!(
                "route_manifest() drift detected.\n\
                 Add the missing entries to route_manifest() (or remove unregistered ones).\n\n\
                 Registered via .route() but missing from manifest ({}):\n  {}\n\n\
                 In manifest but not actually registered ({}):\n  {}",
                registered_but_missing.len(),
                registered_but_missing
                    .iter()
                    .map(|(m, p)| format!("{} {}", m, p))
                    .collect::<Vec<_>>()
                    .join("\n  "),
                in_manifest_but_unregistered.len(),
                in_manifest_but_unregistered
                    .iter()
                    .map(|(m, p)| format!("{} {}", m, p))
                    .collect::<Vec<_>>()
                    .join("\n  "),
            );
        }

        // Sanity floor: catch a regex regression that silently matches nothing.
        assert!(
            source_routes.len() > 100,
            "regex extracted only {} routes — likely broken",
            source_routes.len()
        );
    }
}

#[cfg(test)]
mod page_evaluate_escaping_tests {
    //! Regression tests for the `/control/page/evaluate` direct-WebView path:
    //! the user's expression must be encoded as a JS string literal and
    //! `eval()`ed, so literal newlines / quotes / backslashes inside the
    //! expression survive the splice into the callback JS template.
    //!
    //! We can't spin up a real webview inside a unit test, so instead we
    //! validate the two invariants that make the fix correct:
    //!   1. `serde_json::to_string` produces a valid JS string literal.
    //!   2. Splicing that literal into `eval(...)` yields a JS source where
    //!      newlines in the original expression no longer appear as raw
    //!      line terminators.

    /// Helper mirroring the production encoding used in
    /// `direct_webview_evaluate_with_result`.
    fn build_eval_inner(expression: &str, await_promise: bool) -> String {
        let expr_literal = serde_json::to_string(expression).expect("encode expression");
        if await_promise {
            format!("await Promise.resolve(eval({}))", expr_literal)
        } else {
            format!("eval({})", expr_literal)
        }
    }

    #[test]
    fn expression_with_newline_in_string_literal_is_escaped() {
        // The original Phase 3J repro: `stack: "at A\n  at B"` decoded from
        // JSON produces a real newline inside the expression. Before the fix,
        // this newline appeared verbatim in the generated JS → SyntaxError.
        let expression = "invoke(\"report_ui_error\", {stack: \"at A\n  at B\"})";
        let emitted = build_eval_inner(expression, false);

        // The raw literal newline from the source expression must NOT appear
        // inside the emitted `eval(...)` argument — it should have been
        // escaped to the two-character sequence \n by serde_json.
        // (`emitted` itself still begins `eval("...` with no newlines.)
        assert!(
            !emitted.contains('\n'),
            "emitted JS still contains a raw newline: {:?}",
            emitted
        );
        // But the escaped sequence must be present.
        assert!(
            emitted.contains(r"\n"),
            "emitted JS missing escaped newline: {:?}",
            emitted
        );
        // Shape check: the wrapper is the eval form.
        assert!(
            emitted.starts_with("eval(\""),
            "unexpected prefix: {:?}",
            emitted
        );
        assert!(emitted.ends_with("\")"), "unexpected suffix: {:?}", emitted);
    }

    #[test]
    fn await_promise_emits_promise_resolve_wrapper() {
        let emitted = build_eval_inner("invoke(\"long_async_op\")", true);
        assert!(
            emitted.starts_with("await Promise.resolve(eval("),
            "awaitPromise=true wrapper missing: {:?}",
            emitted
        );
    }

    #[test]
    fn expression_with_quotes_and_backslashes_roundtrips() {
        // A nasty mix: embedded double-quote, single-quote, backslash, and
        // unicode. serde_json must escape all of these such that the emitted
        // string is a single JS string literal — no premature termination.
        let expression = r#"doc.querySelector('a[href="/x\\y"]').click() // "test""#;
        let emitted = build_eval_inner(expression, false);

        // Exactly one `eval("` opening and one `")` closing — no premature
        // quote in the middle would split the literal into multiple tokens.
        assert_eq!(emitted.matches("eval(\"").count(), 1);
        assert!(emitted.ends_with("\")"));
        // And the emitted blob must decode back to the original when parsed
        // as a JSON string (proving the JS eval() would see the same bytes).
        let inner = &emitted["eval(".len()..emitted.len() - 1];
        let decoded: String = serde_json::from_str(inner).expect("decode back");
        assert_eq!(decoded, expression);
    }
}

#[cfg(test)]
mod page_evaluate_tagging_tests {
    //! Plan item D regression tests for request-tagged `/control/page/evaluate`.
    //!
    //! Concurrent `/page/evaluate` callers against the same runner must never
    //! observe each other's responses. The HTTP handler path goes through
    //! `tagged_page_evaluate` → `EvaluateRequestStore` → the
    //! `ui-bridge:evaluate-response` listener installed in `mcp_api.rs`,
    //! with per-call uuid request_ids keying the pending oneshots.
    //!
    //! We can't spin up Tauri inside a unit test, so these tests exercise the
    //! store directly. The store is the sole correlation surface: every
    //! arriving response has to round-trip through `deliver(request_id, …)`,
    //! so showing the store's id-keyed routing is correct is equivalent to
    //! showing the handler path is correct.
    use crate::ui_bridge_evaluate::{EvaluateRequestStore, EvaluateResponse};
    use std::sync::Arc;
    use tokio::sync::oneshot;

    /// Fire two concurrent "page_evaluate" calls through the store-level
    /// surface that `tagged_page_evaluate` relies on. Each call registers
    /// its own oneshot, deliveries land in the opposite order, and each
    /// caller observes exactly its own result. This is the plan's
    /// "unit test that fires two concurrent `/page/evaluate` calls and
    /// asserts they each get their own result" verification criterion.
    #[tokio::test]
    async fn concurrent_page_evaluate_calls_do_not_interleave() {
        let store = Arc::new(EvaluateRequestStore::new());

        // Caller A: evaluates `document.title` → "Runner".
        let request_id_a = "evaluate-call-a".to_string();
        let (tx_a, rx_a) = oneshot::channel::<EvaluateResponse>();
        store.register(request_id_a.clone(), tx_a).await;

        // Caller B: evaluates `window.location.pathname` → "/dashboard".
        let request_id_b = "evaluate-call-b".to_string();
        let (tx_b, rx_b) = oneshot::channel::<EvaluateResponse>();
        store.register(request_id_b.clone(), tx_b).await;

        // Run both HTTP-handler-equivalent awaits concurrently. Each
        // simulates `tagged_page_evaluate`'s `tokio::time::timeout(..., rx)`.
        let caller_a = tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(2), rx_a)
                .await
                .expect("caller A should not time out")
                .expect("caller A sender should not drop")
        });
        let caller_b = tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(2), rx_b)
                .await
                .expect("caller B should not time out")
                .expect("caller B sender should not drop")
        });

        // Frontend (simulated) delivers B's response first, then A's —
        // proving the store's id-keyed routing doesn't care about arrival
        // order and that concurrent pending entries don't shadow each other.
        let store_for_b = store.clone();
        let deliver_b = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            store_for_b
                .deliver(
                    "evaluate-call-b",
                    EvaluateResponse {
                        ok: true,
                        result: Some(serde_json::json!({
                            "success": true,
                            "result": { "value": "/dashboard" }
                        })),
                        error: None,
                    },
                )
                .await
        });
        let store_for_a = store.clone();
        let deliver_a = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            store_for_a
                .deliver(
                    "evaluate-call-a",
                    EvaluateResponse {
                        ok: true,
                        result: Some(serde_json::json!({
                            "success": true,
                            "result": { "value": "Runner" }
                        })),
                        error: None,
                    },
                )
                .await
        });

        assert!(deliver_a.await.expect("deliver_a join"));
        assert!(deliver_b.await.expect("deliver_b join"));

        let result_a = caller_a.await.expect("caller_a join");
        let result_b = caller_b.await.expect("caller_b join");

        // Each caller received its own expression's result, not the other's.
        assert!(result_a.ok);
        assert!(result_b.ok);
        assert_eq!(
            result_a
                .result
                .as_ref()
                .and_then(|v| v.pointer("/result/value"))
                .and_then(|v| v.as_str()),
            Some("Runner"),
            "caller A must see its own result, not caller B's"
        );
        assert_eq!(
            result_b
                .result
                .as_ref()
                .and_then(|v| v.pointer("/result/value"))
                .and_then(|v| v.as_str()),
            Some("/dashboard"),
            "caller B must see its own result, not caller A's"
        );

        // Store should be fully drained after both deliveries.
        assert_eq!(store.pending_len().await, 0);
    }

    /// Timing out one call must not strand the other call's pending slot.
    /// Simulates: A times out (cancelled), B still delivers successfully.
    #[tokio::test]
    async fn timeout_of_one_page_evaluate_does_not_affect_sibling() {
        let store = Arc::new(EvaluateRequestStore::new());

        let request_id_a = "evaluate-timeout".to_string();
        let (tx_a, rx_a) = oneshot::channel::<EvaluateResponse>();
        store.register(request_id_a.clone(), tx_a).await;

        let request_id_b = "evaluate-sibling".to_string();
        let (tx_b, rx_b) = oneshot::channel::<EvaluateResponse>();
        store.register(request_id_b.clone(), tx_b).await;

        // Caller A times out while waiting (mirrors the Elapsed branch of
        // tagged_page_evaluate). The handler cancels its slot.
        let wait_a = tokio::time::timeout(std::time::Duration::from_millis(50), rx_a).await;
        assert!(wait_a.is_err(), "caller A must time out");
        store.cancel(&request_id_a).await;

        // Caller B still gets a clean delivery afterwards.
        let store_for_b = store.clone();
        tokio::spawn(async move {
            store_for_b
                .deliver(
                    "evaluate-sibling",
                    EvaluateResponse {
                        ok: true,
                        result: Some(serde_json::json!({ "value": 7 })),
                        error: None,
                    },
                )
                .await
        });

        let result_b = tokio::time::timeout(std::time::Duration::from_secs(1), rx_b)
            .await
            .expect("caller B should not time out")
            .expect("caller B sender should not drop");

        assert!(result_b.ok);
        assert_eq!(
            result_b.result.as_ref().and_then(|v| v.get("value")),
            Some(&serde_json::json!(7))
        );
        assert_eq!(store.pending_len().await, 0);
    }
}
