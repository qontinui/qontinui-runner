//! Agent sandboxing and security.
//!
//! This module implements the layered security system for workflow step execution,
//! following the Agent Sandbox Taxonomy (AST) 7-layer defense model:
//!
//! - **L1/L2 Compute & Resources**: Container hardening (cap_drop, seccomp, PID limits)
//! - **L3 Filesystem**: Path-scoped access with read-only root filesystem
//! - **L4 Network**: Domain-filtering forward proxy with allowlist/denylist
//! - **L5 Credentials**: Placeholder injection with host-side proxy substitution
//! - **L6 Actions**: Command blocklisting, destructive operation prevention
//! - **L7 Audit**: Security event logging with database persistence
//!
//! # Architecture
//!
//! The security system intercepts at the `HandlerContext` boundary. Every step
//! handler receives a resolved [`SecurityPolicy`] that controls what the step
//! can do. The [`PolicyEngine`] resolves the effective policy from:
//!
//! 1. Step-level overrides (future)
//! 2. Workflow-level overrides
//! 3. Settings-level default profile
//! 4. Built-in "permissive" profile (backward-compatible default)
//!
//! # Modules
//!
//! - [`policy`] — Core security policy types and enums
//! - [`profiles`] — Built-in profiles (permissive, standard, strict)
//! - [`engine`] — Policy resolution and action evaluation

pub mod audit;
pub mod credential_proxy;
pub mod engine;
pub mod network_proxy;
pub mod policy;
pub mod profiles;

// Re-export key types for ergonomic use
pub use engine::{PolicyDenial, PolicyEngine, SecuritySettings};
pub use policy::SecurityPolicy;

use crate::container::container_config::ContainerConfig;
use policy::{CredentialMode, NetworkMode};

// Embedded default seccomp profile for strict/standard modes.
// Blocks dangerous syscalls: mount, ptrace, reboot, sethostname, etc.
const SECCOMP_DEFAULT_PROFILE: &str = include_str!("seccomp_default.json");

/// Translate a [`SecurityPolicy`] into concrete Docker [`ContainerConfig`] settings.
///
/// This bridges the abstract policy model to the Docker execution layer.
/// The `base` config provides user-configured values (image, enabled flag, etc.)
/// that are preserved; security fields are overridden from the policy.
pub fn policy_to_container_config(
    policy: &SecurityPolicy,
    base: &ContainerConfig,
) -> ContainerConfig {
    let mut config = base.clone();

    // Apply resource limits from policy (override base if policy specifies them)
    if let Some(cpu) = policy.resources.cpu_limit {
        config.cpu_limit = Some(cpu);
    }
    if let Some(mem) = policy.resources.memory_limit_mb {
        config.memory_limit_mb = Some(mem);
    }
    if let Some(pids) = policy.resources.pids_limit {
        config.pids_limit = Some(pids);
    }
    config.timeout_secs = policy.resources.timeout_secs;

    // Apply filesystem policy
    config.read_only_rootfs = policy.filesystem.read_only_root;

    // Apply network policy
    match policy.network.mode {
        NetworkMode::Disabled => {
            config.network_enabled = false;
        }
        NetworkMode::AllowList | NetworkMode::DenyList => {
            // Network mediation proxy will handle filtering;
            // container needs network access to reach the proxy
            config.network_enabled = true;
        }
        NetworkMode::Unrestricted => {
            config.network_enabled = true;
        }
    }

    // Apply security hardening based on profile
    match policy.profile_name.as_str() {
        "strict" => {
            config.cap_drop = vec!["ALL".to_string()];
            config.cap_add = vec![]; // No capabilities in strict mode
            config.security_opt = vec![
                "no-new-privileges".to_string(),
                format!("seccomp={}", SECCOMP_DEFAULT_PROFILE),
            ];
            config.user = Some("1000:1000".to_string());
            config.read_only_rootfs = true;
        }
        "standard" => {
            config.cap_drop = vec!["ALL".to_string()];
            // Standard mode allows a minimal set of capabilities needed for typical operations
            config.cap_add = vec![
                "NET_BIND_SERVICE".to_string(), // Bind to privileged ports
            ];
            config.security_opt = vec!["no-new-privileges".to_string()];
            config.user = Some("1000:1000".to_string());
        }
        "permissive" => {
            // Permissive: don't override base config security settings.
            // User may have manually configured cap_drop etc. in ContainerSettings.
        }
        _ => {
            // Custom or unknown profiles: apply resource limits from policy
            // but don't override security hardening from base config.
            // This preserves any manual cap_drop/seccomp settings the user configured.
        }
    }

    config
}

/// Build extra environment variables for container security features.
///
/// Returns env vars for:
/// - HTTP_PROXY/HTTPS_PROXY when network proxy is enabled (filled by caller with proxy URL)
/// - Credential placeholders when credential proxy is enabled
pub fn build_container_security_env(
    policy: &SecurityPolicy,
    settings: &engine::SecuritySettings,
) -> Vec<String> {
    let mut env = Vec::new();

    // Credential placeholders
    if settings.credential_proxy_enabled
        && policy.credentials.mode == policy::CredentialMode::Proxy
    {
        let cred_names: Vec<&str> = if policy.credentials.allowed_credentials.is_empty() {
            vec!["claude_api", "openai", "gemini"]
        } else {
            policy
                .credentials
                .allowed_credentials
                .iter()
                .map(|s| s.as_str())
                .collect()
        };
        let cred_proxy = credential_proxy::CredentialProxy::new(&cred_names);
        env.extend(cred_proxy.placeholder_env_vars());
    }

    env
}
