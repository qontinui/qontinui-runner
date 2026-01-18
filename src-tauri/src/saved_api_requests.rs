//! Saved API Request Library
//!
//! This module provides types for saved API request templates that users can
//! store in the Library and reuse across workflows.

use crate::api_request::types::{ApiAssertion, HttpMethod, VariableExtraction};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A saved API request template in the library
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedApiRequest {
    /// Unique identifier (UUID v4)
    pub id: String,
    /// Display name for the request
    pub name: String,
    /// Optional description of what this request does
    #[serde(default)]
    pub description: String,
    /// Category for organization
    #[serde(default)]
    pub category: String,
    /// Tags for filtering/searching
    #[serde(default)]
    pub tags: Vec<String>,

    // Request configuration
    /// HTTP method
    pub method: HttpMethod,
    /// URL (may contain {{variables}})
    pub url: String,
    /// Request headers
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Request body
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Content type for the body
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_content_type: Option<String>,
    /// Timeout in milliseconds
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    /// Whether to follow redirects
    #[serde(default = "default_true")]
    pub follow_redirects: bool,

    /// Variable extractions from response
    #[serde(default)]
    pub variable_extractions: Vec<VariableExtraction>,
    /// Assertions for response verification
    #[serde(default)]
    pub assertions: Vec<ApiAssertion>,

    /// Credential ID for authentication
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,

    /// ISO 8601 timestamp of creation
    pub created_at: String,
    /// ISO 8601 timestamp of last modification
    pub updated_at: String,
}

fn default_timeout() -> u64 {
    30000
}

fn default_true() -> bool {
    true
}

/// Request body for creating a new saved API request
#[derive(Debug, Clone, Deserialize)]
pub struct CreateSavedApiRequestRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub method: HttpMethod,
    pub url: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
    pub body_content_type: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_true")]
    pub follow_redirects: bool,
    #[serde(default)]
    pub variable_extractions: Vec<VariableExtraction>,
    #[serde(default)]
    pub assertions: Vec<ApiAssertion>,
    pub credential_id: Option<String>,
}

/// Request body for updating a saved API request
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSavedApiRequestRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
    pub method: Option<HttpMethod>,
    pub url: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub body: Option<String>,
    pub body_content_type: Option<String>,
    pub timeout_ms: Option<u64>,
    pub follow_redirects: Option<bool>,
    pub variable_extractions: Option<Vec<VariableExtraction>>,
    pub assertions: Option<Vec<ApiAssertion>>,
    pub credential_id: Option<String>,
}

/// Query parameters for searching saved API requests
#[derive(Debug, Clone, Deserialize)]
pub struct SearchSavedApiRequestsQuery {
    /// Search query (matches name, description, url)
    pub q: Option<String>,
    /// Filter by category
    pub category: Option<String>,
    /// Filter by tag
    pub tag: Option<String>,
}
