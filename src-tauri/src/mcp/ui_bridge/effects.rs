//! Effects endpoints — D3 effect-calculus recent-effect ledger.
//!
//! Read-only proxy over the SDK's `GET /effects/recent` route (handler
//! `getRecentEffects`). The SDK owns the effect ledger (each entry is an
//! `EffectRecordEntry { requestId?, action, elementId?, outcome, cause,
//! verification, timestamp }`); the runner exposes it through the
//! `/ui-bridge/effects/recent` HTTP surface so agents / dashboards driving the
//! runner can read the recent predicted-vs-observed outcomes without reaching
//! the SDK directly.
//!
//! This is a "runner direct" family route per `CONTRACT.md`: the handler lives
//! here and is registered in this family's `routes()` + `route_entries()`. It
//! forwards to the SDK's HTTP surface via `crate::mcp::sdk_client::sdk_request`
//! (the same passthrough helper used by the SDK-proxy GETs in `sdk_client.rs`),
//! returning the SDK's `APIResponse<EffectRecordEntry[]>` JSON verbatim. The
//! `?limit=N` query param is forwarded to the SDK for server-side truncation.

use axum::{
    extract::{Query, State},
    response::Json,
};
use reqwest::Method;
use serde::Deserialize;
use std::sync::Arc;

use crate::mcp::sdk_client::sdk_request;
use crate::mcp::types::ApiState;

/// Query parameters for `GET /ui-bridge/effects/recent`.
#[derive(Debug, Deserialize)]
pub struct RecentEffectsQuery {
    /// Maximum number of recent effect records to return. Forwarded to the SDK
    /// verbatim; the SDK applies the default/cap when omitted.
    pub limit: Option<i64>,
}

/// GET /ui-bridge/effects/recent — proxy to the SDK's `getRecentEffects`.
///
/// Forwards the `limit` query param and relays the SDK's
/// `APIResponse<EffectRecordEntry[]>` JSON. On transport failure returns a
/// `{ success: false, error }` envelope (HTTP 200) mirroring the other
/// `sdk_request`-backed proxies in `sdk_client.rs`.
pub async fn ui_bridge_effects_recent_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<RecentEffectsQuery>,
) -> Json<serde_json::Value> {
    let path = build_sdk_path(query.limit);
    match sdk_request(&state, Method::GET, &path, None).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e })),
    }
}

/// Build the SDK-side path, forwarding `limit` as a query param when present.
fn build_sdk_path(limit: Option<i64>) -> String {
    match limit {
        Some(limit) => format!("/effects/recent?limit={}", limit),
        None => "/effects/recent".to_string(),
    }
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::get;
    axum::Router::new().route(
        "/ui-bridge/effects/recent",
        get(ui_bridge_effects_recent_handler),
    )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[("GET", "/ui-bridge/effects/recent")]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_entries_lists_recent_effects() {
        let entries = route_entries();
        assert_eq!(entries, &[("GET", "/ui-bridge/effects/recent")]);
    }

    #[test]
    fn build_sdk_path_forwards_limit() {
        assert_eq!(build_sdk_path(Some(25)), "/effects/recent?limit=25");
        assert_eq!(build_sdk_path(None), "/effects/recent");
    }
}
