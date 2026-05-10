//! Wrapper-change event broadcaster.
//!
//! A single broadcast channel per process. Registry mutations
//! (`rescan`, `rescan_all`, `remove`) emit a [`WrapperChanged`] event
//! here, and the SSE handler at `GET /wrappers/events` forwards each
//! event to every subscriber.
//!
//! Primary consumer: the stdio MCP bin (`bin/wrappers_mcp.rs`), which
//! uses these events to push `notifications/tools/list_changed` to its
//! AI-client peer so newly-installed wrappers surface as tools without
//! the user restarting the AI session.
//!
//! Mirror of `spec_api::events` — same one-channel pattern.

use std::sync::OnceLock;
use tokio::sync::broadcast;

use serde::{Deserialize, Serialize};

/// Single event variant — the bin only needs to know "something
/// changed, refetch /wrappers". A richer payload would be over-coupling
/// for today's only consumer; future consumers can read the full list
/// from `GET /wrappers` after the wakeup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WrapperChanged {
    /// Epoch milliseconds the event was emitted at.
    pub at_ms: u64,
}

/// Broadcast capacity. Slow subscribers that fall this far behind will
/// Lag; the SSE handler logs and continues. 256 is plenty for the
/// install/uninstall cadence we expect (a handful per session at most).
const CAPACITY: usize = 256;

static SENDER: OnceLock<broadcast::Sender<WrapperChanged>> = OnceLock::new();

fn sender() -> &'static broadcast::Sender<WrapperChanged> {
    SENDER.get_or_init(|| broadcast::channel::<WrapperChanged>(CAPACITY).0)
}

pub fn subscribe() -> broadcast::Receiver<WrapperChanged> {
    sender().subscribe()
}

/// Emit a change event. No-op when there are no subscribers (the
/// channel just drops the message — that's the broadcast-channel
/// contract and what we want here).
pub fn emit() {
    let _ = sender().send(WrapperChanged { at_ms: now_ms() });
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
