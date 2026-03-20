//! Unified workflow and workflow sync operations.
//!
//! Contains CheckpointDb methods for unified workflow CRUD
//! and workflow synchronization helpers.

use chrono::Utc;
use rusqlite::params;

use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Unified Workflows Operations
    // ========================================================================

    /// List all unified workflows
    pub fn list_unified_workflows(
        &self,
    ) -> Result<Vec<crate::unified_workflows::UnifiedWorkflow>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, category, tags, setup_steps, verification_steps,
                       agentic_steps, completion_steps, max_iterations, provider, model,
                       skip_ai_summary, created_at, updated_at, log_source_selection,
                       context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
                       log_watch_enabled, health_check_enabled, health_check_urls, timeout_seconds,
                       preflight_check_enabled, generated_by_task_run_id, enable_sweep, max_sweep_iterations,
                       stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
                       completion_prompts_first, is_favorite, dependency_graph, cost_annotations,
                       quality_report, acceptance_criteria, constraint_overrides,
                       COALESCE(ai_reviewed, 1) as ai_reviewed
                FROM unified_workflows
                ORDER BY is_favorite DESC, updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let workflows = stmt
            .query_map([], |row| {
                Ok(crate::unified_workflows::UnifiedWorkflow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "general".to_string()),
                    tags: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    setup_steps: row
                        .get::<_, Option<String>>(5)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    verification_steps: row
                        .get::<_, Option<String>>(6)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    agentic_steps: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    completion_steps: row
                        .get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    max_iterations: (row.get::<_, i64>(9)? as u32).max(1),
                    provider: row.get(10)?,
                    model: row.get(11)?,
                    skip_ai_summary: row.get::<_, i32>(12)? != 0,
                    created_at: row.get(13)?,
                    updated_at: row.get(14)?,
                    log_source_selection: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    context_ids: row
                        .get::<_, Option<String>>(16)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    disabled_context_ids: row
                        .get::<_, Option<String>>(17)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    auto_include_contexts: row.get::<_, Option<i32>>(18)?.unwrap_or(1) != 0,
                    prompt_template: row.get(19)?,
                    log_watch_enabled: row.get::<_, Option<i32>>(20)?.unwrap_or(1) != 0,
                    health_check_enabled: row.get::<_, Option<i32>>(21)?.unwrap_or(1) != 0,
                    health_check_urls: row
                        .get::<_, Option<String>>(22)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    timeout_seconds: row.get::<_, Option<i64>>(23)?.map(|v| v as u64),
                    preflight_check_enabled: row.get::<_, Option<i32>>(24)?.unwrap_or(1) != 0,
                    generated_by_task_run_id: row.get(25)?,
                    enable_sweep: row.get::<_, Option<i32>>(26)?.unwrap_or(0) != 0,
                    max_sweep_iterations: row.get::<_, Option<i32>>(27)?.unwrap_or(5) as u32,
                    stages: row
                        .get::<_, Option<String>>(28)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    stop_on_failure: row.get::<_, Option<i32>>(29)?.unwrap_or(0) != 0,
                    reflection_mode: row.get::<_, Option<i32>>(30)?.unwrap_or(1) != 0,
                    model_overrides: {
                        let json_str: String = row
                            .get::<_, Option<String>>(31)?
                            .unwrap_or_else(|| "{}".to_string());
                        serde_json::from_str(&json_str).unwrap_or_default()
                    },
                    approval_gate: row.get::<_, Option<i32>>(32)?.unwrap_or(0) != 0,
                    completion_prompts_first: row.get::<_, Option<i32>>(33)?.unwrap_or(0) != 0,
                    is_favorite: row.get::<_, Option<i32>>(34)?.unwrap_or(0) != 0,
                    dependency_graph: row
                        .get::<_, Option<String>>(35)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    cost_annotations: row
                        .get::<_, Option<String>>(36)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    quality_report: row
                        .get::<_, Option<String>>(37)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    acceptance_criteria: row
                        .get::<_, Option<String>>(38)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    constraint_overrides: row
                        .get::<_, Option<String>>(39)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    ai_reviewed: row
                        .get::<_, Option<i32>>(40)
                        .unwrap_or(Some(1))
                        .unwrap_or(1)
                        != 0,
                    multi_agent_mode: true,
                    use_worktree: false,
                    // targeted_error_ids is a runtime field, not stored in DB
                    targeted_error_ids: vec![],
                })
            })
            .map_err(|e| format!("Failed to query unified workflows: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(workflows)
    }

    /// Get a single unified workflow by ID
    pub fn get_unified_workflow(
        &self,
        id: &str,
    ) -> Result<Option<crate::unified_workflows::UnifiedWorkflow>, String> {
        let conn = self.get_conn()?;

        let result = conn.query_row(
            r#"
            SELECT id, name, description, category, tags, setup_steps, verification_steps,
                   agentic_steps, completion_steps, max_iterations, provider, model,
                   skip_ai_summary, created_at, updated_at, log_source_selection,
                   context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
                   log_watch_enabled, health_check_enabled, health_check_urls, timeout_seconds,
                   preflight_check_enabled, generated_by_task_run_id, enable_sweep, max_sweep_iterations,
                   stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
                   completion_prompts_first, is_favorite, dependency_graph, cost_annotations,
                   quality_report, acceptance_criteria, constraint_overrides,
                   COALESCE(ai_reviewed, 1) as ai_reviewed
            FROM unified_workflows
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(crate::unified_workflows::UnifiedWorkflow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "general".to_string()),
                    tags: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    setup_steps: row
                        .get::<_, Option<String>>(5)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    verification_steps: row
                        .get::<_, Option<String>>(6)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    agentic_steps: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    completion_steps: row
                        .get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    max_iterations: (row.get::<_, i64>(9)? as u32).max(1),
                    provider: row.get(10)?,
                    model: row.get(11)?,
                    skip_ai_summary: row.get::<_, i32>(12)? != 0,
                    created_at: row.get(13)?,
                    updated_at: row.get(14)?,
                    log_source_selection: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    context_ids: row
                        .get::<_, Option<String>>(16)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    disabled_context_ids: row
                        .get::<_, Option<String>>(17)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    auto_include_contexts: row.get::<_, Option<i32>>(18)?.unwrap_or(1) != 0,
                    prompt_template: row.get(19)?,
                    log_watch_enabled: row.get::<_, Option<i32>>(20)?.unwrap_or(1) != 0,
                    health_check_enabled: row.get::<_, Option<i32>>(21)?.unwrap_or(1) != 0,
                    health_check_urls: row
                        .get::<_, Option<String>>(22)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    timeout_seconds: row.get::<_, Option<i64>>(23)?.map(|v| v as u64),
                    preflight_check_enabled: row.get::<_, Option<i32>>(24)?.unwrap_or(1) != 0,
                    generated_by_task_run_id: row.get(25)?,
                    enable_sweep: row.get::<_, Option<i32>>(26)?.unwrap_or(0) != 0,
                    max_sweep_iterations: row.get::<_, Option<i32>>(27)?.unwrap_or(5) as u32,
                    stages: row
                        .get::<_, Option<String>>(28)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    stop_on_failure: row.get::<_, Option<i32>>(29)?.unwrap_or(0) != 0,
                    reflection_mode: row.get::<_, Option<i32>>(30)?.unwrap_or(1) != 0,
                    model_overrides: {
                        let json_str: String = row.get::<_, Option<String>>(31)?.unwrap_or_else(|| "{}".to_string());
                        serde_json::from_str(&json_str).unwrap_or_default()
                    },
                    approval_gate: row.get::<_, Option<i32>>(32)?.unwrap_or(0) != 0,
                    completion_prompts_first: row.get::<_, Option<i32>>(33)?.unwrap_or(0) != 0,
                    is_favorite: row.get::<_, Option<i32>>(34)?.unwrap_or(0) != 0,
                    dependency_graph: row.get::<_, Option<String>>(35)?.and_then(|s| serde_json::from_str(&s).ok()),
                    cost_annotations: row.get::<_, Option<String>>(36)?.and_then(|s| serde_json::from_str(&s).ok()),
                    quality_report: row.get::<_, Option<String>>(37)?.and_then(|s| serde_json::from_str(&s).ok()),
                    acceptance_criteria: row.get::<_, Option<String>>(38)?.and_then(|s| serde_json::from_str(&s).ok()),
                    constraint_overrides: row.get::<_, Option<String>>(39)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
                    ai_reviewed: row.get::<_, Option<i32>>(40).unwrap_or(Some(1)).unwrap_or(1) != 0,
                    multi_agent_mode: true,
                    use_worktree: false,
                    // targeted_error_ids is a runtime field, not stored in DB
                    targeted_error_ids: vec![],
                })
            },
        );

        match result {
            Ok(workflow) => Ok(Some(workflow)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get unified workflow: {}", e)),
        }
    }

    /// Get a unified workflow by name (returns the first match if multiple exist)
    pub fn get_unified_workflow_by_name(
        &self,
        name: &str,
    ) -> Result<Option<crate::unified_workflows::UnifiedWorkflow>, String> {
        let conn = self.get_conn()?;

        let result = conn.query_row(
            r#"
            SELECT id, name, description, category, tags, setup_steps, verification_steps,
                   agentic_steps, completion_steps, max_iterations, provider, model,
                   skip_ai_summary, created_at, updated_at, log_source_selection,
                   context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
                   log_watch_enabled, health_check_enabled, health_check_urls, timeout_seconds,
                   preflight_check_enabled, generated_by_task_run_id, enable_sweep, max_sweep_iterations,
                   stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
                   completion_prompts_first, is_favorite, dependency_graph, cost_annotations,
                   quality_report, acceptance_criteria, constraint_overrides,
                   COALESCE(ai_reviewed, 1) as ai_reviewed
            FROM unified_workflows
            WHERE name = ?1
            ORDER BY updated_at DESC
            LIMIT 1
            "#,
            params![name],
            |row| {
                Ok(crate::unified_workflows::UnifiedWorkflow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "general".to_string()),
                    tags: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    setup_steps: row
                        .get::<_, Option<String>>(5)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    verification_steps: row
                        .get::<_, Option<String>>(6)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    agentic_steps: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    completion_steps: row
                        .get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    max_iterations: (row.get::<_, i64>(9)? as u32).max(1),
                    provider: row.get(10)?,
                    model: row.get(11)?,
                    skip_ai_summary: row.get::<_, i32>(12)? != 0,
                    created_at: row.get(13)?,
                    updated_at: row.get(14)?,
                    log_source_selection: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    context_ids: row
                        .get::<_, Option<String>>(16)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    disabled_context_ids: row
                        .get::<_, Option<String>>(17)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    auto_include_contexts: row.get::<_, Option<i32>>(18)?.unwrap_or(1) != 0,
                    prompt_template: row.get(19)?,
                    log_watch_enabled: row.get::<_, Option<i32>>(20)?.unwrap_or(1) != 0,
                    health_check_enabled: row.get::<_, Option<i32>>(21)?.unwrap_or(1) != 0,
                    health_check_urls: row
                        .get::<_, Option<String>>(22)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    timeout_seconds: row.get::<_, Option<i64>>(23)?.map(|v| v as u64),
                    preflight_check_enabled: row.get::<_, Option<i32>>(24)?.unwrap_or(1) != 0,
                    generated_by_task_run_id: row.get(25)?,
                    enable_sweep: row.get::<_, Option<i32>>(26)?.unwrap_or(0) != 0,
                    max_sweep_iterations: row.get::<_, Option<i32>>(27)?.unwrap_or(5) as u32,
                    stages: row
                        .get::<_, Option<String>>(28)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    stop_on_failure: row.get::<_, Option<i32>>(29)?.unwrap_or(0) != 0,
                    reflection_mode: row.get::<_, Option<i32>>(30)?.unwrap_or(1) != 0,
                    model_overrides: {
                        let json_str: String = row.get::<_, Option<String>>(31)?.unwrap_or_else(|| "{}".to_string());
                        serde_json::from_str(&json_str).unwrap_or_default()
                    },
                    approval_gate: row.get::<_, Option<i32>>(32)?.unwrap_or(0) != 0,
                    completion_prompts_first: row.get::<_, Option<i32>>(33)?.unwrap_or(0) != 0,
                    is_favorite: row.get::<_, Option<i32>>(34)?.unwrap_or(0) != 0,
                    dependency_graph: row.get::<_, Option<String>>(35)?.and_then(|s| serde_json::from_str(&s).ok()),
                    cost_annotations: row.get::<_, Option<String>>(36)?.and_then(|s| serde_json::from_str(&s).ok()),
                    quality_report: row.get::<_, Option<String>>(37)?.and_then(|s| serde_json::from_str(&s).ok()),
                    acceptance_criteria: row.get::<_, Option<String>>(38)?.and_then(|s| serde_json::from_str(&s).ok()),
                    constraint_overrides: row.get::<_, Option<String>>(39)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
                    ai_reviewed: row.get::<_, Option<i32>>(40).unwrap_or(Some(1)).unwrap_or(1) != 0,
                    multi_agent_mode: true,
                    use_worktree: false,
                    // targeted_error_ids is a runtime field, not stored in DB
                    targeted_error_ids: vec![],
                })
            },
        );

        match result {
            Ok(workflow) => Ok(Some(workflow)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get unified workflow by name: {}", e)),
        }
    }

    /// Create a new unified workflow
    pub fn create_unified_workflow(
        &self,
        request: &crate::unified_workflows::CreateUnifiedWorkflowRequest,
    ) -> Result<crate::unified_workflows::UnifiedWorkflow, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&request.tags).unwrap_or_else(|_| "[]".to_string());
        let setup_steps_json =
            serde_json::to_string(&request.setup_steps).unwrap_or_else(|_| "[]".to_string());
        let verification_steps_json =
            serde_json::to_string(&request.verification_steps).unwrap_or_else(|_| "[]".to_string());
        let agentic_steps_json =
            serde_json::to_string(&request.agentic_steps).unwrap_or_else(|_| "[]".to_string());
        let completion_steps_json =
            serde_json::to_string(&request.completion_steps).unwrap_or_else(|_| "[]".to_string());
        let log_source_selection_json = request
            .log_source_selection
            .as_ref()
            .map(|ls| serde_json::to_string(ls).unwrap_or_else(|_| "\"default\"".to_string()))
            .unwrap_or_else(|| "\"default\"".to_string());
        let context_ids_json = request
            .context_ids
            .as_ref()
            .map(|ids| serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());
        let disabled_context_ids_json = request
            .disabled_context_ids
            .as_ref()
            .map(|ids| serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());
        let health_check_urls_json = request
            .health_check_urls
            .as_ref()
            .map(|urls| serde_json::to_string(urls).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());
        let auto_include_contexts = request.auto_include_contexts.unwrap_or(true);
        let log_watch_enabled = request.log_watch_enabled.unwrap_or(true);
        let health_check_enabled = request.health_check_enabled.unwrap_or(true);
        let preflight_check_enabled = request.preflight_check_enabled.unwrap_or(true);

        conn.execute(
            r#"
            INSERT INTO unified_workflows (
                id, name, description, category, tags, setup_steps, verification_steps,
                agentic_steps, completion_steps, max_iterations, timeout_seconds, provider, model,
                skip_ai_summary, created_at, updated_at, log_source_selection,
                context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
                log_watch_enabled, health_check_enabled, health_check_urls, preflight_check_enabled,
                generated_by_task_run_id, enable_sweep, max_sweep_iterations,
                stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
                completion_prompts_first, dependency_graph, cost_annotations, quality_report,
                acceptance_criteria, constraint_overrides, ai_reviewed
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40)
            "#,
            params![
                id,
                request.name,
                request.description,
                request.category,
                tags_json,
                setup_steps_json,
                verification_steps_json,
                agentic_steps_json,
                completion_steps_json,
                request.max_iterations as i64,
                request.timeout_seconds.map(|t| t as i64),
                request.provider,
                request.model,
                request.skip_ai_summary,
                now,
                now,
                log_source_selection_json,
                context_ids_json,
                disabled_context_ids_json,
                auto_include_contexts,
                request.prompt_template,
                log_watch_enabled,
                health_check_enabled,
                health_check_urls_json,
                preflight_check_enabled,
                request.generated_by_task_run_id,
                request.enable_sweep.unwrap_or(false),
                request.max_sweep_iterations.unwrap_or(5) as i64,
                serde_json::to_string(&request.stages).unwrap_or_else(|_| "[]".to_string()),
                request.stop_on_failure.unwrap_or(false),
                request.reflection_mode.unwrap_or(true),
                serde_json::to_string(&request.model_overrides.clone().unwrap_or_default()).unwrap_or_else(|_| "{}".to_string()),
                request.approval_gate.unwrap_or(false),
                request.completion_prompts_first.unwrap_or(false),
                request.dependency_graph.as_ref().map(|v| v.to_string()),
                request.cost_annotations.as_ref().map(|v| v.to_string()),
                request.quality_report.as_ref().map(|v| v.to_string()),
                request.acceptance_criteria.as_ref().map(|v| v.to_string()),
                serde_json::to_string(&request.constraint_overrides.clone().unwrap_or_default()).unwrap_or_else(|_| "{}".to_string()),
                request.ai_reviewed.unwrap_or(true),
            ],
        )
        .map_err(|e| format!("Failed to create unified workflow: {}", e))?;

        self.get_unified_workflow(&id)?
            .ok_or_else(|| "Failed to retrieve created workflow".to_string())
    }

    /// Create a new unified workflow with a specific ID (for imports)
    pub fn create_unified_workflow_with_id(
        &self,
        id: &str,
        request: &crate::unified_workflows::CreateUnifiedWorkflowRequest,
    ) -> Result<crate::unified_workflows::UnifiedWorkflow, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&request.tags).unwrap_or_else(|_| "[]".to_string());
        let setup_steps_json =
            serde_json::to_string(&request.setup_steps).unwrap_or_else(|_| "[]".to_string());
        let verification_steps_json =
            serde_json::to_string(&request.verification_steps).unwrap_or_else(|_| "[]".to_string());
        let agentic_steps_json =
            serde_json::to_string(&request.agentic_steps).unwrap_or_else(|_| "[]".to_string());
        let completion_steps_json =
            serde_json::to_string(&request.completion_steps).unwrap_or_else(|_| "[]".to_string());
        let log_source_selection_json = request
            .log_source_selection
            .as_ref()
            .map(|ls| serde_json::to_string(ls).unwrap_or_else(|_| "\"default\"".to_string()))
            .unwrap_or_else(|| "\"default\"".to_string());
        let context_ids_json = request
            .context_ids
            .as_ref()
            .map(|ids| serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());
        let disabled_context_ids_json = request
            .disabled_context_ids
            .as_ref()
            .map(|ids| serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());
        let health_check_urls_json = request
            .health_check_urls
            .as_ref()
            .map(|urls| serde_json::to_string(urls).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());
        let auto_include_contexts = request.auto_include_contexts.unwrap_or(true);
        let log_watch_enabled = request.log_watch_enabled.unwrap_or(true);
        let health_check_enabled = request.health_check_enabled.unwrap_or(true);
        let preflight_check_enabled = request.preflight_check_enabled.unwrap_or(true);

        conn.execute(
            r#"
            INSERT INTO unified_workflows (
                id, name, description, category, tags, setup_steps, verification_steps,
                agentic_steps, completion_steps, max_iterations, timeout_seconds, provider, model,
                skip_ai_summary, created_at, updated_at, log_source_selection,
                context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
                log_watch_enabled, health_check_enabled, health_check_urls, preflight_check_enabled,
                generated_by_task_run_id, enable_sweep, max_sweep_iterations,
                stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
                completion_prompts_first, dependency_graph, cost_annotations, quality_report,
                acceptance_criteria, constraint_overrides, ai_reviewed
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40)
            "#,
            params![
                id,
                request.name,
                request.description,
                request.category,
                tags_json,
                setup_steps_json,
                verification_steps_json,
                agentic_steps_json,
                completion_steps_json,
                request.max_iterations as i64,
                request.timeout_seconds.map(|t| t as i64),
                request.provider,
                request.model,
                request.skip_ai_summary,
                now,
                now,
                log_source_selection_json,
                context_ids_json,
                disabled_context_ids_json,
                auto_include_contexts,
                request.prompt_template,
                log_watch_enabled,
                health_check_enabled,
                health_check_urls_json,
                preflight_check_enabled,
                request.generated_by_task_run_id,
                request.enable_sweep.unwrap_or(false),
                request.max_sweep_iterations.unwrap_or(5) as i64,
                serde_json::to_string(&request.stages).unwrap_or_else(|_| "[]".to_string()),
                request.stop_on_failure.unwrap_or(false),
                request.reflection_mode.unwrap_or(true),
                serde_json::to_string(&request.model_overrides.clone().unwrap_or_default()).unwrap_or_else(|_| "{}".to_string()),
                request.approval_gate.unwrap_or(false),
                request.completion_prompts_first.unwrap_or(false),
                request.dependency_graph.as_ref().map(|v| v.to_string()),
                request.cost_annotations.as_ref().map(|v| v.to_string()),
                request.quality_report.as_ref().map(|v| v.to_string()),
                request.acceptance_criteria.as_ref().map(|v| v.to_string()),
                serde_json::to_string(&request.constraint_overrides.clone().unwrap_or_default()).unwrap_or_else(|_| "{}".to_string()),
                request.ai_reviewed.unwrap_or(true),
            ],
        )
        .map_err(|e| format!("Failed to create unified workflow: {}", e))?;

        self.get_unified_workflow(id)?
            .ok_or_else(|| "Failed to retrieve created workflow".to_string())
    }

    /// Update an existing unified workflow
    pub fn update_unified_workflow(
        &self,
        id: &str,
        request: &crate::unified_workflows::UpdateUnifiedWorkflowRequest,
    ) -> Result<crate::unified_workflows::UnifiedWorkflow, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Get existing workflow
        let existing = self
            .get_unified_workflow(id)?
            .ok_or_else(|| format!("Unified workflow not found: {}", id))?;

        // Merge updates
        let name = request.name.as_ref().unwrap_or(&existing.name);
        let description = request
            .description
            .as_ref()
            .unwrap_or(&existing.description);
        let category = request.category.as_ref().unwrap_or(&existing.category);
        let tags = request.tags.as_ref().unwrap_or(&existing.tags);
        let setup_steps = request
            .setup_steps
            .as_ref()
            .unwrap_or(&existing.setup_steps);
        let verification_steps = request
            .verification_steps
            .as_ref()
            .unwrap_or(&existing.verification_steps);
        let agentic_steps = request
            .agentic_steps
            .as_ref()
            .unwrap_or(&existing.agentic_steps);
        let completion_steps = request
            .completion_steps
            .as_ref()
            .unwrap_or(&existing.completion_steps);
        let max_iterations = request.max_iterations.unwrap_or(existing.max_iterations);
        // For timeout_seconds: Option<Option<u64>> where:
        // - None: Not updating, keep existing
        // - Some(None): Explicitly disable timeout
        // - Some(Some(N)): Set timeout to N seconds
        let timeout_seconds: Option<u64> = match &request.timeout_seconds {
            Some(val) => *val,                // Use provided value (including None to disable)
            None => existing.timeout_seconds, // Not provided, keep existing
        };
        let provider = request.provider.as_ref().or(existing.provider.as_ref());
        let model = request.model.as_ref().or(existing.model.as_ref());
        let skip_ai_summary = request.skip_ai_summary.unwrap_or(existing.skip_ai_summary);
        let log_source_selection = request
            .log_source_selection
            .as_ref()
            .unwrap_or(&existing.log_source_selection);
        // For prompt_template: if request has a value, use it; otherwise keep existing
        // Empty string means clear the template (set to NULL)
        let prompt_template: Option<&str> = match &request.prompt_template {
            Some(val) if val.is_empty() => None, // Empty string clears the template
            Some(val) => Some(val.as_str()),     // Non-empty string sets the template
            None => existing.prompt_template.as_deref(), // Not provided keeps existing
        };
        let context_ids = request
            .context_ids
            .as_ref()
            .unwrap_or(&existing.context_ids);
        let disabled_context_ids = request
            .disabled_context_ids
            .as_ref()
            .unwrap_or(&existing.disabled_context_ids);
        let auto_include_contexts = request
            .auto_include_contexts
            .unwrap_or(existing.auto_include_contexts);
        let log_watch_enabled = request
            .log_watch_enabled
            .unwrap_or(existing.log_watch_enabled);
        let health_check_enabled = request
            .health_check_enabled
            .unwrap_or(existing.health_check_enabled);
        let health_check_urls = request
            .health_check_urls
            .as_ref()
            .unwrap_or(&existing.health_check_urls);
        let preflight_check_enabled = request
            .preflight_check_enabled
            .unwrap_or(existing.preflight_check_enabled);
        let enable_sweep = request.enable_sweep.unwrap_or(existing.enable_sweep);
        let max_sweep_iterations = request
            .max_sweep_iterations
            .unwrap_or(existing.max_sweep_iterations);
        let stages = request.stages.as_ref().unwrap_or(&existing.stages);
        let stop_on_failure = request.stop_on_failure.unwrap_or(existing.stop_on_failure);
        let constraint_overrides = request
            .constraint_overrides
            .as_ref()
            .unwrap_or(&existing.constraint_overrides);
        let approval_gate = request.approval_gate.unwrap_or(existing.approval_gate);
        let reflection_mode = request.reflection_mode.unwrap_or(existing.reflection_mode);
        let completion_prompts_first = request
            .completion_prompts_first
            .unwrap_or(existing.completion_prompts_first);
        let model_overrides = request
            .model_overrides
            .as_ref()
            .unwrap_or(&existing.model_overrides);
        let dependency_graph = request
            .dependency_graph
            .as_ref()
            .or(existing.dependency_graph.as_ref());
        let cost_annotations = request
            .cost_annotations
            .as_ref()
            .or(existing.cost_annotations.as_ref());
        let quality_report = request
            .quality_report
            .as_ref()
            .or(existing.quality_report.as_ref());
        let acceptance_criteria = request
            .acceptance_criteria
            .as_ref()
            .or(existing.acceptance_criteria.as_ref());
        let _ai_reviewed = request.ai_reviewed.unwrap_or(existing.ai_reviewed);

        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
        let setup_steps_json =
            serde_json::to_string(setup_steps).unwrap_or_else(|_| "[]".to_string());
        let verification_steps_json =
            serde_json::to_string(verification_steps).unwrap_or_else(|_| "[]".to_string());
        let agentic_steps_json =
            serde_json::to_string(agentic_steps).unwrap_or_else(|_| "[]".to_string());
        let completion_steps_json =
            serde_json::to_string(completion_steps).unwrap_or_else(|_| "[]".to_string());
        let log_source_selection_json = serde_json::to_string(log_source_selection)
            .unwrap_or_else(|_| "\"default\"".to_string());
        let context_ids_json =
            serde_json::to_string(context_ids).unwrap_or_else(|_| "[]".to_string());
        let disabled_context_ids_json =
            serde_json::to_string(disabled_context_ids).unwrap_or_else(|_| "[]".to_string());
        let health_check_urls_json =
            serde_json::to_string(health_check_urls).unwrap_or_else(|_| "[]".to_string());
        let stages_json = serde_json::to_string(stages).unwrap_or_else(|_| "[]".to_string());
        let model_overrides_json =
            serde_json::to_string(model_overrides).unwrap_or_else(|_| "{}".to_string());

        conn.execute(
            r#"
            UPDATE unified_workflows SET
                name = ?1,
                description = ?2,
                category = ?3,
                tags = ?4,
                setup_steps = ?5,
                verification_steps = ?6,
                agentic_steps = ?7,
                completion_steps = ?8,
                max_iterations = ?9,
                timeout_seconds = ?10,
                provider = ?11,
                model = ?12,
                skip_ai_summary = ?13,
                updated_at = ?14,
                log_source_selection = ?15,
                prompt_template = ?16,
                context_ids = ?17,
                disabled_context_ids = ?18,
                auto_include_contexts = ?19,
                log_watch_enabled = ?20,
                health_check_enabled = ?21,
                health_check_urls = ?22,
                preflight_check_enabled = ?23,
                enable_sweep = ?24,
                max_sweep_iterations = ?25,
                stages = ?26,
                stop_on_failure = ?27,
                approval_gate = ?28,
                reflection_mode = ?29,
                model_overrides = ?30,
                completion_prompts_first = ?31,
                dependency_graph = ?32,
                cost_annotations = ?33,
                quality_report = ?34,
                acceptance_criteria = ?35,
                constraint_overrides = ?36,
                ai_reviewed = ?37
            WHERE id = ?38
            "#,
            params![
                name,
                description,
                category,
                tags_json,
                setup_steps_json,
                verification_steps_json,
                agentic_steps_json,
                completion_steps_json,
                max_iterations as i64,
                timeout_seconds.map(|v| v as i64),
                provider,
                model,
                skip_ai_summary,
                now,
                log_source_selection_json,
                prompt_template,
                context_ids_json,
                disabled_context_ids_json,
                auto_include_contexts,
                log_watch_enabled,
                health_check_enabled,
                health_check_urls_json,
                preflight_check_enabled,
                enable_sweep,
                max_sweep_iterations as i64,
                stages_json,
                stop_on_failure,
                approval_gate,
                reflection_mode,
                model_overrides_json,
                completion_prompts_first,
                dependency_graph.map(|v| v.to_string()),
                cost_annotations.map(|v| v.to_string()),
                quality_report.map(|v| v.to_string()),
                acceptance_criteria.map(|v| v.to_string()),
                serde_json::to_string(constraint_overrides).unwrap_or_else(|_| "{}".to_string()),
                request.ai_reviewed.unwrap_or(existing.ai_reviewed),
                id,
            ],
        )
        .map_err(|e| format!("Failed to update unified workflow: {}", e))?;

        self.get_unified_workflow(id)?
            .ok_or_else(|| "Failed to retrieve updated workflow".to_string())
    }

    /// Delete a unified workflow
    pub fn delete_unified_workflow(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows_affected = conn
            .execute("DELETE FROM unified_workflows WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete unified workflow: {}", e))?;

        Ok(rows_affected > 0)
    }

    /// Search unified workflows with filters
    pub fn search_unified_workflows(
        &self,
        query: &crate::unified_workflows::SearchUnifiedWorkflowsQuery,
    ) -> Result<Vec<crate::unified_workflows::UnifiedWorkflow>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, name, description, category, tags, setup_steps, verification_steps,
                   agentic_steps, completion_steps, max_iterations, provider, model,
                   skip_ai_summary, created_at, updated_at, log_source_selection,
                   context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
                   log_watch_enabled, health_check_enabled, health_check_urls, timeout_seconds,
                   preflight_check_enabled, generated_by_task_run_id, enable_sweep, max_sweep_iterations,
                   stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
                   completion_prompts_first, is_favorite, dependency_graph, cost_annotations,
                   quality_report, acceptance_criteria, constraint_overrides,
                   COALESCE(ai_reviewed, 1) as ai_reviewed
            FROM unified_workflows
            WHERE 1=1
            "#,
        );

        let mut params_vec: Vec<String> = Vec::new();

        if let Some(q) = &query.q {
            sql.push_str(" AND (name LIKE ? OR description LIKE ?)");
            let pattern = format!("%{}%", q);
            params_vec.push(pattern.clone());
            params_vec.push(pattern);
        }

        if let Some(category) = &query.category {
            sql.push_str(" AND category = ?");
            params_vec.push(category.clone());
        }

        if let Some(tag) = &query.tag {
            sql.push_str(" AND tags LIKE ?");
            params_vec.push(format!("%\"{}%", tag));
        }

        sql.push_str(" ORDER BY is_favorite DESC, updated_at DESC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = params_vec
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();

        let workflows = stmt
            .query_map(params.as_slice(), |row| {
                Ok(crate::unified_workflows::UnifiedWorkflow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "general".to_string()),
                    tags: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    setup_steps: row
                        .get::<_, Option<String>>(5)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    verification_steps: row
                        .get::<_, Option<String>>(6)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    agentic_steps: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    completion_steps: row
                        .get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    max_iterations: (row.get::<_, i64>(9)? as u32).max(1),
                    provider: row.get(10)?,
                    model: row.get(11)?,
                    skip_ai_summary: row.get::<_, i32>(12)? != 0,
                    created_at: row.get(13)?,
                    updated_at: row.get(14)?,
                    log_source_selection: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    context_ids: row
                        .get::<_, Option<String>>(16)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    disabled_context_ids: row
                        .get::<_, Option<String>>(17)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    auto_include_contexts: row.get::<_, Option<i32>>(18)?.unwrap_or(1) != 0,
                    prompt_template: row.get(19)?,
                    log_watch_enabled: row.get::<_, Option<i32>>(20)?.unwrap_or(1) != 0,
                    health_check_enabled: row.get::<_, Option<i32>>(21)?.unwrap_or(1) != 0,
                    health_check_urls: row
                        .get::<_, Option<String>>(22)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    timeout_seconds: row.get::<_, Option<i64>>(23)?.map(|v| v as u64),
                    preflight_check_enabled: row.get::<_, Option<i32>>(24)?.unwrap_or(1) != 0,
                    generated_by_task_run_id: row.get(25)?,
                    enable_sweep: row.get::<_, Option<i32>>(26)?.unwrap_or(0) != 0,
                    max_sweep_iterations: row.get::<_, Option<i32>>(27)?.unwrap_or(5) as u32,
                    stages: row
                        .get::<_, Option<String>>(28)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    stop_on_failure: row.get::<_, Option<i32>>(29)?.unwrap_or(0) != 0,
                    reflection_mode: row.get::<_, Option<i32>>(30)?.unwrap_or(1) != 0,
                    model_overrides: {
                        let json_str: String = row
                            .get::<_, Option<String>>(31)?
                            .unwrap_or_else(|| "{}".to_string());
                        serde_json::from_str(&json_str).unwrap_or_default()
                    },
                    approval_gate: row.get::<_, Option<i32>>(32)?.unwrap_or(0) != 0,
                    completion_prompts_first: row.get::<_, Option<i32>>(33)?.unwrap_or(0) != 0,
                    is_favorite: row.get::<_, Option<i32>>(34)?.unwrap_or(0) != 0,
                    dependency_graph: row
                        .get::<_, Option<String>>(35)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    cost_annotations: row
                        .get::<_, Option<String>>(36)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    quality_report: row
                        .get::<_, Option<String>>(37)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    acceptance_criteria: row
                        .get::<_, Option<String>>(38)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    constraint_overrides: row
                        .get::<_, Option<String>>(39)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    ai_reviewed: row
                        .get::<_, Option<i32>>(40)
                        .unwrap_or(Some(1))
                        .unwrap_or(1)
                        != 0,
                    multi_agent_mode: true,
                    use_worktree: false,
                    // targeted_error_ids is a runtime field, not stored in DB
                    targeted_error_ids: vec![],
                })
            })
            .map_err(|e| format!("Failed to search unified workflows: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(workflows)
    }

    /// Duplicate a unified workflow
    pub fn duplicate_unified_workflow(
        &self,
        id: &str,
    ) -> Result<crate::unified_workflows::UnifiedWorkflow, String> {
        let original = self
            .get_unified_workflow(id)?
            .ok_or_else(|| format!("Unified workflow not found: {}", id))?;

        let create_request = crate::unified_workflows::CreateUnifiedWorkflowRequest {
            name: format!("{} (Copy)", original.name),
            description: original.description,
            category: original.category,
            tags: original.tags,
            setup_steps: original.setup_steps,
            verification_steps: original.verification_steps,
            agentic_steps: original.agentic_steps,
            completion_steps: original.completion_steps,
            max_iterations: original.max_iterations,
            timeout_seconds: original.timeout_seconds,
            provider: original.provider,
            model: original.model,
            skip_ai_summary: original.skip_ai_summary,
            log_source_selection: Some(original.log_source_selection),
            context_ids: Some(original.context_ids),
            disabled_context_ids: Some(original.disabled_context_ids),
            auto_include_contexts: Some(original.auto_include_contexts),
            prompt_template: original.prompt_template,
            log_watch_enabled: Some(original.log_watch_enabled),
            health_check_enabled: Some(original.health_check_enabled),
            health_check_urls: Some(original.health_check_urls),
            preflight_check_enabled: Some(original.preflight_check_enabled),
            targeted_error_ids: None,
            generated_by_task_run_id: None, // Don't copy the generator reference
            enable_sweep: Some(original.enable_sweep),
            max_sweep_iterations: Some(original.max_sweep_iterations),
            stages: Some(original.stages),
            stop_on_failure: Some(original.stop_on_failure),
            constraint_overrides: Some(original.constraint_overrides),
            approval_gate: Some(original.approval_gate),
            reflection_mode: Some(original.reflection_mode),
            completion_prompts_first: Some(original.completion_prompts_first),
            model_overrides: Some(original.model_overrides),
            dependency_graph: original.dependency_graph,
            cost_annotations: original.cost_annotations,
            quality_report: original.quality_report,
            acceptance_criteria: original.acceptance_criteria,
            ai_reviewed: Some(original.ai_reviewed),
        };

        self.create_unified_workflow(&create_request)
    }

    /// Toggle the is_favorite flag on a unified workflow.
    /// Returns the new favorite state, or an error if the workflow doesn't exist.
    pub fn toggle_unified_workflow_favorite(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows_affected = conn
            .execute(
                "UPDATE unified_workflows SET is_favorite = CASE WHEN is_favorite = 1 THEN 0 ELSE 1 END WHERE id = ?1",
                params![id],
            )
            .map_err(|e| format!("Failed to toggle favorite: {}", e))?;

        if rows_affected == 0 {
            return Err(format!("Workflow not found: {}", id));
        }

        let new_state: bool = conn
            .query_row(
                "SELECT is_favorite FROM unified_workflows WHERE id = ?1",
                params![id],
                |row| Ok(row.get::<_, i32>(0)? != 0),
            )
            .map_err(|e| format!("Failed to read favorite state: {}", e))?;

        Ok(new_state)
    }

    // ========================================================================
    // Workflow Sync Helpers
    // ========================================================================

    /// Get all workflows with sync_pending = 1 (created/modified offline).
    pub fn get_pending_sync_workflows(
        &self,
    ) -> Result<Vec<crate::unified_workflows::UnifiedWorkflow>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, category, tags, setup_steps, verification_steps,
                       agentic_steps, completion_steps, max_iterations, provider, model,
                       skip_ai_summary, created_at, updated_at, log_source_selection,
                       context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
                       log_watch_enabled, health_check_enabled, health_check_urls, timeout_seconds,
                       preflight_check_enabled, generated_by_task_run_id, enable_sweep,
                       max_sweep_iterations, stages, stop_on_failure, reflection_mode, model_overrides,
                       approval_gate, completion_prompts_first, is_favorite, dependency_graph,
                       cost_annotations, quality_report, acceptance_criteria, constraint_overrides,
                       COALESCE(ai_reviewed, 1) as ai_reviewed
                FROM unified_workflows
                WHERE sync_pending = 1
                "#,
            )
            .map_err(|e| format!("Failed to prepare pending sync query: {}", e))?;

        let workflows = stmt
            .query_map([], |row| {
                Ok(crate::unified_workflows::UnifiedWorkflow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "general".to_string()),
                    tags: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    setup_steps: row
                        .get::<_, Option<String>>(5)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    verification_steps: row
                        .get::<_, Option<String>>(6)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    agentic_steps: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    completion_steps: row
                        .get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    max_iterations: (row.get::<_, i64>(9)? as u32).max(1),
                    provider: row.get(10)?,
                    model: row.get(11)?,
                    skip_ai_summary: row.get::<_, i32>(12)? != 0,
                    created_at: row.get(13)?,
                    updated_at: row.get(14)?,
                    log_source_selection: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    context_ids: row
                        .get::<_, Option<String>>(16)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    disabled_context_ids: row
                        .get::<_, Option<String>>(17)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    auto_include_contexts: row.get::<_, Option<i32>>(18)?.unwrap_or(1) != 0,
                    prompt_template: row.get(19)?,
                    log_watch_enabled: row.get::<_, Option<i32>>(20)?.unwrap_or(1) != 0,
                    health_check_enabled: row.get::<_, Option<i32>>(21)?.unwrap_or(1) != 0,
                    health_check_urls: row
                        .get::<_, Option<String>>(22)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    timeout_seconds: row.get::<_, Option<i64>>(23)?.map(|v| v as u64),
                    preflight_check_enabled: row.get::<_, Option<i32>>(24)?.unwrap_or(1) != 0,
                    generated_by_task_run_id: row.get(25)?,
                    enable_sweep: row.get::<_, Option<i32>>(26)?.unwrap_or(0) != 0,
                    max_sweep_iterations: row.get::<_, Option<i32>>(27)?.unwrap_or(5) as u32,
                    stages: row
                        .get::<_, Option<String>>(28)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    stop_on_failure: row.get::<_, Option<i32>>(29)?.unwrap_or(0) != 0,
                    reflection_mode: row.get::<_, Option<i32>>(30)?.unwrap_or(1) != 0,
                    model_overrides: {
                        let json_str: String = row
                            .get::<_, Option<String>>(31)?
                            .unwrap_or_else(|| "{}".to_string());
                        serde_json::from_str(&json_str).unwrap_or_default()
                    },
                    approval_gate: row.get::<_, Option<i32>>(32)?.unwrap_or(0) != 0,
                    completion_prompts_first: row.get::<_, Option<i32>>(33)?.unwrap_or(0) != 0,
                    is_favorite: row.get::<_, Option<i32>>(34)?.unwrap_or(0) != 0,
                    dependency_graph: row
                        .get::<_, Option<String>>(35)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    cost_annotations: row
                        .get::<_, Option<String>>(36)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    quality_report: row
                        .get::<_, Option<String>>(37)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    acceptance_criteria: row
                        .get::<_, Option<String>>(38)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    constraint_overrides: row
                        .get::<_, Option<String>>(39)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    ai_reviewed: row
                        .get::<_, Option<i32>>(40)
                        .unwrap_or(Some(1))
                        .unwrap_or(1)
                        != 0,
                    multi_agent_mode: true,
                    use_worktree: false,
                    targeted_error_ids: vec![],
                })
            })
            .map_err(|e| format!("Failed to query pending sync workflows: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(workflows)
    }

    /// Clear the sync_pending flag for a workflow after successful push to backend.
    pub fn clear_sync_pending(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE unified_workflows SET sync_pending = 0 WHERE id = ?1",
            params![id],
        )
        .map_err(|e| format!("Failed to clear sync_pending: {}", e))?;
        Ok(())
    }

    /// Set the sync_pending flag for a workflow (created/modified while offline).
    pub fn set_sync_pending(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE unified_workflows SET sync_pending = 1 WHERE id = ?1",
            params![id],
        )
        .map_err(|e| format!("Failed to set sync_pending: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Slash Command Import Methods
    // =========================================================================

    /// List all workflows that were imported from slash commands (have source_file_path set).
    /// Returns (id, source_file_path, source_content_hash) tuples.
    pub fn list_slash_command_sources(&self) -> Result<Vec<(String, String, String)>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, source_file_path, source_content_hash FROM unified_workflows WHERE source_file_path IS NOT NULL",
            )
            .map_err(|e| format!("Failed to prepare slash command query: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| format!("Failed to query slash command sources: {}", e))?;

        let mut results = Vec::new();
        for r in rows.flatten() {
            results.push(r);
        }
        Ok(results)
    }

    /// Create a unified workflow imported from a slash command file.
    pub fn create_slash_command_workflow(
        &self,
        request: &crate::unified_workflows::CreateUnifiedWorkflowRequest,
        source_file_path: &str,
        source_content_hash: &str,
    ) -> Result<crate::unified_workflows::UnifiedWorkflow, String> {
        let workflow = self.create_unified_workflow(request)?;

        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE unified_workflows SET source_file_path = ?1, source_content_hash = ?2 WHERE id = ?3",
            params![source_file_path, source_content_hash, workflow.id],
        )
        .map_err(|e| format!("Failed to set slash command source: {}", e))?;

        Ok(workflow)
    }

    /// Update a slash command workflow's content and hash.
    pub fn update_slash_command_content(
        &self,
        id: &str,
        request: &crate::unified_workflows::UpdateUnifiedWorkflowRequest,
        new_hash: &str,
    ) -> Result<crate::unified_workflows::UnifiedWorkflow, String> {
        let workflow = self.update_unified_workflow(id, request)?;

        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE unified_workflows SET source_content_hash = ?1 WHERE id = ?2",
            params![new_hash, id],
        )
        .map_err(|e| format!("Failed to update slash command hash: {}", e))?;

        Ok(workflow)
    }
}
