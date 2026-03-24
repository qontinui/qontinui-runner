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
        }
    }
}
