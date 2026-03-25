//! Orchestrator database operations.
//!
//! Contains CheckpointDb methods for verification plans, task knowledge,
//! verification results, workflow constraints, and saved API requests.

use chrono::Utc;
use rusqlite::{params, Result as SqliteResult};
use tracing::info;

use super::types::*;
use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Orchestrator Operations (Verification Plans, Task Knowledge, Results)
    // ========================================================================

    /// Create a new verification plan.
    ///
    /// This is called by the planning agent at task start and on replan requests.
    pub fn create_verification_plan(
        &self,
        task_run_id: &str,
        plan: &crate::orchestrator::VerificationPlan,
        replan_reason: Option<&str>,
        previous_version_id: Option<&str>,
    ) -> Result<StoredVerificationPlan, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        // Serialize the plan
        let plan_json = serde_json::to_string(plan)
            .map_err(|e| format!("Failed to serialize verification plan: {}", e))?;

        let criteria_count = plan.success_criteria.len() as i32;
        let has_ai_criteria = plan.has_ai_criteria();

        conn.execute(
            r#"
            INSERT INTO verification_plans (
                id, task_run_id, version, plan_json, goal_summary,
                criteria_count, has_ai_criteria, replan_reason,
                previous_version_id, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                id,
                task_run_id,
                plan.version,
                plan_json,
                plan.goal_summary,
                criteria_count,
                has_ai_criteria as i32,
                replan_reason,
                previous_version_id,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create verification plan: {}", e))?;

        self.get_verification_plan(&id)?
            .ok_or_else(|| "Failed to retrieve created verification plan".to_string())
    }

    /// Get a verification plan by ID.
    pub fn get_verification_plan(
        &self,
        id: &str,
    ) -> Result<Option<StoredVerificationPlan>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<StoredVerificationPlan> = conn.query_row(
            r#"
            SELECT
                id, task_run_id, version, plan_json, goal_summary,
                criteria_count, has_ai_criteria, replan_reason,
                previous_version_id, created_at
            FROM verification_plans
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(StoredVerificationPlan {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    version: row.get::<_, i64>(2)? as u32,
                    plan_json: row.get(3)?,
                    goal_summary: row.get(4)?,
                    criteria_count: row.get::<_, i64>(5)? as u32,
                    has_ai_criteria: row.get::<_, i32>(6)? != 0,
                    replan_reason: row.get(7).ok(),
                    previous_version_id: row.get(8).ok(),
                    created_at: row.get(9)?,
                })
            },
        );

        match result {
            Ok(plan) => Ok(Some(plan)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get verification plan: {}", e)),
        }
    }

    /// Get the latest verification plan for a task run.
    pub fn get_latest_verification_plan(
        &self,
        task_run_id: &str,
    ) -> Result<Option<StoredVerificationPlan>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<StoredVerificationPlan> = conn.query_row(
            r#"
            SELECT
                id, task_run_id, version, plan_json, goal_summary,
                criteria_count, has_ai_criteria, replan_reason,
                previous_version_id, created_at
            FROM verification_plans
            WHERE task_run_id = ?1
            ORDER BY version DESC
            LIMIT 1
            "#,
            params![task_run_id],
            |row| {
                Ok(StoredVerificationPlan {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    version: row.get::<_, i64>(2)? as u32,
                    plan_json: row.get(3)?,
                    goal_summary: row.get(4)?,
                    criteria_count: row.get::<_, i64>(5)? as u32,
                    has_ai_criteria: row.get::<_, i32>(6)? != 0,
                    replan_reason: row.get(7).ok(),
                    previous_version_id: row.get(8).ok(),
                    created_at: row.get(9)?,
                })
            },
        );

        match result {
            Ok(plan) => Ok(Some(plan)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get latest verification plan: {}", e)),
        }
    }

    /// List all verification plans for a task run (all versions).
    pub fn list_verification_plans(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<StoredVerificationPlan>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    id, task_run_id, version, plan_json, goal_summary,
                    criteria_count, has_ai_criteria, replan_reason,
                    previous_version_id, created_at
                FROM verification_plans
                WHERE task_run_id = ?1
                ORDER BY version ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let plans = stmt
            .query_map(params![task_run_id], |row| {
                Ok(StoredVerificationPlan {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    version: row.get::<_, i64>(2)? as u32,
                    plan_json: row.get(3)?,
                    goal_summary: row.get(4)?,
                    criteria_count: row.get::<_, i64>(5)? as u32,
                    has_ai_criteria: row.get::<_, i32>(6)? != 0,
                    replan_reason: row.get(7).ok(),
                    previous_version_id: row.get(8).ok(),
                    created_at: row.get(9)?,
                })
            })
            .map_err(|e| format!("Failed to query verification plans: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(plans)
    }

    /// Create a task knowledge entry (finding, observation, hypothesis, etc.).
    pub fn create_task_knowledge(
        &self,
        task_run_id: &str,
        category: &str,
        agent_type: &str,
        iteration: u32,
        content: &str,
        evidence: Option<&str>,
        confidence: &str,
        related_files: &[String],
    ) -> Result<StoredTaskKnowledge, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let related_files_json = serde_json::to_string(related_files)
            .map_err(|e| format!("Failed to serialize related_files: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO task_knowledge (
                id, task_run_id, category, agent_type, iteration,
                content, evidence, confidence, related_files,
                is_resolved, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10)
            "#,
            params![
                id,
                task_run_id,
                category,
                agent_type,
                iteration,
                content,
                evidence,
                confidence,
                related_files_json,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create task knowledge: {}", e))?;

        self.get_task_knowledge(&id)?
            .ok_or_else(|| "Failed to retrieve created task knowledge".to_string())
    }

    /// Get a task knowledge entry by ID.
    pub fn get_task_knowledge(&self, id: &str) -> Result<Option<StoredTaskKnowledge>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<StoredTaskKnowledge> = conn.query_row(
            r#"
            SELECT
                id, task_run_id, category, agent_type, iteration,
                content, evidence, confidence, related_files,
                related_criterion_id, is_resolved, resolution_notes,
                resolved_at, created_at
            FROM task_knowledge
            WHERE id = ?1
            "#,
            params![id],
            Self::row_to_task_knowledge,
        );

        match result {
            Ok(knowledge) => Ok(Some(knowledge)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get task knowledge: {}", e)),
        }
    }

    /// List all task knowledge for a task run.
    pub fn list_task_knowledge(
        &self,
        task_run_id: &str,
        category: Option<&str>,
        unresolved_only: bool,
    ) -> Result<Vec<StoredTaskKnowledge>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT
                id, task_run_id, category, agent_type, iteration,
                content, evidence, confidence, related_files,
                related_criterion_id, is_resolved, resolution_notes,
                resolved_at, created_at
            FROM task_knowledge
            WHERE task_run_id = ?1
            "#,
        );

        if unresolved_only {
            sql.push_str(" AND is_resolved = 0");
        }
        if category.is_some() {
            sql.push_str(" AND category = ?2");
        }
        sql.push_str(" ORDER BY created_at ASC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let knowledge: Vec<StoredTaskKnowledge> = if let Some(cat) = category {
            stmt.query_map(params![task_run_id, cat], Self::row_to_task_knowledge)
        } else {
            stmt.query_map(params![task_run_id], Self::row_to_task_knowledge)
        }
        .map_err(|e| format!("Failed to query task knowledge: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

        Ok(knowledge)
    }

    /// Mark a task knowledge entry as resolved.
    pub fn resolve_task_knowledge(
        &self,
        id: &str,
        resolution_notes: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_knowledge SET
                is_resolved = 1,
                resolution_notes = ?2,
                resolved_at = ?3
            WHERE id = ?1
            "#,
            params![id, resolution_notes, now],
        )
        .map_err(|e| format!("Failed to resolve task knowledge: {}", e))?;

        Ok(())
    }

    /// List reflection knowledge from previous runs of the same workflow.
    ///
    /// Joins `task_knowledge` with `task_runs` via `workflow_name` to find
    /// reflection-created entries (recurring_pattern, context) from other runs.
    /// This enables cross-run knowledge persistence.
    pub fn list_workflow_knowledge(
        &self,
        workflow_name: &str,
        exclude_task_run_id: &str,
        categories: &[&str],
        limit: u32,
    ) -> Result<Vec<StoredTaskKnowledge>, String> {
        let conn = self.get_conn()?;

        if categories.is_empty() {
            return Ok(Vec::new());
        }

        // Build placeholders for the IN clause: ?3, ?4, ...
        let placeholders: Vec<String> = (0..categories.len())
            .map(|i| format!("?{}", i + 3))
            .collect();
        let in_clause = placeholders.join(", ");

        let sql = format!(
            r#"
            SELECT
                tk.id, tk.task_run_id, tk.category, tk.agent_type, tk.iteration,
                tk.content, tk.evidence, tk.confidence, tk.related_files,
                tk.related_criterion_id, tk.is_resolved, tk.resolution_notes,
                tk.resolved_at, tk.created_at
            FROM task_knowledge tk
            INNER JOIN task_runs tr ON tk.task_run_id = tr.id
            WHERE tr.workflow_name = ?1
              AND tk.task_run_id != ?2
              AND tk.agent_type = 'reflection'
              AND tk.category IN ({})
            ORDER BY tk.created_at DESC
            LIMIT ?{}
            "#,
            in_clause,
            categories.len() + 3,
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare workflow knowledge query: {}", e))?;

        // Build params: workflow_name, exclude_task_run_id, categories..., limit
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        param_values.push(Box::new(workflow_name.to_string()));
        param_values.push(Box::new(exclude_task_run_id.to_string()));
        for cat in categories {
            param_values.push(Box::new(cat.to_string()));
        }
        param_values.push(Box::new(limit));

        let refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|v| v.as_ref()).collect();

        let knowledge: Vec<StoredTaskKnowledge> = stmt
            .query_map(refs.as_slice(), Self::row_to_task_knowledge)
            .map_err(|e| format!("Failed to query workflow knowledge: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(knowledge)
    }

    /// Query knowledge from OTHER workflows with similar names (cross-workflow learning).
    ///
    /// Splits the given workflow name into keywords and searches for knowledge entries
    /// from task runs whose workflow_name contains any of those keywords, excluding the
    /// current task run. Returns tuples of (workflow_name, knowledge_content).
    ///
    /// This enables learning from similar workflows — e.g., if you're running "fix-login-page",
    /// you might benefit from knowledge discovered during "fix-signup-page".
    pub fn get_cross_workflow_knowledge(
        &self,
        workflow_name: &str,
        exclude_task_run_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>, String> {
        let conn = self.get_conn()?;

        // Extract meaningful keywords from workflow name (split on spaces, hyphens, underscores)
        let keywords: Vec<&str> = workflow_name
            .split([' ', '-', '_', '>'])
            .map(|s| s.trim())
            .filter(|s| s.len() >= 3) // Skip short words like "a", "to", etc.
            .collect();

        if keywords.is_empty() {
            return Ok(Vec::new());
        }

        // Build LIKE conditions: workflow_name LIKE '%keyword1%' OR '%keyword2%' ...
        let like_conditions: Vec<String> = keywords
            .iter()
            .enumerate()
            .map(|(i, _)| format!("tr.workflow_name LIKE ?{}", i + 3))
            .collect();
        let where_clause = like_conditions.join(" OR ");

        let sql = format!(
            r#"
            SELECT DISTINCT tr.workflow_name, tk.content
            FROM task_knowledge tk
            INNER JOIN task_runs tr ON tk.task_run_id = tr.id
            WHERE ({})
              AND tk.task_run_id != ?1
              AND tr.workflow_name != ?2
              AND tk.category IN ('recurring_pattern', 'context', 'solution', 'root_cause')
              AND tk.confidence IN ('high', 'medium')
            ORDER BY tk.created_at DESC
            LIMIT ?{}
            "#,
            where_clause,
            keywords.len() + 3,
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare cross-workflow knowledge query: {}", e))?;

        // Build params: exclude_task_run_id, workflow_name (for exact exclusion), keyword LIKE patterns..., limit
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        param_values.push(Box::new(exclude_task_run_id.to_string()));
        param_values.push(Box::new(workflow_name.to_string()));
        for keyword in &keywords {
            param_values.push(Box::new(format!("%{}%", keyword)));
        }
        param_values.push(Box::new(limit as u32));

        let refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|v| v.as_ref()).collect();

        let results: Vec<(String, String)> = stmt
            .query_map(refs.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("Failed to query cross-workflow knowledge: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Query project-scoped knowledge entries for a given project path.
    ///
    /// Returns knowledge entries created by project reflection workflows
    /// that analyzed runs targeting the same project directory.
    pub fn list_project_knowledge(
        &self,
        project_path: &str,
        exclude_task_run_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT tk.category, tk.content
                FROM task_knowledge tk
                WHERE tk.project_path = ?1
                  AND tk.task_run_id != ?2
                  AND tk.agent_type = 'reflection'
                  AND tk.category IN (
                      'project_environment', 'project_architecture',
                      'project_test_pattern', 'project_recurring_issue'
                  )
                  AND tk.confidence IN ('high', 'medium')
                ORDER BY tk.created_at DESC
                LIMIT ?3
                "#,
            )
            .map_err(|e| format!("Failed to prepare project knowledge query: {}", e))?;

        let results: Vec<(String, String)> = stmt
            .query_map(
                rusqlite::params![project_path, exclude_task_run_id, limit as u32],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(|e| format!("Failed to query project knowledge: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Set the project_path on a task_knowledge entry.
    pub fn set_knowledge_project_path(
        &self,
        knowledge_id: &str,
        project_path: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE task_knowledge SET project_path = ?1 WHERE id = ?2",
            rusqlite::params![project_path, knowledge_id],
        )
        .map_err(|e| format!("Failed to set knowledge project_path: {}", e))?;
        Ok(())
    }

    /// Helper function to convert a row to StoredTaskKnowledge.
    fn row_to_task_knowledge(row: &rusqlite::Row) -> rusqlite::Result<StoredTaskKnowledge> {
        Ok(StoredTaskKnowledge {
            id: row.get(0)?,
            task_run_id: row.get(1)?,
            category: row.get(2)?,
            agent_type: row.get(3)?,
            iteration: row.get::<_, i64>(4)? as u32,
            content: row.get(5)?,
            evidence: row.get(6).ok(),
            confidence: row.get(7)?,
            related_files: row
                .get::<_, Option<String>>(8)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            related_criterion_id: row.get(9).ok(),
            is_resolved: row.get::<_, i32>(10)? != 0,
            resolution_notes: row.get(11).ok(),
            resolved_at: row.get(12).ok(),
            created_at: row.get(13)?,
        })
    }

    /// Create an orchestrator verification result.
    pub fn create_orchestrator_verification_result(
        &self,
        task_run_id: &str,
        plan_id: &str,
        iteration: u32,
        result: &crate::orchestrator::VerificationResult,
        is_critical: bool,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let criterion_type = match result.criterion_type {
            crate::orchestrator::CriterionType::Deterministic => "deterministic",
            crate::orchestrator::CriterionType::AiEvaluated => "ai_evaluated",
        };

        let confidence = result.confidence.map(|c| match c {
            crate::orchestrator::Confidence::High => "high",
            crate::orchestrator::Confidence::Medium => "medium",
            crate::orchestrator::Confidence::Low => "low",
        });

        let observations_json = serde_json::to_string(&result.observations)
            .map_err(|e| format!("Failed to serialize observations: {}", e))?;
        let issues_json = serde_json::to_string(&result.issues)
            .map_err(|e| format!("Failed to serialize issues: {}", e))?;
        let suggestions_json = serde_json::to_string(&result.suggestions)
            .map_err(|e| format!("Failed to serialize suggestions: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO orchestrator_verification_results (
                id, task_run_id, plan_id, iteration, criterion_id,
                criterion_type, passed, is_critical, confidence,
                observations, issues, suggestions, raw_output, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            "#,
            params![
                id,
                task_run_id,
                plan_id,
                iteration,
                result.criterion_id,
                criterion_type,
                result.passed as i32,
                is_critical as i32,
                confidence,
                observations_json,
                issues_json,
                suggestions_json,
                result.raw_output,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create verification result: {}", e))?;

        Ok(id)
    }

    /// Get all verification results for a specific iteration.
    pub fn get_iteration_verification_results(
        &self,
        task_run_id: &str,
        iteration: u32,
    ) -> Result<Vec<StoredVerificationResult>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    id, task_run_id, plan_id, iteration, criterion_id,
                    criterion_type, passed, is_critical, confidence,
                    observations, issues, suggestions, raw_output, created_at
                FROM orchestrator_verification_results
                WHERE task_run_id = ?1 AND iteration = ?2
                ORDER BY created_at ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results = stmt
            .query_map(params![task_run_id, iteration], |row| {
                Ok(StoredVerificationResult {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    plan_id: row.get(2)?,
                    iteration: row.get::<_, i64>(3)? as u32,
                    criterion_id: row.get(4)?,
                    criterion_type: row.get(5)?,
                    passed: row.get::<_, i32>(6)? != 0,
                    is_critical: row.get::<_, i32>(7)? != 0,
                    confidence: row.get(8).ok(),
                    observations: row
                        .get::<_, Option<String>>(9)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    issues: row
                        .get::<_, Option<String>>(10)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    suggestions: row
                        .get::<_, Option<String>>(11)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    raw_output: row.get(12).ok(),
                    created_at: row.get(13)?,
                })
            })
            .map_err(|e| format!("Failed to query verification results: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get the latest verification results for a task (most recent iteration).
    pub fn get_latest_verification_results(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<StoredVerificationResult>, String> {
        let conn = self.get_conn()?;

        // First get the max iteration
        let max_iteration: Option<i64> = conn
            .query_row(
                "SELECT MAX(iteration) FROM orchestrator_verification_results WHERE task_run_id = ?1",
                params![task_run_id],
                |row| row.get(0),
            )
            .ok();

        match max_iteration {
            Some(iteration) => {
                self.get_iteration_verification_results(task_run_id, iteration as u32)
            }
            None => Ok(vec![]),
        }
    }

    // ========================================================================
    // Workflow Verification Phase Results (Step-Executor Based)
    // ========================================================================

    /// Store a verification phase result from unified workflow execution.
    ///
    /// This stores the results from `execute_verification_steps` in the step_executor,
    /// which uses the workflow's explicit `verification_steps` (tests, checks) rather
    /// than the orchestrator's AI-generated verification criteria.
    ///
    /// Uses upsert semantics: if a result already exists for (task_run_id, iteration),
    /// it will be updated with the new data while preserving the original id and created_at.
    pub fn store_verification_phase_result(
        &self,
        task_run_id: &str,
        iteration: u32,
        result: &serde_json::Value,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Extract summary fields from the result
        let all_passed = result
            .get("all_passed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let total_steps = result
            .get("total_steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as i32;
        let passed_steps = result
            .get("passed_steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as i32;
        let failed_steps = result
            .get("failed_steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as i32;
        let skipped_steps = result
            .get("skipped_steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as i32;
        let total_duration_ms = result
            .get("total_duration_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as i64;
        let critical_failure = result
            .get("critical_failure")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let result_json = serde_json::to_string(result)
            .map_err(|e| format!("Failed to serialize verification result: {}", e))?;

        // Check if a result already exists for this task_run_id and iteration
        let existing_id: Option<String> = conn
            .query_row(
                "SELECT id FROM workflow_verification_phase_results WHERE task_run_id = ?1 AND iteration = ?2",
                params![task_run_id, iteration],
                |row| row.get(0),
            )
            .ok();

        let id = if let Some(existing_id) = existing_id {
            // Update existing record, preserving id and created_at
            conn.execute(
                r#"
                UPDATE workflow_verification_phase_results
                SET all_passed = ?1, total_steps = ?2, passed_steps = ?3, failed_steps = ?4,
                    skipped_steps = ?5, total_duration_ms = ?6, critical_failure = ?7, result_json = ?8
                WHERE task_run_id = ?9 AND iteration = ?10
                "#,
                params![
                    all_passed as i32,
                    total_steps,
                    passed_steps,
                    failed_steps,
                    skipped_steps,
                    total_duration_ms,
                    critical_failure as i32,
                    result_json,
                    task_run_id,
                    iteration,
                ],
            )
            .map_err(|e| format!("Failed to update verification phase result: {}", e))?;

            info!(
                "Updated verification phase result for task {} iteration {}: all_passed={}, {}/{} steps",
                task_run_id, iteration, all_passed, passed_steps, total_steps
            );

            existing_id
        } else {
            // Insert new record
            let new_id = uuid::Uuid::new_v4().to_string();

            conn.execute(
                r#"
                INSERT INTO workflow_verification_phase_results (
                    id, task_run_id, iteration,
                    all_passed, total_steps, passed_steps, failed_steps, skipped_steps,
                    total_duration_ms, critical_failure, result_json, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                "#,
                params![
                    new_id,
                    task_run_id,
                    iteration,
                    all_passed as i32,
                    total_steps,
                    passed_steps,
                    failed_steps,
                    skipped_steps,
                    total_duration_ms,
                    critical_failure as i32,
                    result_json,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to store verification phase result: {}", e))?;

            info!(
                "Stored verification phase result for task {} iteration {}: all_passed={}, {}/{} steps",
                task_run_id, iteration, all_passed, passed_steps, total_steps
            );

            new_id
        };

        Ok(id)
    }

    /// Delete all verification phase results for a task run.
    /// Used when starting a fresh run to clear stale data from previous interrupted runs.
    pub fn delete_verification_phase_results(&self, task_run_id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;

        conn.execute(
            "DELETE FROM workflow_verification_phase_results WHERE task_run_id = ?1",
            params![task_run_id],
        )
        .map_err(|e| format!("Failed to delete verification phase results: {}", e))?;

        info!(
            "Deleted verification phase results for task {}",
            task_run_id
        );
        Ok(())
    }

    /// Get verification phase results for a specific iteration.
    pub fn get_verification_phase_result(
        &self,
        task_run_id: &str,
        iteration: u32,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<String> = conn.query_row(
            r#"
            SELECT result_json FROM workflow_verification_phase_results
            WHERE task_run_id = ?1 AND iteration = ?2
            LIMIT 1
            "#,
            params![task_run_id, iteration],
            |row| row.get(0),
        );

        match result {
            Ok(json_str) => {
                let parsed: serde_json::Value = serde_json::from_str(&json_str)
                    .map_err(|e| format!("Failed to parse verification result JSON: {}", e))?;
                Ok(Some(parsed))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get verification phase result: {}", e)),
        }
    }

    /// Get all verification phase results for a task run.
    /// With the unique constraint on (task_run_id, iteration), there's exactly one result per iteration.
    pub fn get_all_verification_phase_results(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT result_json FROM workflow_verification_phase_results
                WHERE task_run_id = ?1
                ORDER BY iteration ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results: Vec<serde_json::Value> = stmt
            .query_map(params![task_run_id], |row| {
                let json_str: String = row.get(0)?;
                Ok(json_str)
            })
            .map_err(|e| format!("Failed to query verification results: {}", e))?
            .filter_map(|r| r.ok())
            .filter_map(|json_str| serde_json::from_str(&json_str).ok())
            .collect();

        Ok(results)
    }

    /// Get the first running task along with its step execution events in a single call.
    /// Returns None if no tasks are running.
    /// This is an optimized batch query that avoids two separate round-trips.
    pub fn get_running_task_step_data(
        &self,
    ) -> Result<Option<(TaskRun, Vec<TaskRunEvent>)>, String> {
        let running = self.get_running_task_runs(self.get_runner_port())?;
        let task = match running.into_iter().next() {
            Some(t) => t,
            None => return Ok(None),
        };

        // TODO: Wire to PG when orchestrator_ops callers go async
        let events = self.get_task_run_events(&task.id, None, None)?;
        Ok(Some((task, events)))
    }

    /// Get the set of iteration numbers that have completed verification phase results.
    /// Returns only the iteration integers, not the full result payloads.
    pub fn get_completed_verification_iterations(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<i64>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT iteration FROM workflow_verification_phase_results
                WHERE task_run_id = ?1
                ORDER BY iteration ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let iterations: Vec<i64> = stmt
            .query_map(params![task_run_id], |row| row.get(0))
            .map_err(|e| format!("Failed to query completed iterations: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(iterations)
    }

    // ========================================================================
    // Workflow Constraint Results
    // ========================================================================

    /// Store constraint evaluation results for a given iteration.
    ///
    /// Each `ConstraintResult` is stored as a separate row, enabling per-constraint
    /// queries. Violations are serialized as a JSON array.
    pub fn store_constraint_results(
        &self,
        task_run_id: &str,
        iteration: u32,
        results: &[crate::constraint_engine::ConstraintResult],
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();

        // Delete any existing results for this (task_run_id, iteration) to support upsert semantics
        conn.execute(
            "DELETE FROM workflow_constraint_results WHERE task_run_id = ?1 AND iteration = ?2",
            params![task_run_id, iteration as i64],
        )
        .map_err(|e| format!("Failed to delete old constraint results: {}", e))?;

        for result in results {
            let severity_str = serde_json::to_value(result.severity)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| format!("{:?}", result.severity).to_lowercase());

            let violations_json = if result.violations.is_empty() {
                None
            } else {
                Some(
                    serde_json::to_string(&result.violations)
                        .map_err(|e| format!("Failed to serialize constraint violations: {}", e))?,
                )
            };

            conn.execute(
                r#"
                INSERT INTO workflow_constraint_results (
                    task_run_id, iteration, constraint_id, constraint_name,
                    passed, severity, violations_json, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
                params![
                    task_run_id,
                    iteration as i64,
                    result.constraint_id,
                    result.constraint_name,
                    result.passed as i32,
                    severity_str,
                    violations_json,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to store constraint result: {}", e))?;
        }

        let failed_count = results.iter().filter(|r| !r.passed).count();
        info!(
            "Stored {} constraint results for task {} iteration {} ({} failed)",
            results.len(),
            task_run_id,
            iteration,
            failed_count
        );

        Ok(())
    }

    /// Delete all constraint results for a task run.
    /// Used when starting a fresh run to clear stale data from previous interrupted runs.
    pub fn delete_constraint_results(&self, task_run_id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;

        conn.execute(
            "DELETE FROM workflow_constraint_results WHERE task_run_id = ?1",
            params![task_run_id],
        )
        .map_err(|e| format!("Failed to delete constraint results: {}", e))?;

        info!("Deleted constraint results for task {}", task_run_id);
        Ok(())
    }

    /// Get constraint results for a task run, optionally filtered by iteration.
    /// Returns results as JSON values for flexibility.
    pub fn get_constraint_results(
        &self,
        task_run_id: &str,
        iteration: Option<u32>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        // Row mapper shared by both query branches
        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<serde_json::Value> {
            let constraint_id: String = row.get(0)?;
            let constraint_name: String = row.get(1)?;
            let passed: i32 = row.get(2)?;
            let severity: String = row.get(3)?;
            let violations_json: Option<String> = row.get(4)?;
            let iteration: i64 = row.get(5)?;
            let created_at: String = row.get(6)?;

            let violations: serde_json::Value = violations_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(serde_json::Value::Array(vec![]));

            Ok(serde_json::json!({
                "constraint_id": constraint_id,
                "constraint_name": constraint_name,
                "passed": passed != 0,
                "severity": severity,
                "violations": violations,
                "iteration": iteration,
                "created_at": created_at,
            }))
        };

        let rows: Vec<serde_json::Value> = if let Some(iter) = iteration {
            let mut stmt = conn
                .prepare(
                    r#"
                    SELECT constraint_id, constraint_name, passed, severity, violations_json, iteration, created_at
                    FROM workflow_constraint_results
                    WHERE task_run_id = ?1 AND iteration = ?2
                    ORDER BY id ASC
                    "#,
                )
                .map_err(|e| format!("Failed to prepare constraint results query: {}", e))?;

            let results = stmt
                .query_map(params![task_run_id, iter as i64], &map_row)
                .map_err(|e| format!("Failed to query constraint results: {}", e))?
                .filter_map(|r| r.ok())
                .collect();
            results
        } else {
            let mut stmt = conn
                .prepare(
                    r#"
                    SELECT constraint_id, constraint_name, passed, severity, violations_json, iteration, created_at
                    FROM workflow_constraint_results
                    WHERE task_run_id = ?1
                    ORDER BY iteration ASC, id ASC
                    "#,
                )
                .map_err(|e| format!("Failed to prepare constraint results query: {}", e))?;

            let results = stmt
                .query_map(params![task_run_id], &map_row)
                .map_err(|e| format!("Failed to query constraint results: {}", e))?
                .filter_map(|r| r.ok())
                .collect();
            results
        };

        Ok(rows)
    }

    // ========================================================================
    // Saved API Requests Operations
    // ========================================================================

    /// List all saved API requests
    pub fn list_saved_api_requests(
        &self,
    ) -> Result<Vec<crate::saved_api_requests::SavedApiRequest>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, category, tags, method, url, headers, body,
                       body_content_type, timeout_ms, follow_redirects, variable_extractions,
                       assertions, credential_id, created_at, updated_at
                FROM saved_api_requests
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let requests = stmt
            .query_map([], |row| {
                use crate::api_request::types::HttpMethod;

                let method_str: String = row.get(5)?;
                let method = match method_str.to_uppercase().as_str() {
                    "GET" => HttpMethod::Get,
                    "POST" => HttpMethod::Post,
                    "PUT" => HttpMethod::Put,
                    "PATCH" => HttpMethod::Patch,
                    "DELETE" => HttpMethod::Delete,
                    _ => HttpMethod::Get,
                };

                Ok(crate::saved_api_requests::SavedApiRequest {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    tags: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    method,
                    url: row.get(6)?,
                    headers: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    body: row.get(8)?,
                    body_content_type: row.get(9)?,
                    timeout_ms: row.get::<_, i64>(10)? as u64,
                    follow_redirects: row.get::<_, i32>(11)? != 0,
                    variable_extractions: row
                        .get::<_, Option<String>>(12)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    assertions: row
                        .get::<_, Option<String>>(13)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    credential_id: row.get(14)?,
                    created_at: row.get(15)?,
                    updated_at: row.get(16)?,
                })
            })
            .map_err(|e| format!("Failed to query saved API requests: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(requests)
    }

    /// Get a single saved API request by ID
    pub fn get_saved_api_request(
        &self,
        id: &str,
    ) -> Result<Option<crate::saved_api_requests::SavedApiRequest>, String> {
        let conn = self.get_conn()?;

        let result = conn.query_row(
            r#"
            SELECT id, name, description, category, tags, method, url, headers, body,
                   body_content_type, timeout_ms, follow_redirects, variable_extractions,
                   assertions, credential_id, created_at, updated_at
            FROM saved_api_requests
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                use crate::api_request::types::HttpMethod;

                let method_str: String = row.get(5)?;
                let method = match method_str.to_uppercase().as_str() {
                    "GET" => HttpMethod::Get,
                    "POST" => HttpMethod::Post,
                    "PUT" => HttpMethod::Put,
                    "PATCH" => HttpMethod::Patch,
                    "DELETE" => HttpMethod::Delete,
                    _ => HttpMethod::Get,
                };

                Ok(crate::saved_api_requests::SavedApiRequest {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    tags: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    method,
                    url: row.get(6)?,
                    headers: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    body: row.get(8)?,
                    body_content_type: row.get(9)?,
                    timeout_ms: row.get::<_, i64>(10)? as u64,
                    follow_redirects: row.get::<_, i32>(11)? != 0,
                    variable_extractions: row
                        .get::<_, Option<String>>(12)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    assertions: row
                        .get::<_, Option<String>>(13)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    credential_id: row.get(14)?,
                    created_at: row.get(15)?,
                    updated_at: row.get(16)?,
                })
            },
        );

        match result {
            Ok(request) => Ok(Some(request)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get saved API request: {}", e)),
        }
    }

    /// Create a new saved API request
    pub fn create_saved_api_request(
        &self,
        request: &crate::saved_api_requests::CreateSavedApiRequestRequest,
    ) -> Result<crate::saved_api_requests::SavedApiRequest, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let method_str = format!("{}", request.method);
        let tags_json = serde_json::to_string(&request.tags).unwrap_or_else(|_| "[]".to_string());
        let headers_json =
            serde_json::to_string(&request.headers).unwrap_or_else(|_| "{}".to_string());
        let extractions_json = serde_json::to_string(&request.variable_extractions)
            .unwrap_or_else(|_| "[]".to_string());
        let assertions_json =
            serde_json::to_string(&request.assertions).unwrap_or_else(|_| "[]".to_string());

        conn.execute(
            r#"
            INSERT INTO saved_api_requests (
                id, name, description, category, tags, method, url, headers, body,
                body_content_type, timeout_ms, follow_redirects, variable_extractions,
                assertions, credential_id, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
            "#,
            params![
                id,
                request.name,
                request.description,
                request.category,
                tags_json,
                method_str,
                request.url,
                headers_json,
                request.body,
                request.body_content_type,
                request.timeout_ms as i64,
                request.follow_redirects as i32,
                extractions_json,
                assertions_json,
                request.credential_id,
                now,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create saved API request: {}", e))?;

        self.get_saved_api_request(&id)?
            .ok_or_else(|| "Failed to retrieve created request".to_string())
    }

    /// Update a saved API request
    pub fn update_saved_api_request(
        &self,
        id: &str,
        request: &crate::saved_api_requests::UpdateSavedApiRequestRequest,
    ) -> Result<crate::saved_api_requests::SavedApiRequest, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Get current values
        let current = self
            .get_saved_api_request(id)?
            .ok_or_else(|| format!("Saved API request not found: {}", id))?;

        let name = request.name.as_ref().unwrap_or(&current.name);
        let description = request.description.as_ref().unwrap_or(&current.description);
        let category = request.category.as_ref().unwrap_or(&current.category);
        let tags = request.tags.as_ref().unwrap_or(&current.tags);
        let method = request.method.unwrap_or(current.method);
        let url = request.url.as_ref().unwrap_or(&current.url);
        let headers = request.headers.as_ref().unwrap_or(&current.headers);
        let body = request.body.as_ref().or(current.body.as_ref());
        let body_content_type = request
            .body_content_type
            .as_ref()
            .or(current.body_content_type.as_ref());
        let timeout_ms = request.timeout_ms.unwrap_or(current.timeout_ms);
        let follow_redirects = request.follow_redirects.unwrap_or(current.follow_redirects);
        let variable_extractions = request
            .variable_extractions
            .as_ref()
            .unwrap_or(&current.variable_extractions);
        let assertions = request.assertions.as_ref().unwrap_or(&current.assertions);
        let credential_id = request
            .credential_id
            .as_ref()
            .or(current.credential_id.as_ref());

        let method_str = format!("{}", method);
        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
        let headers_json = serde_json::to_string(headers).unwrap_or_else(|_| "{}".to_string());
        let extractions_json =
            serde_json::to_string(variable_extractions).unwrap_or_else(|_| "[]".to_string());
        let assertions_json =
            serde_json::to_string(assertions).unwrap_or_else(|_| "[]".to_string());

        conn.execute(
            r#"
            UPDATE saved_api_requests SET
                name = ?1, description = ?2, category = ?3, tags = ?4, method = ?5,
                url = ?6, headers = ?7, body = ?8, body_content_type = ?9, timeout_ms = ?10,
                follow_redirects = ?11, variable_extractions = ?12, assertions = ?13,
                credential_id = ?14, updated_at = ?15
            WHERE id = ?16
            "#,
            params![
                name,
                description,
                category,
                tags_json,
                method_str,
                url,
                headers_json,
                body,
                body_content_type,
                timeout_ms as i64,
                follow_redirects as i32,
                extractions_json,
                assertions_json,
                credential_id,
                now,
                id,
            ],
        )
        .map_err(|e| format!("Failed to update saved API request: {}", e))?;

        self.get_saved_api_request(id)?
            .ok_or_else(|| "Failed to retrieve updated request".to_string())
    }

    /// Delete a saved API request
    pub fn delete_saved_api_request(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let deleted = conn
            .execute("DELETE FROM saved_api_requests WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete saved API request: {}", e))?;

        Ok(deleted > 0)
    }

    /// Search saved API requests
    pub fn search_saved_api_requests(
        &self,
        query: &crate::saved_api_requests::SearchSavedApiRequestsQuery,
    ) -> Result<Vec<crate::saved_api_requests::SavedApiRequest>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, name, description, category, tags, method, url, headers, body,
                   body_content_type, timeout_ms, follow_redirects, variable_extractions,
                   assertions, credential_id, created_at, updated_at
            FROM saved_api_requests
            WHERE 1=1
            "#,
        );

        let mut params_vec: Vec<String> = vec![];

        if let Some(q) = &query.q {
            sql.push_str(" AND (name LIKE ?1 OR description LIKE ?1 OR url LIKE ?1)");
            params_vec.push(format!("%{}%", q));
        }

        if let Some(category) = &query.category {
            let idx = params_vec.len() + 1;
            sql.push_str(&format!(" AND category = ?{}", idx));
            params_vec.push(category.clone());
        }

        if let Some(tag) = &query.tag {
            let idx = params_vec.len() + 1;
            sql.push_str(&format!(" AND tags LIKE ?{}", idx));
            params_vec.push(format!("%\"{}%", tag));
        }

        sql.push_str(" ORDER BY updated_at DESC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();

        let requests = stmt
            .query_map(params_refs.as_slice(), |row| {
                use crate::api_request::types::HttpMethod;

                let method_str: String = row.get(5)?;
                let method = match method_str.to_uppercase().as_str() {
                    "GET" => HttpMethod::Get,
                    "POST" => HttpMethod::Post,
                    "PUT" => HttpMethod::Put,
                    "PATCH" => HttpMethod::Patch,
                    "DELETE" => HttpMethod::Delete,
                    _ => HttpMethod::Get,
                };

                Ok(crate::saved_api_requests::SavedApiRequest {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    tags: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    method,
                    url: row.get(6)?,
                    headers: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    body: row.get(8)?,
                    body_content_type: row.get(9)?,
                    timeout_ms: row.get::<_, i64>(10)? as u64,
                    follow_redirects: row.get::<_, i32>(11)? != 0,
                    variable_extractions: row
                        .get::<_, Option<String>>(12)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    assertions: row
                        .get::<_, Option<String>>(13)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    credential_id: row.get(14)?,
                    created_at: row.get(15)?,
                    updated_at: row.get(16)?,
                })
            })
            .map_err(|e| format!("Failed to search saved API requests: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(requests)
    }

    /// Get all unique categories from saved API requests
    pub fn get_saved_api_request_categories(&self) -> Result<Vec<String>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare("SELECT DISTINCT category FROM saved_api_requests WHERE category != '' ORDER BY category")
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let categories = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| format!("Failed to query categories: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(categories)
    }

    /// Get all unique tags from saved API requests
    pub fn get_saved_api_request_tags(&self) -> Result<Vec<String>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare("SELECT tags FROM saved_api_requests WHERE tags != '[]'")
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let mut all_tags: std::collections::HashSet<String> = std::collections::HashSet::new();

        let tags_strings: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("Failed to query tags: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        for tags_json in tags_strings {
            if let Ok(tags) = serde_json::from_str::<Vec<String>>(&tags_json) {
                for tag in tags {
                    all_tags.insert(tag);
                }
            }
        }

        let mut result: Vec<String> = all_tags.into_iter().collect();
        result.sort();
        Ok(result)
    }

    /// Duplicate a saved API request
    pub fn duplicate_saved_api_request(
        &self,
        id: &str,
    ) -> Result<crate::saved_api_requests::SavedApiRequest, String> {
        let original = self
            .get_saved_api_request(id)?
            .ok_or_else(|| format!("Saved API request not found: {}", id))?;

        let create_request = crate::saved_api_requests::CreateSavedApiRequestRequest {
            name: format!("{} (Copy)", original.name),
            description: original.description,
            category: original.category,
            tags: original.tags,
            method: original.method,
            url: original.url,
            headers: original.headers,
            body: original.body,
            body_content_type: original.body_content_type,
            timeout_ms: original.timeout_ms,
            follow_redirects: original.follow_redirects,
            variable_extractions: original.variable_extractions,
            assertions: original.assertions,
            credential_id: original.credential_id,
        };

        self.create_saved_api_request(&create_request)
    }
}
