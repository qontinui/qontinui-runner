//! Unified event broadcaster for dual Tauri + WebSocket event emission.
//!
//! This module provides an `EventBroadcaster` that sends events to both:
//! - Tauri event system (for desktop app)
//! - WebSocket broadcast channel (for external clients like browsers)
//!
//! This enables non-Tauri contexts (e.g., running in a browser or external clients)
//! to receive real-time updates via WebSocket while maintaining full Tauri support.

use super::types::AppEvent;
use crate::commands::AppState;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::broadcast;
use tracing::{error, warn};

/// Unified event broadcaster that sends to both Tauri and WebSocket.
///
/// This struct provides a consistent interface for broadcasting events to all
/// connected clients, regardless of whether they're using Tauri IPC or WebSocket.
///
/// # Example
///
/// ```ignore
/// use crate::event_system::{EventBroadcaster, AppEvent};
///
/// let broadcaster = EventBroadcaster::new(app_handle);
///
/// // Broadcast to all clients (Tauri + WebSocket)
/// broadcaster.broadcast(AppEvent::orchestrator_state_change(
///     "task-123",
///     "verification",
///     1,
///     "running"
/// ));
///
/// // Or use convenience methods
/// broadcaster.orchestrator_state_change("task-123", "verification", 1, "running");
/// ```
#[derive(Clone)]
pub struct EventBroadcaster {
    app_handle: AppHandle,
}

impl EventBroadcaster {
    /// Create a new event broadcaster.
    pub fn new(app_handle: AppHandle) -> Self {
        Self { app_handle }
    }

    /// Get the underlying app handle.
    pub fn app_handle(&self) -> &AppHandle {
        &self.app_handle
    }

    /// Get the WebSocket broadcast channel from AppState.
    fn get_broadcast_channel(&self) -> Option<broadcast::Sender<serde_json::Value>> {
        self.app_handle
            .try_state::<Arc<AppState>>()
            .map(|state| state.event_broadcast.clone())
    }

    /// Broadcast an event to all clients (Tauri + WebSocket).
    ///
    /// Returns an error if the Tauri event emission fails.
    /// WebSocket broadcast failures are logged but don't cause errors
    /// (no connected clients is a valid state).
    pub fn broadcast(&self, event: AppEvent) -> Result<(), String> {
        let event_name = event.event_name();

        // 1. Emit to Tauri frontend
        if let Err(e) = self.app_handle.emit(event_name, &event) {
            return Err(format!("Failed to emit {} to Tauri: {}", event_name, e));
        }

        // 2. Broadcast to WebSocket clients
        if let Some(broadcast_tx) = self.get_broadcast_channel() {
            // Serialize the event to JSON for WebSocket
            match serde_json::to_value(&event) {
                Ok(json_value) => {
                    // Add event_name to the JSON for WebSocket clients to identify the event type
                    let ws_event = serde_json::json!({
                        "channel": event_name,
                        "payload": json_value
                    });

                    if let Err(e) = broadcast_tx.send(ws_event) {
                        // Only warn if there are supposed to be receivers
                        if broadcast_tx.receiver_count() > 0 {
                            warn!("Failed to broadcast {} to WebSocket: {}", event_name, e);
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to serialize event {} for WebSocket: {}", event_name, e);
                }
            }
        }

        Ok(())
    }

    /// Broadcast an event, logging a warning on failure but not propagating the error.
    ///
    /// This is the recommended method for most event emissions where the caller
    /// doesn't need to handle emission failures.
    pub fn broadcast_or_warn(&self, event: AppEvent) {
        let event_name = event.event_name();
        if let Err(e) = self.broadcast(event) {
            warn!("Event broadcast failed for {}: {}", event_name, e);
        }
    }

    /// Broadcast an event, logging an error on failure.
    ///
    /// Use this for critical events where emission failure should be logged
    /// as an error rather than a warning.
    pub fn broadcast_or_error(&self, event: AppEvent) {
        let event_name = event.event_name();
        if let Err(e) = self.broadcast(event) {
            error!("Failed to broadcast critical event {}: {}", event_name, e);
        }
    }

    /// Broadcast an event silently, ignoring any errors.
    ///
    /// Use sparingly - only for events where emission failure is truly not important.
    pub fn broadcast_silent(&self, event: AppEvent) {
        let _ = self.broadcast(event);
    }

    // ========================================================================
    // Convenience Methods for Common Events
    // ========================================================================

    /// Broadcast an orchestrator state change event.
    ///
    /// This is the primary event for workflow stage transitions (Setup, Verification,
    /// Agentic, Completion) that should be received by both Tauri and WebSocket clients.
    pub fn orchestrator_state_change(
        &self,
        task_run_id: &str,
        workflow_stage: &str,
        iteration: u32,
        phase: &str,
    ) {
        self.broadcast_or_warn(AppEvent::orchestrator_state_change(
            task_run_id,
            workflow_stage,
            iteration,
            phase,
        ));
    }

    /// Broadcast an orchestrator state change event with additional data.
    pub fn orchestrator_state_change_with_data(
        &self,
        task_run_id: &str,
        workflow_stage: &str,
        iteration: u32,
        phase: &str,
        state_data: serde_json::Value,
    ) {
        self.broadcast_or_warn(AppEvent::orchestrator_state_change_with_data(
            task_run_id,
            workflow_stage,
            iteration,
            phase,
            state_data,
        ));
    }

    /// Broadcast a step progress event.
    ///
    /// This notifies clients about progress through individual steps within a phase.
    pub fn step_progress(
        &self,
        task_run_id: &str,
        step_index: usize,
        step_name: &str,
        status: &str,
        details: Option<serde_json::Value>,
    ) {
        let event_data = serde_json::json!({
            "task_run_id": task_run_id,
            "step_index": step_index,
            "step_name": step_name,
            "status": status,
            "details": details,
            "timestamp": chrono::Utc::now().timestamp_millis()
        });

        self.broadcast_or_warn(AppEvent::executor_event("step_progress", event_data));
    }

    /// Broadcast a task run update event.
    ///
    /// This notifies clients about changes to a task run's status.
    pub fn task_run_update(
        &self,
        task_run_id: &str,
        status: &str,
        iteration: Option<u32>,
        details: Option<serde_json::Value>,
    ) {
        let event_data = serde_json::json!({
            "task_run_id": task_run_id,
            "status": status,
            "iteration": iteration,
            "details": details,
            "timestamp": chrono::Utc::now().timestamp_millis()
        });

        self.broadcast_or_warn(AppEvent::executor_event("task_run_update", event_data));
    }

    /// Broadcast an executor event with additional WebSocket support.
    pub fn executor_event(&self, event: &str, data: serde_json::Value) {
        self.broadcast_or_warn(AppEvent::executor_event(event, data));
    }

    /// Broadcast an AI output event.
    pub fn ai_output(&self, session_id: &str, content: &str) {
        self.broadcast_or_warn(AppEvent::ai_output(session_id, content));
    }

    /// Broadcast an AI output event with content type.
    pub fn ai_output_with_type(&self, session_id: &str, content: &str, content_type: &str) {
        self.broadcast_or_warn(AppEvent::ai_output_with_type(session_id, content, content_type));
    }

    /// Broadcast a generic error event.
    pub fn error(&self, message: impl Into<String>) {
        self.broadcast_or_warn(AppEvent::error(message));
    }

    /// Broadcast an error event with context.
    pub fn error_with_context(&self, message: impl Into<String>, context: impl Into<String>) {
        self.broadcast_or_warn(AppEvent::error_with_context(message, context));
    }

    // ========================================================================
    // Raw Broadcast Methods (for compatibility)
    // ========================================================================

    /// Broadcast a raw JSON value to a specific event channel.
    ///
    /// This is provided for backward compatibility with existing code that
    /// uses raw JSON payloads. New code should use the typed AppEvent variants.
    pub fn broadcast_raw(&self, event_name: &str, payload: &serde_json::Value) -> Result<(), String> {
        // 1. Emit to Tauri frontend
        if let Err(e) = self.app_handle.emit(event_name, payload) {
            return Err(format!("Failed to emit {} to Tauri: {}", event_name, e));
        }

        // 2. Broadcast to WebSocket clients
        if let Some(broadcast_tx) = self.get_broadcast_channel() {
            let ws_event = serde_json::json!({
                "channel": event_name,
                "payload": payload
            });

            if let Err(e) = broadcast_tx.send(ws_event) {
                if broadcast_tx.receiver_count() > 0 {
                    warn!("Failed to broadcast raw {} to WebSocket: {}", event_name, e);
                }
            }
        }

        Ok(())
    }

    /// Broadcast a raw JSON value, logging a warning on failure.
    pub fn broadcast_raw_or_warn(&self, event_name: &str, payload: &serde_json::Value) {
        if let Err(e) = self.broadcast_raw(event_name, payload) {
            warn!("{}", e);
        }
    }
}

impl std::fmt::Debug for EventBroadcaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBroadcaster")
            .field("app_handle", &"<AppHandle>")
            .finish()
    }
}

/// Thread-safe shared event broadcaster.
///
/// Use this when you need to share a broadcaster across threads or async contexts.
pub type SharedEventBroadcaster = Arc<EventBroadcaster>;

/// Create a shared event broadcaster.
pub fn shared_broadcaster(app_handle: AppHandle) -> SharedEventBroadcaster {
    Arc::new(EventBroadcaster::new(app_handle))
}

#[cfg(test)]
mod tests {
    // Note: Tests that require a real AppHandle would need to be integration tests
    // or use mocking. For now, we just verify the types compile correctly.

    #[test]
    fn test_shared_broadcaster_type() {
        // Just verify the type alias is correct
        fn _accepts_arc(_: super::SharedEventBroadcaster) {}
    }
}
