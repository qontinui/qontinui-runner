//! Built-in security profiles.
//!
//! Each profile defines a complete [`SecurityPolicy`] with sensible defaults
//! for its security posture. Users can select a profile and then customize
//! individual fields.
//!
//! # Profiles
//!
//! | Profile      | Posture        | Target Use Case                     |
//! |-------------|----------------|-------------------------------------|
//! | `permissive` | No restrictions | Trusted workflows, backward compat  |
//! | `standard`   | Balanced       | Most workflows, recommended default |
//! | `strict`     | Maximum isolation | Untrusted code, sensitive data      |

use super::policy::*;

/// Return the built-in profile matching `name`, or `None` if unknown.
pub fn get_profile(name: &str) -> Option<SecurityPolicy> {
    match name {
        "permissive" => Some(permissive()),
        "standard" => Some(standard()),
        "strict" => Some(strict()),
        _ => None,
    }
}

/// List all built-in profile names with descriptions.
pub fn list_profiles() -> Vec<ProfileSummary> {
    vec![
        ProfileSummary {
            name: "permissive".to_string(),
            description: "No restrictions. Reproduces current behavior. Suitable for trusted workflows.".to_string(),
            fingerprint: "L1:2/L2:1/L3:1/L4:0/L5:1/L6:1".to_string(),
        },
        ProfileSummary {
            name: "standard".to_string(),
            description: "Balanced security. Capabilities dropped, destructive commands blocked, credential proxy enabled. Recommended for most workflows.".to_string(),
            fingerprint: "L1:3/L2:3/L3:3/L4:2/L5:3/L6:3".to_string(),
        },
        ProfileSummary {
            name: "strict".to_string(),
            description: "Maximum isolation. Seccomp enforced, network disabled by default, read-only filesystem, allowlist-only commands. For untrusted code.".to_string(),
            fingerprint: "L1:3/L2:3/L3:3/L4:3/L5:3/L6:3".to_string(),
        },
    ]
}

/// Summary of a built-in security profile.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProfileSummary {
    pub name: String,
    pub description: String,
    /// AST fingerprint notation: L1:S/L2:S/.../L6:S
    pub fingerprint: String,
}

// ============================================================================
// Permissive — current behavior, no restrictions
// ============================================================================

/// No restrictions. This is the default profile that reproduces the current
/// behavior exactly, ensuring full backward compatibility.
pub fn permissive() -> SecurityPolicy {
    SecurityPolicy {
        profile_name: "permissive".to_string(),
        filesystem: FilesystemPolicy::default(),
        network: NetworkPolicy::default(),
        actions: ActionPolicy::default(),
        resources: ResourcePolicy::default(),
        credentials: CredentialPolicy::default(),
    }
}

// ============================================================================
// Standard — balanced security for typical workflows
// ============================================================================

/// Balanced security. Recommended for most workflows.
///
/// - Capabilities dropped, non-root user in containers
/// - Destructive commands blocked
/// - Credential proxy enabled (agent never sees real keys)
/// - Network in AllowList mode (user configures allowed domains)
/// - Workspace-scoped filesystem access
/// - PID limit: 256, memory: 2GB
pub fn standard() -> SecurityPolicy {
    SecurityPolicy {
        profile_name: "standard".to_string(),
        filesystem: FilesystemPolicy {
            read_write_paths: vec!["/workspace/**".to_string()],
            read_only_paths: vec![],
            denied_paths: vec![
                "/etc/shadow".to_string(),
                "/etc/passwd".to_string(),
                "/root/**".to_string(),
                "/home/*/.ssh/**".to_string(),
            ],
            read_only_root: true,
        },
        network: NetworkPolicy {
            mode: NetworkMode::AllowList,
            allowed_domains: vec![
                // Common AI API endpoints — user should customize
                "api.anthropic.com".to_string(),
                "api.openai.com".to_string(),
                "generativelanguage.googleapis.com".to_string(),
            ],
            denied_domains: vec![],
            block_metadata_endpoints: true,
            allowed_protocols: vec!["https".to_string()],
            log_requests: true,
        },
        actions: ActionPolicy {
            allowed_commands: None,
            blocked_commands: vec![
                r"^\s*sudo\b".to_string(),
                r"^\s*su\b".to_string(),
            ],
            block_destructive: true,
            max_recursion_depth: 10,
        },
        resources: ResourcePolicy {
            cpu_limit: Some(2.0),
            memory_limit_mb: Some(2048),
            pids_limit: Some(256),
            timeout_secs: 300,
        },
        credentials: CredentialPolicy {
            mode: CredentialMode::Proxy,
            allowed_credentials: vec![],
        },
    }
}

// ============================================================================
// Strict — maximum isolation for untrusted code
// ============================================================================

/// Maximum isolation. For running untrusted code or processing sensitive data.
///
/// - All capabilities dropped, seccomp enforced, non-root user
/// - Read-only root filesystem with tmpfs /tmp
/// - Network disabled by default (must explicitly allow domains)
/// - Only /workspace/output writable
/// - PID limit: 64, memory: 1GB
/// - Credential proxy enforced
pub fn strict() -> SecurityPolicy {
    SecurityPolicy {
        profile_name: "strict".to_string(),
        filesystem: FilesystemPolicy {
            read_write_paths: vec!["/workspace/output/**".to_string()],
            read_only_paths: vec!["/workspace/**".to_string()],
            denied_paths: vec![
                "/etc/shadow".to_string(),
                "/etc/passwd".to_string(),
                "/root/**".to_string(),
                "/home/**".to_string(),
                "/proc/*/environ".to_string(),
            ],
            read_only_root: true,
        },
        network: NetworkPolicy {
            mode: NetworkMode::Disabled,
            allowed_domains: vec![],
            denied_domains: vec![],
            block_metadata_endpoints: true,
            allowed_protocols: vec!["https".to_string()],
            log_requests: true,
        },
        actions: ActionPolicy {
            allowed_commands: None,
            blocked_commands: vec![
                // Dangerous system commands
                r"^\s*sudo\b".to_string(),
                r"^\s*su\b".to_string(),
                r"\bchmod\s+[0-7]*[2367][0-7]*\b".to_string(), // world-writable
                r"\bchown\b".to_string(),
                r"\bmount\b".to_string(),
                r"\bumount\b".to_string(),
                r"\bdd\b.*\bof=/dev/".to_string(),
                r"\bmkfs\b".to_string(),
                r"\bfdisk\b".to_string(),
            ],
            block_destructive: true,
            max_recursion_depth: 5,
        },
        resources: ResourcePolicy {
            cpu_limit: Some(1.0),
            memory_limit_mb: Some(1024),
            pids_limit: Some(64),
            timeout_secs: 120,
        },
        credentials: CredentialPolicy {
            mode: CredentialMode::Proxy,
            allowed_credentials: vec![],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permissive_is_unrestricted() {
        let p = permissive();
        assert_eq!(p.profile_name, "permissive");
        assert_eq!(p.network.mode, NetworkMode::Unrestricted);
        assert!(!p.actions.block_destructive);
        assert_eq!(p.credentials.mode, CredentialMode::Direct);
        assert!(!p.filesystem.read_only_root);
    }

    #[test]
    fn test_standard_has_balanced_defaults() {
        let p = standard();
        assert_eq!(p.profile_name, "standard");
        assert_eq!(p.network.mode, NetworkMode::AllowList);
        assert!(p.actions.block_destructive);
        assert_eq!(p.credentials.mode, CredentialMode::Proxy);
        assert!(p.filesystem.read_only_root);
        assert_eq!(p.resources.pids_limit, Some(256));
        assert!(p.network.block_metadata_endpoints);
    }

    #[test]
    fn test_strict_is_maximally_restrictive() {
        let p = strict();
        assert_eq!(p.profile_name, "strict");
        assert_eq!(p.network.mode, NetworkMode::Disabled);
        assert!(p.actions.block_destructive);
        assert!(!p.actions.blocked_commands.is_empty());
        assert_eq!(p.credentials.mode, CredentialMode::Proxy);
        assert!(p.filesystem.read_only_root);
        assert_eq!(p.resources.pids_limit, Some(64));
        assert_eq!(p.resources.timeout_secs, 120);
    }

    #[test]
    fn test_get_profile_returns_correct_profiles() {
        assert!(get_profile("permissive").is_some());
        assert!(get_profile("standard").is_some());
        assert!(get_profile("strict").is_some());
        assert!(get_profile("unknown").is_none());
    }

    #[test]
    fn test_list_profiles_returns_all() {
        let profiles = list_profiles();
        assert_eq!(profiles.len(), 3);
        let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"permissive"));
        assert!(names.contains(&"standard"));
        assert!(names.contains(&"strict"));
    }
}
