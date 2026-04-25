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
pub mod routing;
pub mod screenshots;
pub mod state_machine;
pub mod stubs;
pub mod toasts;
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
    ui_bridge_wait_for_route_change_handler, ui_bridge_wait_for_route_handler,
    ui_bridge_wait_for_targets_handler,
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

// Imports kept in scope for the `ipc_handler_*!` macro expansions declared
// below. Each macro invocation in a submodule imports the macro via
// `use super::ipc_handler_*;`, and the expanded code references these
// names at the call site — but the macros are also defined here so the
// names must resolve at *definition* scope for rustc to parse them.
use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
};
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};

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

// F2 sweep (2026-04-22): all four `ipc_handler_*!` macros now route through
// `wrap_ipc_result`, which flattens an inner `{success:false, error}` envelope
// from the frontend into a flat HTTP 400 (no inner data, no nested success).
// Macro expansion sites (the per-family `misc.rs` / `state_machine.rs` /
// `screenshots.rs` callers) inherit this behavior with no changes required.
macro_rules! ipc_handler_get {
    ($fn_name:ident, $ipc_type:expr) => {
        pub async fn $fn_name(
            State(state): State<Arc<ApiState>>,
        ) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
            $crate::mcp::ui_bridge::request::wrap_ipc_result(
                ui_bridge_request_sync(&state, $ipc_type, serde_json::json!({})).await,
            )
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
            $crate::mcp::ui_bridge::request::wrap_ipc_result(
                ui_bridge_request_sync(&state, $ipc_type, payload).await,
            )
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
            $crate::mcp::ui_bridge::request::wrap_ipc_result(
                ui_bridge_request_sync(&state, $ipc_type, payload).await,
            )
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
            $crate::mcp::ui_bridge::request::wrap_ipc_result(
                ui_bridge_request_sync(&state, $ipc_type, payload).await,
            )
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
        all.extend_from_slice(analytics::route_entries());
        all.extend_from_slice(bookmarks::route_entries());
        all.extend_from_slice(capabilities::route_entries());
        all.extend_from_slice(design::route_entries());
        all.extend_from_slice(design_eval::route_entries());
        all.extend_from_slice(elements::route_entries());
        all.extend_from_slice(errors::route_entries());
        all.extend_from_slice(exploration::route_entries());
        all.extend_from_slice(forms::route_entries());
        all.extend_from_slice(history::route_entries());
        all.extend_from_slice(intents::route_entries());
        all.extend_from_slice(intents_registry::route_entries());
        all.extend_from_slice(misc::route_entries());
        all.extend_from_slice(network::route_entries());
        all.extend_from_slice(page::route_entries());
        all.extend_from_slice(screenshots::route_entries());
        all.extend_from_slice(state_machine::route_entries());
        all.extend_from_slice(stubs::route_entries());
        all.extend_from_slice(toasts::route_entries());
        all
    })
}

/// Routes that are still defined inline in `mod.rs::routes()`.
/// Per-family extractions append their own `route_entries()` via
/// `route_manifest()` above; do not list those entries here.
fn local_route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        // Phase 3I.1 + 3I.2 — UI Bridge invoke proxy
        ("GET", "/ui-bridge/commands"),
        ("POST", "/ui-bridge/invoke/{command_name}"),
    ]
}

pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    // Each `pub mod` listed at the top of this file owns a thematic slice of
    // the UI Bridge HTTP surface and exposes `routes()` + `route_entries()`.
    // Composition here is purely structural — every handler lives in a
    // submodule. The two trailing `.route(...)` calls register the Tauri
    // invoke-proxy endpoints (Phase 3I.1 + 3I.2) which are dispatched into
    // the runner's command surface rather than the IPC bridge.
    axum::Router::new()
        .merge(ai::routes())
        .merge(ai_analyze::routes())
        .merge(analytics::routes())
        .merge(bookmarks::routes())
        .merge(capabilities::routes())
        .merge(design::routes())
        .merge(design_eval::routes())
        .merge(elements::routes())
        .merge(errors::routes())
        .merge(exploration::routes())
        .merge(forms::routes())
        .merge(history::routes())
        .merge(intents::routes())
        .merge(intents_registry::routes())
        .merge(misc::routes())
        .merge(network::routes())
        .merge(page::routes())
        .merge(screenshots::routes())
        .merge(state_machine::routes())
        .merge(stubs::routes())
        .merge(toasts::routes())
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
        let ui_bridge_dir = PathBuf::from(manifest_dir).join("src/mcp/ui_bridge");

        // After the family split, routes live across ~20 submodule files.
        // Concatenate every .rs file under src/mcp/ui_bridge/ before scanning
        // so the drift test covers the full router composition, not just mod.rs.
        let mut src = String::new();
        let entries = std::fs::read_dir(&ui_bridge_dir).unwrap_or_else(|e| {
            panic!("read_dir {}: {}", ui_bridge_dir.display(), e);
        });
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let file = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
                src.push_str(&file);
                src.push('\n');
            }
        }

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

        // `add_dual!(router, <method>, "<tail>", <handler>)` registers the
        // same handler at both `/ui-bridge/control/<tail>` and
        // `/ui-bridge/ai/<tail>`. Synthesise those two source routes here
        // so the drift test sees them even though the literal strings
        // never appear in the source.
        let add_dual_re = regex::Regex::new(
            r#"add_dual!\s*\(\s*[^,]+,\s*(get|post|put|delete|patch)\s*,\s*"([^"]+)""#,
        )
        .unwrap();
        for cap in add_dual_re.captures_iter(&src) {
            let method = cap[1].to_uppercase();
            let tail = &cap[2];
            source_routes.insert((method.clone(), format!("/ui-bridge/control/{}", tail)));
            source_routes.insert((method, format!("/ui-bridge/ai/{}", tail)));
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
