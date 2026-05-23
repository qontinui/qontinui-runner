//! Coord-sync loop — STUB for Phase 2.
//!
//! Plan §Phase 3 implements the full drain/heartbeat/replay logic. Phase 2
//! ships the API surface so [`crate::session::Session`] can reference it
//! without a runtime loop yet — the [`CoordSync`] facade records events
//! into the local outbox (already-Phase-2) and does NOT yet POST to coord.
//!
//! When Phase 3 lands:
//! - [`CoordSync::start`] spawns a tokio task that wakes every
//!   `QONTINUI_SESSION_HEARTBEAT_SECS` (default 15s, plan §D13) and
//!   drains [`crate::session::local_store::OutboxWriter::pending`] in
//!   `(session_id, seq)` order.
//! - Each batch is POSTed to coord at `/sessions/:id/events` (or the
//!   per-event endpoint, TBD by the Phase 3 API contract); successful
//!   pushes ACK via [`crate::session::local_store::OutboxWriter::ack`].
//! - Disconnect tolerance: queue grows locally, replays on reconnect
//!   (idempotent by `UNIQUE (session_id, seq)` on coord's
//!   `coord.session_events`).
//! - Conflict-on-acquire: 409 from coord → session enters
//!   [`crate::session::SessionState::PendingResolution`] → conflict modal
//!   fires (plan §Phase 6).

use std::sync::Arc;

use super::local_store::OutboxWriter;

/// Phase 3-shaped coord-sync handle. Today this is just a wrapper around
/// the local outbox so callers can write through one facade and the
/// Phase 3 expansion is a single-file change.
#[derive(Clone)]
pub struct CoordSync {
    outbox: Arc<OutboxWriter>,
}

impl CoordSync {
    pub fn new(outbox: Arc<OutboxWriter>) -> Self {
        Self { outbox }
    }

    /// Borrow the local outbox. Phase 2 callers (Session lifecycle) write
    /// directly through this; Phase 3 keeps the same surface and adds the
    /// drain task underneath.
    pub fn outbox(&self) -> &OutboxWriter {
        &self.outbox
    }

    /// Phase 3 entry-point — currently a no-op. Keeps the call-site
    /// stable so main.rs can wire it now and Phase 3 fills in the body.
    pub fn start_drain_task(&self) {
        // intentionally empty in Phase 2
    }
}
