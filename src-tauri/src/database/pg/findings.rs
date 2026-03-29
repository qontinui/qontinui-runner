//! PostgreSQL finding operations via raw SQL.

use super::PgDb;
use crate::findings::{
    Finding, FindingActionType, FindingCategory, FindingCodeContext,
    FindingSeverity, FindingStatus, FindingSummary, FindingUserInput,
};

impl PgDb {
    /// Update a finding's status, optionally setting resolution and resolved_at.
    pub async fn update_finding_status(
        &self,
        id: &str,
        status: &str,
        resolution: Option<&str>,
        session_num: Option<u32>,
    ) -> Result<(), String> {
        let conn = self.pool().get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let resolved_at: Option<String> = if matches!(status, "resolved" | "wont_fix" | "deferred") {
            Some(chrono::Utc::now().to_rfc3339())
        } else {
            None
        };
        let now = chrono::Utc::now().to_rfc3339();
        let session: Option<i32> = session_num.map(|s| s as i32);

        conn.execute(
            "UPDATE task_run_findings SET status = $1, resolution = COALESCE($2, resolution), resolved_at = COALESCE($3, resolved_at), resolved_in_session = COALESCE($4, resolved_in_session), updated_at = $5 WHERE id = $6",
            &[
                &status as &(dyn tokio_postgres::types::ToSql + Sync),
                &resolution as &(dyn tokio_postgres::types::ToSql + Sync),
                &resolved_at as &(dyn tokio_postgres::types::ToSql + Sync),
                &session as &(dyn tokio_postgres::types::ToSql + Sync),
                &now as &(dyn tokio_postgres::types::ToSql + Sync),
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        ).await.map_err(|e| format!("PG update_finding_status: {}", e))?;

        Ok(())
    }

    /// Get a finding by ID.
    pub async fn get_finding(&self, id: &str) -> Result<Option<Finding>, String> {
        let conn = self.pool().get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"SELECT id, task_run_id, detected_in_session, category, severity, status,
                       action_type, title, description, resolution, file_path, line_number,
                       column_number, code_snippet, signature_hash, needs_input, question,
                       input_options, user_response, detected_at, resolved_at,
                       resolved_in_session, updated_at
                FROM task_run_findings
                WHERE id = $1"#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_finding: {}", e))?;

        Ok(row.map(|r| pg_row_to_finding(&r)))
    }

    /// Get all findings for a task run.
    pub async fn get_findings_for_task(&self, task_run_id: &str) -> Result<Vec<Finding>, String> {
        let conn = self.pool().get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, task_run_id, detected_in_session, category, severity, status,
                       action_type, title, description, resolution, file_path, line_number,
                       column_number, code_snippet, signature_hash, needs_input, question,
                       input_options, user_response, detected_at, resolved_at,
                       resolved_in_session, updated_at
                FROM task_run_findings
                WHERE task_run_id = $1
                ORDER BY detected_at ASC"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_findings_for_task: {}", e))?;

        Ok(rows.iter().map(pg_row_to_finding).collect())
    }

    /// Get summary statistics for a task run's findings.
    pub async fn get_finding_summary(&self, task_run_id: &str) -> Result<FindingSummary, String> {
        let findings = self.get_findings_for_task(task_run_id).await?;

        let mut summary = FindingSummary {
            task_run_id: task_run_id.to_string(),
            total: findings.len() as u32,
            ..Default::default()
        };

        for finding in &findings {
            *summary
                .by_category
                .entry(finding.category.as_str().to_string())
                .or_insert(0) += 1;
            *summary
                .by_severity
                .entry(finding.severity.as_str().to_string())
                .or_insert(0) += 1;
            *summary
                .by_status
                .entry(finding.status.as_str().to_string())
                .or_insert(0) += 1;

            if finding.status == FindingStatus::NeedsInput {
                summary.needs_input_count += 1;
            }
            if finding.status == FindingStatus::Resolved {
                summary.resolved_count += 1;
            }
            if !finding.status.is_terminal() && finding.status != FindingStatus::NeedsInput {
                summary.outstanding_count += 1;
            }
        }

        Ok(summary)
    }
}

/// Convert a tokio_postgres::Row to a Finding.
fn pg_row_to_finding(row: &tokio_postgres::Row) -> Finding {
    let category_str: String = row.get(3);
    let severity_str: String = row.get(4);
    let status_str: String = row.get(5);
    let action_type_str: String = row.get(6);

    let file_path: Option<String> = row.get(10);
    let line_number: Option<i32> = row.get(11);
    let column_number: Option<i32> = row.get(12);
    let code_snippet: Option<String> = row.get(13);

    let code_context = if file_path.is_some() || line_number.is_some() {
        Some(FindingCodeContext {
            file: file_path,
            line: line_number.map(|v| v as u32),
            column: column_number.map(|v| v as u32),
            snippet: code_snippet,
        })
    } else {
        None
    };

    let needs_input: bool = row.get::<_, Option<bool>>(15).unwrap_or(false);
    let question: Option<String> = row.get(16);
    let input_options_json: Option<String> = row.get(17);

    let user_input = if let (true, Some(q)) = (needs_input, question) {
        let options =
            input_options_json.and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok());
        Some(FindingUserInput {
            question: q,
            input_type: if options.is_some() {
                "choice".to_string()
            } else {
                "text".to_string()
            },
            options,
        })
    } else {
        None
    };

    let signature_hash: Option<String> = row.get(14);

    // Handle detected_at which is TIMESTAMPTZ in PG
    let detected_at: chrono::DateTime<chrono::Utc> = row.get(19);
    let resolved_at: Option<chrono::DateTime<chrono::Utc>> = row.get(20);
    let updated_at: chrono::DateTime<chrono::Utc> = row.get(22);

    Finding {
        id: row.get(0),
        task_run_id: row.get(1),
        session_num: row.get::<_, i32>(2) as u32,
        category: FindingCategory::from_str(&category_str).unwrap_or(FindingCategory::CodeBug),
        severity: FindingSeverity::from_str(&severity_str).unwrap_or(FindingSeverity::Medium),
        status: FindingStatus::from_str(&status_str).unwrap_or(FindingStatus::Detected),
        action_type: FindingActionType::from_str(&action_type_str)
            .unwrap_or(FindingActionType::AutoFix),
        title: row.get(7),
        description: row.get(8),
        resolution: row.get(9),
        code_context,
        signature_hash: signature_hash.unwrap_or_default(),
        user_input,
        user_response: row.get(18),
        detected_at: detected_at.to_rfc3339(),
        resolved_at: resolved_at.map(|dt| dt.to_rfc3339()),
        resolved_in_session: row.get::<_, Option<i32>>(21).map(|v| v as u32),
        updated_at: updated_at.to_rfc3339(),
    }
}

impl PgDb {
    /// Insert a test failure finding with dedup on signature_hash.
    pub async fn insert_test_failure_finding(
        &self,
        task_run_id: &str,
        session_num: i32,
        title: &str,
        description: &str,
        test_name: &str,
    ) -> Result<String, String> {
        use sha2::{Digest, Sha256};

        let conn = self.pool().get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let signature = format!("test_failure:{}:{}", task_run_id, test_name);
        let mut hasher = Sha256::new();
        hasher.update(signature.as_bytes());
        let signature_hash = format!("{:x}", hasher.finalize());

        // Check for existing
        let existing = conn
            .query_opt(
                "SELECT id FROM task_run_findings WHERE task_run_id = $1 AND signature_hash = $2 AND status NOT IN ('resolved', 'wont_fix')",
                &[&task_run_id, &signature_hash],
            )
            .await
            .map_err(|e| format!("PG test failure dedup check: {}", e))?;

        if let Some(row) = existing {
            return Ok(row.get(0));
        }

        let finding_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"INSERT INTO task_run_findings
               (id, task_run_id, session_num, title, description,
                category, severity, signature_hash, status, is_resolved, created_at)
               VALUES ($1, $2, $3, $4, $5, 'test_failure', 'high', $6, 'active', false, $7)
               ON CONFLICT (id) DO NOTHING"#,
            &[
                &finding_id.as_str(),
                &task_run_id,
                &session_num,
                &title,
                &description,
                &signature_hash.as_str(),
                &now.as_str(),
            ],
        )
        .await
        .map_err(|e| format!("PG insert_test_failure_finding: {}", e))?;

        Ok(finding_id)
    }

    /// Set a user response on a finding.
    pub async fn set_finding_user_response(
        &self,
        id: &str,
        response: &str,
    ) -> Result<(), String> {
        let conn = self.pool().get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE task_run_findings SET user_response = $1, updated_at = NOW() WHERE id = $2",
            &[&response, &id],
        )
        .await
        .map_err(|e| format!("PG set_finding_user_response: {}", e))?;
        Ok(())
    }

    /// Insert a parsed finding from the AI output parser (PG equivalent of finding_storage::insert_finding).
    /// Returns the Finding object.
    pub async fn insert_parsed_finding(
        &self,
        task_run_id: &str,
        session_num: u32,
        parsed: &crate::findings::ParsedFinding,
    ) -> Result<Finding, String> {
        use sha2::{Digest, Sha256};

        let conn = self.pool().get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let now = chrono::Utc::now();
        let now_str = now.to_rfc3339();
        let id = uuid::Uuid::new_v4().to_string();

        // Compute signature hash
        let sig_input = format!(
            "{}:{}:{}:{}",
            parsed.category.as_str(),
            parsed.title,
            parsed.file.as_deref().unwrap_or(""),
            parsed.line.unwrap_or(0),
        );
        let mut hasher = Sha256::new();
        hasher.update(sig_input.as_bytes());
        let signature_hash = format!("{:x}", hasher.finalize());

        // Check for existing finding with same signature
        let existing = conn
            .query_opt(
                "SELECT id FROM task_run_findings WHERE task_run_id = $1 AND signature_hash = $2 AND status NOT IN ('resolved', 'wont_fix')",
                &[&task_run_id, &signature_hash],
            )
            .await
            .map_err(|e| format!("PG finding dedup check: {}", e))?;

        if let Some(row) = existing {
            let existing_id: String = row.get(0);
            return self.get_finding(&existing_id).await?
                .ok_or_else(|| "Existing finding not found after dedup".to_string());
        }

        // Determine action type
        let action_type = if parsed.needs_input {
            "needs_user_input"
        } else {
            match parsed.category {
                FindingCategory::AlreadyFixed | FindingCategory::ExpectedBehavior => "informational",
                _ => "auto_fix",
            }
        };

        // Determine initial status
        let initial_status = if parsed.is_resolved {
            "resolved"
        } else if parsed.needs_input {
            "needs_input"
        } else {
            "detected"
        };

        let input_options_json = parsed
            .options
            .as_ref()
            .map(|opts| serde_json::to_string(opts).unwrap_or_default());

        let session_i32 = session_num as i32;

        conn.execute(
            r#"INSERT INTO task_run_findings (
                id, task_run_id, detected_in_session, category, severity, status,
                action_type, title, description, file_path, line_number, column_number,
                code_snippet, signature_hash, needs_input, question, input_options,
                detected_at, updated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $18)
            ON CONFLICT (id) DO NOTHING"#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &task_run_id,
                &session_i32,
                &parsed.category.as_str(),
                &parsed.severity.as_str(),
                &initial_status,
                &action_type,
                &parsed.title.as_str(),
                &parsed.description.as_str() as &(dyn tokio_postgres::types::ToSql + Sync),
                &parsed.file.as_deref() as &(dyn tokio_postgres::types::ToSql + Sync),
                &parsed.line.map(|l| l as i32) as &(dyn tokio_postgres::types::ToSql + Sync),
                &None::<i32> as &(dyn tokio_postgres::types::ToSql + Sync),
                &None::<String> as &(dyn tokio_postgres::types::ToSql + Sync),
                &signature_hash.as_str(),
                &parsed.needs_input,
                &parsed.question.as_deref() as &(dyn tokio_postgres::types::ToSql + Sync),
                &input_options_json.as_deref() as &(dyn tokio_postgres::types::ToSql + Sync),
                &now_str.as_str(),
            ],
        )
        .await
        .map_err(|e| format!("PG insert_parsed_finding: {}", e))?;

        self.get_finding(&id).await?
            .ok_or_else(|| "Finding not found after PG insert".to_string())
    }
}
