//! AI Context System
//!
//! This module provides storage and management for AI contexts - reusable knowledge
//! snippets that provide domain-specific guidance to AI tasks.
//!
//! Contexts can be:
//! - Project-scoped: Stored in the project config, exported with the project
//! - User-scoped: Stored locally in the runner, personal to the user
//! - Built-in: Shipped with the runner, read-only examples
//!
//! The core Context type matches qontinui-schemas. Runner-specific fields
//! (scope, enabled, usage stats) are stored separately in ContextMetadata.

#![allow(dead_code)]

pub(crate) mod builtins;
pub(crate) mod metadata;
pub(crate) mod project_contexts;
pub(crate) mod resolution;
pub(crate) mod special;
pub(crate) mod storage;
pub(crate) mod types;
pub(crate) mod user_contexts;

// Re-export all public types
pub use types::{
    Context, ContextAutoInclude, ContextScope, ContextWithMetadata, UserContextLibrary,
    WebSyncStatus,
};

// Re-export storage operations
pub use storage::load_user_context_library;

// Re-export user context CRUD
pub use user_contexts::{
    create_context_from_file, create_user_context, delete_user_context, get_all_user_contexts,
    get_user_context, get_user_context_categories, update_user_context,
};

// Re-export metadata operations
pub use metadata::{record_context_use, set_context_enabled, set_web_sync_status};

// Re-export resolution and formatting
pub use resolution::{
    format_contexts_for_prompt, format_observation_memory_for_prompt, format_single_context,
    inject_contexts, record_contexts_used, resolve_contexts,
};

// Re-export built-in contexts
pub use builtins::get_builtin_contexts;

// Re-export project context operations
pub use project_contexts::{
    add_project_context_to_config, create_project_context, delete_project_context_from_config,
    get_project_context_from_config, get_project_contexts, get_project_contexts_from_config,
    load_project_contexts_from_dir, update_project_context_in_config,
};

// Re-export special context handling
pub use special::{get_multi_step_guide, get_service_restart_commands};
