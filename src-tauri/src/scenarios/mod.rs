//! Scenario projection — Section 11, Phase B2 of the UI Bridge redesign.
//!
//! Two HTTP endpoints expose the scenario-projection layer to Phase B3 MCP
//! tools (Python, separate phase) which call this surface over HTTP:
//!
//!   - `GET /scenarios/projection?ir_doc_id=<id>` — pure, deterministic
//!     projection of an IR document. Native Rust implementation in
//!     [`projection::project_scenarios`].
//!
//!   - `GET /scenarios/current?ir_doc_id=<id>` — runtime-aware projection
//!     that combines the static projection with live-registry signal. The
//!     registry lives in the React webview, so the handler IPC's into the
//!     webview via `ui-bridge:project-current-scenario-request` and waits
//!     for the response.
//!
//! Both endpoints return the wire envelope:
//!
//! ```text
//! 200 { ok: true,  data: <projection JSON> }
//! 404 { ok: false, error: "ir-doc-not-found" }
//! 500 { ok: false, error: <message> }
//! ```
//!
//! The TS counterparts of the projection functions live in
//! `ui-bridge-auto/src/state/scenario-projection.ts` (Phase B1, just shipped).
//! The static Rust port intentionally re-implements the deterministic
//! sorting discipline rather than IPC-ing the static call out to the
//! webview — keeping the determinism gate inside the HTTP boundary.

pub mod handlers;
pub mod projection;
pub mod types;

#[cfg(test)]
mod tests;

use axum::routing::get;
use axum::Router;
use std::sync::Arc;

use crate::mcp::types::ApiState;

/// Routes for the Scenarios API. Mounted alongside `/spec/...` in
/// `mcp_api.rs::create_router`.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route(
            "/scenarios/projection",
            get(handlers::get_scenarios_projection),
        )
        .route("/scenarios/current", get(handlers::get_scenarios_current))
}
