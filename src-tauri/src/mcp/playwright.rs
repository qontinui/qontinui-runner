//! Playwright test execution for MCP API
//!
//! Provides Playwright test execution capabilities.

use std::sync::Arc;
use tracing::{error, info, warn};

use super::types::ApiState;
use crate::playwright::{get_script, run_script, PlaywrightResult};

/// Execute a Playwright script by ID
pub async fn execute_playwright_internal(
    _state: Arc<ApiState>,
    script_id: &str,
) -> (bool, Option<String>) {
    info!("Executing Playwright script: {}", script_id);

    // Get the script from storage to verify it exists
    match get_script(script_id) {
        Some(script) => {
            info!("Found script: {} ({})", script.name, script_id);
        }
        None => {
            error!("Playwright script not found: {}", script_id);
            return (
                false,
                Some(format!("Playwright script not found: {}", script_id)),
            );
        }
    };

    // Execute the script using the run_script function
    // run_script is synchronous, so we spawn it in a blocking task
    let id = script_id.to_string();
    let result: Result<PlaywrightResult, String> =
        tokio::task::spawn_blocking(move || run_script(&id, None))
            .await
            .map_err(|e| format!("Task join error: {}", e))
            .and_then(|r| r);

    match result {
        Ok(result) => {
            if result.passed {
                info!(
                    "Playwright script {} completed successfully in {}ms",
                    script_id, result.duration_ms
                );
                (true, None)
            } else {
                warn!(
                    "Playwright script {} failed: {:?}",
                    script_id, result.error
                );
                (false, result.error)
            }
        }
        Err(e) => {
            error!("Playwright execution error for {}: {}", script_id, e);
            (false, Some(format!("Execution error: {}", e)))
        }
    }
}
