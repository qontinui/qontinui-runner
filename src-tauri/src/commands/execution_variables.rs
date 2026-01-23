//! Execution Variables settings commands
//!
//! This module handles execution variables configuration:
//! - Getting and setting auth source settings
//! - Managing custom variables for API execution
//! - Resolving variable values from environment

use crate::settings::{
    self, AuthSource, CustomVariable, ExecutionVariablesSettings, VariableSource,
};
use serde::{Deserialize, Serialize};
use tracing::info;

use super::CommandResponse;

// ============================================================================
// Response Types
// ============================================================================

/// Resolved execution context with actual values
#[derive(Debug, Serialize, Deserialize)]
pub struct ResolvedExecutionContext {
    /// Auth source being used
    pub auth_source: String,
    /// Resolved auth token (if applicable, redacted for security)
    pub auth_token_status: String,
    /// Auth header name
    pub auth_header_name: String,
    /// Resolved custom variables (names and whether they have values)
    pub custom_variables: Vec<ResolvedVariableStatus>,
}

/// Status of a resolved variable
#[derive(Debug, Serialize, Deserialize)]
pub struct ResolvedVariableStatus {
    pub name: String,
    pub source: String,
    pub has_value: bool,
    pub description: Option<String>,
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Get the current execution variables settings.
///
/// Returns the execution variables settings stored in the persistent settings file.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with execution variables settings data
/// * `Err(String)` - Error message if settings cannot be loaded
#[tauri::command]
pub fn get_execution_variables_settings() -> Result<CommandResponse, String> {
    info!("Getting execution variables settings");

    let settings = settings::get_execution_variables_settings();

    Ok(CommandResponse {
        success: true,
        message: Some("Execution variables settings retrieved".to_string()),
        data: Some(
            serde_json::to_value(&settings)
                .map_err(|e| format!("Failed to serialize settings: {}", e))?,
        ),
    })
}

/// Save execution variables settings.
///
/// Updates the execution variables settings in the persistent settings file.
///
/// # Arguments
/// * `auth_source` - Auth source ("captured", "manual", or "environment")
/// * `auth_header_name` - Header name to use for auth (e.g., "Authorization")
/// * `auth_token` - Manual auth token (when auth_source is "manual")
/// * `auth_env_var` - Environment variable name (when auth_source is "environment")
/// * `custom_variables` - Array of custom variables
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if settings cannot be saved
#[tauri::command]
pub fn save_execution_variables_settings(
    auth_source: String,
    auth_header_name: String,
    auth_token: Option<String>,
    auth_env_var: String,
    custom_variables: Vec<serde_json::Value>,
) -> Result<CommandResponse, String> {
    info!(
        "Saving execution variables settings: auth_source={}, {} custom variables",
        auth_source,
        custom_variables.len()
    );

    let auth_source_enum = match auth_source.as_str() {
        "captured" => AuthSource::Captured,
        "manual" => AuthSource::Manual,
        "environment" => AuthSource::Environment,
        _ => return Err(format!("Invalid auth source: {}", auth_source)),
    };

    // Parse custom variables
    let parsed_variables: Result<Vec<CustomVariable>, String> = custom_variables
        .into_iter()
        .map(|v| {
            let name = v
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or("Missing variable name")?
                .to_string();

            let source_str = v.get("source").and_then(|s| s.as_str()).unwrap_or("manual");

            let source = match source_str {
                "manual" => VariableSource::Manual,
                "environment" => VariableSource::Environment,
                _ => VariableSource::Manual,
            };

            let value = v
                .get("value")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());
            let env_var = v
                .get("envVar")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());
            let description = v
                .get("description")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());

            Ok(CustomVariable {
                name,
                source,
                value,
                env_var,
                description,
            })
        })
        .collect();

    let custom_vars = parsed_variables?;

    let execution_settings = ExecutionVariablesSettings {
        auth_source: auth_source_enum,
        auth_header_name,
        auth_token,
        auth_env_var,
        custom_variables: custom_vars,
    };

    settings::save_execution_variables_settings(execution_settings)
        .map_err(|e| format!("Failed to save execution variables settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Execution variables settings saved".to_string()),
        data: None,
    })
}

/// Get resolved execution context.
///
/// Returns the current execution context with resolved variable values.
/// For security, auth tokens are not returned - only their status.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with resolved context
/// * `Err(String)` - Error message if resolution fails
#[tauri::command]
pub fn get_resolved_execution_context() -> Result<CommandResponse, String> {
    info!("Getting resolved execution context");

    let settings = settings::get_execution_variables_settings();
    let resolved_token = settings::get_resolved_auth_token();
    let resolved_vars = settings::get_resolved_custom_variables();

    // Determine auth token status (don't expose the actual token)
    let auth_token_status = match settings.auth_source {
        AuthSource::Captured => "Using captured headers".to_string(),
        AuthSource::Manual => {
            if resolved_token.is_some() {
                "Token configured".to_string()
            } else {
                "No token configured".to_string()
            }
        }
        AuthSource::Environment => {
            if resolved_token.is_some() {
                format!("Found in ${}", settings.auth_env_var)
            } else {
                format!("${} not set", settings.auth_env_var)
            }
        }
    };

    // Build variable status list
    let variable_status: Vec<ResolvedVariableStatus> = settings
        .custom_variables
        .iter()
        .map(|var| ResolvedVariableStatus {
            name: var.name.clone(),
            source: match var.source {
                VariableSource::Manual => "manual".to_string(),
                VariableSource::Environment => "environment".to_string(),
            },
            has_value: resolved_vars.contains_key(&var.name),
            description: var.description.clone(),
        })
        .collect();

    let context = ResolvedExecutionContext {
        auth_source: format!("{:?}", settings.auth_source).to_lowercase(),
        auth_token_status,
        auth_header_name: settings.auth_header_name,
        custom_variables: variable_status,
    };

    Ok(CommandResponse {
        success: true,
        message: Some("Execution context resolved".to_string()),
        data: Some(
            serde_json::to_value(&context)
                .map_err(|e| format!("Failed to serialize context: {}", e))?,
        ),
    })
}

/// Test if an environment variable is set and has a value.
///
/// # Arguments
/// * `env_var` - The environment variable name to check
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with `exists` boolean in data
/// * `Err(String)` - Error message if check fails
#[tauri::command]
pub fn test_env_var(env_var: String) -> Result<CommandResponse, String> {
    info!("Testing environment variable: {}", env_var);

    let exists = std::env::var(&env_var).is_ok();

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "env_var": env_var,
            "exists": exists
        })),
    })
}
