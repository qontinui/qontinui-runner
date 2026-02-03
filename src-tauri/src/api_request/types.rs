//! Type definitions for API requests

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// HTTP methods for API requests
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
#[derive(Default)]
pub enum HttpMethod {
    #[default]
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl From<HttpMethod> for reqwest::Method {
    fn from(method: HttpMethod) -> Self {
        match method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
            HttpMethod::Put => reqwest::Method::PUT,
            HttpMethod::Patch => reqwest::Method::PATCH,
            HttpMethod::Delete => reqwest::Method::DELETE,
        }
    }
}

impl std::fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpMethod::Get => write!(f, "GET"),
            HttpMethod::Post => write!(f, "POST"),
            HttpMethod::Put => write!(f, "PUT"),
            HttpMethod::Patch => write!(f, "PATCH"),
            HttpMethod::Delete => write!(f, "DELETE"),
        }
    }
}

/// Configuration for an API request step
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiRequestConfig {
    /// Step ID for logging
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    /// Step name for logging
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_name: Option<String>,

    /// HTTP method
    pub method: HttpMethod,
    /// URL (may contain {{variables}})
    pub url: String,
    /// URL after variable resolution
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_url: Option<String>,

    /// Request headers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    /// Request body
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Content type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,

    /// Timeout in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Whether to follow redirects
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_redirects: Option<bool>,

    /// Credential ID for authentication
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,

    /// Variable extractions from response
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extractions: Option<Vec<VariableExtraction>>,
    /// Assertions for response verification
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertions: Option<Vec<ApiAssertion>>,
}

impl Default for ApiRequestConfig {
    fn default() -> Self {
        Self {
            step_id: None,
            step_name: None,
            method: HttpMethod::Get,
            url: String::new(),
            resolved_url: None,
            headers: None,
            body: None,
            content_type: None,
            timeout_ms: None, // No timeout by default
            follow_redirects: Some(true),
            credential_id: None,
            extractions: None,
            assertions: None,
        }
    }
}

/// Variable extraction from API response using JSON path
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableExtraction {
    /// Variable name to store the extracted value
    pub variable_name: String,
    /// JSON path to extract (e.g., "$.data.id")
    pub json_path: String,
    /// Default value if extraction fails
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
}

/// Assertion for API response verification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiAssertion {
    /// Type of assertion
    #[serde(rename = "type")]
    pub assertion_type: String,
    /// Expected value
    pub expected: serde_json::Value,
    /// JSON path for json_path assertions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_path: Option<String>,
    /// Header name for header assertions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header_name: Option<String>,
    /// Comparison operator
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
}

/// Result of executing an API request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiRequestResult {
    /// Whether the request was successful (all assertions passed)
    pub success: bool,

    /// HTTP status code
    pub status_code: u16,
    /// HTTP status text
    pub status_text: String,

    /// Response headers
    pub response_headers: HashMap<String, String>,
    /// Response time in milliseconds
    pub response_time_ms: u64,

    /// Type of response body
    pub response_body_type: String,
    /// Response body (for JSON/text responses)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
    /// Path to saved file (for binary responses)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_file_path: Option<String>,
    /// Size of response in bytes
    pub response_size_bytes: usize,

    /// Results of variable extractions
    pub extractions: Vec<ExtractionResult>,
    /// Results of assertions
    pub assertions: Vec<AssertionResult>,

    /// Error message if request failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result of a variable extraction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResult {
    /// Variable name
    pub variable_name: String,
    /// JSON path used
    pub json_path: String,
    /// Extracted value (if successful)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extracted_value: Option<String>,
    /// Whether extraction was successful
    pub success: bool,
    /// Error message if extraction failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result of an assertion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionResult {
    /// Type of assertion
    pub assertion_type: String,
    /// Expected value (as string)
    pub expected: String,
    /// Actual value (as string)
    pub actual: String,
    /// Whether assertion passed
    pub passed: bool,
    /// Error message if assertion failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Parsed cURL command
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedCurl {
    /// HTTP method
    pub method: HttpMethod,
    /// URL
    pub url: String,
    /// Headers
    pub headers: HashMap<String, String>,
    /// Request body
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Content type (extracted from headers)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

impl From<ParsedCurl> for ApiRequestConfig {
    fn from(parsed: ParsedCurl) -> Self {
        Self {
            method: parsed.method,
            url: parsed.url,
            headers: if parsed.headers.is_empty() {
                None
            } else {
                Some(parsed.headers)
            },
            body: parsed.body,
            content_type: parsed.content_type,
            ..Default::default()
        }
    }
}

/// Credential types for API authentication
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialType {
    BearerToken,
    BasicAuth,
    ApiKey,
    OAuth2,
}

/// Credential metadata stored in database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiCredential {
    pub id: String,
    pub name: String,
    pub credential_type: CredentialType,
    pub storage_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// Credential values (stored in secure storage)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CredentialValue {
    /// For bearer_token type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    /// For basic_auth type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// For api_key type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header_name: Option<String>,
    /// For oauth2 type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
}

impl CredentialValue {
    /// Get the header to apply for this credential
    pub fn get_auth_header(&self, credential_type: CredentialType) -> Option<(String, String)> {
        match credential_type {
            CredentialType::BearerToken => self
                .access_token
                .as_ref()
                .map(|token| ("Authorization".to_string(), format!("Bearer {}", token))),
            CredentialType::BasicAuth => match (&self.username, &self.password) {
                (Some(user), Some(pass)) => {
                    use base64::Engine;
                    let encoded = base64::engine::general_purpose::STANDARD
                        .encode(format!("{}:{}", user, pass));
                    Some(("Authorization".to_string(), format!("Basic {}", encoded)))
                }
                _ => None,
            },
            CredentialType::ApiKey => self.api_key.as_ref().map(|key| {
                let header_name = self
                    .header_name
                    .clone()
                    .unwrap_or_else(|| "X-API-Key".to_string());
                (header_name, key.clone())
            }),
            CredentialType::OAuth2 => {
                // OAuth2 uses bearer token for requests
                self.access_token
                    .as_ref()
                    .map(|token| ("Authorization".to_string(), format!("Bearer {}", token)))
            }
        }
    }
}
