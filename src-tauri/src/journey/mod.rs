//! Journey twin producer — the OBSERVED user path through an app.
//!
//! Phase 1 (runner half) of plan
//! `2026-09-20-ui-bridge-represents-the-users-path-and-the-passage-of-time`.
//! Every agent UI action through the runner closes one row of
//! `project.journey_edge_observations` (from-node → to-node, trigger, outcome,
//! provenance), and every seen-but-never-activated affordance sits in
//! `project.journey_frontier`. The wire types and the contract are
//! `qontinui_types::journey`; the tables are qontinui-web migration
//! `journey_01_edge_ledger`.
//!
//! Submodules:
//! - [`node`]      pure node resolution + affordance extraction
//! - [`cursor`]    the pure pending-edge state machine and triggers
//! - [`capture`]   the choke-point entry points, the schema probe, and the
//!   single worker that writes
//! - [`frontier`]  the batched frontier upsert
//! - [`health`]    `GET /apps/{app_id}/journey/health`
//! - [`retention`] the runner-side 90-day prune of its own database

pub mod capture;
pub mod cursor;
pub mod frontier;
pub mod health;
pub mod node;
pub mod retention;

/// Axum routes exported by this module. Merged into the main router in
/// `mcp_api.rs` beside `spec_api::routes()`, under the same per-app
/// `/apps/{app_id}/...` prefix.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::get;
    axum::Router::new().route(
        "/apps/{app_id}/journey/health",
        get(health::get_journey_health),
    )
}
