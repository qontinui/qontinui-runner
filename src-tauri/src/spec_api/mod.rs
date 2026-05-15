//! Spec API — Section 2 of the UI Bridge redesign.
//!
//! Consumer-facing HTTP surface for the IR-based spec system, mounted at
//! `/spec/...` on the runner's Axum router (port 9876). The Spec API stores
//! IR documents and serves both the IR shape (authoring-time) and a
//! bundled-page projection (legacy `*.spec.uibridge.json` shape) so existing
//! consumers (`/update-spec`, runner spec drift / verify, error monitor
//! curator, spec experimentation, AI session) keep working through the
//! migration that lands in section 3.
//!
//! Submodules:
//! - [`types`]      Rust mirrors of `IrPageSpec` + legacy spec types
//! - [`projection`] Pure `IrPageSpec -> LegacySpec` projection (Rust port of
//!   the TS `projectIRToBundledPage`)
//! - [`storage`]    Filesystem layer for the storage layout under `<runner>/specs/`
//! - [`responses`]  Empty/error envelope shapes — every empty response carries `reason`
//! - [`events`]     Broadcast channel for `spec.changed` SSE events
//! - [`handlers`]   Axum handlers; one per endpoint
//!
//! Entry point: [`routes`]. Merge into the main router from `mcp_api.rs`.

pub mod distinctness;
pub mod events;
pub mod handlers;
pub mod hashing;
pub mod projection;
pub mod responses;
pub mod storage;
pub mod types;

#[cfg(feature = "spec-authoring")]
pub mod proposals;

#[cfg(test)]
mod tests;

use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;

use crate::mcp::types::ApiState;

/// Routes for the Spec API. Mounted alongside the existing
/// `/ui-bridge/...` routes — `/spec/...` is a separate top-level prefix per
/// the Section 2 plan ("Spec API is consumed differently from the UI
/// Bridge").
pub fn routes() -> Router<Arc<ApiState>> {
    let r = Router::new()
        .route("/spec/health", get(handlers::get_health))
        .route("/spec/get", get(handlers::get_file))
        .route("/spec/page/{id}", get(handlers::get_page))
        .route("/spec/graph", get(handlers::get_graph))
        .route("/spec/list", get(handlers::get_list))
        .route("/spec/query", post(handlers::post_query))
        .route("/spec/derive", post(handlers::post_derive))
        .route("/spec/diff", get(handlers::get_diff))
        .route("/spec/author", post(handlers::post_author))
        .route("/spec/validate", post(handlers::post_validate))
        .route("/spec/subscribe", get(handlers::get_subscribe));

    // Stream E (Flywheel) — coverage-growth queue endpoints. Gated behind
    // `spec-authoring` so v1.0 release builds don't expose the proposals
    // queue surface area.
    #[cfg(feature = "spec-authoring")]
    let r = r
        .route("/spec/proposals/scan", post(proposals::post_scan))
        .route("/spec/proposals", get(proposals::get_list))
        .route(
            "/spec/proposals/{id}/execute",
            post(proposals::post_execute),
        );

    r
}
