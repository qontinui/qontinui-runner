//! Centralized event emitter for Tauri frontend communication.
//!
//! This module provides a unified interface for emitting events to the frontend,
//! handling errors consistently and providing convenience methods for common events.

use super::types::AppEvent;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tracing::{error, warn};

/// Centralized event emitter for frontend communication.
///
/// This struct provides a consistent interface for emitting events to the Tauri
/// frontend, with proper error handling and logging.
///
/// # Example
///
/// ```ignore
/// use crate::event_system::{EventEmitter, AppEvent};
///
/// let emitter = EventEmitter::new(app_handle);
///
/// // Emit with error handling
/// emitter.emit_or_warn(AppEvent::executor_event("test", json!({})));
///
/// // Or use convenience methods
/// emitter.executor_event("test", json!({}));
/// emitter.rag_progress("project-1", "processing", "Step 1/10", Some(10));
/// ```
#[derive(Clone)]
pub struct EventEmitter {
    app_handle: AppHandle,
}

impl EventEmitter {
    /// Create a new event emitter.
    pub fn new(app_handle: AppHandle) -> Self {
        Self { app_handle }
    }

    /// Get the underlying app handle.
    pub fn app_handle(&self) -> &AppHandle {
        &self.app_handle
    }

    /// Emit an event to the frontend.
    ///
    /// Returns an error if the event emission fails.
    pub fn emit(&self, event: AppEvent) -> Result<(), String> {
        let event_name = event.event_name();
        self.app_handle
            .emit(event_name, &event)
            .map_err(|e| format!("Failed to emit {}: {}", event_name, e))
    }

    /// Emit an event, logging a warning on failure but not propagating the error.
    ///
    /// This is the recommended method for most event emissions where the caller
    /// doesn't need to handle emission failures.
    pub fn emit_or_warn(&self, event: AppEvent) {
        let event_name = event.event_name();
        if let Err(e) = self.emit(event) {
            warn!("Event emission failed for {}: {}", event_name, e);
        }
    }

    /// Emit an event, logging an error on failure.
    ///
    /// Use this for critical events where emission failure should be logged
    /// as an error rather than a warning.
    pub fn emit_or_error(&self, event: AppEvent) {
        let event_name = event.event_name();
        if let Err(e) = self.emit(event) {
            error!("Failed to emit critical event {}: {}", event_name, e);
        }
    }

    /// Emit an event silently, ignoring any errors.
    ///
    /// Use sparingly - only for events where emission failure is truly not important.
    pub fn emit_silent(&self, event: AppEvent) {
        let _ = self.emit(event);
    }

    // ========================================================================
    // Convenience Methods for Common Events
    // ========================================================================

    /// Emit an executor event.
    pub fn executor_event(&self, event: &str, data: serde_json::Value) {
        self.emit_or_warn(AppEvent::executor_event(event, data));
    }

    /// Emit an executor event with sequence number.
    pub fn executor_event_with_sequence(
        &self,
        event: &str,
        data: serde_json::Value,
        timestamp: i64,
        sequence: u32,
    ) {
        self.emit_or_warn(AppEvent::executor_event_with_sequence(
            event, data, timestamp, sequence,
        ));
    }

    /// Emit an executor error.
    pub fn executor_error(&self, message: impl Into<String>) {
        self.emit_or_warn(AppEvent::executor_error(message));
    }

    /// Emit an executor error with details.
    pub fn executor_error_with_details(
        &self,
        message: impl Into<String>,
        details: impl Into<String>,
    ) {
        self.emit_or_warn(AppEvent::executor_error_with_details(message, details));
    }

    /// Emit a tree event.
    pub fn tree_event(
        &self,
        event_type: &str,
        node: serde_json::Value,
        path: Vec<String>,
        timestamp: i64,
        sequence: u32,
    ) {
        self.emit_or_warn(AppEvent::tree_event(event_type, node, path, timestamp, sequence));
    }

    /// Emit an image recognition event.
    pub fn image_recognition(&self, data: serde_json::Value) {
        self.emit_or_warn(AppEvent::image_recognition(data));
    }

    /// Emit a RAG progress event.
    pub fn rag_progress(
        &self,
        project_id: &str,
        status: &str,
        message: &str,
        percent: Option<i32>,
    ) {
        self.emit_or_warn(AppEvent::rag_progress(project_id, status, message, percent));
    }

    /// Emit a RAG progress event with element counts.
    pub fn rag_progress_with_counts(
        &self,
        project_id: &str,
        status: &str,
        message: &str,
        percent: Option<i32>,
        elements_processed: Option<i32>,
        total_elements: Option<i32>,
    ) {
        self.emit_or_warn(AppEvent::rag_progress_with_counts(
            project_id,
            status,
            message,
            percent,
            elements_processed,
            total_elements,
        ));
    }

    /// Emit a RAG completion event.
    pub fn rag_completion(
        &self,
        project_id: &str,
        success: bool,
        total_processed: i32,
        successful: i32,
        failed: i32,
    ) {
        self.emit_or_warn(AppEvent::rag_completion(
            project_id,
            success,
            total_processed,
            successful,
            failed,
        ));
    }

    /// Emit a RAG completion event with web sync status.
    pub fn rag_completion_with_sync(
        &self,
        project_id: &str,
        success: bool,
        total_processed: i32,
        successful: i32,
        failed: i32,
        web_sync_success: bool,
        web_sync_error: Option<String>,
    ) {
        self.emit_or_warn(AppEvent::rag_completion_with_sync(
            project_id,
            success,
            total_processed,
            successful,
            failed,
            web_sync_success,
            web_sync_error,
        ));
    }

    /// Emit a flow event.
    pub fn flow_event(&self, event: super::types::FlowEventData) {
        self.emit_or_warn(AppEvent::flow_event(event));
    }

    /// Emit an AI output event.
    pub fn ai_output(&self, session_id: &str, content: &str) {
        self.emit_or_warn(AppEvent::ai_output(session_id, content));
    }

    /// Emit an AI output event with content type.
    pub fn ai_output_with_type(&self, session_id: &str, content: &str, content_type: &str) {
        self.emit_or_warn(AppEvent::ai_output_with_type(session_id, content, content_type));
    }

    /// Emit a generic error event.
    pub fn error(&self, message: impl Into<String>) {
        self.emit_or_warn(AppEvent::error(message));
    }

    /// Emit an error event with context.
    pub fn error_with_context(&self, message: impl Into<String>, context: impl Into<String>) {
        self.emit_or_warn(AppEvent::error_with_context(message, context));
    }

    // ========================================================================
    // Raw Emit Methods (for compatibility)
    // ========================================================================

    /// Emit a raw JSON value to a specific event channel.
    ///
    /// This is provided for backward compatibility with existing code that
    /// uses raw JSON payloads. New code should use the typed AppEvent variants.
    pub fn emit_raw(&self, event_name: &str, payload: &serde_json::Value) -> Result<(), String> {
        self.app_handle
            .emit(event_name, payload)
            .map_err(|e| format!("Failed to emit {}: {}", event_name, e))
    }

    /// Emit a raw JSON value, logging a warning on failure.
    pub fn emit_raw_or_warn(&self, event_name: &str, payload: &serde_json::Value) {
        if let Err(e) = self.emit_raw(event_name, payload) {
            warn!("{}", e);
        }
    }

    /// Emit a raw JSON value, logging an error on failure.
    pub fn emit_raw_or_error(&self, event_name: &str, payload: &serde_json::Value) {
        if let Err(e) = self.emit_raw(event_name, payload) {
            error!("{}", e);
        }
    }
}

impl std::fmt::Debug for EventEmitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventEmitter")
            .field("app_handle", &"<AppHandle>")
            .finish()
    }
}

/// Thread-safe shared event emitter.
///
/// Use this when you need to share an emitter across threads or async contexts.
pub type SharedEventEmitter = Arc<EventEmitter>;

/// Create a shared event emitter.
pub fn shared_emitter(app_handle: AppHandle) -> SharedEventEmitter {
    Arc::new(EventEmitter::new(app_handle))
}

#[cfg(test)]
mod tests {
    // Note: Tests that require a real AppHandle would need to be integration tests
    // or use mocking. For now, we just verify the types compile correctly.

    #[test]
    fn test_shared_emitter_type() {
        // Just verify the type alias is correct
        fn _accepts_arc(_: super::SharedEventEmitter) {}
    }
}
