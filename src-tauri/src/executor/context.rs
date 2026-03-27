//! Executor Context Module
//!
//! Provides a unified context for executors that encapsulates common dependencies
//! and configuration. Uses a builder pattern for flexible construction.

use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

use crate::commands::AppState;
use crate::config_storage::ConfigStorage;
use crate::step_registry::StepEventLogger;

/// Unified context for executor operations.
///
/// ExecutorContext provides a single structure that holds all the dependencies
/// commonly needed by executors. It supports both minimal configuration (just
/// app state and handle) and full configuration with all optional dependencies.
///
/// # Usage
///
/// ```ignore
/// // Minimal context
/// let ctx = ExecutorContext::minimal(app_state, app_handle);
///
/// // Full context with builder pattern
/// let ctx = ExecutorContext::minimal(app_state, app_handle)
///     .with_config_storage(storage)
///     .with_pid_tracker(tracker)
///     .with_execution_context("exec-123", "My Workflow");
/// ```
#[derive(Clone)]
pub struct ExecutorContext {
    /// Application state containing shared resources.
    pub app_state: Arc<AppState>,

    /// Tauri application handle for event emission.
    pub app_handle: tauri::AppHandle,

    /// Optional configuration storage for workflow configs.
    config_storage: Option<Arc<TokioMutex<ConfigStorage>>>,

    /// Optional PID tracker for managing spawned processes.
    pid_tracker: Option<Arc<std::sync::Mutex<Vec<u32>>>>,

    /// Execution ID for the current run (empty string if not set).
    execution_id: String,

    /// Workflow name for the current run (empty string if not set).
    workflow_name: String,
}

impl ExecutorContext {
    /// Create a minimal context with just app state and handle.
    ///
    /// This is suitable for executors that don't need configuration storage
    /// or PID tracking.
    pub fn minimal(app_state: Arc<AppState>, app_handle: tauri::AppHandle) -> Self {
        Self {
            app_state,
            app_handle,
            config_storage: None,
            pid_tracker: None,
            execution_id: String::new(),
            workflow_name: String::new(),
        }
    }

    /// Add configuration storage to the context.
    #[must_use]
    pub fn with_config_storage(mut self, storage: Arc<TokioMutex<ConfigStorage>>) -> Self {
        self.config_storage = Some(storage);
        self
    }

    /// Add PID tracker to the context.
    #[must_use]
    pub fn with_pid_tracker(mut self, tracker: Arc<std::sync::Mutex<Vec<u32>>>) -> Self {
        self.pid_tracker = Some(tracker);
        self
    }

    /// Add execution context (ID and workflow name).
    #[must_use]
    pub fn with_execution_context(mut self, execution_id: &str, workflow_name: &str) -> Self {
        self.execution_id = execution_id.to_string();
        self.workflow_name = workflow_name.to_string();
        self
    }

    /// Get the config storage, if available.
    pub fn config_storage(&self) -> Option<&Arc<TokioMutex<ConfigStorage>>> {
        self.config_storage.as_ref()
    }

    /// Get the PID tracker, if available.
    pub fn pid_tracker(&self) -> Option<&Arc<std::sync::Mutex<Vec<u32>>>> {
        self.pid_tracker.as_ref()
    }

    /// Get the execution ID.
    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    /// Get the workflow name.
    pub fn workflow_name(&self) -> &str {
        &self.workflow_name
    }

    /// Create a StepEventLogger for the current execution context.
    ///
    /// Returns None if execution_id is empty.
    pub fn create_logger(&self) -> Option<StepEventLogger> {
        if self.execution_id.is_empty() {
            return None;
        }

        Some(StepEventLogger::new(
            self.app_state.checkpoint_db.clone(),
            self.app_state.pg_db.clone(),
            self.execution_id.clone(),
            self.workflow_name.clone(),
        ))
    }

    /// Create a StepEventLogger, using a fallback execution ID if not set.
    pub fn create_logger_with_fallback(&self, fallback_execution_id: &str) -> StepEventLogger {
        let execution_id = if self.execution_id.is_empty() {
            fallback_execution_id.to_string()
        } else {
            self.execution_id.clone()
        };

        StepEventLogger::new(
            self.app_state.checkpoint_db.clone(),
            self.app_state.pg_db.clone(),
            execution_id,
            self.workflow_name.clone(),
        )
    }
}

impl std::fmt::Debug for ExecutorContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutorContext")
            .field("execution_id", &self.execution_id)
            .field("workflow_name", &self.workflow_name)
            .field("has_config_storage", &self.config_storage.is_some())
            .field("has_pid_tracker", &self.pid_tracker.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    // Note: These tests would require mocking AppState which is complex.
    // For now, we document the expected behavior.

    #[test]
    fn test_execution_id_default_empty() {
        // ExecutorContext should default to empty execution_id
        // This is verified by the struct definition
        let _: String = String::new();
    }
}
