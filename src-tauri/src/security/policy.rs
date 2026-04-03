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
