//! Database operations for ticket-task mappings.

use super::PgDb;

#[derive(Debug, Clone)]
pub struct TicketTaskMapping {
    pub id: String,
    pub ticket_source: String,
    pub ticket_external_id: String,
    pub ticket_url: String,
    pub task_run_id: String,
    pub workflow_id: String,
    pub sync_status: String,
}

impl PgDb {
    pub async fn get_ticket_task_mapping(
        &self,
        source: &str,
        external_id: &str,
    ) -> Result<Option<TicketTaskMapping>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, ticket_source, ticket_external_id, ticket_url,
                          task_run_id, workflow_id, sync_status
                   FROM ticket_task_mapping
                   WHERE ticket_source = $1 AND ticket_external_id = $2
                   LIMIT 1"#,
                &[&source, &external_id],
            )
            .await
            .map_err(|e| format!("PG get_ticket_task_mapping: {}", e))?;

        Ok(rows.into_iter().next().map(|r| TicketTaskMapping {
            id: r.get(0),
            ticket_source: r.get(1),
            ticket_external_id: r.get(2),
            ticket_url: r.get(3),
            task_run_id: r.get(4),
            workflow_id: r.get(5),
            sync_status: r.get(6),
        }))
    }

    pub async fn get_ticket_task_mapping_by_task(
        &self,
        task_run_id: &str,
    ) -> Result<Option<TicketTaskMapping>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, ticket_source, ticket_external_id, ticket_url,
                          task_run_id, workflow_id, sync_status
                   FROM ticket_task_mapping
                   WHERE task_run_id = $1
                   LIMIT 1"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_ticket_task_mapping_by_task: {}", e))?;

        Ok(rows.into_iter().next().map(|r| TicketTaskMapping {
            id: r.get(0),
            ticket_source: r.get(1),
            ticket_external_id: r.get(2),
            ticket_url: r.get(3),
            task_run_id: r.get(4),
            workflow_id: r.get(5),
            sync_status: r.get(6),
        }))
    }

    pub async fn insert_ticket_task_mapping(
        &self,
        source: &str,
        external_id: &str,
        ticket_url: &str,
        task_run_id: &str,
        workflow_id: &str,
    ) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();

        let row = conn
            .query_one(
                r#"INSERT INTO ticket_task_mapping
                       (id, ticket_source, ticket_external_id, ticket_url, task_run_id, workflow_id)
                   VALUES ($1, $2, $3, $4, $5, $6)
                   ON CONFLICT (ticket_source, ticket_external_id) DO UPDATE SET
                       task_run_id = EXCLUDED.task_run_id,
                       workflow_id = EXCLUDED.workflow_id,
                       ticket_url = EXCLUDED.ticket_url,
                       updated_at = NOW()
                   RETURNING id"#,
                &[
                    &id,
                    &source,
                    &external_id,
                    &ticket_url,
                    &task_run_id,
                    &workflow_id,
                ],
            )
            .await
            .map_err(|e| format!("PG insert_ticket_task_mapping: {}", e))?;

        Ok(row.get(0))
    }

    /// Upsert a stored ticket provider config (one row per workflow_id).
    /// `config_json` should be the output of `TicketProviderConfig::to_json_with_token`.
    pub async fn upsert_ticket_provider_config(
        &self,
        workflow_id: &str,
        source: &str,
        config_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            r#"INSERT INTO ticket_provider_configs (id, workflow_id, source, config_json)
               VALUES ($1, $2, $3, $4)
               ON CONFLICT (workflow_id) DO UPDATE SET
                   source      = EXCLUDED.source,
                   config_json = EXCLUDED.config_json,
                   updated_at  = NOW()"#,
            &[&id, &workflow_id, &source, &config_json],
        )
        .await
        .map_err(|e| format!("PG upsert_ticket_provider_config: {}", e))?;

        Ok(())
    }

    /// Look up the stored config_json for a workflow_id, if any.
    pub async fn get_ticket_provider_config_by_workflow(
        &self,
        workflow_id: &str,
    ) -> Result<Option<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                "SELECT config_json FROM ticket_provider_configs WHERE workflow_id = $1 LIMIT 1",
                &[&workflow_id],
            )
            .await
            .map_err(|e| format!("PG get_ticket_provider_config_by_workflow: {}", e))?;

        Ok(rows.into_iter().next().map(|r| r.get(0)))
    }

    pub async fn update_ticket_task_mapping_status(
        &self,
        id: &str,
        status: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            "UPDATE ticket_task_mapping SET sync_status = $1, updated_at = NOW() WHERE id = $2",
            &[&status, &id],
        )
        .await
        .map_err(|e| format!("PG update_ticket_task_mapping_status: {}", e))?;

        Ok(())
    }
}
