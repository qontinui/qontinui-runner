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
    /// Render the **client-side** TOML that the runner hands to its embedded
    /// rathole client.
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

    /// Render the mirror **server-side** TOML for the user's rathole VPS.
    ///
    /// Key differences from the client TOML:
    /// - `[server]` with `bind_addr` (the port the client connects to)
    /// - Each service's `bind_addr` is the public port the phone / device
    ///   reaches out to. We pick a deterministic offset from the server's
    ///   bind port if the caller doesn't override `public_bind_start`.
    ///
    /// ```toml
    /// [server]
    /// bind_addr = "0.0.0.0:2333"
    /// default_token = "..."
    ///
    /// [server.services.ui-bridge]
    /// token = "..."
    /// bind_addr = "0.0.0.0:5202"
    /// ```
    ///
    /// The `server_bind_host` argument lets the caller choose `0.0.0.0`
    /// (everything), `[::]:` (dual-stack), or a specific interface.
    pub fn to_server_toml(&self, server_bind_host: &str, public_bind_start: u16) -> String {
        // `server_addr` in this struct is "host:port" — the server listens on
        // that port. The public per-service ports start at `public_bind_start`
        // and increment by one per service (stable order).
        let control_port = self
            .server_addr
            .rsplit_once(':')
            .and_then(|(_, p)| p.parse::<u16>().ok())
            .unwrap_or(2333);

        let mut out = String::new();
        out.push_str("[server]\n");
        out.push_str(&format!(
            "bind_addr = \"{server_bind_host}:{control_port}\"\n"
        ));
        if let Some(token) = &self.default_token {
            out.push_str(&format!("default_token = \"{}\"\n", token));
        }
        out.push('\n');

        for (i, svc) in self.services.iter().enumerate() {
            let public_port = public_bind_start.saturating_add(i as u16);
            out.push_str(&format!("[server.services.{}]\n", svc.name));
            out.push_str(&format!("token = \"{}\"\n", svc.token));
            out.push_str(&format!(
                "bind_addr = \"{server_bind_host}:{public_port}\"\n"
            ));
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

    #[test]
    fn renders_server_toml_mirror() {
        let cfg = RatholeConfig {
            server_addr: "relay.qontinui.io:2333".to_string(),
            default_token: Some("shared-secret".to_string()),
            services: vec![
                TunnelService {
                    name: "ui-bridge".to_string(),
                    local_addr: "127.0.0.1:9876".to_string(),
                    service_type: "tcp".to_string(),
                    token: "svc-a".to_string(),
                },
                TunnelService {
                    name: "api".to_string(),
                    local_addr: "127.0.0.1:8000".to_string(),
                    service_type: "tcp".to_string(),
                    token: "svc-b".to_string(),
                },
            ],
        };
        let server = cfg.to_server_toml("0.0.0.0", 5200);
        assert!(server.contains("bind_addr = \"0.0.0.0:2333\""));
        assert!(server.contains("[server.services.ui-bridge]"));
        assert!(server.contains("bind_addr = \"0.0.0.0:5200\""));
        assert!(server.contains("[server.services.api]"));
        assert!(server.contains("bind_addr = \"0.0.0.0:5201\""));
        // Server TOML must not leak the client's local_addr
        assert!(!server.contains("127.0.0.1:9876"));
    }
}
