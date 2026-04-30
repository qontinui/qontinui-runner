//! Spec API change-event broadcaster.
//!
//! A single broadcast channel per process — each `/spec/subscribe` SSE
//! handler subscribes and forwards the events. `POST /spec/author` (and any
//! future write path) emits a `SpecChanged` here on success.

use std::sync::OnceLock;
use tokio::sync::broadcast;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecChanged {
    pub page_id: String,
    /// "ir-and-projection" today; future kinds may include
    /// "projection-only" if a regen runs without an IR write.
    pub kind: String,
    /// Epoch milliseconds the event was emitted at.
    pub at_ms: u64,
}

/// Broadcast capacity. Slow subscribers will Lag if they fall this far
/// behind; the SSE handler logs and continues.
const CAPACITY: usize = 256;

static SENDER: OnceLock<broadcast::Sender<SpecChanged>> = OnceLock::new();

fn sender() -> &'static broadcast::Sender<SpecChanged> {
    SENDER.get_or_init(|| broadcast::channel::<SpecChanged>(CAPACITY).0)
}

pub fn subscribe() -> broadcast::Receiver<SpecChanged> {
    sender().subscribe()
}

pub fn emit(event: SpecChanged) {
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
