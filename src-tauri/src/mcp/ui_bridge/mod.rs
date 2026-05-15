//! UI Bridge handlers for MCP API.
//!
//! Provides HTTP handlers for UI Bridge control (React UI automation) and
//! UI Bridge exploration (qontinui library via Python bridge).
//!
//! Originally a single 12.7kLOC file; now decomposed across ~25 thematic
//! submodules. Each submodule exposes `pub fn routes() -> Router` and
//! `pub fn route_entries() -> &'static [(&'static str, &'static str)]`.
//! `mod.rs` contains only the `routes()` composer (which `.merge()`s every
//! submodule's router), the `route_manifest()` cache (concatenates
//! submodule entries for the `/_help` and `/_routes` endpoints), the
//! shared `ipc_handler_*` macro definitions, and a small drift-test
//! module. All per-endpoint handlers live in their family submodules.

pub mod ai;
pub mod ai_analyze;
pub mod analytics;
pub mod bookmarks;
pub mod capabilities;
pub mod circuit_breaker;
pub mod component_errors;
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
pub mod sdk_spec_sync;
pub mod state_machine;
pub mod stubs;
pub mod terminals;
pub mod toasts;
pub mod types;
pub mod vision_ai;
pub mod vision_routes;

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
    ui_bridge_read_value_handler, ui_bridge_type_into_handler, ui_bridge_wait_for_element_handler,
    ActionQueryParams,
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
    ui_bridge_query_selector_handler, ui_bridge_tab_activate_handler, ui_bridge_tabs_list_handler,
    BatchEvaluateRequest, BatchExpression, BatchExpressionResult, NavigateAndWaitRequest,
    PageEvaluateRequest, PageNavigateRequest, QuerySelectorRequest, SetTabRequest, SetTabResponse,
    TabActivateRequest,
};
pub use request::{handle_ui_bridge_response, ui_bridge_request_sync};
pub use screenshots::{
    capture_runner_window_base64, ui_bridge_annotations_coverage_handler,
    ui_bridge_annotations_create_handler, ui_bridge_annotations_delete_handler,
    ui_bridge_annotations_export_handler, ui_bridge_annotations_get_handler,
    ui_bridge_annotations_list_handler, ui_bridge_annotations_update_handler,
    ui_bridge_page_health_handler, PageHealthRequest,
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
use axum::{extract::State, http::StatusCode, response::Json};
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

/// Variant of [`ipc_handler_post`] that bumps `vision_mutation_id` before
/// dispatching the IPC. Use for handlers whose action can change rendered
/// pixels but that you'd otherwise generate via the plain macro (so the
/// 3-line handler body stays uniform). The bump uses Relaxed ordering —
/// monotonic counter, no cross-thread happens-before constraints.
macro_rules! ipc_handler_post_bumps_mutation {
    ($fn_name:ident, $ipc_type:expr) => {
        pub async fn $fn_name(
            State(state): State<Arc<ApiState>>,
            Json(body): Json<serde_json::Value>,
        ) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
            $crate::mcp::ui_bridge::vision_routes::bump_mutation_id(&state);
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
pub(crate) use ipc_handler_post_bumps_mutation;

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
        all.extend_from_slice(sdk_spec_sync::route_entries());
        all.extend_from_slice(state_machine::route_entries());
        all.extend_from_slice(stubs::route_entries());
        all.extend_from_slice(terminals::route_entries());
        all.extend_from_slice(toasts::route_entries());
        all.extend_from_slice(vision_routes::route_entries());
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
        .merge(sdk_spec_sync::routes())
        .merge(state_machine::routes())
        .merge(stubs::routes())
        .merge(terminals::routes())
        .merge(toasts::routes())
        .merge(vision_routes::routes())
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
        // Allow `// ...` line comments between `.route(` and the URL literal
        // (some entries annotate which plan phase introduced them — see e.g.
        // `elements.rs::routes()` for the `state-summary` and `expect` routes).
        let route_open_re =
            regex::Regex::new(r#"(?s)\.route\(\s*(?://[^\n]*\n\s*)*"(/ui-bridge/[^"]+)"\s*,"#)
                .unwrap();
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

    /// Phase 2a (plan 2026-05-03 ui-bridge-runner-wireup-discipline).
    ///
    /// Diff the SDK's authoritative `UI_BRIDGE_ROUTES` array against the
    /// runner's `route_manifest()`. Catches the "SDK adds a route, runner
    /// returns 404" gap that the sibling `manifest_matches_route_calls` test
    /// is structurally blind to (it only checks runner-internal consistency).
    ///
    /// Skip — not fail — when the ui-bridge sibling repo isn't checked out
    /// next to qontinui-runner: this test is best-effort, not a hard
    /// dependency on the dev-tree layout.
    #[test]
    fn sdk_manifest_routes_are_exposed_by_runner() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let sdk_types_path = PathBuf::from(manifest_dir)
            .join("../../ui-bridge/packages/ui-bridge/src/server/types.ts");

        // Skip condition: ui-bridge repo not present alongside qontinui-runner.
        // This is a dev-tree convention, not a build-time guarantee, so make
        // the diff non-blocking when the source-of-truth file is unreachable.
        let src = match std::fs::read_to_string(&sdk_types_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "skip sdk_manifest_routes_are_exposed_by_runner: {} unreadable ({})",
                    sdk_types_path.display(),
                    e
                );
                return;
            }
        };

        // Bracket-balanced extraction of the UI_BRIDGE_ROUTES = [ ... ];
        // block. A simple regex on the whole file would fight the nested
        // object braces; restricting subsequent entry-scanning to the array
        // body avoids accidentally matching unrelated `{ method: ... }`
        // shapes elsewhere in the file (e.g. example snippets in JSDoc).
        let array_start_re =
            regex::Regex::new(r"export\s+const\s+UI_BRIDGE_ROUTES[^=]*=\s*\[").unwrap();
        let start_match = array_start_re
            .find(&src)
            .expect("UI_BRIDGE_ROUTES = [ … ] not found in SDK types.ts");
        let body_start = start_match.end();

        let bytes = src.as_bytes();
        let mut depth = 1usize;
        let mut i = body_start;
        let mut body_end = body_start;
        while i < bytes.len() {
            match bytes[i] {
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        body_end = i;
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        assert!(
            body_end > body_start,
            "UI_BRIDGE_ROUTES array body not balanced — regex/scan logic bug"
        );
        let array_body = &src[body_start..body_end];

        // Within the array body, scrape each entry's method and path. Entries
        // can span multiple lines (multiline `{ method: …, path: …, … }`) so
        // match `path` and `method` independently per `{ … }` chunk rather
        // than insisting on a same-line pair.
        let entry_re = regex::Regex::new(r"(?s)\{[^{}]*\}").unwrap();
        let method_re = regex::Regex::new(r#"method:\s*'([A-Z]+)'"#).unwrap();
        let path_re = regex::Regex::new(r#"path:\s*'([^']+)'"#).unwrap();

        let mut sdk_routes: HashSet<(String, String)> = HashSet::new();
        for entry in entry_re.find_iter(array_body) {
            let chunk = entry.as_str();
            let method = match method_re.captures(chunk) {
                Some(c) => c[1].to_string(),
                None => continue,
            };
            let path = match path_re.captures(chunk) {
                Some(c) => c[1].to_string(),
                None => continue,
            };
            // SDK paths are relative to the /ui-bridge mount point and use
            // `:id`-style placeholders; normalise to `/ui-bridge/...{id}`
            // to match the runner's axum-style declarations.
            let full_path = format!("/ui-bridge{}", path);
            let canonical = canonicalise_path(&full_path);
            sdk_routes.insert((method, canonical));
        }

        // Sanity floor: a regex-regression that silently matches nothing
        // would let real drift slide through. The SDK has ~190 entries.
        assert!(
            sdk_routes.len() > 50,
            "scrape extracted only {} SDK routes — regex likely broken",
            sdk_routes.len()
        );

        // ── Runner-only allow-list ──────────────────────────────────────
        //
        // Routes the runner exposes that aren't part of the SDK contract.
        // Captured here as a *baseline snapshot* of accepted divergence so
        // this test stays focused on catching NEW drift, not re-litigating
        // the long tail of pre-existing differences. Two reasons a path
        // belongs here:
        //
        //   (a) Architecturally runner-only — IPC plumbing (`pong`,
        //       `ipc-response`), Tauri-invoke proxy (`commands`, `invoke/*`),
        //       discoverability surfaces (`_routes`, `_help`), debug
        //       analytics (`/analytics/*`, `/explore/*`, `/diagnostics/*`,
        //       `/circuit-breaker/*`, `/discover-states`, `/history/*`,
        //       `/graph/*`).
        //   (b) Pre-existing convenience the runner shipped before the SDK
        //       documented an equivalent — listed here so the TEST CAN PASS
        //       today and only flag NEW drift going forward. Promote to
        //       UI_BRIDGE_ROUTES (and remove from this list) when wiring
        //       the SDK side.
        //
        // The `/control/ai/*` aliases under `/control/` mirror SDK `/ai/*`
        // routes — `add_dual!` registers both for the same handler. SDK
        // declares only the `/ai/` form, hence the runner's `/control/ai/`
        // siblings are intentionally runner-only.
        let runner_only_baseline: HashSet<(&str, &str)> = [
            // Runner infrastructure
            ("GET", "/ui-bridge/_routes"),
            ("GET", "/ui-bridge/_help"),
            ("GET", "/ui-bridge/commands"),
            ("POST", "/ui-bridge/invoke/{}"),
            ("POST", "/ui-bridge/pong"),
            ("POST", "/ui-bridge/ipc-response"),
            ("POST", "/ui-bridge/vision/mutation-occurred"),
            ("POST", "/ui-bridge/vision/extract"),
            ("POST", "/ui-bridge/vision/describe"),
            ("POST", "/ui-bridge/vision/analyze"),
            ("POST", "/ui-bridge/vision/assert"),
            ("POST", "/ui-bridge/vision/baseline"),
            ("GET", "/ui-bridge/vision/baselines"),
            // Vision-family runner-only IPC routes — main's #134/#135/#136
            // (vision-pipeline Phase 3.3/4/6.3) added these on the runner side
            // but the SDK at ^0.7.0 doesn't declare them. Cache/health are
            // diagnostic; annotate/capture/diff/raw are internal capture-path
            // helpers consumed by the runner's own vision subsystem.
            ("GET", "/ui-bridge/vision/cache/{}"),
            ("GET", "/ui-bridge/vision/health"),
            ("POST", "/ui-bridge/vision/annotate"),
            ("POST", "/ui-bridge/vision/capture"),
            ("POST", "/ui-bridge/vision/diff"),
            ("POST", "/ui-bridge/vision/raw"),
            ("POST", "/ui-bridge/batch"),
            ("POST", "/ui-bridge/control/batch"),
            ("POST", "/ui-bridge/render-log"),
            ("POST", "/ui-bridge/control/render-log"),
            ("POST", "/ui-bridge/control/assert"),
            ("GET", "/ui-bridge/control/keyboard-shortcuts"),
            ("GET", "/ui-bridge/control/element/{}/tree"),
            ("POST", "/ui-bridge/control/element/{}/assert"),
            ("POST", "/ui-bridge/control/batch-actions"),
            ("POST", "/ui-bridge/circuit-breaker/reset"),
            // Terminal sessions — runner-only (terminal tabs are a Tauri-host
            // construct; SDK/web/supervisor consumers don't host PTY tabs).
            // Pairs with `terminal-launch-menu` component actions.
            ("GET", "/ui-bridge/control/terminal-sessions"),
            ("GET", "/ui-bridge/control/terminal-sessions/{}"),
            // /control/ai/* aliases mirroring SDK /ai/* (runner-side dual mount)
            ("DELETE", "/ui-bridge/control/ai/bookmark/{}"),
            ("DELETE", "/ui-bridge/control/ai/bookmarks/{}"),
            ("GET", "/ui-bridge/control/ai/bookmark/{}"),
            ("GET", "/ui-bridge/control/ai/bookmark/{}/diff"),
            ("GET", "/ui-bridge/control/ai/bookmarks/{}"),
            ("GET", "/ui-bridge/control/ai/bookmarks/{}/diff"),
            ("GET", "/ui-bridge/control/ai/bookmarks"),
            ("GET", "/ui-bridge/control/ai/categorize-last-diff"),
            ("GET", "/ui-bridge/control/ai/change-buffer/size"),
            ("POST", "/ui-bridge/control/ai/bookmarks"),
            ("POST", "/ui-bridge/control/ai/change-buffer/disable"),
            ("POST", "/ui-bridge/control/ai/change-buffer/drain"),
            ("POST", "/ui-bridge/control/ai/change-buffer/enable"),
            ("POST", "/ui-bridge/control/ai/execute-with-diff"),
            ("POST", "/ui-bridge/control/ai/find"),
            ("POST", "/ui-bridge/control/ai/image-diff"),
            ("POST", "/ui-bridge/control/ai/scoped-diff"),
            ("POST", "/ui-bridge/control/ai/search"),
            ("POST", "/ui-bridge/control/ai/structured-changes"),
            ("POST", "/ui-bridge/control/ai/summarize-diff"),
            ("POST", "/ui-bridge/control/ai/wait-for-change"),
            // /ai/* runner extensions not in SDK contract
            ("GET", "/ui-bridge/ai/element-screenshot"),
            ("GET", "/ui-bridge/ai/elements/last-discovered"),
            ("GET", "/ui-bridge/ai/forms"),
            ("GET", "/ui-bridge/ai/idle-status"),
            ("POST", "/ui-bridge/ai/analyze/data"),
            ("POST", "/ui-bridge/ai/analyze/regions"),
            ("POST", "/ui-bridge/ai/analyze/structured-data"),
            ("POST", "/ui-bridge/ai/design-audit"),
            ("POST", "/ui-bridge/ai/expect"),
            ("POST", "/ui-bridge/ai/fill-form"),
            ("POST", "/ui-bridge/ai/image-diff"),
            ("POST", "/ui-bridge/ai/media/audit/{}"),
            ("POST", "/ui-bridge/ai/page-summary"),
            ("POST", "/ui-bridge/ai/wait-for-idle"),
            ("POST", "/ui-bridge/ai/wait-for-navigation"),
            ("POST", "/ui-bridge/ai/wait-for-route"),
            // Analytics / debug surfaces (runner-only by design)
            ("GET", "/ui-bridge/analytics/action-baselines"),
            ("GET", "/ui-bridge/analytics/annotation-gaps"),
            ("GET", "/ui-bridge/analytics/decay-curve"),
            ("GET", "/ui-bridge/analytics/failure-taxonomy"),
            ("GET", "/ui-bridge/analytics/fragility-heatmap"),
            ("GET", "/ui-bridge/analytics/health-score"),
            ("GET", "/ui-bridge/analytics/intervention-effectiveness"),
            ("GET", "/ui-bridge/analytics/recommendations"),
            ("GET", "/ui-bridge/analytics/regressions"),
            ("GET", "/ui-bridge/analytics/stall-frequency"),
            ("GET", "/ui-bridge/analytics/state-coverage"),
            ("GET", "/ui-bridge/diagnostics/readiness"),
            ("GET", "/ui-bridge/explore/results"),
            ("GET", "/ui-bridge/explore/status"),
            ("POST", "/ui-bridge/explore"),
            ("POST", "/ui-bridge/explore/stop"),
            ("POST", "/ui-bridge/discover-states"),
            ("GET", "/ui-bridge/graph/element-reliability"),
            ("GET", "/ui-bridge/history/element/{}"),
            ("GET", "/ui-bridge/history/elements"),
            ("GET", "/ui-bridge/history/flaky"),
            // Runner-side /control/* convenience routes not yet in UI_BRIDGE_ROUTES
            ("DELETE", "/ui-bridge/control/network/stubs"),
            ("DELETE", "/ui-bridge/control/network/stubs/{}"),
            ("GET", "/ui-bridge/control/action-plan/cache"),
            ("GET", "/ui-bridge/control/action-plan/cache/stats"),
            ("GET", "/ui-bridge/control/annotated-screenshot"),
            ("GET", "/ui-bridge/control/design/evaluate/contexts"),
            ("GET", "/ui-bridge/control/design/style-guide"),
            ("GET", "/ui-bridge/control/element-screenshot"),
            ("GET", "/ui-bridge/control/elements/last-discovered"),
            ("GET", "/ui-bridge/control/health-signals"),
            ("GET", "/ui-bridge/control/network/stubs"),
            ("GET", "/ui-bridge/control/page/playbook"),
            ("GET", "/ui-bridge/control/spec/{}"),
            ("GET", "/ui-bridge/control/tabs"),
            ("GET", "/ui-bridge/control/toasts"),
            ("GET", "/ui-bridge/control/windows"),
            ("POST", "/ui-bridge/control/action-plan"),
            ("POST", "/ui-bridge/control/activate-tab/{}"),
            ("POST", "/ui-bridge/control/annotations"),
            ("POST", "/ui-bridge/control/capture-element-images"),
            ("POST", "/ui-bridge/control/clear-storage"),
            ("POST", "/ui-bridge/control/design/evaluate"),
            ("POST", "/ui-bridge/control/design/evaluate/baseline"),
            ("POST", "/ui-bridge/control/design/evaluate/diff"),
            ("POST", "/ui-bridge/control/design/style-guide/clear"),
            ("POST", "/ui-bridge/control/design/style-guide/load"),
            ("POST", "/ui-bridge/control/diagnose-stuck"),
            ("POST", "/ui-bridge/control/intents/execute"),
            ("POST", "/ui-bridge/control/intents/execute-from-query"),
            ("POST", "/ui-bridge/control/intents/find"),
            ("POST", "/ui-bridge/control/navigate-and-wait"),
            ("POST", "/ui-bridge/control/navigate-tab"),
            ("POST", "/ui-bridge/control/network/stubs"),
            ("POST", "/ui-bridge/control/network/verify-stub"),
            ("POST", "/ui-bridge/control/page-health"),
            ("POST", "/ui-bridge/control/page/close-request"),
            ("POST", "/ui-bridge/control/page/evaluate-batch"),
            ("POST", "/ui-bridge/control/page/evaluate-raw"),
            ("POST", "/ui-bridge/control/page/evaluate-safe"),
            ("POST", "/ui-bridge/control/page/hard-refresh"),
            ("POST", "/ui-bridge/control/page/set-tab"),
            ("POST", "/ui-bridge/control/page/summary"),
            ("POST", "/ui-bridge/control/query-selector"),
            ("POST", "/ui-bridge/control/screenshot"),
            ("POST", "/ui-bridge/control/spec/{}/run"),
            ("POST", "/ui-bridge/control/tab/activate"),
            ("POST", "/ui-bridge/control/wait-for-element"),
            ("POST", "/ui-bridge/control/wait-for-element-stable"),
            ("POST", "/ui-bridge/control/wait-for-element-state"),
            ("POST", "/ui-bridge/control/wait-for-navigation"),
            ("POST", "/ui-bridge/control/wait-for-route"),
            ("POST", "/ui-bridge/control/wait-for-route-change"),
            ("POST", "/ui-bridge/control/with-diff"),
        ]
        .into_iter()
        .collect();

        // Allow-listed prefixes. `/ui-bridge/sdk/*` is the WS-transport outer
        // wrapper layer in `mcp/sdk_client.rs` — runner-only by architectural
        // design, never part of UI_BRIDGE_ROUTES.
        let runner_only_prefixes: &[&str] = &["/ui-bridge/sdk/"];

        let in_prefix_allowlist =
            |p: &str| runner_only_prefixes.iter().any(|pre| p.starts_with(pre));

        // ── SDK-only baseline ───────────────────────────────────────────
        //
        // Routes the SDK declares but the runner does not expose. The
        // 2026-05-07 Bucket C plan wired through the 14 entries that were
        // genuinely missing handlers — what remains here is intentional
        // shape divergence (Bucket D) plus one deferred subsystem
        // (spawn-headless, Bucket C7). Each remaining line documents the
        // reason it's NOT a simple alias.
        //
        // To remove a future entry: implement the runner handler in the
        // matching `mcp/ui_bridge/<family>.rs`, register in `routes()`
        // AND `route_entries()`, then delete the line below. The test
        // will then assert end-to-end wiring forever after.
        let sdk_only_baseline: HashSet<(&str, &str)> = [
            // Intent name-in-URL execute — runner's /control/intents/execute
            // takes the name in the JSON body (different shape from SDK's
            // /control/intent/:name/execute).
            ("POST", "/ui-bridge/control/intent/{}/execute"),
            // Cross-app analyze GET form — runner's analyze handlers are
            // POST-only (they accept a request body); SDK declares a GET
            // shape with query-string args. Different shape, not a simple alias.
            ("GET", "/ui-bridge/ai/analyze/data"),
            ("GET", "/ui-bridge/ai/analyze/regions"),
            ("GET", "/ui-bridge/ai/analyze/structured-data"),
            // Media audit named subpaths — runner exposes the parameterised
            // /ai/media/audit/{audit_type} form which routes via Path<String>.
            // SDK declares concrete subpaths; aliasing them would require
            // wrapper handlers that hardcode the audit-type string.
            ("POST", "/ui-bridge/ai/media/audit/accessibility"),
            ("POST", "/ui-bridge/ai/media/audit/performance"),
            // Spawn-headless — deferred (Bucket C7 of the 2026-05-07 plan).
            // Real subsystem (CLI subprocess orchestration), not a handler;
            // tracked separately.
            ("POST", "/ui-bridge/control/sdk/spawn-headless"),
        ]
        .into_iter()
        .collect();

        // Apply allow-list using the canonicalised path so `{}` placeholders
        // match SDK `:id` and runner `{anything}` uniformly.
        let runner_routes: HashSet<(String, String)> = route_manifest()
            .iter()
            .filter(|(_, p)| !in_prefix_allowlist(p))
            .map(|(m, p)| ((*m).to_string(), canonicalise_path(p)))
            .filter(|(m, p)| !runner_only_baseline.contains(&(m.as_str(), p.as_str())))
            .collect();

        let sdk_routes_filtered: HashSet<(String, String)> = sdk_routes
            .iter()
            .filter(|(m, p)| !sdk_only_baseline.contains(&(m.as_str(), p.as_str())))
            .cloned()
            .collect();

        // SDK-declared routes the runner doesn't expose — what this plan exists to prevent.
        let sdk_missing_in_runner: Vec<&(String, String)> =
            sdk_routes_filtered.difference(&runner_routes).collect();

        // Runner routes the SDK doesn't declare — undocumented runner extensions
        // or stale routes that should be promoted to UI_BRIDGE_ROUTES. Surfaced
        // separately so a real wire-through gap is distinguishable from
        // "extend the allow-list".
        let runner_extra_vs_sdk: Vec<&(String, String)> =
            runner_routes.difference(&sdk_routes_filtered).collect();

        if !sdk_missing_in_runner.is_empty() || !runner_extra_vs_sdk.is_empty() {
            let mut sdk_missing_sorted: Vec<&(String, String)> = sdk_missing_in_runner.clone();
            sdk_missing_sorted.sort();
            let mut runner_extra_sorted: Vec<&(String, String)> = runner_extra_vs_sdk.clone();
            runner_extra_sorted.sort();

            panic!(
                "SDK ↔ runner manifest drift detected.\n\
                 SDK is the source of truth for the contract; runner must expose every entry.\n\
                 Either add the missing handler in mcp/ui_bridge/<family>.rs or extend the\n\
                 sdk_only_baseline / runner_only_baseline allow-list in this test if the\n\
                 divergence is intentional.\n\n\
                 SDK declares but runner does not expose ({}):\n  {}\n\n\
                 Runner exposes but SDK does not declare ({}):\n  {}",
                sdk_missing_in_runner.len(),
                sdk_missing_sorted
                    .iter()
                    .map(|(m, p)| format!("{} {}", m, p))
                    .collect::<Vec<_>>()
                    .join("\n  "),
                runner_extra_vs_sdk.len(),
                runner_extra_sorted
                    .iter()
                    .map(|(m, p)| format!("{} {}", m, p))
                    .collect::<Vec<_>>()
                    .join("\n  "),
            );
        }
    }

    /// Canonicalise a path so SDK `:id` and runner `{id}` placeholders compare
    /// equal. Strip the placeholder *name* too — axum routes by position, not
    /// by binding name, so `{run_id}` vs `{runId}` are the same route
    /// structurally and would otherwise generate spurious "drift" entries.
    fn canonicalise_path(path: &str) -> String {
        path.split('/')
            .map(|seg| {
                let is_axum = seg.starts_with('{') && seg.ends_with('}') && seg.len() >= 2;
                let is_express = seg.starts_with(':');
                if is_axum || is_express {
                    "{}".to_string()
                } else {
                    seg.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    }
}
