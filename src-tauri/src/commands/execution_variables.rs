//! Execution Variables settings commands
//!
//! This module handles execution variables configuration:
//! - Getting and setting auth source settings
//! - Managing custom variables for API execution
//! - Resolving variable values from environment
//!
//! # Typed errors (Workstream D)
//!
//! Handlers keep `-> Result<T, String>` at the Tauri boundary and build
//! errors via [`AppError`] internally, converting with
//! `.map_err(String::from)`. See `commands/mod.rs` for the migration guide.

use crate::error::AppError;
use crate::settings::{
    self, AuthSource, CustomVariable, ExecutionVariablesSettings, VariableSource,
};
use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
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

/// Internal implementation of [`get_execution_variables_settings`] returning [`AppError`].
fn get_execution_variables_settings_impl() -> Result<CommandResponse, AppError> {
    info!("Getting execution variables settings");

    let settings = settings::get_execution_variables_settings();
    let data = serde_json::to_value(&settings)?;

    Ok(CommandResponse {
        success: true,
        message: Some("Execution variables settings retrieved".to_string()),
        data: Some(data),
    })
}

/// Get the current execution variables settings.
#[tauri::command]
pub fn get_execution_variables_settings() -> Result<CommandResponse, String> {
    get_execution_variables_settings_impl().map_err(String::from)
}

/// Internal implementation of [`save_execution_variables_settings`] returning [`AppError`].
fn save_execution_variables_settings_impl(
    auth_source: String,
    auth_header_name: String,
    auth_token: Option<String>,
    auth_env_var: String,
    custom_variables: Vec<serde_json::Value>,
) -> Result<CommandResponse, AppError> {
    info!(
        "Saving execution variables settings: auth_source={}, {} custom variables",
        auth_source,
        custom_variables.len()
    );

    let auth_source_enum = match auth_source.as_str() {
        "captured" => AuthSource::Captured,
        "manual" => AuthSource::Manual,
        "environment" => AuthSource::Environment,
        _ => {
            return Err(AppError::ValidationError(format!(
                "Invalid auth source: {}",
                auth_source
            )))
        }
    };

    // Parse custom variables
    let parsed_variables: Result<Vec<CustomVariable>, AppError> = custom_variables
        .into_iter()
        .map(|v| {
            let name = v
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or_else(|| AppError::ValidationError("Missing variable name".to_string()))?
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
        .map_err(AppError::ConfigError)?;

    Ok(CommandResponse {
        success: true,
        message: Some("Execution variables settings saved".to_string()),
        data: None,
    })
}

/// Save execution variables settings.
#[tauri::command]
pub fn save_execution_variables_settings(
    auth_source: String,
    auth_header_name: String,
    auth_token: Option<String>,
    auth_env_var: String,
    custom_variables: Vec<serde_json::Value>,
) -> Result<CommandResponse, String> {
    save_execution_variables_settings_impl(
        auth_source,
        auth_header_name,
        auth_token,
        auth_env_var,
        custom_variables,
    )
    .map_err(String::from)
}

/// Internal implementation of [`get_resolved_execution_context`] returning [`AppError`].
fn get_resolved_execution_context_impl() -> Result<CommandResponse, AppError> {
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

    let data = serde_json::to_value(&context)?;

    Ok(CommandResponse {
        success: true,
        message: Some("Execution context resolved".to_string()),
        data: Some(data),
    })
}

/// Get resolved execution context.
#[tauri::command]
pub fn get_resolved_execution_context() -> Result<CommandResponse, String> {
    get_resolved_execution_context_impl().map_err(String::from)
}

/// Test if an environment variable is set and has a value.
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

/// Tauri plugin exposing all execution-variables commands.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_execution_variables")
        .invoke_handler(tauri::generate_handler![
            get_execution_variables_settings,
            save_execution_variables_settings,
            get_resolved_execution_context,
            test_env_var,
        ])
        .build()
}
