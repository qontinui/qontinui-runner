//! PostgreSQL security audit event operations.

use super::PgDb;
use serde::{Deserialize, Serialize};

/// A security audit event row from the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityAuditRow {
    pub id: String,
    pub timestamp: String,
    pub task_run_id: Option<String>,
    pub step_name: Option<String>,
    pub workflow_id: Option<String>,
    pub event_type: String,
    pub action: String,
    pub decision: String,
    pub reason: Option<String>,
    pub metadata: Option<String>,
    pub created_at: String,
}

/// Summary counts for audit events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityAuditSummaryRow {
    pub total: i64,
    pub allowed: i64,
    pub denied: i64,
    pub warnings: i64,
}

impl PgDb {
    /// Insert a batch of security audit events.
    pub async fn insert_security_audit_events(
        &self,
        events: &[crate::security::audit::SecurityAuditEvent],
    ) -> Result<usize, String> {
        if events.is_empty() {
            return Ok(0);
        }

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let mut count = 0;
        for event in events {
            let metadata_json = serde_json::to_string(&event.metadata).unwrap_or_default();
            let timestamp = event.timestamp.to_rfc3339();
            let event_type = serde_json::to_string(&event.event_type)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            let decision = serde_json::to_string(&event.decision)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();

            if let Err(e) = conn
                .execute(
                    r#"INSERT INTO security_audit_events
                       (id, timestamp, task_run_id, step_name, workflow_id,
                        event_type, action, decision, reason, metadata)
                       VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                       ON CONFLICT (id) DO NOTHING"#,
                    &[
                        &event.id,
                        &timestamp,
                        &event.task_run_id,
                        &event.step_name,
                        &event.workflow_id,
                        &event_type,
                        &event.action,
                        &decision,
                        &event.reason,
                        &metadata_json,
                    ],
                )
                .await
            {
                tracing::warn!("Failed to insert audit event {}: {}", event.id, e);
            } else {
                count += 1;
            }
        }

        Ok(count)
    }

    /// Query security audit events with optional filters.
    pub async fn query_security_audit_events(
        &self,
        task_run_id: Option<&str>,
        event_type: Option<&str>,
        decision: Option<&str>,
        limit: i64,
    ) -> Result<Vec<SecurityAuditRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Build dynamic WHERE clauses
        let mut conditions = Vec::new();
        let mut param_idx = 1;
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();

        if let Some(tr) = task_run_id {
            conditions.push(format!("task_run_id = ${}", param_idx));
            params.push(Box::new(tr.to_string()));
            param_idx += 1;
        }
        if let Some(et) = event_type {
            conditions.push(format!("event_type = ${}", param_idx));
            params.push(Box::new(et.to_string()));
            param_idx += 1;
        }
        if let Some(d) = decision {
            conditions.push(format!("decision = ${}", param_idx));
            params.push(Box::new(d.to_string()));
            param_idx += 1;
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let query = format!(
            "SELECT id, timestamp, task_run_id, step_name, workflow_id, \
             event_type, action, decision, reason, metadata, created_at \
             FROM security_audit_events {} ORDER BY timestamp DESC LIMIT ${}",
            where_clause, param_idx
        );

        params.push(Box::new(limit));

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| &**p as &(dyn tokio_postgres::types::ToSql + Sync)).collect();

        let rows = conn
            .query(&query, &param_refs)
            .await
            .map_err(|e| format!("PG query audit events: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| SecurityAuditRow {
                id: r.get(0),
                timestamp: r.get(1),
                task_run_id: r.get(2),
                step_name: r.get(3),
                workflow_id: r.get(4),
                event_type: r.get(5),
                action: r.get(6),
                decision: r.get(7),
                reason: r.get(8),
                metadata: r.get(9),
                created_at: r.get(10),
            })
            .collect())
    }

    /// Get summary counts for audit events.
    pub async fn security_audit_summary(
        &self,
        task_run_id: Option<&str>,
    ) -> Result<SecurityAuditSummaryRow, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let tr_owned = task_run_id.map(|s| s.to_string());
        let (query, params): (&str, Vec<&(dyn tokio_postgres::types::ToSql + Sync)>) =
            if let Some(ref tr) = tr_owned {
                (
                    r#"SELECT
                       COUNT(*) as total,
                       COUNT(*) FILTER (WHERE decision = 'allowed') as allowed,
                       COUNT(*) FILTER (WHERE decision = 'denied') as denied,
                       COUNT(*) FILTER (WHERE decision = 'warning') as warnings
                       FROM security_audit_events WHERE task_run_id = $1"#,
                    vec![tr as &(dyn tokio_postgres::types::ToSql + Sync)],
                )
            } else {
                (
                    r#"SELECT
                       COUNT(*) as total,
                       COUNT(*) FILTER (WHERE decision = 'allowed') as allowed,
                       COUNT(*) FILTER (WHERE decision = 'denied') as denied,
                       COUNT(*) FILTER (WHERE decision = 'warning') as warnings
                       FROM security_audit_events"#,
                    vec![],
                )
            };

        let row = conn
            .query_one(query, &params)
            .await
            .map_err(|e| format!("PG audit summary: {}", e))?;

        Ok(SecurityAuditSummaryRow {
            total: row.get(0),
            allowed: row.get(1),
            denied: row.get(2),
            warnings: row.get(3),
        })
    }

    /// Delete audit events older than the given number of days.
    pub async fn cleanup_old_audit_events(&self, retention_days: u32) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let result = conn
            .execute(
                "DELETE FROM security_audit_events WHERE created_at < NOW() - ($1 || ' days')::INTERVAL",
                &[&format!("{}", retention_days)],
            )
            .await
            .map_err(|e| format!("PG cleanup audit events: {}", e))?;

        Ok(result)
    }
}
