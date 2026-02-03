//! Centralized Timeout Configuration
//!
//! All timeout values are defined here for easy management and configuration.
//!
//! ## Design Philosophy
//!
//! - **User-facing operations**: Default to DISABLED (None/0) - let them run to completion
//! - **System health checks**: Have reasonable defaults to detect failures
//! - **All values**: Can be overridden via environment variables
//!
//! ## Environment Variables
//!
//! All timeouts can be overridden by setting environment variables:
//! - `QONTINUI_TIMEOUT_<NAME>` where `<NAME>` is the constant name
//! - Value of 0 means disabled (no timeout)
//! - Example: `QONTINUI_TIMEOUT_ACTION_EXECUTION=300` sets 5 minute timeout
//!
//! ## Usage
//!
//! ```rust
//! use crate::timeout_config::Timeouts;
//!
//! // Get timeout as Option<Duration> (None = disabled)
//! if let Some(timeout) = Timeouts::action_execution() {
//!     // Use timeout
//! }
//!
//! // Get timeout as Duration (for internal ops that need a limit)
//! let timeout = Timeouts::python_startup();
//! ```

use std::env;
use std::time::Duration;

/// Centralized timeout configuration.
///
/// All timeout values in seconds. 0 = disabled (no timeout).
pub struct Timeouts;

// =============================================================================
// USER-FACING OPERATION TIMEOUTS (Default: DISABLED)
// =============================================================================
// These operations can legitimately take any amount of time depending on
// the task complexity. Users can enable timeouts if needed.

impl Timeouts {
    /// Action execution timeout (GUI actions, clicks, etc.)
    /// Default: DISABLED - actions complete when done
    pub fn action_execution() -> Option<Duration> {
        Self::get_optional_timeout("ACTION_EXECUTION", 0)
    }

    /// Python bridge command timeout (generic commands)
    /// Default: DISABLED - AI/ML operations vary wildly
    pub fn python_command() -> Option<Duration> {
        Self::get_optional_timeout("PYTHON_COMMAND", 0)
    }

    /// Image recognition timeout
    /// Default: DISABLED - complex images take longer
    pub fn image_recognition() -> Option<Duration> {
        Self::get_optional_timeout("IMAGE_RECOGNITION", 0)
    }

    /// AI generation timeout (context, tasks, exploration)
    /// Default: DISABLED - AI response times vary
    pub fn ai_generation() -> Option<Duration> {
        Self::get_optional_timeout("AI_GENERATION", 0)
    }

    /// Vision analysis timeout (SAM3, UI-TARS, etc.)
    /// Default: DISABLED - ML model processing varies
    pub fn vision_analysis() -> Option<Duration> {
        Self::get_optional_timeout("VISION_ANALYSIS", 0)
    }

    /// Playwright test execution timeout
    /// Default: DISABLED - tests run until completion
    pub fn playwright_test() -> Option<Duration> {
        Self::get_optional_timeout("PLAYWRIGHT_TEST", 0)
    }

    /// Shell command execution timeout
    /// Default: DISABLED - commands run until completion
    pub fn shell_command() -> Option<Duration> {
        Self::get_optional_timeout("SHELL_COMMAND", 0)
    }

    /// Workflow execution timeout
    /// Default: DISABLED - workflows run until completion
    pub fn workflow_execution() -> Option<Duration> {
        Self::get_optional_timeout("WORKFLOW_EXECUTION", 0)
    }

    /// State navigation timeout
    /// Default: DISABLED - navigation completes when done
    pub fn state_navigation() -> Option<Duration> {
        Self::get_optional_timeout("STATE_NAVIGATION", 0)
    }

    /// UI Bridge exploration timeout
    /// Default: DISABLED - exploration takes varying time
    pub fn ui_bridge_exploration() -> Option<Duration> {
        Self::get_optional_timeout("UI_BRIDGE_EXPLORATION", 0)
    }

    /// Screenshot capture timeout
    /// Default: DISABLED - capture completes when done
    pub fn screenshot_capture() -> Option<Duration> {
        Self::get_optional_timeout("SCREENSHOT_CAPTURE", 0)
    }

    /// API request timeout (HTTP calls)
    /// Default: DISABLED - let requests complete or fail naturally
    pub fn api_request() -> Option<Duration> {
        Self::get_optional_timeout("API_REQUEST", 0)
    }

    /// MCP tool call timeout
    /// Default: DISABLED - tool execution varies
    pub fn mcp_call() -> Option<Duration> {
        Self::get_optional_timeout("MCP_CALL", 0)
    }

    /// AWAS operation timeout
    /// Default: DISABLED - web automation varies
    pub fn awas_operation() -> Option<Duration> {
        Self::get_optional_timeout("AWAS_OPERATION", 0)
    }
}

// =============================================================================
// SYSTEM HEALTH TIMEOUTS (Default: ENABLED with reasonable values)
// =============================================================================
// These are internal system operations that NEED timeouts to detect failures.
// Without these, the system could hang indefinitely on dead processes.

impl Timeouts {
    /// Python executor startup timeout
    /// Default: 30 seconds - need to know if startup failed
    pub fn python_startup() -> Duration {
        Self::get_required_timeout("PYTHON_STARTUP", 30)
    }

    /// Health check ping interval
    /// Default: 5 seconds - regular heartbeat
    pub fn health_ping_interval() -> Duration {
        Self::get_required_timeout("HEALTH_PING_INTERVAL", 5)
    }

    /// Health check pong timeout (response expected within this time)
    /// Default: 3 seconds - detect unresponsive processes
    pub fn health_pong_timeout() -> Duration {
        Self::get_required_timeout("HEALTH_PONG_TIMEOUT", 3)
    }

    /// UI Bridge IPC timeout (frontend communication)
    /// Default: 30 seconds - detect stuck UI
    pub fn ui_bridge_ipc() -> Duration {
        Self::get_required_timeout("UI_BRIDGE_IPC", 30)
    }

    /// HTTP request timeout for external APIs (auth, sync, etc.)
    /// Default: 30 seconds - standard network timeout
    pub fn http_external() -> Duration {
        Self::get_required_timeout("HTTP_EXTERNAL", 30)
    }

    /// Process graceful shutdown timeout
    /// Default: 5 seconds - wait before force kill
    pub fn graceful_shutdown() -> Duration {
        Self::get_required_timeout("GRACEFUL_SHUTDOWN", 5)
    }

    /// Health check URL timeout (for workflow health checks)
    /// Default: 30 seconds - reasonable for server response
    pub fn health_check_url() -> Duration {
        Self::get_required_timeout("HEALTH_CHECK_URL", 30)
    }

    /// Stale step detection threshold
    /// Default: 30 minutes - when to consider a step stale
    pub fn stale_step_threshold() -> Duration {
        Self::get_required_timeout("STALE_STEP_THRESHOLD", 1800)
    }
}

// =============================================================================
// HELPER FUNCTIONS
// =============================================================================

impl Timeouts {
    /// Get an optional timeout (0 = disabled, returns None)
    fn get_optional_timeout(name: &str, default_secs: u64) -> Option<Duration> {
        let secs = Self::get_env_or_default(name, default_secs);
        if secs == 0 {
            None
        } else {
            Some(Duration::from_secs(secs))
        }
    }

    /// Get a required timeout (always returns a Duration)
    fn get_required_timeout(name: &str, default_secs: u64) -> Duration {
        let secs = Self::get_env_or_default(name, default_secs);
        Duration::from_secs(secs)
    }

    /// Get timeout value from environment variable or use default
    fn get_env_or_default(name: &str, default_secs: u64) -> u64 {
        let env_var = format!("QONTINUI_TIMEOUT_{}", name);
        env::var(&env_var)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default_secs)
    }

    /// Log all current timeout values (useful for debugging)
    pub fn log_all() {
        use tracing::info;

        info!("=== Timeout Configuration ===");
        info!("User-facing operations (0 = disabled):");
        info!("  ACTION_EXECUTION: {:?}", Self::action_execution());
        info!("  PYTHON_COMMAND: {:?}", Self::python_command());
        info!("  IMAGE_RECOGNITION: {:?}", Self::image_recognition());
        info!("  AI_GENERATION: {:?}", Self::ai_generation());
        info!("  VISION_ANALYSIS: {:?}", Self::vision_analysis());
        info!("  PLAYWRIGHT_TEST: {:?}", Self::playwright_test());
        info!("  SHELL_COMMAND: {:?}", Self::shell_command());
        info!("  WORKFLOW_EXECUTION: {:?}", Self::workflow_execution());
        info!("  STATE_NAVIGATION: {:?}", Self::state_navigation());
        info!(
            "  UI_BRIDGE_EXPLORATION: {:?}",
            Self::ui_bridge_exploration()
        );
        info!("  SCREENSHOT_CAPTURE: {:?}", Self::screenshot_capture());
        info!("  API_REQUEST: {:?}", Self::api_request());
        info!("  MCP_CALL: {:?}", Self::mcp_call());
        info!("  AWAS_OPERATION: {:?}", Self::awas_operation());
        info!("System health (always enabled):");
        info!("  PYTHON_STARTUP: {:?}", Self::python_startup());
        info!("  HEALTH_PING_INTERVAL: {:?}", Self::health_ping_interval());
        info!("  HEALTH_PONG_TIMEOUT: {:?}", Self::health_pong_timeout());
        info!("  UI_BRIDGE_IPC: {:?}", Self::ui_bridge_ipc());
        info!("  HTTP_EXTERNAL: {:?}", Self::http_external());
        info!("  GRACEFUL_SHUTDOWN: {:?}", Self::graceful_shutdown());
        info!("  HEALTH_CHECK_URL: {:?}", Self::health_check_url());
        info!("  STALE_STEP_THRESHOLD: {:?}", Self::stale_step_threshold());
        info!("==============================");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_optional_timeout_disabled_by_default() {
        // User-facing operations should be disabled by default
        assert!(Timeouts::action_execution().is_none());
        assert!(Timeouts::python_command().is_none());
        assert!(Timeouts::ai_generation().is_none());
    }

    #[test]
    fn test_required_timeout_has_default() {
        // System health operations should have defaults
        assert!(Timeouts::python_startup().as_secs() > 0);
        assert!(Timeouts::health_ping_interval().as_secs() > 0);
        assert!(Timeouts::ui_bridge_ipc().as_secs() > 0);
    }

    #[test]
    fn test_env_override() {
        // Test that environment variables can override defaults
        env::set_var("QONTINUI_TIMEOUT_ACTION_EXECUTION", "300");
        let timeout = Timeouts::action_execution();
        assert!(timeout.is_some());
        assert_eq!(timeout.unwrap().as_secs(), 300);
        env::remove_var("QONTINUI_TIMEOUT_ACTION_EXECUTION");
    }
}
