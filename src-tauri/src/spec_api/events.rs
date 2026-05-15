//! Spec API broadcast channel.
//!
//! A single `tokio::sync::broadcast` channel per process — each
//! `/spec/subscribe` SSE handler subscribes and forwards the events. Every
//! write path on the Spec API emits a [`SpecApiEvent`] here on success.
//!
//! The event taxonomy itself lives in [`qontinui_types::spec_api_events`]
//! so the Rust enum, the generated TS bindings, and the generated Python
//! Pydantic models stay in sync.
//!
//! ## Emit sites
//!
//! - `SpecChanged` — [`handlers.rs` post_author](super::handlers) emits after
//!   a successful IR + projection write.
//! - `SpecCheckInvoked` / `SpecCheckCompleted` — emitted by the
//!   `spec_api/spec_check.rs` HTTP handlers when Plan 06 Step 1's adapter
//!   layer lands (Stream C dependency).
//! - `SpecCheckPolicyViolation` — emitted by Plan 03's
//!   `step_executor/spec_check.rs` workflow-step handler when the conjunct
//!   evaluator returns a Fail.
//! - `FlywheelProposalPromoted` / `FlywheelProposalDemoted` — emitted by the
//!   Plan 05 Step 9 sweep handler (`POST /spec/proposals/sweep-pending` +
//!   `storage::promote_pending` / `storage::demote_pending`). The variants
//!   are reserved here so the wire is ready; the emit sites land when the
//!   sweep handler ships.

use std::sync::OnceLock;
use tokio::sync::broadcast;

pub use qontinui_types::spec_api_events::{SpecApiEvent, SpecChanged};

/// Broadcast capacity. Slow subscribers will Lag if they fall this far
/// behind; the SSE handler logs and continues.
const CAPACITY: usize = 256;

static SENDER: OnceLock<broadcast::Sender<SpecApiEvent>> = OnceLock::new();

fn sender() -> &'static broadcast::Sender<SpecApiEvent> {
    SENDER.get_or_init(|| broadcast::channel::<SpecApiEvent>(CAPACITY).0)
}

pub fn subscribe() -> broadcast::Receiver<SpecApiEvent> {
    sender().subscribe()
}

pub fn emit(event: SpecApiEvent) {
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
