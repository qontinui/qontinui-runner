//! Trace API change-event broadcaster.
//!
//! Stub for v1 — `trace.changed` SSE is not exposed yet, but the broadcast
//! channel is wired so downstream code (and a follow-up SSE endpoint) can
//! subscribe today without further plumbing. Mirrors `spec_api::events` so
//! the migration to a public SSE handler is mechanical.

use std::sync::OnceLock;
use tokio::sync::broadcast;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceChanged {
    /// The recording session whose event set was just mutated.
    pub recording_session_id: String,
    /// "session-saved" today; future kinds may include "session-replayed"
    /// once `/trace/replay` lands in v2.
    pub kind: String,
    /// How many events were touched by the change (0 when not applicable).
    pub event_count: i64,
    /// Epoch milliseconds the event was emitted at.
    pub at_ms: u64,
}

/// Broadcast capacity. Slow subscribers will Lag if they fall this far
/// behind; consumers are expected to drop laggy subscriptions and resubscribe.
const CAPACITY: usize = 256;

static SENDER: OnceLock<broadcast::Sender<TraceChanged>> = OnceLock::new();

fn sender() -> &'static broadcast::Sender<TraceChanged> {
    SENDER.get_or_init(|| broadcast::channel::<TraceChanged>(CAPACITY).0)
}

#[allow(dead_code)]
pub fn subscribe() -> broadcast::Receiver<TraceChanged> {
    sender().subscribe()
}

pub fn emit(event: TraceChanged) {
    // Ignore SendError: it just means there are no subscribers — drop the
    // event silently in that case.
    let _ = sender().send(event);
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
