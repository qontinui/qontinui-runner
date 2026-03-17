//! Type definitions for orchestration loop config CRUD operations.

use serde::{Deserialize, Serialize};

/// A saved orchestration loop configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OlConfig {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub is_favorite: bool,
    /// The full OrchestrationLoopConfig as a JSON blob.
    pub config_json: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
}

/// Request to create a new saved config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateOlConfigRequest {
    pub name: String,
    pub description: Option<String>,
    pub config_json: serde_json::Value,
}

/// Request to update an existing saved config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateOlConfigRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub is_favorite: Option<bool>,
    pub config_json: Option<serde_json::Value>,
}
