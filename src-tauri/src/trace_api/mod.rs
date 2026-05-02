//! Trace API — Section 5b deliverable of the UI Bridge redesign.
//!
//! Runner-side persistence + read API for the in-memory causal trace shipped
//! by Section 5 Plan A in `ui-bridge-auto`. Mounted at `/trace/...` on the
//! runner's Axum router (port 9876), mirroring the existing `/spec/...`
//! Spec API surface.
//!
//! ## Required migration
//!
//! This module assumes two new columns on `project.ui_bridge_events`:
//!
//! ```sql
//! ALTER TABLE project.ui_bridge_events
//!   ADD COLUMN recording_session_id text,
//!   ADD COLUMN caused_by_event_id bigint REFERENCES project.ui_bridge_events(id);
//! CREATE INDEX ui_bridge_events_recording_session_idx
//!   ON project.ui_bridge_events(recording_session_id)
//!   WHERE recording_session_id IS NOT NULL;
//! ```
//!
//! These are added by the Alembic migration
//! `section_5b_01_ui_bridge_causal_columns`, which is applied later by the
//! user on the shared Postgres host ("spaceship"). Until that migration runs,
//! enabling the trace API will produce SQL errors at runtime.
//!
//! ## Runtime gate
//!
//! The routes are mounted only when `settings.trace_api.enabled` is true.
//! The default is `false`, matching the "shipped but inert" rollout pattern
//! used by the rest of the redesign.
//!
//! ## Storage approach
//!
//! All DB access uses raw `tokio_postgres` via `PgDb::pool()`. The Clorinde
//! queries declared in `queries/ui_bridge.sql` (under the `SECTION 5B` block)
//! reference the new columns and would fail to validate against the cached
//! `schema.pg.sql.generated` until spaceship regenerates it. Raw SQL keeps
//! this module compilable today; the Clorinde swap-over is mechanical once
//! the schema catches up.
//!
//! ## Submodules
//! - [`types`]      Wire types — inbound recorder session + outbound rows
//! - [`storage`]    Raw `tokio_postgres` queries
//! - [`responses`]  Empty/error envelope shapes — every empty response carries `reason`
//! - [`events`]     Broadcast channel for `trace.changed` (SSE wiring deferred)
//! - [`handlers`]   Axum handlers; one per endpoint
//!
//! Entry point: [`routes`]. Merge into the main router from `mcp_api.rs`.

pub mod events;
pub mod handlers;
pub mod responses;
pub mod storage;
pub mod types;

use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;

use crate::mcp::types::ApiState;

/// Routes for the Trace API. Mount only when `settings.trace_api.enabled`
/// is true — wiring lives in `mcp_api.rs` next to the `/spec/...` merge.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/trace/health", get(handlers::get_health))
        .route("/trace/list", get(handlers::get_list))
        .route(
            "/trace/session/{recording_session_id}",
            get(handlers::get_session),
        )
        .route(
            "/trace/event/{id}/causal-chain",
            get(handlers::get_causal_chain),
        )
        .route("/trace/save", post(handlers::post_save))
        .route("/trace/replay", post(handlers::post_replay))
}
