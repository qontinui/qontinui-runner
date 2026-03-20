//! Special context handling (multi-step guide, service restart overrides).

#![allow(dead_code)]

use tracing::info;

use super::builtins::get_builtin_contexts;
use super::types::Context;
use super::user_contexts::get_all_user_contexts;

/// ID of the builtin Multi-Step Task Guide context.
pub const MULTI_STEP_GUIDE_ID: &str = "builtin-multi-step-guide";

/// ID of the builtin Service Restart Commands context.
pub const SERVICE_RESTART_ID: &str = "builtin-service-restart";

/// Get the Service Restart Commands context, preferring user override if exists.
///
/// Checks if the user has a context with the same name ("Service Restart Commands").
/// If so, returns the user's version (allowing customization).
/// Otherwise, returns the builtin version.
pub fn get_service_restart_commands() -> Context {
    // Check for user override by name
    let user_contexts = get_all_user_contexts();
    if let Some(user_override) = user_contexts
        .into_iter()
        .find(|c| c.name == "Service Restart Commands")
    {
        info!(
            "Using user-customized Service Restart Commands (id: {})",
            user_override.id
        );
        return user_override;
    }

    // Return builtin version
    get_builtin_contexts()
        .into_iter()
        .find(|c| c.id == SERVICE_RESTART_ID)
        .expect("Builtin Service Restart Commands should exist")
}

/// Get the Multi-Step Task Guide context, preferring user override if exists.
///
/// Checks if the user has a context with the same name ("Multi-Step Task Guide").
/// If so, returns the user's version (allowing customization).
/// Otherwise, returns the builtin version.
pub fn get_multi_step_guide() -> Context {
    // Check for user override by name
    let user_contexts = get_all_user_contexts();
    if let Some(user_override) = user_contexts
        .into_iter()
        .find(|c| c.name == "Multi-Step Task Guide")
    {
        info!(
            "Using user-customized Multi-Step Task Guide (id: {})",
            user_override.id
        );
        return user_override;
    }

    // Return builtin version
    get_builtin_contexts()
        .into_iter()
        .find(|c| c.id == MULTI_STEP_GUIDE_ID)
        .expect("Builtin Multi-Step Task Guide should exist")
}
