//! Git Supervision Channel (D5 Phase 1).
//!
//! The D5 supervision channel keeps the runner's Ω(t) model synchronized
//! with the actual environment by treating git events as **proposals** —
//! not workflow triggers. Instead of spawning a workflow when a commit
//! lands, the trigger system routes the event into this module, which:
//!
//! 1. Wraps it as a typed `GitSupervisionEvent` with provenance metadata,
//! 2. Appends it to a bounded in-process ring buffer (last 256 events),
//! 3. Emits a Tauri event on channel `"git-supervision"` so the frontend
//!    `useGitSupervision` hook can thread the proposal into the spec
//!    compile pipeline (`compileStateMachineFromSpecs`).
//!
//! The ring buffer is intentionally in-memory only — Phase 1 does not
//! persist supervision events to Postgres. If the channel proves useful,
//! persistence is Phase 1.5.
//!
//! ## Wiring
//!
//! - Watcher: `trigger_system::watchers::git_watcher` (commits) +
//!   `trigger_system::watchers::file_watcher` (spec FS drift).
//! - Dispatcher: `trigger_system::service::handle_event` checks the
//!   trigger's effective `ActionType` (see `trigger_system::executor`).
//!   When the action is `SupervisionProposal`, it calls
//!   [`handle_supervision_action`] instead of executing a workflow.
//! - Bootstrap: at runner boot, `bootstrap_default_supervision_triggers`
//!   inserts a `__git-supervision-default__` row and a
//!   `__spec-file-supervision-default__` row if they don't already exist.
//!   The bootstrap is skipped when `QONTINUI_DISABLE_GIT_SUPERVISION=1`.
//! - Frontend: listens on the `"git-supervision"` Tauri event channel and
//!   maintains its own bounded buffer; the HTTP endpoint
//!   `GET /git-supervision/recent-events` exposes the ring for diagnostics
//!   and headless smoke tests.

use std::collections::VecDeque;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tokio::sync::RwLock;
use tracing::{debug, warn};

pub mod commit_forwarder;

/// Maximum number of supervision events kept in the in-process ring buffer.
/// When full, the oldest entry is evicted to make room for the newest.
pub const RING_CAPACITY: usize = 256;

/// Tauri event channel that the frontend listens on.
pub const TAURI_EVENT_CHANNEL: &str = "git-supervision";

/// A single supervision proposal observed from a watch channel.
///
/// The serde rename to camelCase is **load-bearing** for the Tauri event
/// payload: the TypeScript-side `useGitSupervision` hook expects camelCase
/// keys when it deserializes the event. See
/// `proj_tauri_event_payload_camelcase.md` in the user-memory index for
/// the failure mode this prevents (snake_case keys silently become
/// `undefined` on the listener side).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSupervisionEvent {
    /// Unique event id (uuid v4).
    pub id: String,
    /// Event kind — one of `"commit"`, `"branch_switch"`, `"tag"`,
    /// `"spec_changed"`, `"working_tree_drift"`. Free-form to allow Phase
    /// 1.5+ extensions without a typed-enum migration.
    pub kind: String,
    /// Kind-specific payload (free-form JSON, contract documented in the
    /// D5 Phase 1 plan §3.2).
    pub payload: serde_json::Value,
    /// Unix epoch milliseconds when this event was recorded.
    pub timestamp: i64,
    /// Provenance tag — always `"git-supervised"` in Phase 1. Reserved for
    /// future channels (e.g. `"file-supervised"`, `"ci-supervised"`).
    pub provenance: String,
}

impl GitSupervisionEvent {
    /// Build a new event with a generated UUID and current epoch-ms timestamp.
    pub fn new(
        kind: impl Into<String>,
        payload: serde_json::Value,
        provenance: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            kind: kind.into(),
            payload,
            timestamp: chrono::Utc::now().timestamp_millis(),
            provenance: provenance.into(),
        }
    }
}

/// Bounded ring buffer of recent supervision events, plus the Tauri handle
/// used to fan events out to the frontend.
///
/// Cloned cheaply (`Arc<Inner>`); stored on `ApiState` so HTTP handlers can
/// read the buffer and the trigger dispatcher can append to it.
#[derive(Clone)]
pub struct SupervisionState {
    inner: Arc<RwLock<VecDeque<GitSupervisionEvent>>>,
}

impl Default for SupervisionState {
    fn default() -> Self {
        Self {
            inner: Arc::new(RwLock::new(VecDeque::with_capacity(RING_CAPACITY))),
        }
    }
}

impl SupervisionState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an event, evicting the oldest entry if the ring is full.
    /// Also emits the event on the `git-supervision` Tauri channel so
    /// the frontend hook can react.
    pub async fn record_event(&self, event: GitSupervisionEvent, app_handle: &tauri::AppHandle) {
        {
            let mut buf = self.inner.write().await;
            if buf.len() >= RING_CAPACITY {
                buf.pop_front();
            }
            buf.push_back(event.clone());
        }

        // Tauri's `Emitter::emit` only fails if the AppHandle is shutting
        // down or the payload can't serialize — neither is recoverable
        // here, so we log and move on.
        if let Err(e) = app_handle.emit(TAURI_EVENT_CHANNEL, &event) {
            warn!(
                "Failed to emit git-supervision Tauri event (kind={}): {}",
                event.kind, e
            );
        } else {
            debug!(
                "Emitted git-supervision event id={} kind={} provenance={}",
                event.id, event.kind, event.provenance
            );
        }

        // Commit-action plan Phase 1: best-effort, env-gated, non-blocking
        // forward of `commit`-kind events to coord's
        // `POST /coord/commits/observations`. The spawn keeps the ring / Tauri
        // path above untouched and undelayed; the helper itself no-ops unless
        // `QONTINUI_COMMIT_FORWARDER_ENABLED` is set AND `event.kind == "commit"`.
        commit_forwarder::spawn_forward_commit_event(&event);
    }

    /// Snapshot the ring (oldest → newest) for HTTP exposure / diagnostics.
    pub async fn recent_events(&self) -> Vec<GitSupervisionEvent> {
        let buf = self.inner.read().await;
        buf.iter().cloned().collect()
    }
}

/// Entry point called by `trigger_system::service::handle_event` when it
/// observes a trigger whose effective `ActionType` is
/// `SupervisionProposal`. Converts the raw `TriggerEvent` payload into a
/// `GitSupervisionEvent` and records it on the supervision ring.
///
/// Resolves the `SupervisionState` via the Tauri-managed `Arc<ApiState>`
/// singleton — keeps the trigger system from having to thread an extra
/// dependency through `TriggerExecutorDeps`.
///
/// `event_type` is the trigger system's event type string (e.g.
/// `"git_commit"`, `"git_branch_switch"`, `"git_tag"`, `"file_changed"`).
/// `provenance` is the `provenance` string from the
/// `ActionType::SupervisionProposal { provenance }` variant — Phase 1
/// always supplies `"git-supervised"`.
pub async fn handle_supervision_action(
    app_handle: &tauri::AppHandle,
    event_type: &str,
    event_data: serde_json::Value,
    provenance: &str,
) {
    use tauri::Manager;
    let api_state = match app_handle.try_state::<std::sync::Arc<crate::mcp::types::ApiState>>() {
        Some(s) => s.inner().clone(),
        None => {
            warn!(
                "SupervisionProposal dispatch skipped: ApiState not yet registered \
                 (event_type={}, provenance={})",
                event_type, provenance
            );
            return;
        }
    };

    let kind = trigger_event_type_to_supervision_kind(event_type);
    let event = GitSupervisionEvent::new(kind, event_data, provenance);
    api_state
        .supervision_state
        .record_event(event, app_handle)
        .await;
}

/// Map a trigger-system event type to the supervision event kind exposed
/// to the frontend. Unknown event types pass through unchanged so future
/// trigger types can flow without a code change here.
fn trigger_event_type_to_supervision_kind(event_type: &str) -> String {
    match event_type {
        "git_commit" => "commit".to_string(),
        "git_branch_switch" => "branch_switch".to_string(),
        "git_tag" => "tag".to_string(),
        // FileWatch trigger emits `file_changed` for any file event under a
        // watched path. The bootstrap row narrows this to spec files via
        // its `patterns` filter, so a `file_changed` event from that row
        // semantically means "a spec file changed on disk."
        "file_changed" => "spec_changed".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ring_evicts_oldest_when_full() {
        let state = SupervisionState::new();
        // Append more than capacity to confirm FIFO eviction.
        for i in 0..(RING_CAPACITY + 5) {
            let ev =
                GitSupervisionEvent::new("commit", serde_json::json!({ "i": i }), "git-supervised");
            let mut buf = state.inner.write().await;
            if buf.len() >= RING_CAPACITY {
                buf.pop_front();
            }
            buf.push_back(ev);
        }
        let snapshot = state.recent_events().await;
        assert_eq!(snapshot.len(), RING_CAPACITY);
        // First retained entry should be index 5 (oldest 5 evicted).
        assert_eq!(
            snapshot.first().unwrap().payload.get("i").unwrap().as_u64(),
            Some(5)
        );
    }

    #[test]
    fn supervision_event_serializes_camelcase() {
        let ev = GitSupervisionEvent::new("commit", serde_json::json!({}), "git-supervised");
        let s = serde_json::to_string(&ev).unwrap();
        // serde rename_all = "camelCase" — single-word fields stay
        // lowercase, but the field names must serialize without
        // underscores. Confirm by absence of any snake_case keys the
        // frontend would treat as `undefined`.
        assert!(!s.contains("\"author_name\""));
        assert!(s.contains("\"timestamp\""));
        assert!(s.contains("\"provenance\""));
    }

    #[test]
    fn event_type_mapping() {
        assert_eq!(
            trigger_event_type_to_supervision_kind("git_commit"),
            "commit"
        );
        assert_eq!(
            trigger_event_type_to_supervision_kind("git_branch_switch"),
            "branch_switch"
        );
        assert_eq!(
            trigger_event_type_to_supervision_kind("file_changed"),
            "spec_changed"
        );
        // Unknown passes through.
        assert_eq!(trigger_event_type_to_supervision_kind("weird"), "weird");
    }
}
