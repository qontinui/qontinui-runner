//! Restate durable execution configuration.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Settings for the Restate durable execution integration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestateSettings {
    /// Whether Restate durable execution is enabled (default: false).
    /// When disabled, all workflows use the legacy checkpoint-based execution.
    #[serde(default)]
    pub enabled: bool,

    /// Custom path to the `restate-server` binary.
    /// If not set, the runner downloads and manages the binary automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,

    /// Restate ingress port for workflow invocations (default: 8080).
    #[serde(default = "default_ingress_port")]
    pub ingress_port: u16,

    /// Restate admin API port for health checks and deployment registration (default: 9070).
    #[serde(default = "default_admin_port")]
    pub admin_port: u16,

    /// Port for the runner's Restate service HTTP endpoint (default: 9080).
    /// This is the port Restate calls back into for workflow execution.
    #[serde(default = "default_service_endpoint_port")]
    pub service_endpoint_port: u16,

    /// Whether to auto-download the Restate binary if not found (default: true).
    #[serde(default = "default_true")]
    pub auto_download: bool,

    /// Custom data directory for Restate's RocksDB journal.
    /// If not set, defaults to `{app_data_dir}/restate/data/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<String>,

    /// Target Restate server version to download (default: latest known compatible).
    #[serde(default = "default_restate_version")]
    pub target_version: String,

    /// When set, skips the managed-server binary path entirely and uses this URL
    /// for admin-plane calls (health, service registration). Phase 2's supervisor
    /// port-allocator injects this.
    #[serde(default)]
    pub external_admin_url: Option<String>,

    /// Paired with external_admin_url. When set, the runner treats the external
    /// server as authoritative and bypasses all local spawning.
    #[serde(default)]
    pub external_ingress_url: Option<String>,
}

impl Default for RestateSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            binary_path: None,
            ingress_port: default_ingress_port(),
            admin_port: default_admin_port(),
            service_endpoint_port: default_service_endpoint_port(),
            auto_download: true,
            data_dir: None,
            target_version: default_restate_version(),
            external_admin_url: None,
            external_ingress_url: None,
        }
    }
}

impl RestateSettings {
    /// Resolve the data directory path, using the custom path or the default.
    ///
    /// For secondary runners an `instance-<name>` subdirectory is appended so
    /// concurrent runners don't corrupt each other's RocksDB journal — RocksDB
    /// refuses concurrent writers and would crash on startup otherwise.
    pub fn resolve_data_dir(&self, app_data_dir: &Path) -> PathBuf {
        let base = if let Some(ref custom) = self.data_dir {
            PathBuf::from(custom)
        } else {
            app_data_dir.join("restate").join("data")
        };
        crate::instance::scope_path(&base)
    }

    /// Resolve the binary directory path (where restate-server is stored).
    pub fn resolve_binary_dir(&self, app_data_dir: &Path) -> PathBuf {
        app_data_dir.join("restate").join("bin")
    }

    /// Get the ingress URL for workflow invocations.
    ///
    /// Prefers `external_ingress_url` when the external pair is configured.
    pub fn ingress_url(&self) -> String {
        if self.is_external() {
            if let Some(ref url) = self.external_ingress_url {
                return url.trim_end_matches('/').to_string();
            }
        }
        format!("http://127.0.0.1:{}", self.ingress_port)
    }

    /// Get the admin API URL for health checks and deployment management.
    ///
    /// Prefers `external_admin_url` when the external pair is configured.
    pub fn admin_url(&self) -> String {
        if self.is_external() {
            if let Some(ref url) = self.external_admin_url {
                return url.trim_end_matches('/').to_string();
            }
        }
        format!("http://127.0.0.1:{}", self.admin_port)
    }

    /// Returns true when both external URLs are set, indicating the runner should
    /// bypass all local Restate spawning and target the external server instead.
    pub fn is_external(&self) -> bool {
        self.external_admin_url.is_some() && self.external_ingress_url.is_some()
    }

    /// Get the service endpoint URL that Restate calls back into.
    pub fn service_endpoint_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.service_endpoint_port)
    }

    /// Generate a TOML configuration file for the Restate server.
    pub fn generate_server_config(&self, app_data_dir: &Path) -> String {
        let data_dir = self.resolve_data_dir(app_data_dir);
        let data_dir_str = data_dir.to_string_lossy().replace('\\', "/");

        format!(
            r#"# Auto-generated Restate server configuration
# Managed by qontinui-runner

[admin]
bind-address = "127.0.0.1:{admin_port}"

[ingress]
bind-address = "127.0.0.1:{ingress_port}"

[bifrost.local.rocksdb]
path = "{data_dir}"
"#,
            admin_port = self.admin_port,
            ingress_port = self.ingress_port,
            data_dir = data_dir_str,
        )
    }
}

fn default_ingress_port() -> u16 {
    8080
}

fn default_admin_port() -> u16 {
    9070
}

fn default_service_endpoint_port() -> u16 {
    9080
}

fn default_true() -> bool {
    true
}

fn default_restate_version() -> String {
    "1.6.2".to_string()
}
