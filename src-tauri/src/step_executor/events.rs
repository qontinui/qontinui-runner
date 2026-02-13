//! Tree Event Emission Helpers
//!
//! This module provides unified tree event emission for step execution,
//! eliminating duplicated event emission code across different step handlers.
//!
//! Tree events are used for:
//! - File logging (to runner-actions.jsonl)
//! - DisplayProcessor (for Session/Actions page)
//! - Tauri frontend (for live action log refresh)

use crate::commands::AppState;
use crate::display::RawEvent;
use crate::executor::file_logger::FileLogger;
use serde_json::json;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tauri::Emitter;
use tracing::warn;

/// Global sequence counter for tree events
static EVENT_SEQUENCE: AtomicU32 = AtomicU32::new(1);

/// Helper for emitting tree events during step execution.
///
/// This consolidates the three event emission paths:
/// 1. FileLogger::log_tree_event() - writes to runner-actions.jsonl
/// 2. DisplayProcessor.add_event() - for Session/Actions page
/// 3. Tauri emit() - for live frontend updates
pub struct TreeEventEmitter {
    app_state: Arc<AppState>,
    app_handle: Option<tauri::AppHandle>,
}

impl TreeEventEmitter {
    /// Create a new tree event emitter.
    pub fn new(app_state: Arc<AppState>, app_handle: Option<tauri::AppHandle>) -> Self {
        Self {
            app_state,
            app_handle,
        }
    }

    /// Get a reference to the app handle (if available).
    pub fn app_handle(&self) -> Option<&tauri::AppHandle> {
        self.app_handle.as_ref()
    }

    /// Get the next sequence number for events.
    pub fn next_sequence() -> u32 {
        EVENT_SEQUENCE.fetch_add(1, Ordering::SeqCst)
    }

    /// Get the current timestamp as seconds since epoch (with millisecond precision).
    pub fn now_timestamp() -> f64 {
        chrono::Utc::now().timestamp_millis() as f64 / 1000.0
    }

    /// Generate a unique action ID for a step.
    pub fn generate_action_id(prefix: &str, sequence: u32) -> String {
        format!("{}-{}", prefix, sequence)
    }

    /// Emit an action_started event.
    ///
    /// This should be called before executing a step to log the start of the action.
    pub async fn emit_action_started(
        &self,
        action_id: &str,
        action_name: &str,
        metadata: serde_json::Value,
    ) -> (f64, u32) {
        let sequence = Self::next_sequence();
        let timestamp = Self::now_timestamp();

        let action_node = json!({
            "id": action_id,
            "node_type": "action",
            "name": action_name,
            "timestamp": timestamp,
            "status": "running",
            "metadata": metadata,
        });

        self.emit_tree_event_internal("action_started", &action_node, timestamp, sequence)
            .await;

        (timestamp, sequence)
    }

    /// Emit an action_completed event (success case).
    ///
    /// This should be called after a step completes successfully.
    pub async fn emit_action_completed(
        &self,
        action_id: &str,
        action_name: &str,
        start_timestamp: f64,
        sequence: u32,
        result_metadata: serde_json::Value,
    ) {
        let end_timestamp = Self::now_timestamp();
        let duration_ms = ((end_timestamp - start_timestamp) * 1000.0) as u64;

        let completed_node = json!({
            "id": action_id,
            "node_type": "action",
            "name": action_name,
            "timestamp": end_timestamp,
            "status": "success",
            "duration_ms": duration_ms,
            "metadata": result_metadata,
        });

        self.emit_tree_event_internal("action_completed", &completed_node, end_timestamp, sequence)
            .await;
    }

    /// Emit an action_failed event.
    ///
    /// This should be called after a step fails.
    pub async fn emit_action_failed(
        &self,
        action_id: &str,
        action_name: &str,
        start_timestamp: f64,
        sequence: u32,
        error: &str,
        error_metadata: Option<serde_json::Value>,
    ) {
        let end_timestamp = Self::now_timestamp();
        let duration_ms = ((end_timestamp - start_timestamp) * 1000.0) as u64;

        let mut metadata = error_metadata.unwrap_or_else(|| json!({}));
        if let Some(obj) = metadata.as_object_mut() {
            obj.insert("error".to_string(), json!(error));
        }

        let failed_node = json!({
            "id": action_id,
            "node_type": "action",
            "name": action_name,
            "timestamp": end_timestamp,
            "status": "failed",
            "duration_ms": duration_ms,
            "error": error,
            "metadata": metadata,
        });

        self.emit_tree_event_internal("action_failed", &failed_node, end_timestamp, sequence)
            .await;
    }

    /// Emit a generic tree event to all three destinations.
    ///
    /// This is the internal method that handles the actual emission.
    async fn emit_tree_event_internal(
        &self,
        event_type: &str,
        node: &serde_json::Value,
        timestamp: f64,
        sequence: u32,
    ) {
        // 1. Log to file (runner-actions.jsonl)
        FileLogger::log_tree_event(event_type, node, &[], timestamp, sequence);

        // 2. Add to DisplayProcessor for Session/Actions page
        {
            let raw_event = RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: event_type.to_string(),
                timestamp,
                data: json!({ "node": node.clone() }),
                sequence: sequence as u64,
            };
            let mut processor = self.app_state.display_processor.lock().await;
            processor.event_log_mut().add_event(raw_event);
        }

        // 3. Emit to Tauri frontend for live updates
        if let Some(ref app_handle) = self.app_handle {
            let tree_event = json!({
                "type": "tree_event",
                "event_type": event_type,
                "node": node,
                "path": [],
                "timestamp": timestamp,
                "sequence": sequence,
            });
            if let Err(e) = app_handle.emit("executor-event", &tree_event) {
                warn!("Failed to emit tree event to frontend: {}", e);
            }
        }
    }
}

/// Convenience struct for tracking step execution timing.
pub struct StepTimer {
    action_id: String,
    action_name: String,
    start_timestamp: f64,
    sequence: u32,
}

impl StepTimer {
    /// Create a new step timer (called after emit_action_started).
    pub fn new(
        action_id: String,
        action_name: String,
        start_timestamp: f64,
        sequence: u32,
    ) -> Self {
        Self {
            action_id,
            action_name,
            start_timestamp,
            sequence,
        }
    }

    /// Get the action ID.
    pub fn action_id(&self) -> &str {
        &self.action_id
    }

    /// Get the action name.
    pub fn action_name(&self) -> &str {
        &self.action_name
    }

    /// Get the start timestamp.
    pub fn start_timestamp(&self) -> f64 {
        self.start_timestamp
    }

    /// Get the sequence number.
    pub fn sequence(&self) -> u32 {
        self.sequence
    }

    /// Calculate elapsed time in milliseconds.
    pub fn elapsed_ms(&self) -> u64 {
        let now = TreeEventEmitter::now_timestamp();
        ((now - self.start_timestamp) * 1000.0) as u64
    }

    /// Complete this step with success.
    pub async fn complete(self, emitter: &TreeEventEmitter, result_metadata: serde_json::Value) {
        emitter
            .emit_action_completed(
                &self.action_id,
                &self.action_name,
                self.start_timestamp,
                self.sequence,
                result_metadata,
            )
            .await;
    }

    /// Complete this step with failure.
    pub async fn fail(
        self,
        emitter: &TreeEventEmitter,
        error: &str,
        error_metadata: Option<serde_json::Value>,
    ) {
        emitter
            .emit_action_failed(
                &self.action_id,
                &self.action_name,
                self.start_timestamp,
                self.sequence,
                error,
                error_metadata,
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sequence_increments() {
        let seq1 = TreeEventEmitter::next_sequence();
        let seq2 = TreeEventEmitter::next_sequence();
        assert!(seq2 > seq1);
    }

    #[test]
    fn test_generate_action_id() {
        let id = TreeEventEmitter::generate_action_id("screenshot", 42);
        assert_eq!(id, "screenshot-42");
    }

    #[test]
    fn test_now_timestamp_is_recent() {
        let ts = TreeEventEmitter::now_timestamp();
        // Should be after year 2020 (timestamp > 1577836800)
        assert!(ts > 1577836800.0);
    }
}
