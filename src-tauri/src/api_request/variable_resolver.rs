//! Variable resolution for API requests
//!
//! Handles {{variable}} substitution in URLs, headers, and request bodies.

use regex::Regex;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Variable resolver for API requests
#[derive(Debug, Clone)]
pub struct VariableResolver {
    /// Storage for variables extracted from responses
    variables: Arc<RwLock<HashMap<String, String>>>,
}

impl Default for VariableResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl VariableResolver {
    /// Create a new variable resolver
    pub fn new() -> Self {
        Self {
            variables: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a resolver with initial variables
    pub fn with_variables(variables: HashMap<String, String>) -> Self {
        Self {
            variables: Arc::new(RwLock::new(variables)),
        }
    }

    /// Set a variable value
    pub fn set(&self, name: &str, value: &str) {
        if let Ok(mut vars) = self.variables.write() {
            vars.insert(name.to_string(), value.to_string());
        }
    }

    /// Get a variable value
    pub fn get(&self, name: &str) -> Option<String> {
        self.variables.read().ok()?.get(name).cloned()
    }

    /// Get all variables
    pub fn get_all(&self) -> HashMap<String, String> {
        self.variables
            .read()
            .ok()
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Clear all variables
    pub fn clear(&self) {
        if let Ok(mut vars) = self.variables.write() {
            vars.clear();
        }
    }

    /// Resolve variables in a string
    ///
    /// Replaces {{variable_name}} with the stored value.
    /// Unknown variables are left as-is.
    pub fn resolve(&self, input: &str) -> String {
        let re = Regex::new(r"\{\{(\w+)\}\}").unwrap();
        let vars = crate::safe_lock::safe_read_or_recover(&self.variables, "variables");

        re.replace_all(input, |caps: &regex::Captures| {
            let var_name = &caps[1];
            vars.get(var_name)
                .cloned()
                .unwrap_or_else(|| caps[0].to_string())
        })
        .to_string()
    }

    /// Resolve variables in all string values of a HashMap
    pub fn resolve_map(&self, input: &HashMap<String, String>) -> HashMap<String, String> {
        input
            .iter()
            .map(|(k, v)| (k.clone(), self.resolve(v)))
            .collect()
    }

    /// Check if a string contains any unresolved variables
    pub fn has_unresolved(&self, input: &str) -> bool {
        let re = Regex::new(r"\{\{(\w+)\}\}").unwrap();
        let vars = crate::safe_lock::safe_read_or_recover(&self.variables, "variables");

        for caps in re.captures_iter(input) {
            let var_name = &caps[1];
            if !vars.contains_key(var_name) {
                return true;
            }
        }
        false
    }

    /// Get list of unresolved variable names in a string
    pub fn get_unresolved(&self, input: &str) -> Vec<String> {
        let re = Regex::new(r"\{\{(\w+)\}\}").unwrap();
        let vars = crate::safe_lock::safe_read_or_recover(&self.variables, "variables");
        let mut unresolved = Vec::new();

        for caps in re.captures_iter(input) {
            let var_name = caps[1].to_string();
            if !vars.contains_key(&var_name) && !unresolved.contains(&var_name) {
                unresolved.push(var_name);
            }
        }
        unresolved
    }

    /// Extract variable names from a string (regardless of resolution status)
    pub fn extract_variable_names(input: &str) -> Vec<String> {
        let re = Regex::new(r"\{\{(\w+)\}\}").unwrap();
        let mut names = Vec::new();

        for caps in re.captures_iter(input) {
            let var_name = caps[1].to_string();
            if !names.contains(&var_name) {
                names.push(var_name);
            }
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_simple() {
        let resolver = VariableResolver::new();
        resolver.set("name", "John");
        resolver.set("id", "123");

        assert_eq!(
            resolver.resolve("Hello {{name}}, your ID is {{id}}"),
            "Hello John, your ID is 123"
        );
    }

    #[test]
    fn test_resolve_unknown_variable() {
        let resolver = VariableResolver::new();
        resolver.set("name", "John");

        // Unknown variables are left as-is
        assert_eq!(
            resolver.resolve("Hello {{name}}, {{unknown}}"),
            "Hello John, {{unknown}}"
        );
    }

    #[test]
    fn test_resolve_url() {
        let resolver = VariableResolver::new();
        resolver.set("base_url", "https://api.example.com");
        resolver.set("user_id", "42");

        assert_eq!(
            resolver.resolve("{{base_url}}/users/{{user_id}}/profile"),
            "https://api.example.com/users/42/profile"
        );
    }

    #[test]
    fn test_has_unresolved() {
        let resolver = VariableResolver::new();
        resolver.set("name", "John");

        assert!(!resolver.has_unresolved("Hello {{name}}"));
        assert!(resolver.has_unresolved("Hello {{unknown}}"));
        assert!(resolver.has_unresolved("{{name}} and {{unknown}}"));
    }

    #[test]
    fn test_get_unresolved() {
        let resolver = VariableResolver::new();
        resolver.set("name", "John");

        let unresolved = resolver.get_unresolved("{{name}}, {{foo}}, {{bar}}, {{foo}}");
        assert_eq!(unresolved.len(), 2);
        assert!(unresolved.contains(&"foo".to_string()));
        assert!(unresolved.contains(&"bar".to_string()));
    }

    #[test]
    fn test_extract_variable_names() {
        let names = VariableResolver::extract_variable_names(
            "URL: {{base_url}}/{{resource}}/{{id}}?token={{token}}",
        );
        assert_eq!(names.len(), 4);
        assert!(names.contains(&"base_url".to_string()));
        assert!(names.contains(&"resource".to_string()));
        assert!(names.contains(&"id".to_string()));
        assert!(names.contains(&"token".to_string()));
    }
}
