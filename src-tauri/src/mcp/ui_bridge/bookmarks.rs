//! Change-tracking and bookmark HTTP handlers.
//!
//! Extracted from `mod.rs` to keep the per-family handler set in one
//! place. Every handler is `pub` so the public re-exports preserved by
//! `mod.rs` (`pub use bookmarks::*;`) keep `crate::mcp::ui_bridge::<name>`
//! resolvable for any external caller — even though no external caller
//! exists today, the prior pass adopted a "re-export everything" rule for
//! the moved handlers and we keep that here.
//!
//! Routes are registered via `routes()` and then merged into the parent
//! router; the parallel `route_entries()` list feeds `route_manifest()`
//! so the `/ui-bridge/_routes` discovery endpoint stays accurate.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use tracing::info;

use crate::mcp::types::{ApiResponse, ApiState};

use super::request::{ui_bridge_request_sync, wrap_ipc_result};

/// Save a bookmark (snapshot) by name.
pub async fn ui_bridge_save_bookmark_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Save bookmark");
    wrap_ipc_result(ui_bridge_request_sync(&state, "save_bookmark", body).await)
}

/// Get a bookmark by name.
pub async fn ui_bridge_get_bookmark_handler(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get bookmark '{}'", name);
    wrap_ipc_result(
        ui_bridge_request_sync(&state, "get_bookmark", serde_json::json!({"name": name})).await,
    )
}

/// Delete a bookmark by name.
pub async fn ui_bridge_delete_bookmark_handler(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Delete bookmark '{}'", name);
    wrap_ipc_result(
        ui_bridge_request_sync(&state, "delete_bookmark", serde_json::json!({"name": name})).await,
    )
}

/// List all bookmarks.
pub async fn ui_bridge_list_bookmarks_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: List bookmarks");
    wrap_ipc_result(ui_bridge_request_sync(&state, "list_bookmarks", serde_json::json!({})).await)
}

/// Diff current state from a named bookmark.
pub async fn ui_bridge_diff_from_bookmark_handler(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Diff from bookmark '{}'", name);
    wrap_ipc_result(
        ui_bridge_request_sync(
            &state,
            "diff_from_bookmark",
            serde_json::json!({"name": name}),
        )
        .await,
    )
}

/// Execute an action and return the diff.
pub async fn ui_bridge_execute_with_diff_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Execute with diff");
    wrap_ipc_result(ui_bridge_request_sync(&state, "execute_with_diff", body).await)
}

/// Composite endpoint: execute one or more actions with atomic change-buffer tracking.
///
/// Accepts either a single operation `{operation, elementId, params}` or a batch
/// `{operations: [{operation, elementId, params}, ...]}`. Returns the action
/// result(s) alongside the DOM changes captured during execution.
pub async fn ui_bridge_with_diff_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Detect batch vs single based on presence of "operations" array
    if body.get("operations").is_some() {
        info!("UI Bridge API: Batch execute with diff");
        wrap_ipc_result(ui_bridge_request_sync(&state, "execute_batch_with_diff", body).await)
    } else {
        info!("UI Bridge API: Single execute with diff");
        let payload = with_diff_single_payload(body);
        wrap_ipc_result(ui_bridge_request_sync(&state, "execute_with_diff", payload).await)
    }
}

/// Build the `execute_with_diff` payload for the single-operation arm of
/// `/control/with-diff`, by ADDING the `elementAction` envelope to the caller's
/// body rather than rebuilding the body from it.
///
/// `ChangeTracker.executeWithDiff` takes an `ActionWithDiffRequest`, whose
/// `elementAction` is only one of ten declared fields — the others being
/// `instruction`, `settleTimeout`, `settleMinStable`, `scope`, `categorize`,
/// `timeline`, `timelineInterval`, `summaryBudget` and `analyzeStructured`.
/// This arm used to emit a payload built from scratch out of
/// `{elementId, operation, params}`, so **every one of those nine was silently
/// dropped**: a caller scoping the diff with `{"scope": "#main"}` or capping it
/// with `{"summaryBudget": 500}` got neither, while the request still reported
/// success. The batch arm forwards the body whole, so the two arms of one
/// endpoint disagreed — the asymmetry named in
/// `plans/2026-08-25-ui-bridge-request-path-loses-fields-structurally.md`,
/// and the reason this hop is now additive.
///
/// A non-object body is returned untouched: it cannot carry a field.
fn with_diff_single_payload(body: serde_json::Value) -> serde_json::Value {
    let mut payload = body;
    let element_id = payload.get("elementId").cloned().unwrap_or_default();
    // `operation` is this endpoint's own spelling for the action name; a caller
    // following the SDK type sends `action`. Accept both rather than failing
    // whichever half is not the historical one — the same tolerance
    // `request::step_action_payload` applies to `elementId` / `element_id`.
    let operation = payload
        .get("operation")
        .or_else(|| payload.get("action"))
        .cloned()
        .unwrap_or_default();
    let params = payload
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    // Synthesize the envelope only for a body that actually names an element
    // action. `instruction` and `elementAction` are mutually exclusive on
    // `ActionWithDiffRequest`, and the old code emitted an all-null
    // `elementAction` unconditionally — junk that could never dispatch, and
    // which would now shadow the `instruction` arm that forwarding whole makes
    // reachable for the first time. A body naming neither an element nor an
    // operation is forwarded untouched.
    let names_an_action = !element_id.is_null() || !operation.is_null();
    let element_id_flat = element_id.clone();
    let params_flat = params.clone();

    if let Some(obj) = payload.as_object_mut().filter(|_| names_an_action) {
        // Do not clobber an `elementAction` the caller built themselves — that
        // is the SDK-native shape, and is already what the receiver wants.
        let envelope = serde_json::json!({
            "elementId": element_id,
            "action": operation.clone(),
            "params": params,
        });
        obj.entry("elementAction").or_insert(envelope);
        // Flat siblings for commandHandlers.ts compatibility — the same trio
        // the rebuilt payload always carried. `or_insert` rather than a plain
        // insert so a caller's own value is never overwritten: `action` is
        // filled only when they spelled it `operation`, and `elementId` /
        // `params` only when they were absent (where the old payload emitted
        // an explicit `null`, which this preserves).
        obj.entry("action").or_insert(operation);
        obj.entry("elementId").or_insert(element_id_flat);
        obj.entry("params").or_insert(params_flat);
    }
    payload
}

#[cfg(test)]
mod with_diff_single_payload_tests {
    use super::with_diff_single_payload;
    use serde_json::json;

    #[test]
    fn builds_the_element_action_envelope_from_operation() {
        let payload = with_diff_single_payload(json!({
            "elementId": "btn-1",
            "operation": "type",
            "params": {"text": "hello"},
        }));

        assert_eq!(
            payload["elementAction"],
            json!({"elementId": "btn-1", "action": "type", "params": {"text": "hello"}})
        );
        // Flat siblings preserved for commandHandlers.ts.
        assert_eq!(payload["elementId"], json!("btn-1"));
        assert_eq!(payload["action"], json!("type"));
        assert_eq!(payload["params"], json!({"text": "hello"}));
    }

    /// The regression this helper exists for: every `ActionWithDiffRequest`
    /// option other than `elementAction` used to be dropped on the floor.
    #[test]
    fn preserves_every_action_with_diff_option() {
        let payload = with_diff_single_payload(json!({
            "elementId": "btn-1",
            "operation": "click",
            "settleTimeout": 9000,
            "settleMinStable": 250,
            "scope": "#main",
            "categorize": false,
            "timeline": true,
            "timelineInterval": 50,
            "summaryBudget": 500,
            "analyzeStructured": true,
        }));

        assert_eq!(payload["settleTimeout"], json!(9000));
        assert_eq!(payload["settleMinStable"], json!(250));
        assert_eq!(payload["scope"], json!("#main"));
        assert_eq!(payload["categorize"], json!(false));
        assert_eq!(payload["timeline"], json!(true));
        assert_eq!(payload["timelineInterval"], json!(50));
        assert_eq!(payload["summaryBudget"], json!(500));
        assert_eq!(payload["analyzeStructured"], json!(true));
    }

    /// Forwarding is by identity, so a field this repo has never heard of
    /// survives the hop. Rebuilding field-by-field fails this test rather than
    /// shipping silently.
    #[test]
    fn forwards_an_unknown_future_field() {
        let payload = with_diff_single_payload(json!({
            "elementId": "btn-1",
            "operation": "click",
            "unknownFutureOptIn": {"nested": [1, 2, 3]},
        }));

        assert_eq!(payload["unknownFutureOptIn"], json!({"nested": [1, 2, 3]}));
    }

    #[test]
    fn accepts_the_sdk_action_spelling() {
        let payload = with_diff_single_payload(json!({
            "elementId": "btn-1",
            "action": "focus",
        }));

        assert_eq!(payload["elementAction"]["action"], json!("focus"));
        // The caller's own `action` is not overwritten.
        assert_eq!(payload["action"], json!("focus"));
    }

    #[test]
    fn does_not_clobber_a_caller_built_envelope() {
        let payload = with_diff_single_payload(json!({
            // The caller names the element inside the envelope AND flat, so
            // the synthesis path runs and must still defer to their envelope.
            "elementId": "btn-9",
            "elementAction": {"elementId": "btn-9", "action": "submit"},
            "scope": "#form",
        }));

        assert_eq!(
            payload["elementAction"],
            json!({"elementId": "btn-9", "action": "submit"})
        );
        assert_eq!(payload["scope"], json!("#form"));
    }

    /// `instruction` and `elementAction` are mutually exclusive. The old code
    /// synthesized an all-null `elementAction` unconditionally, which would
    /// shadow the instruction arm that forwarding whole makes reachable.
    #[test]
    fn an_instruction_only_body_gets_no_synthesized_envelope() {
        let payload = with_diff_single_payload(json!({"instruction": "click save"}));

        assert_eq!(payload["instruction"], json!("click save"));
        assert!(payload.get("elementAction").is_none());
        assert!(payload.get("action").is_none());
    }

    /// The rebuilt payload always carried a flat `{elementId, action, params}`
    /// trio for commandHandlers.ts, including explicit nulls when the caller
    /// omitted them. Forwarding the body whole must not quietly drop that.
    #[test]
    fn always_emits_the_flat_compat_trio() {
        let payload = with_diff_single_payload(json!({"operation": "click"}));

        assert_eq!(payload["action"], json!("click"));
        assert_eq!(payload["elementId"], json!(null));
        assert_eq!(payload["params"], json!(null));
        assert_eq!(payload["elementAction"]["elementId"], json!(null));
    }

    #[test]
    fn a_non_object_body_is_returned_untouched() {
        assert_eq!(with_diff_single_payload(json!("nope")), json!("nope"));
        assert_eq!(with_diff_single_payload(json!(null)), json!(null));
    }
}

/// Wait for a change to occur.
pub async fn ui_bridge_wait_for_change_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for change");
    wrap_ipc_result(ui_bridge_request_sync(&state, "wait_for_change", body).await)
}

/// Categorize the last diff.
pub async fn ui_bridge_categorize_last_diff_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Categorize last diff");
    wrap_ipc_result(
        ui_bridge_request_sync(&state, "categorize_last_diff", serde_json::json!({})).await,
    )
}

/// Compute a scoped diff.
pub async fn ui_bridge_scoped_diff_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Scoped diff");
    wrap_ipc_result(ui_bridge_request_sync(&state, "scoped_diff", body).await)
}

/// Summarize a diff.
pub async fn ui_bridge_summarize_diff_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Summarize diff");
    wrap_ipc_result(ui_bridge_request_sync(&state, "summarize_diff", body).await)
}

/// Get structured changes.
pub async fn ui_bridge_structured_changes_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Structured changes");
    wrap_ipc_result(ui_bridge_request_sync(&state, "structured_changes", body).await)
}

/// Enable the change buffer.
pub async fn ui_bridge_enable_change_buffer_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Enable change buffer");
    wrap_ipc_result(
        ui_bridge_request_sync(&state, "enable_change_buffer", serde_json::json!({})).await,
    )
}

/// Disable the change buffer.
pub async fn ui_bridge_disable_change_buffer_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Disable change buffer");
    wrap_ipc_result(
        ui_bridge_request_sync(&state, "disable_change_buffer", serde_json::json!({})).await,
    )
}

/// Drain the change buffer.
pub async fn ui_bridge_drain_change_buffer_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Drain change buffer");
    wrap_ipc_result(
        ui_bridge_request_sync(&state, "drain_change_buffer", serde_json::json!({})).await,
    )
}

/// Get the change buffer size.
pub async fn ui_bridge_get_change_buffer_size_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get change buffer size");
    wrap_ipc_result(
        ui_bridge_request_sync(&state, "get_change_buffer_size", serde_json::json!({})).await,
    )
}

/// Bookmark + change-tracking + with-diff routes (control + ai aliases).
///
/// Mirrors the original `routes()` registrations exactly — including the
/// `/control/ai/*` ↔ `/ai/*` aliasing — so the public surface is unchanged.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        // Change tracking — /control/ai/* family
        //
        // The list/save endpoint uses the plural `/bookmarks`; the per-resource
        // endpoints historically used the singular `/bookmark/{name}` variant.
        // Plural aliases (`/bookmarks/{name}` and `/bookmarks/{name}/diff`) are
        // mounted on the same handlers so callers reading the canonical
        // reference (which uses plural throughout) don't hit 404s when paths
        // drift in their head. Mirrors the alias added to the SDK's API_ROUTES
        // table at `ui-bridge/packages/ui-bridge/src/server/types.ts:1164` (commit 732e15a).
        .route(
            "/ui-bridge/control/ai/bookmarks",
            get(ui_bridge_list_bookmarks_handler).post(ui_bridge_save_bookmark_handler),
        )
        .route(
            "/ui-bridge/control/ai/bookmark/{name}",
            get(ui_bridge_get_bookmark_handler).delete(ui_bridge_delete_bookmark_handler),
        )
        .route(
            "/ui-bridge/control/ai/bookmarks/{name}",
            get(ui_bridge_get_bookmark_handler).delete(ui_bridge_delete_bookmark_handler),
        )
        .route(
            "/ui-bridge/control/ai/bookmark/{name}/diff",
            get(ui_bridge_diff_from_bookmark_handler),
        )
        .route(
            "/ui-bridge/control/ai/bookmarks/{name}/diff",
            get(ui_bridge_diff_from_bookmark_handler),
        )
        .route(
            "/ui-bridge/control/ai/execute-with-diff",
            post(ui_bridge_execute_with_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/wait-for-change",
            post(ui_bridge_wait_for_change_handler),
        )
        .route(
            "/ui-bridge/control/ai/categorize-last-diff",
            get(ui_bridge_categorize_last_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/scoped-diff",
            post(ui_bridge_scoped_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/summarize-diff",
            post(ui_bridge_summarize_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/structured-changes",
            post(ui_bridge_structured_changes_handler),
        )
        .route(
            "/ui-bridge/control/ai/change-buffer/enable",
            post(ui_bridge_enable_change_buffer_handler),
        )
        .route(
            "/ui-bridge/control/ai/change-buffer/disable",
            post(ui_bridge_disable_change_buffer_handler),
        )
        .route(
            "/ui-bridge/control/ai/change-buffer/drain",
            post(ui_bridge_drain_change_buffer_handler),
        )
        .route(
            "/ui-bridge/control/ai/change-buffer/size",
            get(ui_bridge_get_change_buffer_size_handler),
        )
        // Composite: execute action(s) with atomic change-buffer diffing
        .route(
            "/ui-bridge/control/with-diff",
            post(ui_bridge_with_diff_handler),
        )
        // /ai/* aliases (mirror of /control/ai/*)
        //
        // Same plural/singular alias rationale as above — `/ai/bookmarks/{name}`
        // and `/ai/bookmarks/{name}/diff` are accepted in addition to the
        // singular forms for symmetry with list/save.
        .route(
            "/ui-bridge/ai/bookmarks",
            get(ui_bridge_list_bookmarks_handler).post(ui_bridge_save_bookmark_handler),
        )
        .route(
            "/ui-bridge/ai/bookmark/{name}",
            get(ui_bridge_get_bookmark_handler).delete(ui_bridge_delete_bookmark_handler),
        )
        .route(
            "/ui-bridge/ai/bookmarks/{name}",
            get(ui_bridge_get_bookmark_handler).delete(ui_bridge_delete_bookmark_handler),
        )
        .route(
            "/ui-bridge/ai/bookmark/{name}/diff",
            get(ui_bridge_diff_from_bookmark_handler),
        )
        .route(
            "/ui-bridge/ai/bookmarks/{name}/diff",
            get(ui_bridge_diff_from_bookmark_handler),
        )
        .route(
            "/ui-bridge/ai/execute-with-diff",
            post(ui_bridge_execute_with_diff_handler),
        )
        .route(
            "/ui-bridge/ai/wait-for-change",
            post(ui_bridge_wait_for_change_handler),
        )
        .route(
            "/ui-bridge/ai/categorize-last-diff",
            get(ui_bridge_categorize_last_diff_handler),
        )
        .route(
            "/ui-bridge/ai/scoped-diff",
            post(ui_bridge_scoped_diff_handler),
        )
        .route(
            "/ui-bridge/ai/summarize-diff",
            post(ui_bridge_summarize_diff_handler),
        )
        .route(
            "/ui-bridge/ai/structured-changes",
            post(ui_bridge_structured_changes_handler),
        )
        .route(
            "/ui-bridge/ai/change-buffer/enable",
            post(ui_bridge_enable_change_buffer_handler),
        )
        .route(
            "/ui-bridge/ai/change-buffer/disable",
            post(ui_bridge_disable_change_buffer_handler),
        )
        .route(
            "/ui-bridge/ai/change-buffer/drain",
            post(ui_bridge_drain_change_buffer_handler),
        )
        .route(
            "/ui-bridge/ai/change-buffer/size",
            get(ui_bridge_get_change_buffer_size_handler),
        )
}

/// Static (method, path) tuples corresponding to every route registered
/// by `routes()`. Concatenated into the parent `route_manifest()` so the
/// `/ui-bridge/_routes` discovery endpoint stays in sync.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        // /control/ai/*
        ("GET", "/ui-bridge/control/ai/bookmarks"),
        ("POST", "/ui-bridge/control/ai/bookmarks"),
        ("GET", "/ui-bridge/control/ai/bookmark/{name}"),
        ("GET", "/ui-bridge/control/ai/bookmarks/{name}"),
        ("DELETE", "/ui-bridge/control/ai/bookmark/{name}"),
        ("DELETE", "/ui-bridge/control/ai/bookmarks/{name}"),
        ("GET", "/ui-bridge/control/ai/bookmark/{name}/diff"),
        ("GET", "/ui-bridge/control/ai/bookmarks/{name}/diff"),
        ("POST", "/ui-bridge/control/ai/execute-with-diff"),
        ("POST", "/ui-bridge/control/ai/wait-for-change"),
        ("GET", "/ui-bridge/control/ai/categorize-last-diff"),
        ("POST", "/ui-bridge/control/ai/scoped-diff"),
        ("POST", "/ui-bridge/control/ai/summarize-diff"),
        ("POST", "/ui-bridge/control/ai/structured-changes"),
        ("POST", "/ui-bridge/control/ai/change-buffer/enable"),
        ("POST", "/ui-bridge/control/ai/change-buffer/disable"),
        ("POST", "/ui-bridge/control/ai/change-buffer/drain"),
        ("GET", "/ui-bridge/control/ai/change-buffer/size"),
        // Composite
        ("POST", "/ui-bridge/control/with-diff"),
        // /ai/* aliases
        ("GET", "/ui-bridge/ai/bookmarks"),
        ("POST", "/ui-bridge/ai/bookmarks"),
        ("GET", "/ui-bridge/ai/bookmark/{name}"),
        ("GET", "/ui-bridge/ai/bookmarks/{name}"),
        ("DELETE", "/ui-bridge/ai/bookmark/{name}"),
        ("DELETE", "/ui-bridge/ai/bookmarks/{name}"),
        ("GET", "/ui-bridge/ai/bookmark/{name}/diff"),
        ("GET", "/ui-bridge/ai/bookmarks/{name}/diff"),
        ("POST", "/ui-bridge/ai/execute-with-diff"),
        ("POST", "/ui-bridge/ai/wait-for-change"),
        ("GET", "/ui-bridge/ai/categorize-last-diff"),
        ("POST", "/ui-bridge/ai/scoped-diff"),
        ("POST", "/ui-bridge/ai/summarize-diff"),
        ("POST", "/ui-bridge/ai/structured-changes"),
        ("POST", "/ui-bridge/ai/change-buffer/enable"),
        ("POST", "/ui-bridge/ai/change-buffer/disable"),
        ("POST", "/ui-bridge/ai/change-buffer/drain"),
        ("GET", "/ui-bridge/ai/change-buffer/size"),
    ]
}
