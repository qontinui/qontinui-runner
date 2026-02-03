//! Unified Action Service
//!
//! This module provides a unified interface for executing actions across
//! different automation backends (Python executor, Playwright, etc.).
//!
//! The service connects to the Python executor via the PythonBridge and
//! provides high-level methods for workflow execution, action execution,
//! state navigation, and screenshot capture.

use crate::commands::AppState;
use crate::config::QontinuiConfig;
use crate::config_storage::{ConfigStorage, ConfigStorageError};
use crate::executor::with_default_bridge;
use crate::timeout_config::Timeouts;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Mutex as TokioMutex;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

/// Errors that can occur during action execution
#[derive(Debug, Error)]
#[allow(dead_code)] // Some variants are defined for future use or API completeness
pub enum ActionError {
    #[error("Python executor not initialized")]
    ExecutorNotInitialized,

    #[error("Python executor not running")]
    ExecutorNotRunning,

    #[error("Configuration not loaded")]
    ConfigNotLoaded,

    #[error("Config not found: {0}")]
    ConfigNotFound(String),

    #[error("Workflow not found: {0}")]
    WorkflowNotFound(String),

    #[error("State not found: {0}")]
    StateNotFound(String),

    #[error("Action failed: {0}")]
    ActionFailed(String),

    #[error("Execution timeout after {0} seconds")]
    Timeout(u64),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Storage error: {0}")]
    StorageError(#[from] ConfigStorageError),
}

/// Result of an action execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    /// Whether the action succeeded
    pub success: bool,
    /// Optional message describing the result
    pub message: Option<String>,
    /// Optional data payload
    pub data: Option<serde_json::Value>,
    /// Duration in milliseconds
    pub duration_ms: Option<u64>,
}

impl ActionResult {
    /// Create a successful result
    #[allow(dead_code)] // Part of public API, reserved for future use
    pub fn success() -> Self {
        Self {
            success: true,
            message: None,
            data: None,
            duration_ms: None,
        }
    }

    /// Create a successful result with a message
    pub fn success_with_message(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: Some(message.into()),
            data: None,
            duration_ms: None,
        }
    }

    /// Create a successful result with data
    #[allow(dead_code)] // Part of public API, reserved for future use
    pub fn success_with_data(data: serde_json::Value) -> Self {
        Self {
            success: true,
            message: None,
            data: Some(data),
            duration_ms: None,
        }
    }

    /// Create a failure result
    pub fn failure(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: Some(message.into()),
            data: None,
            duration_ms: None,
        }
    }

    /// Add duration to the result
    pub fn with_duration(mut self, duration_ms: u64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }
}

/// Result of a workflow execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowResult {
    /// Whether the workflow completed successfully
    pub success: bool,
    /// Name of the workflow that was executed
    pub workflow_name: String,
    /// Optional error message if failed
    pub error: Option<String>,
    /// Duration in milliseconds
    pub duration_ms: Option<u64>,
}

/// Result of a state navigation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateResult {
    /// Whether the navigation succeeded
    pub success: bool,
    /// Target state ID
    pub state_id: String,
    /// Optional error message if failed
    pub error: Option<String>,
    /// Duration in milliseconds
    pub duration_ms: Option<u64>,
}

/// Result of a screenshot capture
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotResult {
    /// Whether the capture succeeded
    pub success: bool,
    /// Path to the saved screenshot (relative)
    pub screenshot_path: Option<String>,
    /// Absolute path to the screenshot
    pub absolute_path: Option<String>,
    /// Image width in pixels
    pub width: Option<u32>,
    /// Image height in pixels
    pub height: Option<u32>,
    /// Monitor index used
    pub monitor: Option<i32>,
    /// Optional error message
    pub error: Option<String>,
}

/// Unified Action Service for executing actions
///
/// This service provides a common interface for action execution,
/// abstracting over different backends (Python executor, Playwright, etc.).
/// It connects to the existing PythonBridge through the AppState.
pub struct UnifiedActionService {
    /// Reference to the application state containing the Python bridge
    app_state: Arc<AppState>,
    /// Config storage for resolving config IDs
    #[allow(dead_code)] // Reserved for future use
    config_storage: Arc<TokioMutex<ConfigStorage>>,
}

impl UnifiedActionService {
    /// Create a new UnifiedActionService
    pub fn new(app_state: Arc<AppState>, config_storage: Arc<TokioMutex<ConfigStorage>>) -> Self {
        info!("Creating UnifiedActionService");
        Self {
            app_state,
            config_storage,
        }
    }

    /// Check if the Python executor is running
    #[allow(dead_code)] // Part of public API, reserved for future use
    pub fn is_executor_running(&self) -> bool {
        with_default_bridge(&self.app_state, |bridge| bridge.is_running()).unwrap_or(false)
    }

    /// Get the current loaded configuration
    #[allow(dead_code)] // Part of public API, reserved for future use
    pub fn get_current_config(&self) -> Option<QontinuiConfig> {
        let config_lock = self
            .app_state
            .current_config
            .lock()
            .unwrap_or_else(|poisoned| {
                warn!("current_config mutex was poisoned, recovering");
                poisoned.into_inner()
            });
        config_lock.clone()
    }

    /// Execute a single action deterministically
    ///
    /// # Arguments
    /// * `action_type` - The type of action (click, double_click, right_click, etc.)
    /// * `target_image_id` - The image ID from the loaded config to target
    /// * `config` - Optional additional configuration parameters
    /// * `monitor_index` - Optional monitor index (defaults to 0)
    ///
    /// # Returns
    /// `ActionResult` with success/failure information
    pub async fn execute_action(
        &self,
        action_type: &str,
        target_image_id: &str,
        config: Option<&serde_json::Value>,
        monitor_index: Option<i32>,
    ) -> Result<ActionResult, ActionError> {
        let start = std::time::Instant::now();
        info!(
            "Executing action: {} on image {} (monitor: {:?})",
            action_type, target_image_id, monitor_index
        );

        // Build action parameters
        let mut action_params = serde_json::json!({
            "action_type": action_type.to_uppercase(),
            "image_id": target_image_id,
            "monitor_index": monitor_index.unwrap_or(0)
        });

        // Merge additional config if provided
        if let Some(extra_config) = config {
            if let (Some(params_obj), Some(config_obj)) =
                (action_params.as_object_mut(), extra_config.as_object())
            {
                for (key, value) in config_obj {
                    params_obj.insert(key.clone(), value.clone());
                }
            }
        }

        // Use configurable timeout (default: disabled - run until completion)
        // Falls back to 1 hour if timeout is disabled to prevent infinite hangs on IPC
        let timeout_duration = Timeouts::action_execution()
            .unwrap_or_else(|| Duration::from_secs(3600));

        // Execute via spawn_blocking since PythonBridge uses block_on internally
        let app_state = self.app_state.clone();
        let result = tokio::task::spawn_blocking(move || {
            with_default_bridge(&app_state, |bridge| {
                if !bridge.is_running() {
                    return Err(ActionError::ExecutorNotRunning);
                }

                match bridge.send_command_and_wait(
                    "execute_action",
                    Some(action_params),
                    timeout_duration,
                ) {
                    Ok(response) => {
                        if response.success {
                            Ok(ActionResult::success_with_message(
                                "Action executed successfully",
                            ))
                        } else {
                            Ok(ActionResult::failure(
                                response
                                    .error
                                    .unwrap_or_else(|| "Unknown error".to_string()),
                            ))
                        }
                    }
                    Err(e) => Err(ActionError::ActionFailed(e)),
                }
            })
            .map_err(|_| ActionError::ExecutorNotInitialized)?
        })
        .await
        .map_err(|e| ActionError::Internal(format!("spawn_blocking error: {}", e)))??;

        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(result.with_duration(duration_ms))
    }

    /// Run a workflow by name from the loaded config
    ///
    /// # Arguments
    /// * `workflow_name` - Name of the workflow to run
    /// * `config` - Optional additional configuration parameters
    /// * `monitor_index` - Optional monitor index (defaults to 0)
    /// * `timeout_seconds` - Optional maximum time to wait for completion (None = no timeout)
    /// * `initial_state_ids` - Optional override for initial active states
    ///
    /// # Returns
    /// `WorkflowResult` with success/failure information
    pub async fn run_workflow(
        &self,
        workflow_name: &str,
        config: Option<&serde_json::Value>,
        monitor_index: Option<i32>,
        timeout_seconds: Option<u64>,
        initial_state_ids: Option<&[String]>,
    ) -> Result<WorkflowResult, ActionError> {
        let start = std::time::Instant::now();
        let timeout_str = timeout_seconds
            .map(|t| format!("{}s", t))
            .unwrap_or_else(|| "disabled".to_string());
        info!(
            "Running workflow: {} (monitor: {:?}, timeout: {}, initial_states: {:?})",
            workflow_name, monitor_index, timeout_str, initial_state_ids
        );

        // Use a very large duration when timeout is disabled (effectively infinite)
        let timeout_duration = timeout_seconds
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(u64::MAX / 2));
        let workflow_name_clone = workflow_name.to_string();

        // First, get the lifecycle Arc from the bridge
        let app_state_clone = self.app_state.clone();
        let lifecycle = tokio::task::spawn_blocking(move || {
            with_default_bridge(&app_state_clone, |bridge| {
                if !bridge.is_running() {
                    return Err(ActionError::ExecutorNotRunning);
                }
                Ok(bridge.get_lifecycle())
            })
            .map_err(|_| ActionError::ExecutorNotInitialized)?
        })
        .await
        .map_err(|e| ActionError::Internal(format!("spawn_blocking error: {}", e)))??;

        // Register for completion notification
        let lifecycle_guard = lifecycle.read().await;
        let completion_rx = lifecycle_guard.register_execution_completion().await;
        drop(lifecycle_guard);

        // Build execution parameters
        let resolved_monitor = monitor_index.unwrap_or(0);
        let mut params = serde_json::Map::new();
        params.insert(
            "monitor_index".to_string(),
            serde_json::json!(resolved_monitor),
        );
        params.insert(
            "workflow".to_string(),
            serde_json::json!(workflow_name_clone.clone()),
        );

        // Add initial state IDs if provided (overrides default initial states)
        if let Some(state_ids) = initial_state_ids {
            if !state_ids.is_empty() {
                params.insert(
                    "initial_state_ids".to_string(),
                    serde_json::json!(state_ids),
                );
            }
        }

        // Merge additional config if provided
        if let Some(extra_config) = config {
            if let Some(config_obj) = extra_config.as_object() {
                for (key, value) in config_obj {
                    params.insert(key.clone(), value.clone());
                }
            }
        }

        // Start the workflow execution
        let app_state = self.app_state.clone();
        tokio::task::spawn_blocking(move || {
            with_default_bridge(&app_state, |bridge| {
                match bridge.start_execution_with_params(Some(serde_json::Value::Object(params))) {
                    Ok(_) => Ok(()),
                    Err(e) => Err(ActionError::ActionFailed(format!(
                        "Failed to start workflow: {}",
                        e
                    ))),
                }
            })
            .map_err(|_| ActionError::ExecutorNotInitialized)?
        })
        .await
        .map_err(|e| ActionError::Internal(format!("spawn_blocking error: {}", e)))??;

        debug!("Workflow started, waiting for completion...");

        // Wait for execution completion with timeout
        let result = match timeout(timeout_duration, completion_rx).await {
            Ok(Ok(completion_result)) => {
                info!(
                    "Workflow completed: success={}, error={:?}",
                    completion_result.success, completion_result.error
                );
                WorkflowResult {
                    success: completion_result.success,
                    workflow_name: workflow_name.to_string(),
                    error: completion_result.error,
                    duration_ms: Some(start.elapsed().as_millis() as u64),
                }
            }
            Ok(Err(_)) => {
                warn!("Completion channel closed unexpectedly");
                WorkflowResult {
                    success: true,
                    workflow_name: workflow_name.to_string(),
                    error: Some("Completion channel closed unexpectedly".to_string()),
                    duration_ms: Some(start.elapsed().as_millis() as u64),
                }
            }
            Err(_) => {
                let timeout_val = timeout_seconds.unwrap_or(0);
                error!("Workflow execution timed out after {}s", timeout_val);
                return Err(ActionError::Timeout(timeout_val));
            }
        };

        Ok(result)
    }

    /// Navigate to a state using pathfinding
    ///
    /// # Arguments
    /// * `state_id` - The target state ID to navigate to
    /// * `config` - Optional additional configuration parameters
    /// * `monitor_index` - Optional monitor index (defaults to 0)
    /// * `timeout_seconds` - Optional maximum time to wait for completion (None = no timeout)
    ///
    /// # Returns
    /// `StateResult` with success/failure information
    #[allow(dead_code)] // Part of public API, reserved for future use
    pub async fn go_to_state(
        &self,
        state_id: &str,
        config: Option<&serde_json::Value>,
        monitor_index: Option<i32>,
        timeout_seconds: Option<u64>,
    ) -> Result<StateResult, ActionError> {
        let start = std::time::Instant::now();
        let timeout_str = timeout_seconds
            .map(|t| format!("{}s", t))
            .unwrap_or_else(|| "disabled".to_string());
        info!(
            "Navigating to state: {} (monitor: {:?}, timeout: {})",
            state_id, monitor_index, timeout_str
        );

        // Use a very large duration when timeout is disabled (effectively infinite)
        let timeout_duration = timeout_seconds
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(u64::MAX / 2));

        // Build navigation parameters
        let mut params = serde_json::json!({
            "target_state_id": state_id,
            "monitor_index": monitor_index.unwrap_or(0)
        });

        // Merge additional config if provided
        if let Some(extra_config) = config {
            if let (Some(params_obj), Some(config_obj)) =
                (params.as_object_mut(), extra_config.as_object())
            {
                for (key, value) in config_obj {
                    params_obj.insert(key.clone(), value.clone());
                }
            }
        }

        let state_id_clone = state_id.to_string();
        let app_state = self.app_state.clone();

        let result = tokio::task::spawn_blocking(move || {
            with_default_bridge(&app_state, |bridge| {
                if !bridge.is_running() {
                    return Err(ActionError::ExecutorNotRunning);
                }

                match bridge.send_command_and_wait(
                    "navigate_to_state",
                    Some(params),
                    timeout_duration,
                ) {
                    Ok(response) => {
                        if response.success {
                            Ok(StateResult {
                                success: true,
                                state_id: state_id_clone,
                                error: None,
                                duration_ms: None,
                            })
                        } else {
                            Ok(StateResult {
                                success: false,
                                state_id: state_id_clone,
                                error: response.error,
                                duration_ms: None,
                            })
                        }
                    }
                    Err(e) => Err(ActionError::ActionFailed(e)),
                }
            })
            .map_err(|_| ActionError::ExecutorNotInitialized)?
        })
        .await
        .map_err(|e| ActionError::Internal(format!("spawn_blocking error: {}", e)))??;

        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(StateResult {
            duration_ms: Some(duration_ms),
            ..result
        })
    }

    /// Capture a screenshot via IPC to Python bridge
    ///
    /// # Arguments
    /// * `monitor_index` - Optional monitor index (None for all monitors)
    /// * `delay_seconds` - Optional delay before capture (0-30 seconds)
    ///
    /// # Returns
    /// `ScreenshotResult` with path and metadata
    #[allow(dead_code)] // Part of public API, reserved for future use
    pub async fn capture_screenshot(
        &self,
        monitor_index: Option<i32>,
        delay_seconds: Option<f64>,
    ) -> Result<ScreenshotResult, ActionError> {
        info!(
            "Capturing screenshot via IPC (monitor: {:?}, delay: {:?}s)",
            monitor_index, delay_seconds
        );

        // Apply delay if specified (clamped to 0-30 seconds)
        if let Some(delay) = delay_seconds {
            if delay > 0.0 {
                let clamped_delay = delay.clamp(0.0, 30.0);
                info!("Waiting {}s before screenshot capture", clamped_delay);
                tokio::time::sleep(tokio::time::Duration::from_secs_f64(clamped_delay)).await;
            }
        }

        // Capture screenshot via Python IPC
        let app_state = self.app_state.clone();
        let params = serde_json::json!({
            "monitor": monitor_index,
            "format": "png",
        });

        let result = tokio::task::spawn_blocking(move || {
            with_default_bridge(&app_state, |bridge| {
                if !bridge.is_running() {
                    return Err(ActionError::ExecutorNotRunning);
                }

                // Use configurable timeout (default: disabled - run until completion)
                // Falls back to 5 minutes if timeout is disabled to prevent infinite hangs on IPC
                let timeout_duration = Timeouts::screenshot_capture()
                    .unwrap_or_else(|| Duration::from_secs(300));
                match bridge.send_command_and_wait(
                    "capture_screenshot",
                    Some(params),
                    timeout_duration,
                ) {
                    Ok(response) => {
                        if response.success {
                            Ok(response.data)
                        } else {
                            Err(ActionError::ActionFailed(
                                response
                                    .error
                                    .unwrap_or_else(|| "Screenshot capture failed".to_string()),
                            ))
                        }
                    }
                    Err(e) => Err(ActionError::ActionFailed(e)),
                }
            })
            .map_err(|_| ActionError::ExecutorNotInitialized)?
        })
        .await
        .map_err(|e| ActionError::Internal(format!("spawn_blocking error: {}", e)))??;

        let json_response = result.unwrap_or(serde_json::json!({}));

        // Extract data from response
        let width = json_response
            .get("width")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        let height = json_response
            .get("height")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        // Generate filename and save
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S%.3f");
        let filename = format!("screenshot_{}.png", timestamp);
        let screenshots_dir = crate::paths::get_screenshots_dir();

        let screenshot_path = screenshots_dir.join(&filename);

        // Save the image data if available
        let screenshot_saved = if let Some(image_data) = json_response
            .get("screenshot_base64")
            .and_then(|v| v.as_str())
        {
            use base64::Engine;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(image_data)
                .map_err(|e| ActionError::Internal(format!("Failed to decode image: {}", e)))?;
            std::fs::write(&screenshot_path, decoded)
                .map_err(|e| ActionError::Internal(format!("Failed to save screenshot: {}", e)))?;
            info!("Screenshot saved to: {}", screenshot_path.to_string_lossy());
            true
        } else {
            // Log available keys for debugging
            let keys: Vec<&str> = json_response
                .as_object()
                .map(|obj| obj.keys().map(|k| k.as_str()).collect())
                .unwrap_or_default();
            warn!(
                "No screenshot_base64 in IPC response. Available keys: {:?}",
                keys
            );
            false
        };

        if screenshot_saved {
            Ok(ScreenshotResult {
                success: true,
                screenshot_path: Some(format!(".dev-logs/screenshots/{}", filename)),
                absolute_path: Some(screenshot_path.to_string_lossy().to_string()),
                width,
                height,
                monitor: monitor_index,
                error: None,
            })
        } else {
            Ok(ScreenshotResult {
                success: false,
                screenshot_path: None,
                absolute_path: None,
                width,
                height,
                monitor: monitor_index,
                error: Some("IPC response did not contain screenshot_base64".to_string()),
            })
        }
    }
}

/// Helper function to resolve config from storage by ID
///
/// # Arguments
/// * `config_id` - The ID of the stored configuration
/// * `config_storage` - Reference to the config storage
///
/// # Returns
/// The configuration data as JSON value
#[allow(dead_code)] // Part of public API, reserved for future use
pub async fn get_config_for_execution(
    config_id: &str,
    config_storage: &Arc<TokioMutex<ConfigStorage>>,
) -> Result<serde_json::Value, ActionError> {
    info!("Resolving config for execution: {}", config_id);

    let storage = config_storage.lock().await;
    let stored_config = storage.get(config_id)?;

    // Convert QontinuiConfig to serde_json::Value
    let config_value = serde_json::to_value(&stored_config.config)
        .map_err(|e| ActionError::Internal(format!("Failed to serialize config: {}", e)))?;

    Ok(config_value)
}

/// Helper function to load config from storage and set it as current
///
/// # Arguments
/// * `config_id` - The ID of the stored configuration
/// * `config_storage` - Reference to the config storage
/// * `app_state` - Reference to the app state
///
/// # Returns
/// Summary of the loaded configuration
#[allow(dead_code)] // Part of public API, reserved for future use
pub async fn load_config_from_storage(
    config_id: &str,
    config_storage: &Arc<TokioMutex<ConfigStorage>>,
    app_state: &Arc<AppState>,
) -> Result<String, ActionError> {
    info!("Loading config from storage: {}", config_id);

    let storage = config_storage.lock().await;
    let stored_config = storage.get(config_id)?;
    let summary = format!(
        "{} ({} states, {} workflows)",
        stored_config.metadata.name,
        stored_config.metadata.state_count,
        stored_config.metadata.workflow_count
    );
    let config = stored_config.config.clone();
    drop(storage);

    // Store in app state
    let mut config_lock = app_state.current_config.lock().unwrap_or_else(|poisoned| {
        warn!("current_config mutex was poisoned, recovering");
        poisoned.into_inner()
    });
    *config_lock = Some(config);

    info!("Config loaded: {}", summary);
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_result_success() {
        let result = ActionResult::success();
        assert!(result.success);
        assert!(result.message.is_none());
        assert!(result.data.is_none());
    }

    #[test]
    fn test_action_result_failure() {
        let result = ActionResult::failure("Test error");
        assert!(!result.success);
        assert_eq!(result.message, Some("Test error".to_string()));
    }

    #[test]
    fn test_action_result_with_duration() {
        let result = ActionResult::success().with_duration(100);
        assert!(result.success);
        assert_eq!(result.duration_ms, Some(100));
    }
}
