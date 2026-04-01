//! Security audit trail.
//!
//! Provides structured logging of security-relevant events with database
//! persistence. Events are written asynchronously to avoid blocking the
//! step execution pipeline.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

// ============================================================================
// Audit Event Types
// ============================================================================

/// A security audit event recording a policy decision or security-relevant action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityAuditEvent {
    /// Unique event ID.
    pub id: String,
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// Associated task run ID (if within a workflow execution).
    pub task_run_id: Option<String>,
    /// Step name that triggered the event.
    pub step_name: Option<String>,
    /// Workflow ID that owns the step.
    pub workflow_id: Option<String>,
    /// Type of security event.
    pub event_type: AuditEventType,
    /// What was attempted (e.g., the command, domain, or file path).
    pub action: String,
    /// The policy decision.
    pub decision: AuditDecision,
    /// Human-readable reason for the decision.
    pub reason: Option<String>,
    /// Additional structured metadata.
    pub metadata: serde_json::Value,
}

/// Classification of security audit events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    /// A policy was evaluated for a step.
    PolicyEvaluation,
    /// A capability was granted to an execution context.
    CapabilityGrant,
    /// A capability was denied.
    CapabilityDenial,
    /// A network request was made (or attempted).
    NetworkRequest,
    /// A filesystem access was attempted.
    FilesystemAccess,
    /// The credential proxy injected credentials.
    CredentialInjection,
    /// A command was blocked by policy.
    CommandBlocked,
    /// Container lifecycle event (create, start, stop, timeout).
    ContainerLifecycle,
}

/// The decision made for an audit event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditDecision {
    /// The action was allowed by policy.
    Allowed,
    /// The action was denied by policy.
    Denied,
    /// The action was allowed but flagged as a potential concern.
    Warning,
}

/// Summary statistics for audit events.
/// Used by the `get_security_audit_summary` query endpoint (pending DB integration).
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditSummary {
    pub total_events: u64,
    pub allowed: u64,
    pub denied: u64,
    pub warnings: u64,
    pub by_type: Vec<(String, u64)>,
}

// ============================================================================
// Audit Logger
// ============================================================================

/// Async audit event logger.
///
/// Events are sent to a background writer via an mpsc channel to avoid
/// blocking the execution pipeline. The background writer batches events
/// and writes them to the database.
#[derive(Clone)]
pub struct AuditLogger {
    tx: mpsc::Sender<SecurityAuditEvent>,
    enabled: bool,
}

impl AuditLogger {
    /// Create a new audit logger that writes events to the database.
    ///
    /// Spawns a background task that receives events and persists them.
    /// The `db` parameter should be the PostgreSQL database handle.
    pub fn new(enabled: bool) -> Self {
        let (tx, mut rx) = mpsc::channel::<SecurityAuditEvent>(1024);

        if enabled {
            tokio::spawn(async move {
                let mut batch: Vec<SecurityAuditEvent> = Vec::new();
                let flush_interval = tokio::time::interval(std::time::Duration::from_secs(2));
                tokio::pin!(flush_interval);

                loop {
                    tokio::select! {
                        event = rx.recv() => {
                            match event {
                                Some(evt) => {
                                    debug!(
                                        "Audit event: type={:?} decision={:?} action={}",
                                        evt.event_type, evt.decision, evt.action
                                    );
                                    batch.push(evt);

                                    // Flush immediately if batch is large
                                    if batch.len() >= 50 {
                                        Self::flush_batch(&mut batch).await;
                                    }
                                }
                                None => {
                                    // Channel closed, flush remaining and exit
                                    if !batch.is_empty() {
                                        Self::flush_batch(&mut batch).await;
                                    }
                                    debug!("Audit logger shutting down");
                                    break;
                                }
                            }
                        }
                        _ = flush_interval.tick() => {
                            if !batch.is_empty() {
                                Self::flush_batch(&mut batch).await;
                            }
                        }
                    }
                }
            });
        }

        Self { tx, enabled }
    }

    /// Create a no-op logger (for when audit logging is disabled).
    /// The receiver is intentionally dropped — `enabled: false` prevents any sends.
    pub fn noop() -> Self {
        let (tx, _rx) = mpsc::channel(1);
        Self {
            tx,
            enabled: false,
        }
    }

    /// Log a security audit event (non-blocking).
    pub fn log(&self, event: SecurityAuditEvent) {
        if !self.enabled {
            return;
        }
        if let Err(e) = self.tx.try_send(event) {
            warn!("Failed to send audit event (channel full or closed): {}", e);
        }
    }

    /// Convenience: log a policy denial.
    pub fn log_denial(
        &self,
        denial: &super::engine::PolicyDenial,
        task_run_id: Option<&str>,
        step_name: Option<&str>,
    ) {
        self.log(SecurityAuditEvent {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            task_run_id: task_run_id.map(|s| s.to_string()),
            step_name: step_name.map(|s| s.to_string()),
            workflow_id: None,
            event_type: match denial.denial_type {
                super::engine::DenialType::CommandBlocked
                | super::engine::DenialType::DestructiveCommand => AuditEventType::CommandBlocked,
                super::engine::DenialType::NetworkDenied
                | super::engine::DenialType::MetadataEndpoint
                | super::engine::DenialType::ProtocolDenied => AuditEventType::NetworkRequest,
                super::engine::DenialType::FilesystemDenied => AuditEventType::FilesystemAccess,
                super::engine::DenialType::CredentialDenied => AuditEventType::CapabilityDenial,
                super::engine::DenialType::RecursionLimit => AuditEventType::PolicyEvaluation,
            },
            action: denial.action.clone(),
            decision: AuditDecision::Denied,
            reason: Some(denial.reason.clone()),
            metadata: serde_json::json!({
                "denial_type": format!("{:?}", denial.denial_type),
            }),
        });
    }

    /// Convenience: log a container lifecycle event.
    pub fn log_container_event(
        &self,
        action: &str,
        container_id: &str,
        task_run_id: Option<&str>,
        decision: AuditDecision,
    ) {
        self.log(SecurityAuditEvent {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            task_run_id: task_run_id.map(|s| s.to_string()),
            step_name: None,
            workflow_id: None,
            event_type: AuditEventType::ContainerLifecycle,
            action: action.to_string(),
            decision,
            reason: None,
            metadata: serde_json::json!({
                "container_id": container_id,
            }),
        });
    }

    /// Flush a batch of events to the database.
    async fn flush_batch(batch: &mut Vec<SecurityAuditEvent>) {
        let count = batch.len();
        if count == 0 {
            batch.clear();
            return;
        }

        // Write to PostgreSQL via the global PgDb instance
        if let Some(pg_db) = crate::database::pg::PgDb::try_global() {
            match pg_db.insert_security_audit_events(batch).await {
                Ok(inserted) => {
                    debug!("Flushed {} audit events to PG ({} inserted)", count, inserted);
                }
                Err(e) => {
                    warn!("Failed to flush audit events to PG: {}", e);
                    // Log to tracing as fallback
                    for event in batch.iter() {
                        debug!(
                            "Audit event (PG unavailable): id={} type={:?} decision={:?} action={}",
                            event.id, event.event_type, event.decision, event.action
                        );
                    }
                }
            }
        } else {
            // PG not initialized yet — log to tracing as fallback
            for event in batch.iter() {
                debug!(
                    "Audit event (no PG): id={} type={:?} decision={:?} action={}",
                    event.id, event.event_type, event.decision, event.action
                );
            }
        }

        batch.clear();
    }
}

impl SecurityAuditEvent {
    /// Create a new audit event with minimal fields.
    pub fn new(
        event_type: AuditEventType,
        action: impl Into<String>,
        decision: AuditDecision,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            task_run_id: None,
            step_name: None,
            workflow_id: None,
            event_type,
            action: action.into(),
            decision,
            reason: None,
            metadata: serde_json::Value::Null,
        }
    }

    /// Set the task run ID.
    pub fn with_task_run(mut self, task_run_id: &str) -> Self {
        self.task_run_id = Some(task_run_id.to_string());
        self
    }

    /// Set the step name.
    pub fn with_step(mut self, step_name: &str) -> Self {
        self.step_name = Some(step_name.to_string());
        self
    }

    /// Set the reason.
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    /// Set metadata.
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_event_creation() {
        let event = SecurityAuditEvent::new(
            AuditEventType::CommandBlocked,
            "rm -rf /",
            AuditDecision::Denied,
        )
        .with_reason("Destructive command blocked")
        .with_task_run("run-123")
        .with_step("shell_step_1");

        assert_eq!(event.event_type, AuditEventType::CommandBlocked);
        assert_eq!(event.decision, AuditDecision::Denied);
        assert_eq!(event.action, "rm -rf /");
        assert_eq!(event.task_run_id, Some("run-123".to_string()));
        assert_eq!(event.step_name, Some("shell_step_1".to_string()));
        assert!(event.reason.is_some());
    }

    #[test]
    fn test_noop_logger() {
        let logger = AuditLogger::noop();
        // Should not panic
        logger.log(SecurityAuditEvent::new(
            AuditEventType::PolicyEvaluation,
            "test",
            AuditDecision::Allowed,
        ));
    }
}
