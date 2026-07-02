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

pub mod apps;
pub mod auth_injection;
pub mod distinctness;
pub mod events;
pub mod handlers;
pub mod hashing;
pub mod projection;
pub mod responses;
pub mod spec_check;
pub mod storage;
pub mod thresholds;
pub mod types;
pub mod util;

#[cfg(feature = "spec-authoring")]
pub mod proposals;
#[cfg(feature = "spec-authoring")]
pub mod validator;

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
///
/// Per spec-multi-app Stream C (PLAN.md §C.1 + §C.2), every previously
/// global `/spec/*` route is re-mounted under `/apps/:app_id/spec/*`.
/// Unscoped `/spec/list`, `/spec/get`, etc. return 404 — the regression
/// guard is `unscoped_spec_list_route_returns_404` in `spec_api/tests.rs`.
/// Spec-check (`/spec-check`, `/spec-check/batch`) remains top-level;
/// Stream D extends its request body with `app_id`.
pub fn routes() -> Router<Arc<ApiState>> {
    let r = Router::new()
        // App registry (spec-multi-app Stream B). The five `/apps/*` routes
        // live alongside the per-app `/apps/:app_id/spec/*` mount below.
        .route(
            "/apps",
            get(apps::handle_list_apps).post(apps::handle_register_app),
        )
        .route(
            "/apps/{app_id}",
            get(apps::handle_get_app)
                .patch(apps::handle_update_app)
                .delete(apps::handle_delete_app),
        )
        // Per-app Spec API surface (Stream C — PLAN.md §C.2).
        .route("/apps/{app_id}/spec/health", get(handlers::get_health))
        .route("/apps/{app_id}/spec/get", get(handlers::get_file))
        .route("/apps/{app_id}/spec/page/{id}", get(handlers::get_page))
        .route("/apps/{app_id}/spec/graph", get(handlers::get_graph))
        .route("/apps/{app_id}/spec/list", get(handlers::get_list))
        .route("/apps/{app_id}/spec/query", post(handlers::post_query))
        .route("/apps/{app_id}/spec/derive", post(handlers::post_derive))
        .route("/apps/{app_id}/spec/diff", get(handlers::get_diff))
        .route("/apps/{app_id}/spec/author", post(handlers::post_author))
        .route(
            "/apps/{app_id}/spec/validate",
            post(handlers::post_validate),
        )
        .route(
            "/apps/{app_id}/spec/subscribe",
            get(handlers::get_subscribe),
        )
        // Core spec-check surface (Plan 03 Steps 1-3) — NOT feature-gated,
        // stays top-level. Stream D extends the request body with `app_id`.
        .route("/spec-check", post(spec_check::post_spec_check))
        .route("/spec-check/batch", post(spec_check::post_spec_check_batch));

    // Stream E (Flywheel) — coverage-growth queue endpoints. Gated behind
    // `spec-authoring` so v1.0 release builds don't expose the proposals
    // queue surface area. Stream C re-mounts these under
    // `/apps/:app_id/spec/proposals/*` (PLAN.md §C.2).
    #[cfg(feature = "spec-authoring")]
    let r = r
        .route(
            "/apps/{app_id}/spec/proposals/scan",
            post(proposals::post_scan),
        )
        .route("/apps/{app_id}/spec/proposals", get(proposals::get_list))
        .route(
            "/apps/{app_id}/spec/proposals/{id}/execute",
            post(proposals::post_execute),
        )
        .route(
            "/apps/{app_id}/spec/proposals/sweep-pending",
            post(proposals::post_sweep_pending),
        )
        .route("/spec/proposals/metrics", get(proposals::get_metrics));

    r
}
