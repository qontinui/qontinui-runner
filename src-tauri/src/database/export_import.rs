//! Data export, import, and mobile development operations.
//!
//! Contains CheckpointDb methods for data export/import,
//! backup, mobile state, MCP servers, and MCP calls.

use chrono::Utc;
use rusqlite::{params, OptionalExtension, Result as SqliteResult};

use super::types::*;
use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================

    /// Get a summary of all exportable data counts.
    pub fn get_export_summary(&self) -> Result<serde_json::Value, String> {
        let conn = self.get_conn()?;

        let flows_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM orchestrator_flows", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        let flow_executions_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM flow_executions", [], |row| row.get(0))
            .unwrap_or(0);

        let checkpoints_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM orchestrator_checkpoints", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        let learning_outcomes_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM learning_outcomes", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        let learning_patterns_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM learning_patterns", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        let settings_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))
            .unwrap_or(0);

        let prompts_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
            .unwrap_or(0);

        let ai_workflows_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ai_workflows", [], |row| row.get(0))
            .unwrap_or(0);

        let unified_workflows_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM unified_workflows", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        let verification_tests_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM verification_tests", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        let task_hooks_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_hooks", [], |row| row.get(0))
            .unwrap_or(0);

        let scheduled_tasks_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM scheduled_tasks", [], |row| row.get(0))
            .unwrap_or(0);

        let saved_api_requests_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM saved_api_requests", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        let configs_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM configs", [], |row| row.get(0))
            .unwrap_or(0);

        Ok(serde_json::json!({
            "flows": flows_count,
            "flow_executions": flow_executions_count,
            "checkpoints": checkpoints_count,
            "learning_outcomes": learning_outcomes_count,
            "learning_patterns": learning_patterns_count,
            "settings": settings_count,
            "prompts": prompts_count,
            "ai_workflows": ai_workflows_count,
            "unified_workflows": unified_workflows_count,
            "verification_tests": verification_tests_count,
            "task_hooks": task_hooks_count,
            "scheduled_tasks": scheduled_tasks_count,
            "saved_api_requests": saved_api_requests_count,
            "configs": configs_count,
        }))
    }

    /// Export all flows (orchestrator_flows table).
    pub fn export_all_flows(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, definition_json, tags, version, created_at, updated_at
                FROM orchestrator_flows
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let tags_str: String = row.get(4)?;
                let tags: serde_json::Value =
                    serde_json::from_str(&tags_str).unwrap_or(serde_json::json!([]));
                let definition_str: String = row.get(3)?;
                let definition: serde_json::Value =
                    serde_json::from_str(&definition_str).unwrap_or(serde_json::json!({}));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "definition": definition,
                    "tags": tags,
                    "version": row.get::<_, String>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                    "updated_at": row.get::<_, String>(7)?,
                }))
            })
            .map_err(|e| format!("Failed to export flows: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all flow executions (flow_executions table).
    pub fn export_all_flow_executions(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT instance_id, flow_id, current_step, context_json, status, error, step_results_json, started_at, completed_at
                FROM flow_executions
                ORDER BY started_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let context_str: Option<String> = row.get(3)?;
                let context: serde_json::Value = context_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!({}));
                let step_results_str: Option<String> = row.get(6)?;
                let step_results: serde_json::Value = step_results_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!([]));

                Ok(serde_json::json!({
                    "instance_id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "current_step": row.get::<_, Option<String>>(2)?,
                    "context": context,
                    "status": row.get::<_, String>(4)?,
                    "error": row.get::<_, Option<String>>(5)?,
                    "step_results": step_results,
                    "started_at": row.get::<_, String>(7)?,
                    "completed_at": row.get::<_, Option<String>>(8)?,
                }))
            })
            .map_err(|e| format!("Failed to export flow executions: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all orchestrator checkpoints.
    pub fn export_all_orchestrator_checkpoints(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_id, iteration, trigger, state, name, created_at
                FROM orchestrator_checkpoints
                ORDER BY created_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let state_str: String = row.get(4)?;
                let state: serde_json::Value =
                    serde_json::from_str(&state_str).unwrap_or(serde_json::json!({}));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": state,
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to export checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all settings.
    pub fn export_all_settings(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare("SELECT key, value, updated_at FROM settings ORDER BY key")
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let value_str: String = row.get(1)?;
                let value: serde_json::Value =
                    serde_json::from_str(&value_str).unwrap_or(serde_json::Value::Null);

                Ok(serde_json::json!({
                    "key": row.get::<_, String>(0)?,
                    "value": value,
                    "updated_at": row.get::<_, String>(2)?,
                }))
            })
            .map_err(|e| format!("Failed to export settings: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all prompts.
    pub fn export_all_prompts(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, category, content, variables, created_at, updated_at
                FROM prompts
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let vars_str: String = row.get(4)?;
                let variables: serde_json::Value =
                    serde_json::from_str(&vars_str).unwrap_or(serde_json::json!([]));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "category": row.get::<_, Option<String>>(2)?,
                    "content": row.get::<_, String>(3)?,
                    "variables": variables,
                    "created_at": row.get::<_, String>(5)?,
                    "updated_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to export prompts: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all AI workflows.
    pub fn export_all_ai_workflows(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, config, created_at, updated_at
                FROM ai_workflows
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let config_str: String = row.get(3)?;
                let config: serde_json::Value =
                    serde_json::from_str(&config_str).unwrap_or(serde_json::json!({}));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "config": config,
                    "created_at": row.get::<_, String>(4)?,
                    "updated_at": row.get::<_, String>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to export AI workflows: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all unified workflows.
    pub fn export_all_unified_workflows(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, category, tags, setup_steps, verification_steps,
                       agentic_steps, max_iterations, provider, model, created_at, updated_at,
                       completion_steps, skip_ai_summary, timeout_seconds,
                       log_watch_enabled, health_check_enabled, health_check_urls,
                       preflight_check_enabled, log_source_selection, context_ids,
                       disabled_context_ids, auto_include_contexts, prompt_template,
                       generated_by_task_run_id, enable_sweep, max_sweep_iterations,
                       stages, stop_on_failure, reflection_mode, sync_pending, example_status,
                       model_overrides, approval_gate, completion_prompts_first, is_favorite,
                       dependency_graph, cost_annotations, quality_report, acceptance_criteria,
                       constraint_overrides
                FROM unified_workflows
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                // Parse JSON text columns
                let tags_str: String = row.get(4)?;
                let tags: serde_json::Value =
                    serde_json::from_str(&tags_str).unwrap_or(serde_json::json!([]));
                let setup_str: String = row.get(5)?;
                let setup: serde_json::Value =
                    serde_json::from_str(&setup_str).unwrap_or(serde_json::json!([]));
                let verif_str: String = row.get(6)?;
                let verification: serde_json::Value =
                    serde_json::from_str(&verif_str).unwrap_or(serde_json::json!([]));
                let agent_str: String = row.get(7)?;
                let agentic: serde_json::Value =
                    serde_json::from_str(&agent_str).unwrap_or(serde_json::json!([]));

                // Post-v19 JSON text columns
                let completion_str: Option<String> = row.get(13)?;
                let completion: serde_json::Value = completion_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!([]));
                let health_check_urls_str: Option<String> = row.get(18)?;
                let health_check_urls: serde_json::Value = health_check_urls_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!([]));
                let log_source_str: Option<String> = row.get(20)?;
                let log_source: serde_json::Value = log_source_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!("default"));
                let context_ids_str: Option<String> = row.get(21)?;
                let context_ids: serde_json::Value = context_ids_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!([]));
                let disabled_context_ids_str: Option<String> = row.get(22)?;
                let disabled_context_ids: serde_json::Value = disabled_context_ids_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!([]));
                let stages_str: Option<String> = row.get(28)?;
                let stages: serde_json::Value = stages_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!([]));
                let model_overrides_str: Option<String> = row.get(33)?;
                let model_overrides: serde_json::Value = model_overrides_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!({}));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "category": row.get::<_, Option<String>>(3)?,
                    "tags": tags,
                    "setup_steps": setup,
                    "verification_steps": verification,
                    "agentic_steps": agentic,
                    "max_iterations": row.get::<_, Option<i64>>(8)?,
                    "provider": row.get::<_, Option<String>>(9)?,
                    "model": row.get::<_, Option<String>>(10)?,
                    "created_at": row.get::<_, String>(11)?,
                    "updated_at": row.get::<_, String>(12)?,
                    "completion_steps": completion,
                    "skip_ai_summary": row.get::<_, Option<bool>>(14)?,
                    "timeout_seconds": row.get::<_, Option<i64>>(15)?,
                    "log_watch_enabled": row.get::<_, Option<i64>>(16)?,
                    "health_check_enabled": row.get::<_, Option<i64>>(17)?,
                    "health_check_urls": health_check_urls,
                    "preflight_check_enabled": row.get::<_, Option<i64>>(19)?,
                    "log_source_selection": log_source,
                    "context_ids": context_ids,
                    "disabled_context_ids": disabled_context_ids,
                    "auto_include_contexts": row.get::<_, Option<i64>>(23)?,
                    "prompt_template": row.get::<_, Option<String>>(24)?,
                    "generated_by_task_run_id": row.get::<_, Option<String>>(25)?,
                    "enable_sweep": row.get::<_, Option<i64>>(26)?,
                    "max_sweep_iterations": row.get::<_, Option<i64>>(27)?,
                    "stages": stages,
                    "stop_on_failure": row.get::<_, Option<i64>>(29)?,
                    "reflection_mode": row.get::<_, Option<i64>>(30)?,
                    "sync_pending": row.get::<_, Option<i64>>(31)?,
                    "example_status": row.get::<_, Option<String>>(32)?,
                    "model_overrides": model_overrides,
                    "approval_gate": row.get::<_, Option<i64>>(34)?,
                    "completion_prompts_first": row.get::<_, Option<i64>>(35)?,
                    "is_favorite": row.get::<_, Option<i64>>(36)?,
                    "dependency_graph": row.get::<_, Option<String>>(37)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "cost_annotations": row.get::<_, Option<String>>(38)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "quality_report": row.get::<_, Option<String>>(39)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "acceptance_criteria": row.get::<_, Option<String>>(40)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "constraint_overrides": row.get::<_, Option<String>>(41)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()).unwrap_or(serde_json::json!({})),
                }))
            })
            .map_err(|e| format!("Failed to export unified workflows: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all verification tests.
    pub fn export_all_verification_tests(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, test_type, category, playwright_code, vision_config,
                       python_code, repo_test_config, success_criteria, config, timeout_seconds,
                       is_critical, enabled, ai_generated, ai_generation_prompt, tags,
                       source_file, last_exported_at, created_at, updated_at
                FROM verification_tests
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let vision_str: Option<String> = row.get(6)?;
                let vision: serde_json::Value = vision_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::Value::Null);
                let repo_str: Option<String> = row.get(8)?;
                let repo: serde_json::Value = repo_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::Value::Null);
                let config_str: String = row.get(10)?;
                let config: serde_json::Value =
                    serde_json::from_str(&config_str).unwrap_or(serde_json::json!({}));
                let tags_str: String = row.get(16)?;
                let tags: serde_json::Value =
                    serde_json::from_str(&tags_str).unwrap_or(serde_json::json!([]));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "test_type": row.get::<_, String>(3)?,
                    "category": row.get::<_, Option<String>>(4)?,
                    "playwright_code": row.get::<_, Option<String>>(5)?,
                    "vision_config": vision,
                    "python_code": row.get::<_, Option<String>>(7)?,
                    "repo_test_config": repo,
                    "success_criteria": row.get::<_, Option<String>>(9)?,
                    "config": config,
                    "timeout_seconds": row.get::<_, i64>(11)?,
                    "is_critical": row.get::<_, bool>(12)?,
                    "enabled": row.get::<_, bool>(13)?,
                    "ai_generated": row.get::<_, bool>(14)?,
                    "ai_generation_prompt": row.get::<_, Option<String>>(15)?,
                    "tags": tags,
                    "source_file": row.get::<_, Option<String>>(17)?,
                    "last_exported_at": row.get::<_, Option<String>>(18)?,
                    "created_at": row.get::<_, String>(19)?,
                    "updated_at": row.get::<_, String>(20)?,
                }))
            })
            .map_err(|e| format!("Failed to export verification tests: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all task hooks.
    pub fn export_all_task_hooks(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, trigger, action_type, action_config,
                       enabled, execution_order, continue_on_failure, conditions,
                       task_run_id, created_at, updated_at
                FROM task_hooks
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let action_str: String = row.get(5)?;
                let action_config: serde_json::Value =
                    serde_json::from_str(&action_str).unwrap_or(serde_json::json!({}));
                let cond_str: String = row.get(9)?;
                let conditions: serde_json::Value =
                    serde_json::from_str(&cond_str).unwrap_or(serde_json::json!([]));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "action_type": row.get::<_, String>(4)?,
                    "action_config": action_config,
                    "enabled": row.get::<_, bool>(6)?,
                    "execution_order": row.get::<_, i64>(7)?,
                    "continue_on_failure": row.get::<_, bool>(8)?,
                    "conditions": conditions,
                    "task_run_id": row.get::<_, Option<String>>(10)?,
                    "created_at": row.get::<_, String>(11)?,
                    "updated_at": row.get::<_, String>(12)?,
                }))
            })
            .map_err(|e| format!("Failed to export task hooks: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all scheduled tasks.
    pub fn export_all_scheduled_tasks(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, enabled, schedule_type, schedule_value,
                       task_config, skip_if_completed, auto_fix_on_failure, success_criteria,
                       created_at, modified_at, next_run, last_run_id
                FROM scheduled_tasks
                ORDER BY modified_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let config_str: String = row.get(6)?;
                let task_config: serde_json::Value =
                    serde_json::from_str(&config_str).unwrap_or(serde_json::json!({}));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "enabled": row.get::<_, bool>(3)?,
                    "schedule_type": row.get::<_, String>(4)?,
                    "schedule_value": row.get::<_, String>(5)?,
                    "task_config": task_config,
                    "skip_if_completed": row.get::<_, bool>(7)?,
                    "auto_fix_on_failure": row.get::<_, bool>(8)?,
                    "success_criteria": row.get::<_, Option<String>>(9)?,
                    "created_at": row.get::<_, String>(10)?,
                    "modified_at": row.get::<_, String>(11)?,
                    "next_run": row.get::<_, Option<String>>(12)?,
                    "last_run_id": row.get::<_, Option<String>>(13)?,
                }))
            })
            .map_err(|e| format!("Failed to export scheduled tasks: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all saved API requests.
    pub fn export_all_saved_api_requests(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, category, tags, method, url, headers,
                       body, body_content_type, timeout_ms, follow_redirects,
                       variable_extractions, assertions, credential_id, created_at, updated_at
                FROM saved_api_requests
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let tags_str: String = row.get(4)?;
                let tags: serde_json::Value =
                    serde_json::from_str(&tags_str).unwrap_or(serde_json::json!([]));
                let headers_str: String = row.get(7)?;
                let headers: serde_json::Value =
                    serde_json::from_str(&headers_str).unwrap_or(serde_json::json!({}));
                let extractions_str: String = row.get(12)?;
                let extractions: serde_json::Value =
                    serde_json::from_str(&extractions_str).unwrap_or(serde_json::json!([]));
                let assertions_str: String = row.get(13)?;
                let assertions: serde_json::Value =
                    serde_json::from_str(&assertions_str).unwrap_or(serde_json::json!([]));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "category": row.get::<_, Option<String>>(3)?,
                    "tags": tags,
                    "method": row.get::<_, String>(5)?,
                    "url": row.get::<_, String>(6)?,
                    "headers": headers,
                    "body": row.get::<_, Option<String>>(8)?,
                    "body_content_type": row.get::<_, Option<String>>(9)?,
                    "timeout_ms": row.get::<_, Option<i64>>(10)?,
                    "follow_redirects": row.get::<_, bool>(11)?,
                    "variable_extractions": extractions,
                    "assertions": assertions,
                    "credential_id": row.get::<_, Option<String>>(14)?,
                    "created_at": row.get::<_, String>(15)?,
                    "updated_at": row.get::<_, String>(16)?,
                }))
            })
            .map_err(|e| format!("Failed to export saved API requests: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all configs.
    pub fn export_all_configs(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, config_json, source_type, source_path, created_at, updated_at
                FROM configs
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let config_str: String = row.get(2)?;
                let config: serde_json::Value =
                    serde_json::from_str(&config_str).unwrap_or(serde_json::json!({}));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "config": config,
                    "source_type": row.get::<_, String>(3)?,
                    "source_path": row.get::<_, Option<String>>(4)?,
                    "created_at": row.get::<_, String>(5)?,
                    "updated_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to export configs: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Import flows (with conflict handling).
    pub fn import_flows(
        &self,
        flows: &[serde_json::Value],
        conflict_mode: &str,
    ) -> Result<ImportResult, String> {
        let conn = self.get_conn()?;
        let mut imported = 0;
        let mut skipped = 0;
        let mut errors = Vec::new();
        let now = Utc::now().to_rfc3339();

        for flow in flows {
            let id = flow["id"].as_str().unwrap_or("");
            if id.is_empty() {
                errors.push("Flow missing ID".to_string());
                continue;
            }

            // Check if exists
            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM orchestrator_flows WHERE id = ?1",
                    params![id],
                    |_| Ok(true),
                )
                .unwrap_or(false);

            if exists && conflict_mode == "skip" {
                skipped += 1;
                continue;
            }

            let name = flow["name"].as_str().unwrap_or("Unnamed");
            let description = flow["description"].as_str();
            let definition = serde_json::to_string(&flow["definition"]).unwrap_or("{}".to_string());
            let tags = serde_json::to_string(&flow["tags"]).unwrap_or("[]".to_string());
            let version = flow["version"].as_str().unwrap_or("1.0.0");

            let result = conn.execute(
                r#"
                INSERT OR REPLACE INTO orchestrator_flows (id, name, description, definition_json, tags, version, created_at, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
                params![id, name, description, definition, tags, version, now, now],
            );

            match result {
                Ok(_) => imported += 1,
                Err(e) => errors.push(format!("Failed to import flow {}: {}", id, e)),
            }
        }

        Ok(ImportResult {
            imported,
            skipped,
            errors,
        })
    }

    /// Import prompts (with conflict handling).
    pub fn import_prompts(
        &self,
        prompts: &[serde_json::Value],
        conflict_mode: &str,
    ) -> Result<ImportResult, String> {
        let conn = self.get_conn()?;
        let mut imported = 0;
        let mut skipped = 0;
        let mut errors = Vec::new();
        let now = Utc::now().to_rfc3339();

        for prompt in prompts {
            let id = prompt["id"].as_str().unwrap_or("");
            if id.is_empty() {
                errors.push("Prompt missing ID".to_string());
                continue;
            }

            let exists: bool = conn
                .query_row("SELECT 1 FROM prompts WHERE id = ?1", params![id], |_| {
                    Ok(true)
                })
                .unwrap_or(false);

            if exists && conflict_mode == "skip" {
                skipped += 1;
                continue;
            }

            let name = prompt["name"].as_str().unwrap_or("Unnamed");
            let category = prompt["category"].as_str();
            let content = prompt["content"].as_str().unwrap_or("");
            let variables = serde_json::to_string(&prompt["variables"]).unwrap_or("[]".to_string());

            let result = conn.execute(
                r#"
                INSERT OR REPLACE INTO prompts (id, name, category, content, variables, created_at, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                "#,
                params![id, name, category, content, variables, now, now],
            );

            match result {
                Ok(_) => imported += 1,
                Err(e) => errors.push(format!("Failed to import prompt {}: {}", id, e)),
            }
        }

        Ok(ImportResult {
            imported,
            skipped,
            errors,
        })
    }

    /// Import settings (with conflict handling).
    pub fn import_settings(
        &self,
        settings: &[serde_json::Value],
        conflict_mode: &str,
    ) -> Result<ImportResult, String> {
        let conn = self.get_conn()?;
        let mut imported = 0;
        let mut skipped = 0;
        let mut errors = Vec::new();
        let now = Utc::now().to_rfc3339();

        for setting in settings {
            let key = setting["key"].as_str().unwrap_or("");
            if key.is_empty() {
                errors.push("Setting missing key".to_string());
                continue;
            }

            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM settings WHERE key = ?1",
                    params![key],
                    |_| Ok(true),
                )
                .unwrap_or(false);

            if exists && conflict_mode == "skip" {
                skipped += 1;
                continue;
            }

            let value = serde_json::to_string(&setting["value"]).unwrap_or("null".to_string());

            let result = conn.execute(
                "INSERT OR REPLACE INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)",
                params![key, value, now],
            );

            match result {
                Ok(_) => imported += 1,
                Err(e) => errors.push(format!("Failed to import setting {}: {}", key, e)),
            }
        }

        Ok(ImportResult {
            imported,
            skipped,
            errors,
        })
    }

    /// Import unified workflows (with conflict handling).
    pub fn import_unified_workflows(
        &self,
        workflows: &[serde_json::Value],
        conflict_mode: &str,
    ) -> Result<ImportResult, String> {
        let conn = self.get_conn()?;
        let mut imported = 0;
        let mut skipped = 0;
        let mut errors = Vec::new();
        let now = Utc::now().to_rfc3339();

        for workflow in workflows {
            let id = workflow["id"].as_str().unwrap_or("");
            if id.is_empty() {
                errors.push("Workflow missing ID".to_string());
                continue;
            }

            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM unified_workflows WHERE id = ?1",
                    params![id],
                    |_| Ok(true),
                )
                .unwrap_or(false);

            if exists && conflict_mode == "skip" {
                skipped += 1;
                continue;
            }

            let name = workflow["name"].as_str().unwrap_or("Unnamed");
            let description = workflow["description"].as_str();
            let category = workflow["category"].as_str();
            let tags = serde_json::to_string(&workflow["tags"]).unwrap_or("[]".to_string());
            let setup = serde_json::to_string(&workflow["setup_steps"]).unwrap_or("[]".to_string());
            let verif =
                serde_json::to_string(&workflow["verification_steps"]).unwrap_or("[]".to_string());
            let agent =
                serde_json::to_string(&workflow["agentic_steps"]).unwrap_or("[]".to_string());
            let max_iter = workflow["max_iterations"].as_i64();
            let provider = workflow["provider"].as_str();
            let model = workflow["model"].as_str();

            // Post-v19 columns
            let completion =
                serde_json::to_string(&workflow["completion_steps"]).unwrap_or("[]".to_string());
            let skip_ai_summary = workflow["skip_ai_summary"].as_bool().unwrap_or(false);
            let timeout_seconds = workflow["timeout_seconds"].as_i64();
            let log_watch_enabled = workflow["log_watch_enabled"].as_i64().unwrap_or(1);
            let health_check_enabled = workflow["health_check_enabled"].as_i64().unwrap_or(1);
            let health_check_urls =
                serde_json::to_string(&workflow["health_check_urls"]).unwrap_or("[]".to_string());
            let preflight_check_enabled = workflow["preflight_check_enabled"].as_i64().unwrap_or(1);
            let log_source_selection = serde_json::to_string(&workflow["log_source_selection"])
                .unwrap_or("\"default\"".to_string());
            let context_ids =
                serde_json::to_string(&workflow["context_ids"]).unwrap_or("[]".to_string());
            let disabled_context_ids = serde_json::to_string(&workflow["disabled_context_ids"])
                .unwrap_or("[]".to_string());
            let auto_include_contexts = workflow["auto_include_contexts"].as_i64().unwrap_or(1);
            let prompt_template = workflow["prompt_template"].as_str();
            let generated_by_task_run_id = workflow["generated_by_task_run_id"].as_str();
            let enable_sweep = workflow["enable_sweep"].as_i64().unwrap_or(0);
            let max_sweep_iterations = workflow["max_sweep_iterations"].as_i64().unwrap_or(5);
            let stages = serde_json::to_string(&workflow["stages"]).unwrap_or("[]".to_string());
            let stop_on_failure = workflow["stop_on_failure"].as_i64().unwrap_or(0);
            let approval_gate = workflow["approval_gate"].as_i64().unwrap_or(0);
            let reflection_mode = workflow["reflection_mode"].as_i64().unwrap_or(1);
            let completion_prompts_first =
                workflow["completion_prompts_first"].as_i64().unwrap_or(0);
            let is_favorite = workflow["is_favorite"].as_i64().unwrap_or(0);
            let sync_pending = workflow["sync_pending"].as_i64().unwrap_or(0);
            let example_status = workflow["example_status"].as_str().unwrap_or("pending");
            let constraint_overrides = serde_json::to_string(&workflow["constraint_overrides"])
                .unwrap_or_else(|_| "{}".to_string());
            let acceptance_criteria = if workflow["acceptance_criteria"].is_null() {
                None
            } else {
                Some(
                    serde_json::to_string(&workflow["acceptance_criteria"])
                        .unwrap_or_else(|_| "null".to_string()),
                )
            };

            let result = conn.execute(
                r#"
                INSERT OR REPLACE INTO unified_workflows (
                    id, name, description, category, tags, setup_steps, verification_steps,
                    agentic_steps, max_iterations, provider, model, created_at, updated_at,
                    completion_steps, skip_ai_summary, timeout_seconds,
                    log_watch_enabled, health_check_enabled, health_check_urls,
                    preflight_check_enabled, log_source_selection, context_ids,
                    disabled_context_ids, auto_include_contexts, prompt_template,
                    generated_by_task_run_id, enable_sweep, max_sweep_iterations,
                    stages, stop_on_failure, approval_gate, reflection_mode, completion_prompts_first,
                    is_favorite, sync_pending, example_status, acceptance_criteria, constraint_overrides
                )
                VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                    ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25,
                    ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38
                )
                "#,
                params![
                    id,
                    name,
                    description,
                    category,
                    tags,
                    setup,
                    verif,
                    agent,
                    max_iter,
                    provider,
                    model,
                    now,
                    now,
                    completion,
                    skip_ai_summary,
                    timeout_seconds,
                    log_watch_enabled,
                    health_check_enabled,
                    health_check_urls,
                    preflight_check_enabled,
                    log_source_selection,
                    context_ids,
                    disabled_context_ids,
                    auto_include_contexts,
                    prompt_template,
                    generated_by_task_run_id,
                    enable_sweep,
                    max_sweep_iterations,
                    stages,
                    stop_on_failure,
                    approval_gate,
                    reflection_mode,
                    completion_prompts_first,
                    is_favorite,
                    sync_pending,
                    example_status,
                    acceptance_criteria,
                    constraint_overrides
                ],
            );

            match result {
                Ok(_) => imported += 1,
                Err(e) => errors.push(format!("Failed to import workflow {}: {}", id, e)),
            }
        }

        Ok(ImportResult {
            imported,
            skipped,
            errors,
        })
    }

    /// Import learning outcomes (with conflict handling).
    pub fn import_learning_outcomes(
        &self,
        outcomes: &[serde_json::Value],
        conflict_mode: &str,
    ) -> Result<ImportResult, String> {
        let conn = self.get_conn()?;
        let mut imported = 0;
        let mut skipped = 0;
        let mut errors = Vec::new();

        for outcome in outcomes {
            let id = outcome["id"].as_i64();
            let task_id = outcome["task_id"].as_str().unwrap_or("");

            if task_id.is_empty() {
                errors.push("Learning outcome missing task_id".to_string());
                continue;
            }

            // For learning outcomes, check by task_id since id is auto-generated
            let exists: bool = if let Some(outcome_id) = id {
                conn.query_row(
                    "SELECT 1 FROM learning_outcomes WHERE id = ?1",
                    params![outcome_id],
                    |_| Ok(true),
                )
                .unwrap_or(false)
            } else {
                false
            };

            if exists && conflict_mode == "skip" {
                skipped += 1;
                continue;
            }

            let status = outcome["status"].as_str().unwrap_or("unknown");
            let duration = outcome["duration_secs"].as_f64();
            let iterations = outcome["iterations"].as_i64().map(|i| i as i32);
            let strategy = outcome["strategy"].as_str();
            let tools_json = serde_json::to_string(&outcome["tools_used"]).ok();
            let agents_json = serde_json::to_string(&outcome["agents_involved"]).ok();
            let error_type = outcome["error_type"].as_str();
            let error_msg = outcome["error_message"].as_str();
            let feedback_json = serde_json::to_string(&outcome["feedback"]).ok();
            let workflow_architecture = outcome["workflow_architecture"].as_str();

            let result = conn.execute(
                r#"
                INSERT INTO learning_outcomes (task_id, status, duration_secs, iterations, strategy, tools_used, agents_involved, error_type, error_message, feedback, workflow_architecture, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, datetime('now'))
                "#,
                params![task_id, status, duration, iterations, strategy, tools_json, agents_json, error_type, error_msg, feedback_json, workflow_architecture],
            );

            match result {
                Ok(_) => imported += 1,
                Err(e) => errors.push(format!("Failed to import learning outcome: {}", e)),
            }
        }

        Ok(ImportResult {
            imported,
            skipped,
            errors,
        })
    }

    /// Import learning patterns (with conflict handling).
    pub fn import_learning_patterns(
        &self,
        patterns: &[serde_json::Value],
        conflict_mode: &str,
    ) -> Result<ImportResult, String> {
        let conn = self.get_conn()?;
        let mut imported = 0;
        let mut skipped = 0;
        let mut errors = Vec::new();
        let now = Utc::now().to_rfc3339();

        for pattern in patterns {
            let id = pattern["id"].as_str().unwrap_or("");
            if id.is_empty() {
                errors.push("Pattern missing ID".to_string());
                continue;
            }

            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM learning_patterns WHERE id = ?1",
                    params![id],
                    |_| Ok(true),
                )
                .unwrap_or(false);

            if exists && conflict_mode == "skip" {
                skipped += 1;
                continue;
            }

            let pattern_type = pattern["pattern_type"].as_str().unwrap_or("unknown");
            let description = pattern["description"].as_str().unwrap_or("");
            let confidence = pattern["confidence"].as_f64().unwrap_or(0.0);
            let occurrences = pattern["occurrences"].as_i64().unwrap_or(0) as i32;
            let context = serde_json::to_string(&pattern["context"]).ok();

            let result = conn.execute(
                r#"
                INSERT OR REPLACE INTO learning_patterns (id, pattern_type, description, confidence, occurrences, context, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                "#,
                params![id, pattern_type, description, confidence, occurrences, context, now],
            );

            match result {
                Ok(_) => imported += 1,
                Err(e) => errors.push(format!("Failed to import pattern {}: {}", id, e)),
            }
        }

        Ok(ImportResult {
            imported,
            skipped,
            errors,
        })
    }

    // ==================== Mobile Development Feedback ====================

    /// Helper to map a row to MobileState.
    fn row_to_mobile_state(row: &rusqlite::Row) -> SqliteResult<MobileState> {
        Ok(MobileState {
            id: row.get(0)?,
            task_run_id: row.get(1)?,
            timestamp: row.get(2)?,
            device_id: row.get(3).ok(),
            device_type: row.get(4).ok(),
            device_model: row.get(5).ok(),
            app_package: row.get(6).ok(),
            app_activity: row.get(7).ok(),
            app_state: row.get(8).ok(),
            metro_connected: row.get::<_, i32>(9)? != 0,
            bundle_status: row.get(10).ok(),
            last_reload_type: row.get(11).ok(),
            last_reload_time: row.get(12).ok(),
            screenshot_path: row.get(13).ok(),
            logcat_path: row.get(14).ok(),
            has_errors: row.get::<_, i32>(15)? != 0,
            error_summary: row.get(16).ok(),
            created_at: row.get(17)?,
        })
    }

    /// Helper to map a row to MobileLog.
    fn row_to_mobile_log(row: &rusqlite::Row) -> SqliteResult<MobileLog> {
        Ok(MobileLog {
            id: row.get(0)?,
            task_run_id: row.get(1)?,
            mobile_state_id: row.get(2).ok(),
            log_source: row.get(3)?,
            log_level: row.get(4).ok(),
            log_tag: row.get(5).ok(),
            message: row.get(6)?,
            raw_line: row.get(7).ok(),
            data: row.get(8).ok(),
            error_type: row.get(9).ok(),
            error_code: row.get(10).ok(),
            stack_trace: row.get(11).ok(),
            file_path: row.get(12).ok(),
            line_number: row.get(13).ok(),
            column_number: row.get(14).ok(),
            timestamp: row.get(15)?,
            device_timestamp: row.get(16).ok(),
            created_at: row.get(17)?,
        })
    }

    /// Create a new mobile state capture.
    pub fn create_mobile_state(
        &self,
        input: &CreateMobileStateInput,
    ) -> Result<MobileState, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO task_run_mobile_state (
                task_run_id, timestamp, device_id, device_type, device_model,
                app_package, app_activity, app_state, metro_connected, bundle_status,
                last_reload_type, last_reload_time, screenshot_path, logcat_path,
                has_errors, error_summary, created_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14,
                ?15, ?16, ?17
            )
            "#,
            params![
                input.task_run_id,
                now,
                input.device_id,
                input.device_type,
                input.device_model,
                input.app_package,
                input.app_activity,
                input.app_state,
                input.metro_connected as i32,
                input.bundle_status,
                input.last_reload_type,
                input.last_reload_time,
                input.screenshot_path,
                input.logcat_path,
                input.has_errors as i32,
                input.error_summary,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create mobile state: {}", e))?;

        let id = conn.last_insert_rowid();
        self.get_mobile_state(id)?
            .ok_or_else(|| "Failed to retrieve created mobile state".to_string())
    }

    /// Get a mobile state by ID.
    pub fn get_mobile_state(&self, id: i64) -> Result<Option<MobileState>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<MobileState> = conn.query_row(
            r#"
            SELECT
                id, task_run_id, timestamp, device_id, device_type, device_model,
                app_package, app_activity, app_state, metro_connected, bundle_status,
                last_reload_type, last_reload_time, screenshot_path, logcat_path,
                has_errors, error_summary, created_at
            FROM task_run_mobile_state
            WHERE id = ?1
            "#,
            params![id],
            Self::row_to_mobile_state,
        );

        match result {
            Ok(state) => Ok(Some(state)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get mobile state: {}", e)),
        }
    }

    /// Get mobile state captures for a task run.
    pub fn get_mobile_states(
        &self,
        task_run_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<MobileState>, String> {
        let conn = self.get_conn()?;
        let limit_val = limit.unwrap_or(100);

        let sql = format!(
            r#"
            SELECT
                id, task_run_id, timestamp, device_id, device_type, device_model,
                app_package, app_activity, app_state, metro_connected, bundle_status,
                last_reload_type, last_reload_time, screenshot_path, logcat_path,
                has_errors, error_summary, created_at
            FROM task_run_mobile_state
            WHERE task_run_id = ?1
            ORDER BY timestamp DESC
            LIMIT {}
            "#,
            limit_val
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let states: Vec<MobileState> = stmt
            .query_map(params![task_run_id], Self::row_to_mobile_state)
            .map_err(|e| format!("Failed to query mobile states: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(states)
    }

    /// Get the latest mobile state for a task run.
    pub fn get_latest_mobile_state(
        &self,
        task_run_id: &str,
    ) -> Result<Option<MobileState>, String> {
        let states = self.get_mobile_states(task_run_id, Some(1))?;
        Ok(states.into_iter().next())
    }

    /// Create a new mobile log entry.
    pub fn create_mobile_log(&self, input: &CreateMobileLogInput) -> Result<MobileLog, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO task_run_mobile_logs (
                task_run_id, mobile_state_id, log_source, log_level, log_tag,
                message, raw_line, data, error_type, error_code,
                stack_trace, file_path, line_number, column_number,
                timestamp, device_timestamp, created_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14,
                ?15, ?16, ?17
            )
            "#,
            params![
                input.task_run_id,
                input.mobile_state_id,
                input.log_source,
                input.log_level,
                input.log_tag,
                input.message,
                input.raw_line,
                input.data,
                input.error_type,
                input.error_code,
                input.stack_trace,
                input.file_path,
                input.line_number,
                input.column_number,
                now,
                input.device_timestamp,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create mobile log: {}", e))?;

        let id = conn.last_insert_rowid();
        self.get_mobile_log(id)?
            .ok_or_else(|| "Failed to retrieve created mobile log".to_string())
    }

    /// Get a mobile log by ID.
    pub fn get_mobile_log(&self, id: i64) -> Result<Option<MobileLog>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<MobileLog> = conn.query_row(
            r#"
            SELECT
                id, task_run_id, mobile_state_id, log_source, log_level, log_tag,
                message, raw_line, data, error_type, error_code,
                stack_trace, file_path, line_number, column_number,
                timestamp, device_timestamp, created_at
            FROM task_run_mobile_logs
            WHERE id = ?1
            "#,
            params![id],
            Self::row_to_mobile_log,
        );

        match result {
            Ok(log) => Ok(Some(log)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get mobile log: {}", e)),
        }
    }

    /// Get mobile logs for a task run with optional filtering.
    pub fn get_mobile_logs(
        &self,
        task_run_id: &str,
        log_source: Option<&str>,
        errors_only: bool,
        limit: Option<u32>,
    ) -> Result<Vec<MobileLog>, String> {
        let conn = self.get_conn()?;
        let limit_val = limit.unwrap_or(500);

        let mut conditions = vec!["task_run_id = ?1".to_string()];
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(task_run_id.to_string())];

        if let Some(source) = log_source {
            conditions.push(format!("log_source = ?{}", params.len() + 1));
            params.push(Box::new(source.to_string()));
        }

        if errors_only {
            conditions.push("log_level IN ('error', 'fatal', 'ERROR', 'FATAL', 'E')".to_string());
        }

        let sql = format!(
            r#"
            SELECT
                id, task_run_id, mobile_state_id, log_source, log_level, log_tag,
                message, raw_line, data, error_type, error_code,
                stack_trace, file_path, line_number, column_number,
                timestamp, device_timestamp, created_at
            FROM task_run_mobile_logs
            WHERE {}
            ORDER BY timestamp DESC
            LIMIT {}
            "#,
            conditions.join(" AND "),
            limit_val
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let params_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

        let logs: Vec<MobileLog> = stmt
            .query_map(params_refs.as_slice(), Self::row_to_mobile_log)
            .map_err(|e| format!("Failed to query mobile logs: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(logs)
    }

    /// Get mobile error logs for a task run.
    pub fn get_mobile_errors(
        &self,
        task_run_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<MobileLog>, String> {
        self.get_mobile_logs(task_run_id, None, true, limit)
    }

    /// Delete all mobile data for a task run.
    pub fn delete_mobile_data_for_task(&self, task_run_id: &str) -> Result<(usize, usize), String> {
        let conn = self.get_conn()?;

        let logs_deleted = conn
            .execute(
                "DELETE FROM task_run_mobile_logs WHERE task_run_id = ?1",
                params![task_run_id],
            )
            .map_err(|e| format!("Failed to delete mobile logs: {}", e))?;

        let states_deleted = conn
            .execute(
                "DELETE FROM task_run_mobile_state WHERE task_run_id = ?1",
                params![task_run_id],
            )
            .map_err(|e| format!("Failed to delete mobile states: {}", e))?;

        Ok((states_deleted, logs_deleted))
    }

    // ========================================================================
    // MCP Server Operations
    // ========================================================================

    /// List all MCP server configurations.
    pub fn list_mcp_servers(&self) -> Result<Vec<crate::mcp_client::McpServerConfig>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, transport,
                       stdio_config, http_config,
                       enabled, auto_start, timeout_seconds,
                       cached_tools, tools_cached_at,
                       created_at, updated_at
                FROM mcp_servers
                ORDER BY name ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let transport_str: String = row.get(3)?;
                let transport = match transport_str.as_str() {
                    "http" => crate::mcp_client::McpTransport::Http,
                    _ => crate::mcp_client::McpTransport::Stdio,
                };

                let stdio_config: Option<String> = row.get(4)?;
                let http_config: Option<String> = row.get(5)?;

                Ok(crate::mcp_client::McpServerConfig {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    transport,
                    stdio_config: stdio_config.and_then(|s| serde_json::from_str(&s).ok()),
                    http_config: http_config.and_then(|s| serde_json::from_str(&s).ok()),
                    enabled: row.get(6)?,
                    auto_start: row.get(7)?,
                    timeout_seconds: row.get(8)?,
                    cached_tools: row.get(9)?,
                    tools_cached_at: row.get(10)?,
                    created_at: row.get(11)?,
                    updated_at: row.get(12)?,
                })
            })
            .map_err(|e| format!("Failed to list MCP servers: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get a specific MCP server by ID.
    pub fn get_mcp_server(
        &self,
        id: &str,
    ) -> Result<Option<crate::mcp_client::McpServerConfig>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, transport,
                       stdio_config, http_config,
                       enabled, auto_start, timeout_seconds,
                       cached_tools, tools_cached_at,
                       created_at, updated_at
                FROM mcp_servers
                WHERE id = ?1
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let result = stmt
            .query_row(params![id], |row| {
                let transport_str: String = row.get(3)?;
                let transport = match transport_str.as_str() {
                    "http" => crate::mcp_client::McpTransport::Http,
                    _ => crate::mcp_client::McpTransport::Stdio,
                };

                let stdio_config: Option<String> = row.get(4)?;
                let http_config: Option<String> = row.get(5)?;

                Ok(crate::mcp_client::McpServerConfig {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    transport,
                    stdio_config: stdio_config.and_then(|s| serde_json::from_str(&s).ok()),
                    http_config: http_config.and_then(|s| serde_json::from_str(&s).ok()),
                    enabled: row.get(6)?,
                    auto_start: row.get(7)?,
                    timeout_seconds: row.get(8)?,
                    cached_tools: row.get(9)?,
                    tools_cached_at: row.get(10)?,
                    created_at: row.get(11)?,
                    updated_at: row.get(12)?,
                })
            })
            .optional()
            .map_err(|e| format!("Failed to get MCP server: {}", e))?;

        Ok(result)
    }

    /// Create a new MCP server configuration.
    pub fn create_mcp_server(
        &self,
        input: crate::mcp_client::CreateMcpServerInput,
    ) -> Result<crate::mcp_client::McpServerConfig, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let transport_str = match input.transport {
            crate::mcp_client::McpTransport::Http => "http",
            crate::mcp_client::McpTransport::Stdio => "stdio",
        };

        let stdio_config_json = input
            .stdio_config
            .as_ref()
            .map(|c| serde_json::to_string(c).unwrap_or_default());
        let http_config_json = input
            .http_config
            .as_ref()
            .map(|c| serde_json::to_string(c).unwrap_or_default());

        conn.execute(
            r#"
            INSERT INTO mcp_servers (
                id, name, description, transport,
                stdio_config, http_config,
                enabled, auto_start, timeout_seconds,
                created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            params![
                id,
                input.name,
                input.description,
                transport_str,
                stdio_config_json,
                http_config_json,
                input.enabled.unwrap_or(true),
                input.auto_start.unwrap_or(false),
                input.timeout_seconds.unwrap_or(30) as i64,
                now,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create MCP server: {}", e))?;

        self.get_mcp_server(&id)?
            .ok_or_else(|| "Failed to retrieve created MCP server".to_string())
    }

    /// Update an MCP server configuration.
    pub fn update_mcp_server(
        &self,
        id: &str,
        input: crate::mcp_client::UpdateMcpServerInput,
    ) -> Result<crate::mcp_client::McpServerConfig, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Get existing record and merge with input
        let existing = self
            .get_mcp_server(id)?
            .ok_or_else(|| format!("MCP server not found: {}", id))?;

        let name = input.name.unwrap_or(existing.name);
        let description = input.description.or(existing.description);
        let transport = input.transport.unwrap_or(existing.transport);
        let stdio_config = input.stdio_config.or(existing.stdio_config);
        let http_config = input.http_config.or(existing.http_config);
        let enabled = input.enabled.unwrap_or(existing.enabled);
        let auto_start = input.auto_start.unwrap_or(existing.auto_start);
        let timeout_seconds = input.timeout_seconds.unwrap_or(existing.timeout_seconds);

        let transport_str = match transport {
            crate::mcp_client::McpTransport::Http => "http",
            crate::mcp_client::McpTransport::Stdio => "stdio",
        };
        let stdio_config_json = stdio_config
            .as_ref()
            .map(|c| serde_json::to_string(c).unwrap_or_default());
        let http_config_json = http_config
            .as_ref()
            .map(|c| serde_json::to_string(c).unwrap_or_default());

        conn.execute(
            r#"
            UPDATE mcp_servers SET
                name = ?1, description = ?2, transport = ?3,
                stdio_config = ?4, http_config = ?5,
                enabled = ?6, auto_start = ?7, timeout_seconds = ?8,
                updated_at = ?9
            WHERE id = ?10
            "#,
            params![
                name,
                description,
                transport_str,
                stdio_config_json,
                http_config_json,
                enabled,
                auto_start,
                timeout_seconds as i64,
                now,
                id,
            ],
        )
        .map_err(|e| format!("Failed to update MCP server: {}", e))?;

        self.get_mcp_server(id)?
            .ok_or_else(|| "Failed to retrieve updated MCP server".to_string())
    }

    /// Delete an MCP server configuration.
    pub fn delete_mcp_server(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;

        conn.execute("DELETE FROM mcp_servers WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete MCP server: {}", e))?;

        Ok(())
    }

    /// Update the cached tools for an MCP server.
    pub fn update_mcp_server_tools_cache(
        &self,
        id: &str,
        tools_json: &str,
        cached_at: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;

        conn.execute(
            r#"
            UPDATE mcp_servers SET
                cached_tools = ?1,
                tools_cached_at = ?2,
                updated_at = ?3
            WHERE id = ?4
            "#,
            params![tools_json, cached_at, cached_at, id],
        )
        .map_err(|e| format!("Failed to update MCP server tools cache: {}", e))?;

        Ok(())
    }

    // ========================================================================
    // MCP Call Operations
    // ========================================================================

    /// Create a task run MCP call record.
    pub fn create_task_run_mcp_call(
        &self,
        input: &crate::mcp_client::CreateMcpCallInput,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO task_run_mcp_calls (
                id, task_run_id, step_id, step_name,
                server_id, server_name, tool_name,
                arguments, resolved_arguments,
                response, response_type, duration_ms,
                extractions, assertions,
                success, error_message, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
            "#,
            params![
                id,
                input.task_run_id,
                input.step_id,
                input.step_name,
                input.server_id,
                input.server_name,
                input.tool_name,
                input.arguments,
                input.resolved_arguments,
                input.response,
                input.response_type,
                input.duration_ms,
                input.extractions,
                input.assertions,
                input.success,
                input.error_message,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create task run MCP call: {}", e))?;

        Ok(id)
    }

    /// Get MCP calls for a task run.
    pub fn get_task_run_mcp_calls(
        &self,
        task_run_id: &str,
        success_filter: Option<bool>,
    ) -> Result<crate::mcp_client::McpCallsResult, String> {
        let conn = self.get_conn()?;

        let query = if success_filter.is_some() {
            r#"
            SELECT id, task_run_id, step_id, step_name,
                   server_id, server_name, tool_name,
                   arguments, resolved_arguments,
                   response, response_type, duration_ms,
                   extractions, assertions,
                   success, error_message, created_at
            FROM task_run_mcp_calls
            WHERE task_run_id = ?1 AND success = ?2
            ORDER BY created_at ASC
            "#
        } else {
            r#"
            SELECT id, task_run_id, step_id, step_name,
                   server_id, server_name, tool_name,
                   arguments, resolved_arguments,
                   response, response_type, duration_ms,
                   extractions, assertions,
                   success, error_message, created_at
            FROM task_run_mcp_calls
            WHERE task_run_id = ?1
            ORDER BY created_at ASC
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let row_mapper =
            |row: &rusqlite::Row| -> rusqlite::Result<crate::mcp_client::McpCallRecord> {
                Ok(crate::mcp_client::McpCallRecord {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    step_id: row.get(2)?,
                    step_name: row.get(3)?,
                    server_id: row.get(4)?,
                    server_name: row.get(5)?,
                    tool_name: row.get(6)?,
                    arguments: row.get(7)?,
                    resolved_arguments: row.get(8)?,
                    response: row.get(9)?,
                    response_type: row.get(10)?,
                    duration_ms: row.get(11)?,
                    extractions: row.get(12)?,
                    assertions: row.get(13)?,
                    success: row.get(14)?,
                    error_message: row.get(15)?,
                    created_at: row.get(16)?,
                })
            };

        let calls: Vec<crate::mcp_client::McpCallRecord> = if let Some(success) = success_filter {
            stmt.query_map(params![task_run_id, success], row_mapper)
                .map_err(|e| format!("Failed to get MCP calls: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map(params![task_run_id], row_mapper)
                .map_err(|e| format!("Failed to get MCP calls: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        };

        let success_count = calls.iter().filter(|c| c.success).count();
        let failed_count = calls.iter().filter(|c| !c.success).count();

        Ok(crate::mcp_client::McpCallsResult {
            task_run_id: task_run_id.to_string(),
            calls: calls.clone(),
            count: calls.len(),
            success_count,
            failed_count,
        })
    }
}
