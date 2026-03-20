//! Context metadata operations (usage tracking, enable/disable, sync status).

#![allow(dead_code)]

use tracing::info;

use super::storage::{load_user_context_library, save_user_context_library};
use super::types::{ContextMetadata, WebSyncStatus};

/// Record that a context was used (increments use_count, updates last_used_at)
pub fn record_context_use(context_id: &str) -> Result<(), String> {
    let mut library = load_user_context_library();
    let metadata = library.get_or_create_metadata(context_id);
    metadata.record_use();
    save_user_context_library(&library)?;
    info!("Recorded use of context: {}", context_id);
    Ok(())
}

/// Enable or disable a context
pub fn set_context_enabled(context_id: &str, enabled: bool) -> Result<(), String> {
    let mut library = load_user_context_library();
    let metadata = library.get_or_create_metadata(context_id);
    metadata.enabled = enabled;
    save_user_context_library(&library)?;
    info!(
        "Set context {} enabled={}",
        context_id,
        if enabled { "true" } else { "false" }
    );
    Ok(())
}

/// Set the web sync status for a context
pub fn set_web_sync_status(context_id: &str, status: Option<WebSyncStatus>) -> Result<(), String> {
    let mut library = load_user_context_library();
    let metadata = library.get_or_create_metadata(context_id);
    metadata.web_sync_status = status.clone();
    save_user_context_library(&library)?;
    info!("Set context {} web_sync_status={:?}", context_id, status);
    Ok(())
}

/// Get metadata for a context
pub fn get_context_metadata(context_id: &str) -> Option<ContextMetadata> {
    load_user_context_library()
        .metadata
        .into_iter()
        .find(|m| m.context_id == context_id)
}
