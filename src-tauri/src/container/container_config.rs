use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_base_image")]
    pub base_image: String,
    #[serde(default)]
    pub cpu_limit: Option<f64>,
    #[serde(default)]
    pub memory_limit_mb: Option<u64>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub read_only_mount: bool,
    #[serde(default = "default_true")]
    pub network_enabled: bool,

    // === Security hardening fields (Phase 2) ===
    /// Linux capabilities to drop (e.g., ["ALL"]).
    /// Dropping ALL capabilities and adding back only what's needed is the recommended pattern.
    #[serde(default)]
    pub cap_drop: Vec<String>,
    /// Linux capabilities to add back after dropping (e.g., ["NET_BIND_SERVICE"]).
    #[serde(default)]
    pub cap_add: Vec<String>,
    /// Docker security options (e.g., ["no-new-privileges", "seccomp=<json>"]).
    #[serde(default)]
    pub security_opt: Vec<String>,
    /// Make the container root filesystem read-only. Requires tmpfs mounts for writable areas.
    #[serde(default)]
    pub read_only_rootfs: bool,
    /// Run the container as a non-root user (e.g., "1000:1000").
    #[serde(default)]
    pub user: Option<String>,
    /// Maximum number of processes the container can create (PID limit).
    #[serde(default)]
    pub pids_limit: Option<i64>,
    /// Tmpfs mounts for writable temporary directories (e.g., ["/tmp:rw,noexec,nosuid,size=64m"]).
    #[serde(default)]
    pub tmpfs_mounts: Vec<String>,
    /// Additional environment variables to inject into the container.
    /// Used by the credential proxy and network mediator.
    #[serde(default)]
    pub extra_env: Vec<String>,
}

fn default_base_image() -> String {
    "ubuntu:22.04".to_string()
}
fn default_timeout() -> u64 {
    300
}
fn default_true() -> bool {
    true
}

impl Default for ContainerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_image: default_base_image(),
            cpu_limit: None,
            memory_limit_mb: None,
            timeout_secs: default_timeout(),
            read_only_mount: false,
            network_enabled: true,
            cap_drop: vec![],
            cap_add: vec![],
            security_opt: vec![],
            read_only_rootfs: false,
            user: None,
            pids_limit: None,
            tmpfs_mounts: vec![],
            extra_env: vec![],
        }
    }
}
