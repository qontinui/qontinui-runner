//! Rathole TOML config generation.
//!
//! Rathole's client config lives under the `[client]` / `[client.services.*]`
//! tables. We build this programmatically so the wizard / settings page can
//! drive it without hand-editing TOML.

use serde::{Deserialize, Serialize};

/// Per-service entry inside a `[client.services.*]` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelService {
    /// Table key (e.g. `"ui-bridge"`).
    pub name: String,
    /// Where the runner is listening locally (e.g. `"127.0.0.1:9876"`).
    pub local_addr: String,
    /// Protocol type — rathole accepts `"tcp"` and `"udp"`.
    #[serde(default = "default_service_type")]
    pub service_type: String,
    /// Per-service token shared with the rathole server.
    pub token: String,
}

fn default_service_type() -> String {
    "tcp".to_string()
}

/// Top-level rathole client config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatholeConfig {
    /// Rathole server `host:port` (e.g. `"relay.qontinui.io:2333"`).
    pub server_addr: String,
    /// Optional default token inherited by services that don't set their own.
    #[serde(default)]
    pub default_token: Option<String>,
    /// One or more exposed services.
    pub services: Vec<TunnelService>,
}

impl RatholeConfig {
    /// Render this config as rathole-compatible TOML.
    ///
    /// Rathole's client-side config shape (from their docs):
    ///
    /// ```toml
    /// [client]
    /// remote_addr = "relay.example.com:2333"
    /// default_token = "..."
    ///
    /// [client.services.ui-bridge]
    /// type = "tcp"
    /// token = "..."
    /// local_addr = "127.0.0.1:9876"
    /// ```
    pub fn to_toml(&self) -> String {
        let mut out = String::new();
        out.push_str("[client]\n");
        out.push_str(&format!("remote_addr = \"{}\"\n", self.server_addr));
        if let Some(token) = &self.default_token {
            out.push_str(&format!("default_token = \"{}\"\n", token));
        }
        out.push('\n');

        for svc in &self.services {
            out.push_str(&format!("[client.services.{}]\n", svc.name));
            out.push_str(&format!("type = \"{}\"\n", svc.service_type));
            out.push_str(&format!("token = \"{}\"\n", svc.token));
            out.push_str(&format!("local_addr = \"{}\"\n", svc.local_addr));
            out.push('\n');
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_expected_toml() {
        let cfg = RatholeConfig {
            server_addr: "relay.qontinui.io:2333".to_string(),
            default_token: Some("shared-secret".to_string()),
            services: vec![TunnelService {
                name: "ui-bridge".to_string(),
                local_addr: "127.0.0.1:9876".to_string(),
                service_type: "tcp".to_string(),
                token: "svc-tok".to_string(),
            }],
        };
        let toml = cfg.to_toml();
        assert!(toml.contains("remote_addr = \"relay.qontinui.io:2333\""));
        assert!(toml.contains("[client.services.ui-bridge]"));
        assert!(toml.contains("local_addr = \"127.0.0.1:9876\""));
    }
}
