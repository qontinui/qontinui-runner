//! Security policy types for agent sandboxing.
//!
//! Defines the capability-based permission model used to control what workflow
//! steps can do. Policies are resolved per-step by the [`PolicyEngine`] and
//! enforced by container hardening, network mediation, and credential proxying.

use serde::{Deserialize, Serialize};

// ============================================================================
// Top-level SecurityPolicy
// ============================================================================

/// A resolved security policy for one execution unit (step or workflow).
///
/// Each sub-policy controls a different defense layer, following the
/// Agent Sandbox Taxonomy (AST) 7-layer model:
///
/// - L1/L2: `resources` (compute isolation, resource limits)
/// - L3: `filesystem` (filesystem boundary)
/// - L4: `network` (network boundary)
/// - L5: `credentials` (credential management)
/// - L6: `actions` (action governance)
/// - L7: Audit logging (handled by [`AuditLogger`], not a policy field)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityPolicy {
    /// Name of the profile this policy was derived from.
    pub profile_name: String,
    /// Filesystem access controls.
    pub filesystem: FilesystemPolicy,
    /// Network access controls.
    pub network: NetworkPolicy,
    /// Command and action governance.
    pub actions: ActionPolicy,
    /// Compute resource limits.
    pub resources: ResourcePolicy,
    /// Credential access and isolation mode.
    pub credentials: CredentialPolicy,
}

// ============================================================================
// Filesystem Policy (L3)
// ============================================================================

/// Controls which filesystem paths the execution context can access.
///
/// Paths use glob patterns and are evaluated in order:
/// denied > read_only > read_write.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FilesystemPolicy {
    /// Glob patterns for paths with read-write access.
    #[serde(default)]
    pub read_write_paths: Vec<String>,
    /// Glob patterns for paths with read-only access.
    #[serde(default)]
    pub read_only_paths: Vec<String>,
    /// Glob patterns for paths that are completely denied.
    #[serde(default)]
    pub denied_paths: Vec<String>,
    /// If true, the container root filesystem is mounted read-only.
    /// Writable tmpfs mounts are provided for /tmp and /var/tmp.
    #[serde(default)]
    pub read_only_root: bool,
}

// ============================================================================
// Network Policy (L4)
// ============================================================================

/// Controls how the execution context can access the network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicy {
    /// Network access mode.
    #[serde(default)]
    pub mode: NetworkMode,
    /// Domains allowed when `mode` is `AllowList`.
    /// Supports exact match and wildcard prefix (e.g., `*.anthropic.com`).
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Domains denied when `mode` is `DenyList`.
    #[serde(default)]
    pub denied_domains: Vec<String>,
    /// Block cloud metadata endpoints (169.254.169.254, fd00:ec2::254, etc.).
    /// Enabled by default in all profiles except permissive.
    #[serde(default)]
    pub block_metadata_endpoints: bool,
    /// Allowed protocols. Empty means all protocols allowed.
    #[serde(default)]
    pub allowed_protocols: Vec<String>,
    /// Log all network requests to the security audit trail.
    #[serde(default)]
    pub log_requests: bool,
}

/// Network access mode controlling the default posture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum NetworkMode {
    /// All network access is blocked.
    Disabled,
    /// Only explicitly listed domains are allowed (default deny).
    AllowList,
    /// All domains except explicitly listed ones are allowed (default allow).
    DenyList,
    /// No network restrictions. Current behavior.
    #[default]
    Unrestricted,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            mode: NetworkMode::Unrestricted,
            allowed_domains: vec![],
            denied_domains: vec![],
            block_metadata_endpoints: false,
            allowed_protocols: vec![],
            log_requests: false,
        }
    }
}

// ============================================================================
// Action Policy (L6)
// ============================================================================

/// Controls which commands and actions the execution context may perform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionPolicy {
    /// If `Some`, only these commands are allowed (allowlist mode).
    /// If `None`, all commands are allowed unless blocked.
    #[serde(default)]
    pub allowed_commands: Option<Vec<String>>,
    /// Regex patterns for commands that are always blocked.
    #[serde(default)]
    pub blocked_commands: Vec<String>,
    /// Block known destructive operations (rm -rf /, DROP TABLE, etc.).
    #[serde(default)]
    pub block_destructive: bool,
    /// Maximum workflow recursion depth.
    #[serde(default = "default_max_recursion")]
    pub max_recursion_depth: u32,
}

fn default_max_recursion() -> u32 {
    10
}

impl Default for ActionPolicy {
    fn default() -> Self {
        Self {
            allowed_commands: None,
            blocked_commands: vec![],
            block_destructive: false,
            max_recursion_depth: default_max_recursion(),
        }
    }
}

// ============================================================================
// Resource Policy (L1/L2)
// ============================================================================

/// Compute resource limits for the execution context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcePolicy {
    /// CPU limit in cores (e.g., 1.0 = 1 core).
    #[serde(default)]
    pub cpu_limit: Option<f64>,
    /// Memory limit in megabytes.
    #[serde(default)]
    pub memory_limit_mb: Option<u64>,
    /// Maximum number of processes (PID limit).
    #[serde(default)]
    pub pids_limit: Option<i64>,
    /// Execution timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_timeout() -> u64 {
    300
}

impl Default for ResourcePolicy {
    fn default() -> Self {
        Self {
            cpu_limit: None,
            memory_limit_mb: None,
            pids_limit: None,
            timeout_secs: default_timeout(),
        }
    }
}

// ============================================================================
// Credential Policy (L5)
// ============================================================================

/// Controls how credentials are made available to the execution context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialPolicy {
    /// Credential access mode.
    #[serde(default)]
    pub mode: CredentialMode,
    /// Names of credentials the execution context is allowed to use.
    /// Empty means all available credentials.
    #[serde(default)]
    pub allowed_credentials: Vec<String>,
}

/// How credentials are provided to agent processes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum CredentialMode {
    /// Credentials are passed directly as env vars/headers (current behavior).
    #[default]
    Direct,
    /// Credentials are replaced with placeholders; a host-side proxy injects
    /// real values into outbound requests. Agent never sees real keys.
    Proxy,
    /// No credentials are provided. Steps that need credentials will fail.
    Denied,
}

impl Default for CredentialPolicy {
    fn default() -> Self {
        Self {
            mode: CredentialMode::Direct,
            allowed_credentials: vec![],
        }
    }
}

// ============================================================================
// Convenience constructors
// ============================================================================

impl SecurityPolicy {
    /// Create a permissive policy (no restrictions, current behavior).
    pub fn permissive() -> Self {
        Self {
            profile_name: "permissive".to_string(),
            filesystem: FilesystemPolicy::default(),
            network: NetworkPolicy::default(),
            actions: ActionPolicy::default(),
            resources: ResourcePolicy::default(),
            credentials: CredentialPolicy::default(),
        }
    }
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        Self::permissive()
    }
}

// ============================================================================
// Blueprint tool-policy → ActionPolicy mirror (Phase 3b)
// ============================================================================

/// Overlay a node's command-arg-scoped denies onto a base [`ActionPolicy`] so
/// the same forbidden command is also blocked if the node runs it through a
/// runner-executed command/check step (not the CLI's `Bash` tool).
///
/// Synthesizes a minimal `ActionPolicy` when `base` is `None`, so the mirror is
/// never silently absent. For each [`DenyEntry::CommandScoped`] the command
/// prefix is appended to `blocked_commands` as a prefix-anchored regex
/// (`^<escaped-prefix>`). Tool-name denies and the allow-list are NOT mirrored
/// here — those are tool-layer concepts the CLI carrier owns; this builder only
/// mirrors command-argument-scoped denies onto the command-execution layer.
///
/// An existing `base` policy's blocks/allows are preserved and extended.
pub fn build_effective_action_policy(
    base: Option<&ActionPolicy>,
    policy: &crate::workflow::dag_schema::ToolPolicy,
) -> ActionPolicy {
    use crate::workflow::dag_schema::DenyEntry;

    let mut effective = base.cloned().unwrap_or_default();

    for entry in policy.classified_denies() {
        if let DenyEntry::CommandScoped { command_prefix, .. } = entry {
            let pattern = format!("^{}", regex::escape(&command_prefix));
            if !effective.blocked_commands.contains(&pattern) {
                effective.blocked_commands.push(pattern);
            }
        }
    }

    effective
}

#[cfg(test)]
mod blueprint_action_policy_tests {
    use super::*;
    use crate::workflow::dag_schema::{ToolPolicy, ViolationAction};
    use regex::Regex;

    fn tool_policy(allow: Option<Vec<&str>>, deny: Option<Vec<&str>>) -> ToolPolicy {
        ToolPolicy {
            allow: allow.map(|v| v.into_iter().map(String::from).collect()),
            deny: deny.map(|v| v.into_iter().map(String::from).collect()),
            on_violation: ViolationAction::default(),
        }
    }

    fn any_blocks(policy: &ActionPolicy, cmd: &str) -> bool {
        policy
            .blocked_commands
            .iter()
            .any(|p| Regex::new(p).map(|re| re.is_match(cmd)).unwrap_or(false))
    }

    // Plan-test (b): allow:[Bash] + deny:["Bash:git push --force"] ⇒
    // blocked_commands gains a "git push --force" pattern; plain "git status"
    // is NOT matched.
    #[test]
    fn command_scoped_deny_appends_prefix_anchored_block() {
        let tp = tool_policy(Some(vec!["Bash"]), Some(vec!["Bash:git push --force"]));
        let ap = build_effective_action_policy(None, &tp);
        assert!(
            any_blocks(&ap, "git push --force --tags"),
            "the forbidden prefix should be blocked"
        );
        assert!(
            !any_blocks(&ap, "git status"),
            "an unrelated git command must NOT be blocked"
        );
    }

    #[test]
    fn base_blocks_are_preserved_and_extended() {
        let base = ActionPolicy {
            blocked_commands: vec!["^sudo ".to_string()],
            ..ActionPolicy::default()
        };
        let tp = tool_policy(None, Some(vec!["Bash:rm -rf"]));
        let ap = build_effective_action_policy(Some(&base), &tp);
        assert!(any_blocks(&ap, "sudo reboot"), "base block preserved");
        assert!(any_blocks(&ap, "rm -rf /tmp/x"), "node deny appended");
        assert_eq!(ap.blocked_commands.len(), 2);
    }

    #[test]
    fn tool_name_denies_are_not_mirrored_to_commands() {
        // A bare tool-name deny (Write) has no command-prefix to mirror.
        let tp = tool_policy(None, Some(vec!["Write"]));
        let ap = build_effective_action_policy(None, &tp);
        assert!(ap.blocked_commands.is_empty());
    }

    #[test]
    fn regex_metachars_in_prefix_are_escaped() {
        // A prefix with regex metacharacters must match literally, not as regex.
        let tp = tool_policy(None, Some(vec!["Bash:echo a.b"]));
        let ap = build_effective_action_policy(None, &tp);
        assert!(any_blocks(&ap, "echo a.b"), "literal prefix matches");
        assert!(
            !any_blocks(&ap, "echo aXb"),
            "the '.' must be escaped, not a wildcard"
        );
    }
}
