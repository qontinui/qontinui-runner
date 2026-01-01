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
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Mutex as TokioMutex;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

/// Errors that can occur during action execution
#[derive(Debug, Error)]
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
    pub fn is_executor_running(&self) -> bool {
        let bridge_lock = self
            .app_state
            .python_bridge
            .lock()
            .unwrap_or_else(|poisoned| {
                warn!("python_bridge mutex was poisoned, recovering");
                poisoned.into_inner()
            });

        if let Some(ref bridge) = *bridge_lock {
            bridge.is_running()
        } else {
            false
        }
    }

    /// Get the current loaded configuration
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

        let timeout_duration = Duration::from_secs(30);

        // Execute via spawn_blocking since PythonBridge uses block_on internally
        let app_state = self.app_state.clone();
        let result = tokio::task::spawn_blocking(move || {
            let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
                warn!("python_bridge mutex was poisoned, recovering");
                poisoned.into_inner()
            });

            if let Some(ref mut bridge) = *bridge_lock {
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
            } else {
                Err(ActionError::ExecutorNotInitialized)
            }
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
    /// * `timeout_seconds` - Maximum time to wait for completion
    ///
    /// # Returns
    /// `WorkflowResult` with success/failure information
    pub async fn run_workflow(
        &self,
        workflow_name: &str,
        config: Option<&serde_json::Value>,
        monitor_index: Option<i32>,
        timeout_seconds: u64,
    ) -> Result<WorkflowResult, ActionError> {
        let start = std::time::Instant::now();
        info!(
            "Running workflow: {} (monitor: {:?}, timeout: {}s)",
            workflow_name, monitor_index, timeout_seconds
        );

        let timeout_duration = Duration::from_secs(timeout_seconds);
        let workflow_name_clone = workflow_name.to_string();

        // First, get the lifecycle Arc from the bridge
        let app_state_clone = self.app_state.clone();
        let lifecycle = tokio::task::spawn_blocking(move || {
            let bridge_lock = app_state_clone
                .python_bridge
                .lock()
                .unwrap_or_else(|poisoned| {
                    warn!("python_bridge mutex was poisoned, recovering");
                    poisoned.into_inner()
                });

            if let Some(ref bridge) = *bridge_lock {
                if !bridge.is_running() {
                    return Err(ActionError::ExecutorNotRunning);
                }
                Ok(bridge.get_lifecycle())
            } else {
                Err(ActionError::ExecutorNotInitialized)
            }
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
        let start_result = tokio::task::spawn_blocking(move || {
            let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
                warn!("python_bridge mutex was poisoned, recovering");
                poisoned.into_inner()
            });

            if let Some(ref mut bridge) = *bridge_lock {
                match bridge.start_execution_with_params(Some(serde_json::Value::Object(params))) {
                    Ok(_) => Ok(()),
                    Err(e) => Err(ActionError::ActionFailed(format!(
                        "Failed to start workflow: {}",
                        e
                    ))),
                }
            } else {
                Err(ActionError::ExecutorNotInitialized)
            }
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
                error!("Workflow execution timed out after {}s", timeout_seconds);
                return Err(ActionError::Timeout(timeout_seconds));
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
    /// * `timeout_seconds` - Maximum time to wait for completion
    ///
    /// # Returns
    /// `StateResult` with success/failure information
    pub async fn go_to_state(
        &self,
        state_id: &str,
        config: Option<&serde_json::Value>,
        monitor_index: Option<i32>,
        timeout_seconds: u64,
    ) -> Result<StateResult, ActionError> {
        let start = std::time::Instant::now();
        info!(
            "Navigating to state: {} (monitor: {:?}, timeout: {}s)",
            state_id, monitor_index, timeout_seconds
        );

        let timeout_duration = Duration::from_secs(timeout_seconds);

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
            let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
                warn!("python_bridge mutex was poisoned, recovering");
                poisoned.into_inner()
            });

            if let Some(ref mut bridge) = *bridge_lock {
                if !bridge.is_running() {
                    return Err(ActionError::ExecutorNotRunning);
                }

                match bridge.send_command_and_wait("go_to_state", Some(params), timeout_duration) {
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
            } else {
                Err(ActionError::ExecutorNotInitialized)
            }
        })
        .await
        .map_err(|e| ActionError::Internal(format!("spawn_blocking error: {}", e)))??;

        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(StateResult {
            duration_ms: Some(duration_ms),
            ..result
        })
    }

    /// Capture a screenshot
    ///
    /// # Arguments
    /// * `monitor_index` - Optional monitor index (None for all monitors)
    /// * `delay_seconds` - Optional delay before capture (0-30 seconds)
    ///
    /// # Returns
    /// `ScreenshotResult` with path and metadata
    pub async fn capture_screenshot(
        &self,
        monitor_index: Option<i32>,
        delay_seconds: Option<f64>,
    ) -> Result<ScreenshotResult, ActionError> {
        info!(
            "Capturing screenshot (monitor: {:?}, delay: {:?}s)",
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

        // Get qontinui-api URL
        let api_url = std::env::var("QONTINUI_API_URL_VISION")
            .unwrap_or_else(|_| "http://localhost:8001".to_string());

        // Build URL with query parameters
        let mut url = format!("{}/api/capture/screenshot/current", api_url);
        let mut params = Vec::new();
        if let Some(mon) = monitor_index {
            params.push(format!("monitor={}", mon));
        }
        params.push("quality=95".to_string());
        if !params.is_empty() {
            url = format!("{}?{}", url, params.join("&"));
        }

        // Capture screenshot via qontinui-api
        let client = reqwest::Client::new();
        let response = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to capture screenshot: {}", e);
                return Ok(ScreenshotResult {
                    success: false,
                    screenshot_path: None,
                    absolute_path: None,
                    width: None,
                    height: None,
                    monitor: monitor_index,
                    error: Some(format!("Network error: {}", e)),
                });
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!(
                "Screenshot capture failed with status {}: {}",
                status, error_text
            );
            return Ok(ScreenshotResult {
                success: false,
                screenshot_path: None,
                absolute_path: None,
                width: None,
                height: None,
                monitor: monitor_index,
                error: Some(format!("API error {}: {}", status, error_text)),
            });
        }

        // Parse the response
        let json_response: serde_json::Value = response.json().await.map_err(|e| {
            ActionError::Internal(format!("Failed to parse screenshot response: {}", e))
        })?;

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
        let screenshots_dir = std::path::Path::new(".dev-logs/screenshots");
        std::fs::create_dir_all(screenshots_dir).map_err(|e| {
            ActionError::Internal(format!("Failed to create screenshots dir: {}", e))
        })?;

        let screenshot_path = screenshots_dir.join(&filename);
        let absolute_path = std::fs::canonicalize(&screenshot_path)
            .ok()
            .map(|p| p.to_string_lossy().to_string());

        // Save the image data if available
        if let Some(image_data) = json_response.get("image").and_then(|v| v.as_str()) {
            use base64::Engine;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(image_data)
                .map_err(|e| ActionError::Internal(format!("Failed to decode image: {}", e)))?;
            std::fs::write(&screenshot_path, decoded)
                .map_err(|e| ActionError::Internal(format!("Failed to save screenshot: {}", e)))?;
        }

        Ok(ScreenshotResult {
            success: true,
            screenshot_path: Some(format!(".dev-logs/screenshots/{}", filename)),
            absolute_path,
            width,
            height,
            monitor: monitor_index,
            error: None,
        })
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
