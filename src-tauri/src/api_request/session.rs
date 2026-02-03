//! API Request Session for Chained Requests
//!
//! Provides a session abstraction for executing multiple API requests
//! with shared variable state. Variables extracted from one request
//! are automatically available in subsequent requests.
//!
//! The session can be integrated with the StepExecutor's SharedVariableStore
//! for proper variable chaining across workflow steps.

use super::executor::ApiRequestExecutor;
use super::types::{ApiRequestConfig, ApiRequestResult, CredentialType, CredentialValue};
use crate::orchestrator::context_propagation::SharedVariableStore;
use std::collections::HashMap;

/// A session for executing chained API requests.
///
/// Variables extracted from response bodies are automatically shared
/// across requests within the same session.
///
/// # Example
///
/// ```ignore
/// let mut session = ApiRequestSession::new(None);
///
/// // First request extracts auth_token
/// session.execute(&login_config, None).await?;
///
/// // Second request uses {{auth_token}}
/// session.execute(&data_config, None).await?;
/// ```
pub struct ApiRequestSession {
    /// The underlying executor with shared resolver
    executor: ApiRequestExecutor,
    /// Results from all requests in this session
    results: Vec<ApiRequestResult>,
}

impl ApiRequestSession {
    /// Create a new session with optional initial variables.
    pub fn new(initial_variables: Option<HashMap<String, String>>) -> Self {
        let executor = match initial_variables {
            Some(vars) => ApiRequestExecutor::with_variables(vars),
            None => ApiRequestExecutor::new(),
        };

        Self {
            executor,
            results: Vec::new(),
        }
    }

    /// Create a new session initialized from a SharedVariableStore.
    ///
    /// This imports all existing variables from the store, allowing the session
    /// to use variables set by previous workflow steps.
    pub fn from_shared_store(store: &SharedVariableStore) -> Self {
        let vars = store.get_all();
        Self::new(if vars.is_empty() { None } else { Some(vars) })
    }

    /// Sync all session variables back to a SharedVariableStore.
    ///
    /// Call this after executing requests to propagate extracted variables
    /// back to the workflow's shared variable store.
    pub fn sync_to_shared_store(&self, store: &SharedVariableStore) {
        for (name, value) in self.get_variables() {
            store.set(&name, value);
        }
    }

    /// Import variables from a SharedVariableStore into this session.
    ///
    /// This merges the store's variables with the session's existing variables.
    pub fn import_from_shared_store(&mut self, store: &SharedVariableStore) {
        let vars = store.get_all();
        self.executor.resolver().import_variables(&vars);
    }

    /// Execute a single API request within this session.
    ///
    /// Variables extracted from the response are automatically stored
    /// and available for subsequent requests.
    pub async fn execute(
        &mut self,
        config: &ApiRequestConfig,
        credential: Option<(&CredentialType, &CredentialValue)>,
    ) -> anyhow::Result<ApiRequestResult> {
        let result = self.executor.execute(config, credential).await?;
        self.results.push(result.clone());
        Ok(result)
    }

    /// Execute a chain of API requests in sequence.
    ///
    /// Stops on first failure and returns all results up to that point.
    pub async fn execute_chain(
        &mut self,
        configs: &[ApiRequestConfig],
        credential: Option<(&CredentialType, &CredentialValue)>,
    ) -> anyhow::Result<Vec<ApiRequestResult>> {
        let mut chain_results = Vec::new();

        for config in configs {
            let result = self.execute(config, credential).await?;
            let success = result.success;
            chain_results.push(result);

            if !success {
                break;
            }
        }

        Ok(chain_results)
    }

    /// Get all variables currently stored in the session.
    pub fn get_variables(&self) -> HashMap<String, String> {
        self.executor.get_all_variables()
    }

    /// Set a variable in the session.
    pub fn set_variable(&mut self, name: &str, value: &str) {
        self.executor.set_variable(name, value);
    }

    /// Get all results from this session.
    pub fn get_results(&self) -> &[ApiRequestResult] {
        &self.results
    }

    /// Clear all results (but keep variables).
    pub fn clear_results(&mut self) {
        self.results.clear();
    }

    /// Clear all variables and results.
    pub fn reset(&mut self) {
        self.executor.clear_variables();
        self.results.clear();
    }

    /// Validate that all variables in the config are defined.
    pub fn validate_variables(&self, config: &ApiRequestConfig) -> Result<(), Vec<String>> {
        self.executor.validate_variables(config)
    }
}

impl Default for ApiRequestSession {
    fn default() -> Self {
        Self::new(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_creation() {
        let session = ApiRequestSession::new(None);
        assert!(session.get_variables().is_empty());
        assert!(session.get_results().is_empty());
    }

    #[test]
    fn test_session_with_initial_variables() {
        let mut vars = HashMap::new();
        vars.insert(
            "base_url".to_string(),
            "https://api.example.com".to_string(),
        );

        let session = ApiRequestSession::new(Some(vars));
        let variables = session.get_variables();

        assert_eq!(
            variables.get("base_url"),
            Some(&"https://api.example.com".to_string())
        );
    }

    #[test]
    fn test_session_set_variable() {
        let mut session = ApiRequestSession::new(None);
        session.set_variable("token", "abc123");

        let variables = session.get_variables();
        assert_eq!(variables.get("token"), Some(&"abc123".to_string()));
    }

    #[test]
    fn test_session_reset() {
        let mut session = ApiRequestSession::new(None);
        session.set_variable("token", "abc123");
        session.reset();

        assert!(session.get_variables().is_empty());
        assert!(session.get_results().is_empty());
    }
}
