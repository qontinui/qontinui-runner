//! State discovery pipeline — co-occurrence-driven state definition.
//!
//! See `qontinui-dev-notes/session-prompts/state-definition-observation-pipeline.md`
//! for the end-to-end design. This module implements Steps 2 and 3 of that plan:
//! fingerprinting + capture hook (capture.rs), and the derivation endpoint
//! that reshapes accumulated observations and delegates clustering to the
//! Python `qontinui.discovery` primitive (derive.rs).

pub mod capture;
pub mod derive;
pub mod fingerprint;

pub use capture::enqueue_observation;
pub use fingerprint::stable_element_fingerprint;

/// Axum routes exported by this module. Wire into the main router in
/// `mcp_api.rs` with `.merge(crate::state_discovery::routes())`.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::post;
    axum::Router::new().route("/state-discovery/derive", post(derive::derive_handler))
}
