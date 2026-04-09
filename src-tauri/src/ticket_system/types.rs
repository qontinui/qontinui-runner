//! Core types and traits for the ticket system.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Which ticket provider this refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketSource {
    GitHub,
    Linear,
    Jira,
}

impl TicketSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            TicketSource::GitHub => "github",
            TicketSource::Linear => "linear",
            TicketSource::Jira => "jira",
        }
    }
}

/// Abstract ticket state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketState {
    /// Ticket is open and ready for work.
    Open,
    /// Task is actively working on this ticket.
    InProgress,
    /// Task is done, awaiting review/close.
    Done,
    /// Ticket is closed.
    Closed,
}

/// A ticket fetched from an external system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    /// External ID (issue number, ticket key).
    pub external_id: String,
    pub source: TicketSource,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub url: String,
    pub state: TicketState,
    pub created_at: String,
    pub updated_at: String,
}

/// A comment on a ticket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketComment {
    pub id: String,
    pub author: String,
    pub body: String,
    pub created_at: String,
}

/// Provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketProviderConfig {
    pub source: TicketSource,
    /// API token. NOTE: this is persisted in the DB (inside `workflow_triggers.trigger_config`
    /// and `ticket_provider_configs.config_json`) so the watcher can be reconstructed across
    /// restarts. The MCP/HTTP API layer is responsible for redacting this field in any
    /// outbound responses to the frontend. Treat it as secret-at-rest only.
    pub api_token: String,
    /// GitHub: "owner/repo". Linear: team key. Jira: project key.
    pub target: String,
    /// Labels/filters to determine "actionable" tickets (e.g., ["automate", "qontinui"]).
    pub actionable_labels: Vec<String>,
    /// Workflow ID to create tasks from matched tickets.
    pub workflow_id: String,
    /// Poll interval in seconds (default: 60).
    #[serde(default = "default_poll_interval")]
    pub poll_interval_seconds: u64,
    /// Whether to update ticket state when the task completes.
    #[serde(default = "default_true")]
    pub update_on_completion: bool,
}

fn default_poll_interval() -> u64 {
    60
}
fn default_true() -> bool {
    true
}

impl TicketProviderConfig {
    /// Serialize the full config including the api_token for DB persistence.
    /// Use this for storing in `ticket_provider_configs.config_json` so that
    /// the watcher can be reconstructed across restarts.
    pub fn to_json_with_token(&self) -> Result<String, String> {
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

    /// Deserialize a config previously stored via `to_json_with_token`.
    pub fn from_json_with_token(json: &str) -> Result<Self, String> {
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

/// Trait for ticket provider implementations.
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

