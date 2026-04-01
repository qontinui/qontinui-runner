//! Security policy engine.
//!
//! Resolves the effective [`SecurityPolicy`] for a step and evaluates
//! individual actions against the policy. This is the single source of truth
//! for all security decisions in the runner.

use super::policy::*;
use super::profiles;
use regex::Regex;
use std::collections::HashMap;
use std::fmt;
use std::sync::{LazyLock, Mutex};
use tracing::warn;

// ============================================================================
// Policy Denial
// ============================================================================

/// A security policy violation with machine-readable type and human-readable reason.
#[derive(Debug, Clone)]
pub struct PolicyDenial {
    pub denial_type: DenialType,
    pub action: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DenialType {
    CommandBlocked,
    DestructiveCommand,
    NetworkDenied,
    MetadataEndpoint,
    ProtocolDenied,
    FilesystemDenied,
    CredentialDenied,
    RecursionLimit,
}

use serde::{Deserialize, Serialize};

impl fmt::Display for PolicyDenial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{:?}] {}: {}", self.denial_type, self.action, self.reason)
    }
}

impl std::error::Error for PolicyDenial {}

// ============================================================================
// Settings struct for security configuration
// ============================================================================

/// Persistent security settings stored in settings.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecuritySettings {
    /// Default profile name: "permissive", "standard", "strict", or "custom".
    #[serde(default = "default_profile")]
    pub default_profile: String,
    /// Custom policy overrides (used when default_profile is "custom").
    #[serde(default)]
    pub custom_policy: Option<SecurityPolicy>,
    /// Enable security audit logging.
    #[serde(default)]
    pub audit_enabled: bool,
    /// Days to retain audit events before cleanup.
    #[serde(default = "default_retention_days")]
    pub audit_retention_days: u32,
    /// Enable credential proxy (placeholder injection).
    #[serde(default)]
    pub credential_proxy_enabled: bool,
    /// Enable network mediation proxy.
    #[serde(default)]
    pub network_proxy_enabled: bool,
}

fn default_profile() -> String {
    "permissive".to_string()
}

fn default_retention_days() -> u32 {
    30
}

impl Default for SecuritySettings {
    fn default() -> Self {
        Self {
            default_profile: default_profile(),
            custom_policy: None,
            audit_enabled: false,
            audit_retention_days: default_retention_days(),
            credential_proxy_enabled: false,
            network_proxy_enabled: false,
        }
    }
}

// ============================================================================
// Policy Engine
// ============================================================================

/// Resolves and evaluates security policies.
pub struct PolicyEngine;

impl PolicyEngine {
    /// Resolve the effective security policy for a step.
    ///
    /// Priority (highest to lowest):
    /// 1. Step-level policy override (future: stored in ExecutionStepConfig)
    /// 2. Workflow-level policy override
    /// 3. Settings-level default profile
    /// 4. Built-in "permissive" profile (absolute fallback)
    pub fn resolve(
        workflow_policy: Option<&SecurityPolicy>,
        settings: &SecuritySettings,
    ) -> SecurityPolicy {
        // Step-level overrides are a future extension point.
        // For now, workflow policy takes precedence over settings.

        if let Some(wf_policy) = workflow_policy {
            return wf_policy.clone();
        }

        // If custom profile is selected and a custom policy exists, use it
        if settings.default_profile == "custom" {
            if let Some(ref custom) = settings.custom_policy {
                return custom.clone();
            }
        }

        // Look up built-in profile
        profiles::get_profile(&settings.default_profile).unwrap_or_else(|| {
            warn!(
                "Unknown security profile '{}', falling back to permissive",
                settings.default_profile
            );
            SecurityPolicy::permissive()
        })
    }

    /// Evaluate whether a command is allowed under the given policy.
    pub fn evaluate_command(policy: &SecurityPolicy, command: &str) -> Result<(), PolicyDenial> {
        let ap = &policy.actions;

        // Check command allowlist (if set, only listed commands are allowed)
        if let Some(ref allowed) = ap.allowed_commands {
            let cmd_base = extract_command_base(command);
            if !allowed.iter().any(|a| cmd_base == a.as_str()) {
                return Err(PolicyDenial {
                    denial_type: DenialType::CommandBlocked,
                    action: command.to_string(),
                    reason: format!("Command '{}' is not in the allowed commands list", cmd_base),
                });
            }
        }

        // Check blocked command patterns (compiled and cached)
        for pattern in &ap.blocked_commands {
            if let Some(re) = get_or_compile_regex(pattern) {
                if re.is_match(command) {
                    return Err(PolicyDenial {
                        denial_type: DenialType::CommandBlocked,
                        action: command.to_string(),
                        reason: format!("Command matches blocked pattern: {}", pattern),
                    });
                }
            }
        }

        // Check destructive command patterns
        if ap.block_destructive && is_destructive_command(command) {
            return Err(PolicyDenial {
                denial_type: DenialType::DestructiveCommand,
                action: command.to_string(),
                reason: "Command appears destructive and block_destructive is enabled".to_string(),
            });
        }

        Ok(())
    }

    /// Evaluate whether a network request to a domain is allowed.
    pub fn evaluate_network(
        policy: &SecurityPolicy,
        domain: &str,
        protocol: &str,
    ) -> Result<(), PolicyDenial> {
        let np = &policy.network;

        // Always block metadata endpoints when configured
        if np.block_metadata_endpoints && is_metadata_endpoint(domain) {
            return Err(PolicyDenial {
                denial_type: DenialType::MetadataEndpoint,
                action: format!("{}://{}", protocol, domain),
                reason: "Cloud metadata endpoint is blocked".to_string(),
            });
        }

        // Check protocol restrictions
        if !np.allowed_protocols.is_empty()
            && !np.allowed_protocols.iter().any(|p| p.eq_ignore_ascii_case(protocol))
        {
            return Err(PolicyDenial {
                denial_type: DenialType::ProtocolDenied,
                action: format!("{}://{}", protocol, domain),
                reason: format!("Protocol '{}' is not in allowed protocols list", protocol),
            });
        }

        match np.mode {
            NetworkMode::Disabled => Err(PolicyDenial {
                denial_type: DenialType::NetworkDenied,
                action: format!("{}://{}", protocol, domain),
                reason: "Network access is disabled".to_string(),
            }),
            NetworkMode::AllowList => {
                if domain_matches_list(domain, &np.allowed_domains) {
                    Ok(())
                } else {
                    Err(PolicyDenial {
                        denial_type: DenialType::NetworkDenied,
                        action: format!("{}://{}", protocol, domain),
                        reason: format!("Domain '{}' is not in the allowed list", domain),
                    })
                }
            }
            NetworkMode::DenyList => {
                if domain_matches_list(domain, &np.denied_domains) {
                    Err(PolicyDenial {
                        denial_type: DenialType::NetworkDenied,
                        action: format!("{}://{}", protocol, domain),
                        reason: format!("Domain '{}' is in the denied list", domain),
                    })
                } else {
                    Ok(())
                }
            }
            NetworkMode::Unrestricted => Ok(()),
        }
    }

    /// Evaluate whether a filesystem access is allowed.
    pub fn evaluate_filesystem(
        policy: &SecurityPolicy,
        path: &str,
        write: bool,
    ) -> Result<(), PolicyDenial> {
        let fp = &policy.filesystem;

        // Check denied paths first (highest priority)
        if path_matches_list(path, &fp.denied_paths) {
            return Err(PolicyDenial {
                denial_type: DenialType::FilesystemDenied,
                action: path.to_string(),
                reason: "Path matches a denied path pattern".to_string(),
            });
        }

        // For writes, check read-only paths first, then read-write allowlist
        if write {
            // Explicitly read-only paths deny writes
            if !fp.read_only_paths.is_empty() && path_matches_list(path, &fp.read_only_paths) {
                // Check if it's also in read_write (read_write overrides read_only)
                if !path_matches_list(path, &fp.read_write_paths) {
                    return Err(PolicyDenial {
                        denial_type: DenialType::FilesystemDenied,
                        action: path.to_string(),
                        reason: "Write access denied: path is read-only".to_string(),
                    });
                }
            }

            // If read_write allowlist is set, path must be in it
            if !fp.read_write_paths.is_empty() && !path_matches_list(path, &fp.read_write_paths) {
                return Err(PolicyDenial {
                    denial_type: DenialType::FilesystemDenied,
                    action: path.to_string(),
                    reason: "Write access denied: path is not in the read-write list".to_string(),
                });
            }
        }

        // For reads, check read-only and read-write allowlists (if both are set,
        // path must be in at least one)
        if !write && !fp.read_only_paths.is_empty() && !fp.read_write_paths.is_empty() {
            if !path_matches_list(path, &fp.read_only_paths)
                && !path_matches_list(path, &fp.read_write_paths)
            {
                return Err(PolicyDenial {
                    denial_type: DenialType::FilesystemDenied,
                    action: path.to_string(),
                    reason: "Read access denied: path is not in any allowed paths list"
                        .to_string(),
                });
            }
        }

        Ok(())
    }

    /// Check if the recursion depth exceeds the policy limit.
    pub fn evaluate_recursion(policy: &SecurityPolicy, depth: u32) -> Result<(), PolicyDenial> {
        if depth >= policy.actions.max_recursion_depth {
            return Err(PolicyDenial {
                denial_type: DenialType::RecursionLimit,
                action: format!("recursion depth {}", depth),
                reason: format!(
                    "Recursion depth {} exceeds limit {}",
                    depth, policy.actions.max_recursion_depth
                ),
            });
        }
        Ok(())
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Extract the base command name (first word) from a shell command string.
fn extract_command_base(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or("")
}

/// Cached compiled regexes for destructive command detection.
static DESTRUCTIVE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    let patterns = [
        // File system destruction
        r"\brm\s+.*-[^\s]*r[^\s]*f",
        r"\brm\s+.*-[^\s]*f[^\s]*r",
        r"\brm\s+-rf\s+/\s*$",
        r"\bmkfs\b",
        r"\bdd\b.*\bof\s*=\s*/dev/",
        // Database destruction
        r"(?i)\bDROP\s+(TABLE|DATABASE|SCHEMA)\b",
        r"(?i)\bTRUNCATE\s+TABLE\b",
        r"(?i)\bDELETE\s+FROM\b.*\bWHERE\b.*\b1\s*=\s*1\b",
        // Git destruction
        r"\bgit\s+push\s+.*--force\b",
        r"\bgit\s+reset\s+--hard\b",
        r"\bgit\s+clean\s+.*-f",
        // System commands
        r"\bshutdown\b",
        r"\breboot\b",
        r"\binit\s+0\b",
        r"\bkillall\b",
        r"\bpkill\s+-9\b",
    ];
    patterns
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect()
});

/// Cache for compiled blocked_commands regexes (keyed by pattern string).
static BLOCKED_COMMAND_CACHE: LazyLock<Mutex<HashMap<String, Regex>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Get or compile a regex from the blocked command cache.
fn get_or_compile_regex(pattern: &str) -> Option<Regex> {
    let mut cache = BLOCKED_COMMAND_CACHE.lock().ok()?;
    if let Some(re) = cache.get(pattern) {
        return Some(re.clone());
    }
    match Regex::new(pattern) {
        Ok(re) => {
            cache.insert(pattern.to_string(), re.clone());
            Some(re)
        }
        Err(e) => {
            warn!("Invalid blocked command regex '{}': {}", pattern, e);
            None
        }
    }
}

/// Check if a command appears to be destructive.
fn is_destructive_command(command: &str) -> bool {
    let cmd = command.trim();
    DESTRUCTIVE_PATTERNS.iter().any(|re| re.is_match(cmd))
}

/// Check if a domain is a cloud metadata endpoint.
fn is_metadata_endpoint(domain: &str) -> bool {
    let metadata_addresses = [
        "169.254.169.254",       // AWS/GCP/Azure metadata
        "169.254.170.2",         // AWS ECS task metadata
        "fd00:ec2::254",         // AWS IPv6 metadata
        "metadata.google.internal",
        "metadata.goog",
    ];
    metadata_addresses.contains(&domain)
}

/// Check if a domain matches any pattern in a list.
/// Supports exact match and wildcard prefix (e.g., `*.example.com`).
fn domain_matches_list(domain: &str, patterns: &[String]) -> bool {
    let domain_lower = domain.to_lowercase();
    for pattern in patterns {
        let pat = pattern.to_lowercase();
        if pat.starts_with("*.") {
            // Wildcard match: *.example.com matches sub.example.com and example.com
            let suffix = &pat[1..]; // .example.com
            if domain_lower.ends_with(suffix) || domain_lower == pat[2..] {
                return true;
            }
        } else if domain_lower == pat {
            return true;
        }
    }
    false
}

/// Check if a path matches any glob pattern in a list.
/// Simple glob matching: supports `*` and `**`.
fn path_matches_list(path: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
        if glob_matches(pattern, path) {
            return true;
        }
    }
    false
}

/// Glob pattern matching for filesystem paths.
///
/// Supports:
/// - `**` — matches any number of path segments (including zero)
/// - `*` — matches any characters within a single path segment (no `/`)
/// - Exact match for literal segments
fn glob_matches(pattern: &str, path: &str) -> bool {
    glob_match_segments(
        &pattern.split('/').collect::<Vec<_>>(),
        &path.split('/').collect::<Vec<_>>(),
    )
}

/// Recursive segment-based glob matcher.
fn glob_match_segments(pattern_parts: &[&str], path_parts: &[&str]) -> bool {
    let mut pi = 0; // pattern index
    let mut si = 0; // path (subject) index

    // Track backtracking point for `**`
    let mut star_pi = None;
    let mut star_si = None;

    while si < path_parts.len() {
        if pi < pattern_parts.len() && pattern_parts[pi] == "**" {
            // `**` can match zero or more segments
            star_pi = Some(pi);
            star_si = Some(si);
            pi += 1; // try matching zero segments first
        } else if pi < pattern_parts.len() && segment_matches(pattern_parts[pi], path_parts[si]) {
            pi += 1;
            si += 1;
        } else if let (Some(spi), Some(ssi)) = (star_pi, star_si) {
            // Backtrack: let `**` consume one more segment
            let new_si = ssi + 1;
            star_si = Some(new_si);
            pi = spi + 1;
            si = new_si;
        } else {
            return false;
        }
    }

    // Consume trailing `**` patterns
    while pi < pattern_parts.len() && pattern_parts[pi] == "**" {
        pi += 1;
    }

    pi == pattern_parts.len()
}

/// Match a single path segment against a pattern segment.
/// `*` matches any characters within the segment.
fn segment_matches(pattern: &str, segment: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == segment;
    }
    // Simple wildcard: split on `*` and check prefix/suffix
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 2 {
        segment.starts_with(parts[0]) && segment.ends_with(parts[1])
    } else {
        // Multiple wildcards — fall back to character-by-character match
        wildcard_match(pattern, segment)
    }
}

/// Character-by-character wildcard match (within a single segment).
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (plen, tlen) = (p.len(), t.len());
    let mut dp = vec![vec![false; tlen + 1]; plen + 1];
    dp[0][0] = true;
    for i in 1..=plen {
        if p[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for i in 1..=plen {
        for j in 1..=tlen {
            if p[i - 1] == '*' {
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
            } else if p[i - 1] == t[j - 1] || p[i - 1] == '?' {
                dp[i][j] = dp[i - 1][j - 1];
            }
        }
    }
    dp[plen][tlen]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_default_is_permissive() {
        let settings = SecuritySettings::default();
        let policy = PolicyEngine::resolve(None, &settings);
        assert_eq!(policy.profile_name, "permissive");
        assert_eq!(policy.network.mode, NetworkMode::Unrestricted);
    }

    #[test]
    fn test_resolve_workflow_override() {
        let settings = SecuritySettings::default();
        let wf_policy = SecurityPolicy {
            profile_name: "workflow-custom".to_string(),
            ..SecurityPolicy::default()
        };
        let policy = PolicyEngine::resolve(Some(&wf_policy), &settings);
        assert_eq!(policy.profile_name, "workflow-custom");
    }

    #[test]
    fn test_resolve_settings_profile() {
        let settings = SecuritySettings {
            default_profile: "strict".to_string(),
            ..SecuritySettings::default()
        };
        let policy = PolicyEngine::resolve(None, &settings);
        assert_eq!(policy.profile_name, "strict");
    }

    #[test]
    fn test_resolve_custom_profile() {
        let custom = SecurityPolicy {
            profile_name: "custom".to_string(),
            ..SecurityPolicy::default()
        };
        let settings = SecuritySettings {
            default_profile: "custom".to_string(),
            custom_policy: Some(custom),
            ..SecuritySettings::default()
        };
        let policy = PolicyEngine::resolve(None, &settings);
        assert_eq!(policy.profile_name, "custom");
    }

    #[test]
    fn test_resolve_unknown_profile_falls_back() {
        let settings = SecuritySettings {
            default_profile: "nonexistent".to_string(),
            ..SecuritySettings::default()
        };
        let policy = PolicyEngine::resolve(None, &settings);
        assert_eq!(policy.profile_name, "permissive");
    }

    #[test]
    fn test_evaluate_command_permissive() {
        let policy = SecurityPolicy::permissive();
        assert!(PolicyEngine::evaluate_command(&policy, "rm -rf /tmp/test").is_ok());
    }

    #[test]
    fn test_evaluate_command_blocked_destructive() {
        let policy = SecurityPolicy {
            actions: ActionPolicy {
                block_destructive: true,
                ..ActionPolicy::default()
            },
            ..SecurityPolicy::default()
        };
        assert!(PolicyEngine::evaluate_command(&policy, "rm -rf /").is_err());
        assert!(PolicyEngine::evaluate_command(&policy, "DROP TABLE users").is_err());
        assert!(PolicyEngine::evaluate_command(&policy, "git push --force").is_err());
        // Non-destructive should still pass
        assert!(PolicyEngine::evaluate_command(&policy, "ls -la").is_ok());
        assert!(PolicyEngine::evaluate_command(&policy, "echo hello").is_ok());
    }

    #[test]
    fn test_evaluate_command_blocked_pattern() {
        let policy = SecurityPolicy {
            actions: ActionPolicy {
                blocked_commands: vec![r"^\s*sudo\b".to_string()],
                ..ActionPolicy::default()
            },
            ..SecurityPolicy::default()
        };
        assert!(PolicyEngine::evaluate_command(&policy, "sudo rm -rf /").is_err());
        assert!(PolicyEngine::evaluate_command(&policy, "ls -la").is_ok());
    }

    #[test]
    fn test_evaluate_command_allowlist() {
        let policy = SecurityPolicy {
            actions: ActionPolicy {
                allowed_commands: Some(vec!["ls".to_string(), "cat".to_string()]),
                ..ActionPolicy::default()
            },
            ..SecurityPolicy::default()
        };
        assert!(PolicyEngine::evaluate_command(&policy, "ls -la /tmp").is_ok());
        assert!(PolicyEngine::evaluate_command(&policy, "cat /etc/hosts").is_ok());
        assert!(PolicyEngine::evaluate_command(&policy, "rm file.txt").is_err());
    }

    #[test]
    fn test_evaluate_network_unrestricted() {
        let policy = SecurityPolicy::permissive();
        assert!(PolicyEngine::evaluate_network(&policy, "evil.com", "http").is_ok());
    }

    #[test]
    fn test_evaluate_network_disabled() {
        let policy = SecurityPolicy {
            network: NetworkPolicy {
                mode: NetworkMode::Disabled,
                ..NetworkPolicy::default()
            },
            ..SecurityPolicy::default()
        };
        assert!(PolicyEngine::evaluate_network(&policy, "api.anthropic.com", "https").is_err());
    }

    #[test]
    fn test_evaluate_network_allowlist() {
        let policy = SecurityPolicy {
            network: NetworkPolicy {
                mode: NetworkMode::AllowList,
                allowed_domains: vec![
                    "api.anthropic.com".to_string(),
                    "*.openai.com".to_string(),
                ],
                ..NetworkPolicy::default()
            },
            ..SecurityPolicy::default()
        };
        assert!(PolicyEngine::evaluate_network(&policy, "api.anthropic.com", "https").is_ok());
        assert!(PolicyEngine::evaluate_network(&policy, "api.openai.com", "https").is_ok());
        assert!(PolicyEngine::evaluate_network(&policy, "evil.com", "https").is_err());
    }

    #[test]
    fn test_evaluate_network_denylist() {
        let policy = SecurityPolicy {
            network: NetworkPolicy {
                mode: NetworkMode::DenyList,
                denied_domains: vec!["evil.com".to_string()],
                ..NetworkPolicy::default()
            },
            ..SecurityPolicy::default()
        };
        assert!(PolicyEngine::evaluate_network(&policy, "good.com", "https").is_ok());
        assert!(PolicyEngine::evaluate_network(&policy, "evil.com", "https").is_err());
    }

    #[test]
    fn test_evaluate_network_metadata_blocked() {
        let policy = SecurityPolicy {
            network: NetworkPolicy {
                mode: NetworkMode::Unrestricted,
                block_metadata_endpoints: true,
                ..NetworkPolicy::default()
            },
            ..SecurityPolicy::default()
        };
        assert!(PolicyEngine::evaluate_network(&policy, "169.254.169.254", "http").is_err());
        assert!(PolicyEngine::evaluate_network(&policy, "fd00:ec2::254", "http").is_err());
        assert!(PolicyEngine::evaluate_network(&policy, "google.com", "https").is_ok());
    }

    #[test]
    fn test_evaluate_network_protocol_restriction() {
        let policy = SecurityPolicy {
            network: NetworkPolicy {
                mode: NetworkMode::Unrestricted,
                allowed_protocols: vec!["https".to_string()],
                ..NetworkPolicy::default()
            },
            ..SecurityPolicy::default()
        };
        assert!(PolicyEngine::evaluate_network(&policy, "api.example.com", "https").is_ok());
        assert!(PolicyEngine::evaluate_network(&policy, "api.example.com", "http").is_err());
    }

    #[test]
    fn test_evaluate_filesystem() {
        let policy = SecurityPolicy {
            filesystem: FilesystemPolicy {
                read_write_paths: vec!["/workspace/**".to_string()],
                denied_paths: vec!["/workspace/.env".to_string()],
                ..FilesystemPolicy::default()
            },
            ..SecurityPolicy::default()
        };
        // Denied path takes priority
        assert!(PolicyEngine::evaluate_filesystem(&policy, "/workspace/.env", true).is_err());
        // Write to workspace is allowed
        assert!(PolicyEngine::evaluate_filesystem(&policy, "/workspace/src/main.rs", true).is_ok());
        // Write outside workspace is denied
        assert!(PolicyEngine::evaluate_filesystem(&policy, "/etc/passwd", true).is_err());
        // Read is always allowed (no read restrictions in this policy)
        assert!(PolicyEngine::evaluate_filesystem(&policy, "/etc/hosts", false).is_ok());
    }

    #[test]
    fn test_evaluate_recursion() {
        let policy = SecurityPolicy {
            actions: ActionPolicy {
                max_recursion_depth: 5,
                ..ActionPolicy::default()
            },
            ..SecurityPolicy::default()
        };
        assert!(PolicyEngine::evaluate_recursion(&policy, 0).is_ok());
        assert!(PolicyEngine::evaluate_recursion(&policy, 4).is_ok());
        assert!(PolicyEngine::evaluate_recursion(&policy, 5).is_err());
        assert!(PolicyEngine::evaluate_recursion(&policy, 10).is_err());
    }

    #[test]
    fn test_domain_wildcard_matching() {
        assert!(domain_matches_list("api.openai.com", &["*.openai.com".to_string()]));
        assert!(domain_matches_list("openai.com", &["*.openai.com".to_string()]));
        assert!(!domain_matches_list("notopenai.com", &["*.openai.com".to_string()]));
        assert!(domain_matches_list("api.anthropic.com", &["api.anthropic.com".to_string()]));
        assert!(!domain_matches_list("evil.anthropic.com", &["api.anthropic.com".to_string()]));
    }

    #[test]
    fn test_glob_matches_double_star() {
        assert!(glob_matches("/workspace/**", "/workspace/src/main.rs"));
        assert!(glob_matches("/workspace/**", "/workspace/deep/nested/file"));
        assert!(glob_matches("/workspace/**", "/workspace/file"));
        assert!(!glob_matches("/workspace/**", "/etc/passwd"));
    }

    #[test]
    fn test_glob_matches_single_star() {
        assert!(glob_matches("/proc/*/environ", "/proc/1234/environ"));
        assert!(glob_matches("/proc/*/environ", "/proc/self/environ"));
        assert!(!glob_matches("/proc/*/environ", "/proc/1234/maps"));
        assert!(!glob_matches("/proc/*/environ", "/proc/environ"));
    }

    #[test]
    fn test_glob_matches_nested_wildcards() {
        assert!(glob_matches("/home/*/.ssh/**", "/home/user/.ssh/id_rsa"));
        assert!(glob_matches("/home/*/.ssh/**", "/home/admin/.ssh/authorized_keys"));
        assert!(glob_matches("/home/*/.ssh/**", "/home/user/.ssh/config"));
        assert!(!glob_matches("/home/*/.ssh/**", "/home/user/.bashrc"));
        assert!(!glob_matches("/home/*/.ssh/**", "/root/.ssh/id_rsa"));
    }

    #[test]
    fn test_glob_matches_exact() {
        assert!(glob_matches("/etc/shadow", "/etc/shadow"));
        assert!(!glob_matches("/etc/shadow", "/etc/passwd"));
        assert!(!glob_matches("/etc/shadow", "/etc/shadow/extra"));
    }

    #[test]
    fn test_destructive_command_detection() {
        assert!(is_destructive_command("rm -rf /"));
        assert!(is_destructive_command("rm -fr /tmp"));
        assert!(is_destructive_command("DROP TABLE users"));
        assert!(is_destructive_command("TRUNCATE TABLE sessions"));
        assert!(is_destructive_command("git push --force"));
        assert!(is_destructive_command("git reset --hard"));
        assert!(is_destructive_command("shutdown -h now"));
        assert!(is_destructive_command("dd if=/dev/zero of=/dev/sda"));
        assert!(!is_destructive_command("ls -la"));
        assert!(!is_destructive_command("echo hello"));
        assert!(!is_destructive_command("git push origin main"));
        assert!(!is_destructive_command("rm file.txt"));
    }
}
