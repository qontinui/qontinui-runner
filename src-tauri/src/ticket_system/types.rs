//! Core types and traits for the ticket system.
//!
//! The wire-format DTO types (`TicketSource`, `TicketState`, `Ticket`,
//! `TicketComment`, `TicketProviderConfig`) live in the `qontinui-types` crate
//! (`qontinui_types::ticket_system`) and are re-exported here so the rest of
//! the runner can continue `use crate::ticket_system::types::*`. Runner-local
//! behavior — the `TicketProvider` trait, the `as_str()` helper on
//! `TicketSource`, and the explicit `to_json_with_token` / `from_json_with_token`
//! DB-serialization helpers on `TicketProviderConfig` — stays here, attached
//! via extension traits.

use async_trait::async_trait;

pub use qontinui_types::ticket_system::{
    Ticket, TicketComment, TicketProviderConfig, TicketSource, TicketState,
};

/// Runner-local helpers on `TicketSource`.
pub trait TicketSourceExt {
    /// Lowercase string tag, used for DB storage keys and log lines.
    fn as_str(&self) -> &'static str;
}

impl TicketSourceExt for TicketSource {
    fn as_str(&self) -> &'static str {
        match self {
            TicketSource::GitHub => "github",
            TicketSource::Linear => "linear",
            TicketSource::Jira => "jira",
        }
    }
}

/// Runner-local DB-serialization helpers on `TicketProviderConfig`.
///
/// These exist as separate methods (rather than relying on the `Serialize`
/// impl directly) to make the "this serialization includes the secret
/// `api_token` and is intended for at-rest persistence only" intent explicit
/// at the call site.
pub trait TicketProviderConfigExt: Sized {
    /// Serialize the full config including the `api_token` for DB persistence.
    ///
    /// Used when writing to `ticket_provider_configs.config_json` so the
    /// watcher can be reconstructed across restarts. Never return the result
    /// of this over a UI-facing API without redacting the token first.
    fn to_json_with_token(&self) -> Result<String, String>;

    /// Deserialize a config previously stored via `to_json_with_token`.
    fn from_json_with_token(json: &str) -> Result<Self, String>;
}

impl TicketProviderConfigExt for TicketProviderConfig {
    fn to_json_with_token(&self) -> Result<String, String> {
        let v = serde_json::json!({
            "source": self.source,
            "api_token": self.api_token,
            "target": self.target,
            "actionable_labels": self.actionable_labels,
            "workflow_id": self.workflow_id,
            "poll_interval_seconds": self.poll_interval_seconds,
            "update_on_completion": self.update_on_completion,
        });
        serde_json::to_string(&v).map_err(|e| e.to_string())
    }

    fn from_json_with_token(json: &str) -> Result<Self, String> {
        let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        Ok(Self {
            source: serde_json::from_value(v["source"].clone()).map_err(|e| e.to_string())?,
            api_token: v["api_token"].as_str().unwrap_or("").to_string(),
            target: v["target"].as_str().unwrap_or("").to_string(),
            actionable_labels: v["actionable_labels"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            workflow_id: v["workflow_id"].as_str().unwrap_or("").to_string(),
            poll_interval_seconds: v["poll_interval_seconds"].as_u64().unwrap_or(60),
            update_on_completion: v["update_on_completion"].as_bool().unwrap_or(true),
        })
    }
}

/// Trait for ticket provider implementations. Held in the runner because
/// implementations carry runtime state (HTTP clients, caches) and aren't
/// part of the wire contract.
#[async_trait]
pub trait TicketProvider: Send + Sync {
    fn source(&self) -> TicketSource;

    /// Fetch actionable tickets (matching configured labels/filters).
    async fn fetch_actionable(&self, config: &TicketProviderConfig) -> Result<Vec<Ticket>, String>;

    /// Fetch comments on a ticket.
    async fn fetch_comments(
        &self,
        ticket_id: &str,
        config: &TicketProviderConfig,
    ) -> Result<Vec<TicketComment>, String>;

    /// Post a comment on a ticket.
    async fn add_comment(
        &self,
        ticket_id: &str,
        comment: &str,
        config: &TicketProviderConfig,
    ) -> Result<(), String>;

    /// Update ticket state.
    async fn update_state(
        &self,
        ticket_id: &str,
        state: TicketState,
        config: &TicketProviderConfig,
    ) -> Result<(), String>;
}
