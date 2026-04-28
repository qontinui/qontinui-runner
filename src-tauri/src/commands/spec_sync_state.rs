//! Spec-sync state mirror — receives state updates from useSpecSync and
//! makes them available to SSE consumers via a `tokio::sync::watch` channel.
//!
//! Background:
//!   The frontend's `useSpecSync` hook (src/hooks/useSpecSync.ts) drives a
//!   multi-phase AI sync that takes ~3.5 min/spec. External tools (manual-test
//!   sessions, automation scripts) used to have to poll `document.body.innerText`
//!   regex to track progress because there was no streaming endpoint.
//!
//!   This module is the runner-side mirror: the hook calls
//!   `invoke("push_spec_sync_state", ...)` after every `setState`, the
//!   command pushes the JSON into a `watch::Sender`, and SSE consumers
//!   subscribe to a cloned `watch::Receiver` (see
//!   `mcp::ui_bridge::sdk_spec_sync` for the streaming endpoint).
//!
//! Backpressure:
//!   `tokio::sync::watch` keeps only the latest value — older states are
//!   dropped if the SSE consumer is slow. This is the desired behavior for
//!   progress mirroring (we always want the freshest snapshot).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;

/// Pass-through wrapper around the JSON payload pushed by the frontend.
///
/// The hook's `SpecSyncState` interface lives in
/// `src/hooks/useSpecSync.ts:38-43` — we deliberately don't mirror the
/// shape in Rust so we can evolve the frontend struct without breaking
/// the bus. The HTTP/SSE consumers receive the JSON verbatim.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SpecSyncStateMirror(pub Value);

/// Single-producer / many-consumer bus for the latest `SpecSyncState`.
///
/// Construct one and `.manage(...)` it on the Tauri app builder; both the
/// `push_spec_sync_state` command and the SSE handler resolve the same
/// instance via `app_handle.try_state::<Arc<SpecSyncStateBus>>()`.
pub struct SpecSyncStateBus {
    tx: watch::Sender<SpecSyncStateMirror>,
    /// Subscribers clone this receiver to observe updates.
    pub rx: watch::Receiver<SpecSyncStateMirror>,
}

impl SpecSyncStateBus {
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(SpecSyncStateMirror::default());
        Self { tx, rx }
    }

    /// Replace the latest state. Send errors (no receivers) are ignored —
    /// the receiver kept on the bus itself ensures this never fires, but
    /// we don't want the frontend's invoke to fail in that edge case.
    pub fn push(&self, state: SpecSyncStateMirror) {
        let _ = self.tx.send(state);
    }
}

impl Default for SpecSyncStateBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Tauri command — invoked by the frontend's `useSpecSync` effect after
/// each `setState`. The `state` argument is the raw `SpecSyncState` JSON;
/// we don't deserialize it into a typed struct because we want frontend
/// shape evolution to be lossless across the bus.
#[tauri::command]
pub async fn push_spec_sync_state(
    state: serde_json::Value,
    bus: tauri::State<'_, std::sync::Arc<SpecSyncStateBus>>,
) -> Result<(), String> {
    bus.push(SpecSyncStateMirror(state));
    Ok(())
}
