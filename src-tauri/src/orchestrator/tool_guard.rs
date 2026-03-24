//! Tool Guard Module
//!
//! Enforces tool whitelists for agent roles. When a role defines an
//! `allowed_tools` list, the guard rejects any tool invocation not in
//! that list.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ============================================================================
// Error Type
// ============================================================================

/// Error returned when a tool invocation is blocked by the guard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolGuardError {
    /// Name of the tool that was blocked.
    pub tool_name: String,
    /// Role that attempted to use the tool.
    pub role_id: String,
    /// Tools the role is allowed to use.
    pub allowed_tools: Vec<String>,
    /// Human-readable error message.
    pub message: String,
}

impl std::fmt::Display for ToolGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ToolGuardError {}

// ============================================================================
// Guard
// ============================================================================

/// Enforces a per-role tool whitelist.
///
/// An empty `allowed_tools` set means the role is unrestricted and any tool
/// may be called.
pub struct ToolGuard {
    allowed_tools: HashSet<String>,
    role_id: String,
}

impl ToolGuard {
    /// Create a new tool guard for the given role and whitelist.
    pub fn new(role_id: &str, allowed_tools: &[String]) -> Self {
        Self {
            allowed_tools: allowed_tools.iter().cloned().collect(),
            role_id: role_id.to_string(),
        }
    }

    /// Returns `Ok(())` if the tool is allowed, or `Err` with a message
    /// suggesting the right role.
    pub fn check_tool(&self, tool_name: &str) -> Result<(), ToolGuardError> {
        if self.allowed_tools.is_empty() || self.allowed_tools.contains(tool_name) {
            Ok(())
        } else {
            Err(ToolGuardError {
                tool_name: tool_name.to_string(),
                role_id: self.role_id.clone(),
                allowed_tools: self.allowed_tools.iter().cloned().collect(),
                message: format!(
                    "Tool '{}' is not allowed for the '{}' role. Allowed tools: {}. \
                     Consider using a different agent role.",
                    tool_name,
                    self.role_id,
                    self.allowed_tools
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            })
        }
    }

    /// Returns `true` if this guard imposes no restrictions.
    pub fn is_unrestricted(&self) -> bool {
        self.allowed_tools.is_empty()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unrestricted_guard() {
        let guard = ToolGuard::new("any_role", &[]);
        assert!(guard.is_unrestricted());
        assert!(guard.check_tool("anything").is_ok());
    }

    #[test]
    fn test_allowed_tool_passes() {
        let guard = ToolGuard::new("analyzer", &["get_state".to_string(), "query_rag".to_string()]);
        assert!(!guard.is_unrestricted());
        assert!(guard.check_tool("get_state").is_ok());
        assert!(guard.check_tool("query_rag").is_ok());
    }

    #[test]
    fn test_disallowed_tool_blocked() {
        let guard = ToolGuard::new("analyzer", &["get_state".to_string()]);
        let result = guard.check_tool("execute_shell_command");
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert_eq!(err.tool_name, "execute_shell_command");
        assert_eq!(err.role_id, "analyzer");
        assert!(err.message.contains("not allowed"));
    }

    #[test]
    fn test_error_display() {
        let err = ToolGuardError {
            tool_name: "rm".to_string(),
            role_id: "analyzer".to_string(),
            allowed_tools: vec!["get_state".to_string()],
            message: "Tool 'rm' is not allowed".to_string(),
        };
        assert_eq!(format!("{}", err), "Tool 'rm' is not allowed");
    }
}
