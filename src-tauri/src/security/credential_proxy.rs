//! Credential proxy for agent sandboxing.
//!
//! Implements gondolin-inspired credential isolation: agent processes receive
//! random placeholder tokens instead of real API keys. The network mediator
//! proxy intercepts outbound requests and replaces placeholders with real
//! credentials from the OS keychain before forwarding.
//!
//! This ensures that real API keys never enter the container's memory space.

use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, warn};

// ============================================================================
// Credential Source
// ============================================================================

/// Where to retrieve the real credential value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CredentialSource {
    /// Retrieve from the OS keychain.
    Keychain {
        service: String,
        key: String,
    },
    /// Retrieve from an environment variable.
    Environment {
        var_name: String,
    },
}

// ============================================================================
// Credential Proxy
// ============================================================================

/// Manages placeholder tokens for credential isolation.
///
/// Before container creation, generate random placeholders for each credential
/// the step is allowed to use. Inject these as env vars. The network mediator
/// calls `inject_credentials()` to replace placeholders with real values in
/// outbound request headers.
#[derive(Debug, Clone)]
pub struct CredentialProxy {
    /// Maps placeholder token → credential source.
    placeholder_map: HashMap<String, CredentialSource>,
    /// Maps env var name → placeholder token (for container env injection).
    env_var_map: HashMap<String, String>,
    /// HTTP header names to scan for placeholders.
    header_patterns: Vec<String>,
}

/// Result of credential injection (used by future per-request reporting).
#[allow(dead_code)]
#[derive(Debug)]
pub struct InjectResult {
    /// Number of placeholders replaced.
    pub injected_count: usize,
    /// Credential names that were injected.
    pub injected_names: Vec<String>,
}

impl CredentialProxy {
    /// Create a new credential proxy for the given credential names.
    ///
    /// Generates random placeholder tokens for each credential and maps
    /// them to their keychain source.
    pub fn new(credential_names: &[&str]) -> Self {
        let mut placeholder_map = HashMap::new();
        let mut env_var_map = HashMap::new();
        let mut rng = rand::rng();

        for &name in credential_names {
            // Generate a random placeholder token
            let token: String = format!(
                "QCRED_{}_{:016x}",
                name.to_uppercase().replace(' ', "_"),
                rng.random::<u64>()
            );

            let (service, key) = credential_source_for_name(name);
            placeholder_map.insert(
                token.clone(),
                CredentialSource::Keychain {
                    service: service.to_string(),
                    key: key.to_string(),
                },
            );

            // Map the standard env var name to the placeholder
            let env_var_name = env_var_name_for_credential(name);
            env_var_map.insert(env_var_name, token);
        }

        Self {
            placeholder_map,
            env_var_map,
            header_patterns: vec![
                "authorization".to_string(),
                "x-api-key".to_string(),
                "api-key".to_string(),
            ],
        }
    }

    /// Generate environment variable entries for container injection.
    ///
    /// Returns entries in `KEY=VALUE` format suitable for Docker container env.
    /// The values are placeholder tokens, NOT real credentials.
    pub fn placeholder_env_vars(&self) -> Vec<String> {
        self.env_var_map
            .iter()
            .map(|(name, placeholder)| format!("{}={}", name, placeholder))
            .collect()
    }

    /// Check if a header value contains any known placeholder token.
    ///
    /// Returns the real credential value if a placeholder is found.
    /// Only replaces in the configured header names (authorization, x-api-key, etc.).
    pub fn resolve_placeholder(&self, header_value: &str) -> Option<(String, String)> {
        for (placeholder, source) in &self.placeholder_map {
            if header_value.contains(placeholder.as_str()) {
                // Retrieve the real credential
                match resolve_credential(source) {
                    Some(real_value) => {
                        let replaced = header_value.replace(placeholder.as_str(), &real_value);
                        // Extract credential name: QCRED_{NAME}_{hex16}
                        // Strip the prefix and the last 17 chars (_<16 hex digits>)
                        let name = placeholder
                            .strip_prefix("QCRED_")
                            .and_then(|s| {
                                if s.len() > 17 {
                                    Some(&s[..s.len() - 17])
                                } else {
                                    None
                                }
                            })
                            .unwrap_or("unknown");
                        return Some((replaced, name.to_lowercase()));
                    }
                    None => {
                        warn!(
                            "Credential proxy: failed to resolve credential for placeholder {}",
                            &placeholder[..placeholder.len().min(20)]
                        );
                        return None;
                    }
                }
            }
        }
        None
    }

    /// Check if a header name is one we should scan for placeholders.
    pub fn should_scan_header(&self, header_name: &str) -> bool {
        let lower = header_name.to_lowercase();
        self.header_patterns.iter().any(|p| lower == *p)
    }

    /// Get the list of all placeholder tokens (for logging/debugging).
    /// Does NOT expose real credential values.
    pub fn placeholder_tokens(&self) -> Vec<String> {
        self.placeholder_map.keys().cloned().collect()
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Map a credential name to its keychain service and key.
fn credential_source_for_name(name: &str) -> (&str, &str) {
    match name {
        "claude_api" | "claude" | "anthropic" => {
            ("com.qontinui.runner.ai", "claude_api_key")
        }
        "openai" => ("com.qontinui.runner.ai", "openai_api_key"),
        "gemini" | "google" => ("com.qontinui.runner.gemini", "gemini_api_key"),
        _ => ("com.qontinui.runner.ai", name),
    }
}

/// Map a credential name to the standard environment variable name.
fn env_var_name_for_credential(name: &str) -> String {
    match name {
        "claude_api" | "claude" | "anthropic" => "ANTHROPIC_API_KEY".to_string(),
        "openai" => "OPENAI_API_KEY".to_string(),
        "gemini" | "google" => "GEMINI_API_KEY".to_string(),
        _ => format!("{}_API_KEY", name.to_uppercase()),
    }
}

/// Resolve a credential from its source.
fn resolve_credential(source: &CredentialSource) -> Option<String> {
    match source {
        CredentialSource::Keychain { service, key } => {
            // Use the keyring crate to retrieve from OS keychain
            match keyring::Entry::new(service, key) {
                Ok(entry) => match entry.get_password() {
                    Ok(value) => {
                        debug!("Credential proxy: resolved credential from keychain");
                        Some(value)
                    }
                    Err(e) => {
                        warn!(
                            "Credential proxy: keychain retrieval failed for {}/{}: {}",
                            service, key, e
                        );
                        None
                    }
                },
                Err(e) => {
                    warn!("Credential proxy: keyring entry error: {}", e);
                    None
                }
            }
        }
        CredentialSource::Environment { var_name } => {
            std::env::var(var_name).ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_placeholder_generation() {
        let proxy = CredentialProxy::new(&["claude_api", "openai"]);

        let env_vars = proxy.placeholder_env_vars();
        assert_eq!(env_vars.len(), 2);

        // Check that env vars contain placeholder tokens, not real keys
        for var in &env_vars {
            assert!(var.contains("QCRED_"), "Env var should contain placeholder token");
            let (name, value) = var.split_once('=').unwrap();
            assert!(
                name == "ANTHROPIC_API_KEY" || name == "OPENAI_API_KEY",
                "Unexpected env var name: {}",
                name
            );
            assert!(
                value.starts_with("QCRED_"),
                "Value should be a placeholder"
            );
        }
    }

    #[test]
    fn test_should_scan_header() {
        let proxy = CredentialProxy::new(&[]);

        assert!(proxy.should_scan_header("Authorization"));
        assert!(proxy.should_scan_header("x-api-key"));
        assert!(proxy.should_scan_header("Api-Key"));
        assert!(!proxy.should_scan_header("Content-Type"));
        assert!(!proxy.should_scan_header("Host"));
    }

    #[test]
    fn test_placeholder_tokens_dont_expose_real_values() {
        let proxy = CredentialProxy::new(&["claude_api"]);
        let tokens = proxy.placeholder_tokens();
        assert_eq!(tokens.len(), 1);
        assert!(tokens[0].starts_with("QCRED_"));
    }
}
