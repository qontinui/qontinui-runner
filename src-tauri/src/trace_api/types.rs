//! Wire types for the Trace API.
//!
//! These mirror the TS `RecordingSession` / `RecorderEvent` shapes emitted by
//! `ui-bridge-auto`'s in-memory causal trace (Section 5 Plan A). They're kept
//! deliberately liberal — unknown fields on incoming JSON are accepted and
//! preserved as raw `serde_json::Value`s where useful, so the TS shape can
//! evolve without forcing a Rust-side breaking change.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Inbound: recorder session payload (POST /trace/save)
// ---------------------------------------------------------------------------

/// A single event emitted by the in-memory causal trace on the runner-frontend
/// side. Field set is intentionally loose; unknown fields land in `extra`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct InboundRecorderEvent {
    /// Optional client-side id. The DB row gets a fresh PG-assigned id; this
    /// field is purely informational and used to wire `caused_by` links.
    pub client_id: Option<String>,
    /// Client id of the parent event in the same session, if any.
    pub caused_by: Option<String>,
    /// Epoch milliseconds the event was observed at.
    pub timestamp: Option<i64>,
    /// Sequence number within the session. When omitted we fall back to the
    /// position in the `events` array.
    pub sequence: Option<i64>,
    /// Event type tag (e.g. `"action_executed"`, `"discover_returned"`).
    pub event_type: Option<String>,
    pub element_id: Option<String>,
    pub state_id: Option<String>,
    pub transition_id: Option<String>,
    pub action: Option<String>,
    /// Free-form structured params (serialized to JSON string in the DB row).
    pub params: Option<Value>,
    /// Free-form structured result (serialized to JSON string in the DB row).
    pub result: Option<Value>,
    pub duration_ms: Option<f64>,
    pub success: Option<bool>,
    pub error_message: Option<String>,
    /// Free-form metadata (serialized to JSON string in the DB row).
    pub metadata: Option<Value>,
    /// Optional task-run linkage; when set the row is also discoverable via
    /// the existing `get_element_interactions(task_run_id)` path.
    pub task_run_id: Option<i64>,
    /// Catch-all for unknown fields, preserved so future TS-side evolutions
    /// don't break this endpoint.
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, Value>,
}

/// JSON body for `POST /trace/save`. The recorder posts a single session
/// containing an ordered list of events. `recordingSessionId` is required —
/// it's the join key for `/trace/session/{id}` lookups.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct InboundRecordingSession {
    pub recording_session_id: String,
    pub events: Vec<InboundRecorderEvent>,
    /// Pass-through metadata about the session as a whole. Currently unused
    /// server-side but accepted so the TS recorder can post a single object.
    pub session_metadata: Option<Value>,
}

// ---------------------------------------------------------------------------
// Outbound: shared row + session/chain envelopes
// ---------------------------------------------------------------------------

/// Stored row shape returned by the read endpoints. Mirrors the columns
/// selected by the new `get_recording_session` / `get_causal_chain` queries.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEvent {
    pub id: i64,
    pub task_run_id: Option<i64>,
    pub timestamp: i64,
    pub sequence: i64,
    pub event_type: String,
    pub element_id: Option<String>,
    pub state_id: Option<String>,
    pub transition_id: Option<String>,
    pub action: Option<String>,
    /// Stored as JSON-encoded text in the DB; surfaced verbatim here so
    /// callers can re-parse if they need structured data.
    pub params: Option<String>,
    pub result: Option<String>,
    pub duration_ms: Option<f64>,
    pub success: bool,
    pub error_message: Option<String>,
    pub metadata: Option<String>,
    pub recording_session_id: Option<String>,
    pub caused_by_event_id: Option<i64>,
}

/// Aggregate row produced by `list_recording_sessions`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingSessionMeta {
    pub recording_session_id: String,
    pub event_count: i64,
    pub started_at: i64,
    pub ended_at: i64,
}

/// Output of `GET /trace/event/{id}/causal-chain`. The query walks parent
/// links backward; we surface both the ordered chain and the originating
/// `event_id` for caller convenience.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CausalChain {
    pub event_id: i64,
    /// Ordered root-first; the requested event is the last entry.
    pub chain: Vec<TraceEvent>,
}
