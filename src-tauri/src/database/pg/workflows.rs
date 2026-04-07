//! PostgreSQL unified workflow operations via Clorinde-generated queries.
//!
//! Core CRUD module for workflow definitions.
//! Uses a shared row-mapping helper since all SELECT queries return the same column set.

use std::collections::HashMap;

use super::PgDb;
use crate::unified_workflows::UnifiedWorkflow;

/// Convert Clorinde empty-string (from NULL) to Option::None.
fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Parse JSON string to a typed value, returning the default on failure.
fn json_or_default<T: serde::de::DeserializeOwned + Default>(s: &str) -> T {
    if s.is_empty() {
        T::default()
    } else {
        serde_json::from_str(s).unwrap_or_default()
    }
}

/// Parse JSON string to Option<Value>, returning None on empty/failure.
fn json_opt(s: &str) -> Option<serde_json::Value> {
    if s.is_empty() {
        None
    } else {
        serde_json::from_str(s).ok()
    }
}

/// Map a Clorinde row (all workflow SELECT queries share the same column set) to UnifiedWorkflow.
///
/// This macro avoids repeating the 48-field mapping for every query variant.
/// Clorinde generates identically-shaped structs for list/get/search/pending queries.
macro_rules! row_to_workflow {
    ($r:expr) => {{
        UnifiedWorkflow {
            id: $r.id,
            name: $r.name,
            description: $r.description,
            category: $r.category,
            tags: json_or_default(&$r.tags),
            setup_steps: json_or_default(&$r.setup_steps),
            verification_steps: json_or_default(&$r.verification_steps),
            agentic_steps: json_or_default(&$r.agentic_steps),
            completion_steps: json_or_default(&$r.completion_steps),
            max_iterations: ($r.max_iterations as u32).max(1),
            provider: non_empty($r.provider),
            model: non_empty($r.model),
            skip_ai_summary: $r.skip_ai_summary,
            created_at: $r.created_at.to_rfc3339(),
            updated_at: $r.updated_at.to_rfc3339(),
            log_source_selection: json_or_default(&$r.log_source_selection),
            context_ids: json_or_default(&$r.context_ids),
            disabled_context_ids: json_or_default(&$r.disabled_context_ids),
            auto_include_contexts: $r.auto_include_contexts,
            prompt_template: non_empty($r.prompt_template),
            log_watch_enabled: $r.log_watch_enabled,
            health_check_enabled: $r.health_check_enabled,
            health_check_urls: json_or_default(&$r.health_check_urls),
            timeout_seconds: if $r.timeout_seconds == 0 {
                None
            } else {
                Some($r.timeout_seconds as u64)
            },
            preflight_check_enabled: $r.preflight_check_enabled,
            generated_by_task_run_id: non_empty($r.generated_by_task_run_id),
            enable_sweep: $r.enable_sweep,
            max_sweep_iterations: $r.max_sweep_iterations as u32,
            stages: json_or_default(&$r.stages),
            stop_on_failure: $r.stop_on_failure,
            reflection_mode: $r.reflection_mode,
            model_overrides: json_or_default(&$r.model_overrides),
            approval_gate: $r.approval_gate,
            completion_prompts_first: $r.completion_prompts_first,
            is_favorite: $r.is_favorite,
            dependency_graph: json_opt(&$r.dependency_graph),
            cost_annotations: json_opt(&$r.cost_annotations),
            quality_report: json_opt(&$r.quality_report),
            acceptance_criteria: json_opt(&$r.acceptance_criteria),
            constraint_overrides: json_or_default(&$r.constraint_overrides),
            ai_reviewed: $r.ai_reviewed,
            workflow_architecture: {
                let s = &$r.workflow_architecture;
                if s.is_empty() {
                    None
                } else {
                    serde_json::from_str(&format!("\"{}\"", s)).ok()
                }
            },
            multi_agent_mode: true,
            enforce_token_budget: $r.enforce_token_budget,
            rollback_policy: non_empty($r.rollback_policy),
            strict_cwd: $r.strict_cwd,
            tool_tags: json_or_default(&$r.tool_tags),
            flow_control_json: non_empty($r.flow_control_json),
            phase_timeouts_json: non_empty($r.phase_timeouts_json),
            use_worktree: false,
            targeted_error_ids: vec![],
            security_profile: None, // TODO: read from DB once column is added
            max_fix_attempts: 3,
            max_ci_auto_resumes: 10,
        }
    }};
}

impl PgDb {
    /// List all unified workflows, ordered by favorite then updated_at.
    pub async fn list_unified_workflows(&self) -> Result<Vec<UnifiedWorkflow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::workflows::list_unified_workflows()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG list_unified_workflows: {}", e))?;

        Ok(rows.into_iter().map(|r| row_to_workflow!(r)).collect())
    }

    /// Get a single unified workflow by ID.
    pub async fn get_unified_workflow(&self, id: &str) -> Result<Option<UnifiedWorkflow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = qontinui_db::queries::workflows::get_unified_workflow()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG get_unified_workflow: {}", e))?;

        Ok(row.map(|r| row_to_workflow!(r)))
    }

    /// Get a unified workflow by name (first match).
    pub async fn get_unified_workflow_by_name(
        &self,
        name: &str,
    ) -> Result<Option<UnifiedWorkflow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = qontinui_db::queries::workflows::get_unified_workflow_by_name()
            .bind(&conn, &name)
            .opt()
            .await
            .map_err(|e| format!("PG get_unified_workflow_by_name: {}", e))?;

        Ok(row.map(|r| row_to_workflow!(r)))
    }

    /// Create a new unified workflow.
    pub async fn create_unified_workflow(
        &self,
        request: &crate::unified_workflows::CreateUnifiedWorkflowRequest,
    ) -> Result<UnifiedWorkflow, String> {
        let id = uuid::Uuid::new_v4().to_string();
        self.create_unified_workflow_with_id(&id, request).await
    }

    /// Create a unified workflow with a specific ID (for imports).
    pub async fn create_unified_workflow_with_id(
        &self,
        id: &str,
        request: &crate::unified_workflows::CreateUnifiedWorkflowRequest,
    ) -> Result<UnifiedWorkflow, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let tags_json = serde_json::to_string(&request.tags).unwrap_or_else(|_| "[]".to_string());
        let setup_json =
            serde_json::to_string(&request.setup_steps).unwrap_or_else(|_| "[]".to_string());
        let verification_json =
            serde_json::to_string(&request.verification_steps).unwrap_or_else(|_| "[]".to_string());
        let agentic_json =
            serde_json::to_string(&request.agentic_steps).unwrap_or_else(|_| "[]".to_string());
        let completion_json =
            serde_json::to_string(&request.completion_steps).unwrap_or_else(|_| "[]".to_string());
        let log_sel_json = request
            .log_source_selection
            .as_ref()
            .map(|ls| serde_json::to_string(ls).unwrap_or_else(|_| "\"default\"".to_string()))
            .unwrap_or_else(|| "\"default\"".to_string());
        let ctx_json = request
            .context_ids
            .as_ref()
            .map(|ids| serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());
        let dis_ctx_json = request
            .disabled_context_ids
            .as_ref()
            .map(|ids| serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());
        let hc_urls_json = request
            .health_check_urls
            .as_ref()
            .map(|urls| serde_json::to_string(urls).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());
        let stages_json =
            serde_json::to_string(&request.stages).unwrap_or_else(|_| "[]".to_string());
        let model_ov_json =
            serde_json::to_string(&request.model_overrides.clone().unwrap_or_default())
                .unwrap_or_else(|_| "{}".to_string());
        let constraint_ov_json =
            serde_json::to_string(&request.constraint_overrides.clone().unwrap_or_default())
                .unwrap_or_else(|_| "{}".to_string());
        let tool_tags_json =
            serde_json::to_string(&request.tool_tags).unwrap_or_else(|_| "[]".to_string());
        let timeout_secs: Option<i64> = request.timeout_seconds.map(|t| t as i64);
        let max_iters = request.max_iterations as i64;
        let max_sweep = request.max_sweep_iterations.unwrap_or(5) as i64;
        let dep_graph_str = request.dependency_graph.as_ref().map(|v| v.to_string());
        let cost_ann_str = request.cost_annotations.as_ref().map(|v| v.to_string());
        let quality_str = request.quality_report.as_ref().map(|v| v.to_string());
        let accept_str = request.acceptance_criteria.as_ref().map(|v| v.to_string());
        let arch_str = request.workflow_architecture.as_ref().map(|a| {
            serde_json::to_value(a)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| format!("{:?}", a).to_lowercase())
        });
        let rollback = request
            .rollback_policy
            .clone()
            .unwrap_or_else(|| "none".to_string());

        let desc_opt: Option<&str> = Some(request.description.as_str());
        let rollback_opt: Option<&str> = Some(rollback.as_str());

        qontinui_db::queries::workflows::create_unified_workflow()
            .bind(
                &conn,
                &id,
                &request.name.as_str(),
                &desc_opt,
                &request.category.as_str(),
                &tags_json.as_str(),
                &setup_json.as_str(),
                &verification_json.as_str(),
                &agentic_json.as_str(),
                &completion_json.as_str(),
                &max_iters,
                &timeout_secs,
                &request.provider.as_deref(),
                &request.model.as_deref(),
                &request.skip_ai_summary,
                &log_sel_json.as_str(),
                &ctx_json.as_str(),
                &dis_ctx_json.as_str(),
                &request.auto_include_contexts.unwrap_or(true),
                &request.prompt_template.as_deref(),
                &request.log_watch_enabled.unwrap_or(true),
                &request.health_check_enabled.unwrap_or(true),
                &hc_urls_json.as_str(),
                &request.preflight_check_enabled.unwrap_or(true),
                &request.generated_by_task_run_id.as_deref(),
                &request.enable_sweep.unwrap_or(false),
                &max_sweep,
                &stages_json.as_str(),
                &request.stop_on_failure.unwrap_or(false),
                &request.reflection_mode.unwrap_or(true),
                &model_ov_json.as_str(),
                &request.approval_gate.unwrap_or(false),
                &request.completion_prompts_first.unwrap_or(false),
                &dep_graph_str.as_deref(),
                &cost_ann_str.as_deref(),
                &quality_str.as_deref(),
                &accept_str.as_deref(),
                &constraint_ov_json.as_str(),
                &request.ai_reviewed.unwrap_or(true),
                &arch_str.as_deref(),
                &request.strict_cwd,
                &tool_tags_json.as_str(),
                &rollback_opt,
                &request.enforce_token_budget.unwrap_or(false),
                &request.flow_control_json.as_deref(),
                &request.phase_timeouts_json.as_deref(),
            )
            .one()
            .await
            .map_err(|e| format!("PG create_unified_workflow: {}", e))?;

        self.get_unified_workflow(id)
            .await?
            .ok_or_else(|| "Failed to retrieve created workflow".to_string())
    }

    /// Update a unified workflow (read-modify-write pattern).
    pub async fn update_unified_workflow(
        &self,
        id: &str,
        request: &crate::unified_workflows::UpdateUnifiedWorkflowRequest,
    ) -> Result<UnifiedWorkflow, String> {
        let existing = self
            .get_unified_workflow(id)
            .await?
            .ok_or_else(|| format!("Unified workflow not found: {}", id))?;

        // Merge updates with existing values
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
        let timeout_seconds: Option<u64> = match &request.timeout_seconds {
            Some(val) => *val,
            None => existing.timeout_seconds,
        };
        let provider = request.provider.as_ref().or(existing.provider.as_ref());
        let model = request.model.as_ref().or(existing.model.as_ref());
        let skip_ai_summary = request.skip_ai_summary.unwrap_or(existing.skip_ai_summary);
        let log_source_selection = request
            .log_source_selection
            .as_ref()
            .unwrap_or(&existing.log_source_selection);
        let prompt_template: Option<&str> = match &request.prompt_template {
            Some(val) if val.is_empty() => None,
            Some(val) => Some(val.as_str()),
            None => existing.prompt_template.as_deref(),
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
        let ai_reviewed = request.ai_reviewed.unwrap_or(existing.ai_reviewed);
        let strict_cwd = request.strict_cwd.unwrap_or(existing.strict_cwd);
        let tool_tags = request.tool_tags.as_ref().unwrap_or(&existing.tool_tags);
        let rollback_policy = request
            .rollback_policy
            .as_deref()
            .or(existing.rollback_policy.as_deref())
            .unwrap_or("none");
        let enforce_token_budget = request
            .enforce_token_budget
            .unwrap_or(existing.enforce_token_budget);
        let flow_control_json = request
            .flow_control_json
            .as_ref()
            .or(existing.flow_control_json.as_ref());
        let phase_timeouts_json = request
            .phase_timeouts_json
            .as_ref()
            .or(existing.phase_timeouts_json.as_ref());
        let workflow_architecture = request
            .workflow_architecture
            .as_ref()
            .or(existing.workflow_architecture.as_ref())
            .map(|a| {
                serde_json::to_value(a)
                    .ok()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| format!("{:?}", a).to_lowercase())
            });

        // Serialize JSON fields
        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
        let setup_json = serde_json::to_string(setup_steps).unwrap_or_else(|_| "[]".to_string());
        let verification_json =
            serde_json::to_string(verification_steps).unwrap_or_else(|_| "[]".to_string());
        let agentic_json =
            serde_json::to_string(agentic_steps).unwrap_or_else(|_| "[]".to_string());
        let completion_json =
            serde_json::to_string(completion_steps).unwrap_or_else(|_| "[]".to_string());
        let log_sel_json = serde_json::to_string(log_source_selection)
            .unwrap_or_else(|_| "\"default\"".to_string());
        let ctx_json = serde_json::to_string(context_ids).unwrap_or_else(|_| "[]".to_string());
        let dis_ctx_json =
            serde_json::to_string(disabled_context_ids).unwrap_or_else(|_| "[]".to_string());
        let hc_urls_json =
            serde_json::to_string(health_check_urls).unwrap_or_else(|_| "[]".to_string());
        let stages_json = serde_json::to_string(stages).unwrap_or_else(|_| "[]".to_string());
        let model_ov_json =
            serde_json::to_string(model_overrides).unwrap_or_else(|_| "{}".to_string());
        let constraint_ov_json =
            serde_json::to_string(constraint_overrides).unwrap_or_else(|_| "{}".to_string());
        let tool_tags_json = serde_json::to_string(tool_tags).unwrap_or_else(|_| "[]".to_string());
        let timeout_secs: Option<i64> = timeout_seconds.map(|v| v as i64);
        let max_iters = max_iterations as i64;
        let max_sweep = max_sweep_iterations as i64;
        let dep_graph_str = dependency_graph.map(|v| v.to_string());
        let cost_ann_str = cost_annotations.map(|v| v.to_string());
        let quality_str = quality_report.map(|v| v.to_string());
        let accept_str = acceptance_criteria.map(|v| v.to_string());

        let desc_opt: Option<&str> = Some(description.as_str());
        let rollback_opt: Option<&str> = Some(rollback_policy);
        let provider_opt: Option<&str> = provider.map(|s| s.as_str());
        let model_opt: Option<&str> = model.map(|s| s.as_str());

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        qontinui_db::queries::workflows::update_unified_workflow()
            .bind(
                &conn,
                &name.as_str(),
                &desc_opt,
                &category.as_str(),
                &tags_json.as_str(),
                &setup_json.as_str(),
                &verification_json.as_str(),
                &agentic_json.as_str(),
                &completion_json.as_str(),
                &max_iters,
                &timeout_secs,
                &provider_opt,
                &model_opt,
                &skip_ai_summary,
                &log_sel_json.as_str(),
                &prompt_template,
                &ctx_json.as_str(),
                &dis_ctx_json.as_str(),
                &auto_include_contexts,
                &log_watch_enabled,
                &health_check_enabled,
                &hc_urls_json.as_str(),
                &preflight_check_enabled,
                &enable_sweep,
                &max_sweep,
                &stages_json.as_str(),
                &stop_on_failure,
                &approval_gate,
                &reflection_mode,
                &model_ov_json.as_str(),
                &completion_prompts_first,
                &dep_graph_str.as_deref(),
                &cost_ann_str.as_deref(),
                &quality_str.as_deref(),
                &accept_str.as_deref(),
                &constraint_ov_json.as_str(),
                &ai_reviewed,
                &workflow_architecture.as_deref(),
                &strict_cwd,
                &tool_tags_json.as_str(),
                &rollback_opt,
                &enforce_token_budget,
                &flow_control_json.map(|s| s.as_str()),
                &phase_timeouts_json.map(|s| s.as_str()),
                &id,
            )
            .opt()
            .await
            .map_err(|e| format!("PG update_unified_workflow: {}", e))?;

        self.get_unified_workflow(id)
            .await?
            .ok_or_else(|| "Failed to retrieve updated workflow".to_string())
    }

    /// Delete a unified workflow.
    pub async fn delete_unified_workflow(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let deleted = qontinui_db::queries::workflows::delete_unified_workflow()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG delete_unified_workflow: {}", e))?;
        Ok(deleted.is_some())
    }

    /// Full-text search over workflows.
    pub async fn search_unified_workflows(
        &self,
        query: &str,
    ) -> Result<Vec<UnifiedWorkflow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::workflows::search_unified_workflows()
            .bind(&conn, &query)
            .all()
            .await
            .map_err(|e| format!("PG search_unified_workflows: {}", e))?;

        Ok(rows.into_iter().map(|r| row_to_workflow!(r)).collect())
    }

    /// Search for successful workflows to use as RAG examples for generation.
    /// Returns up to `limit` workflows with `example_status = 'active'` or
    /// recently successful ones, ordered by relevance to the description.
    pub async fn search_unified_workflows_for_examples(
        &self,
        _description: &str,
        limit: usize,
    ) -> Result<Vec<UnifiedWorkflow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let limit_i64 = limit as i64;

        let rows = conn
            .query(
                r#"SELECT id, name, description, category, tags,
                          setup_steps, verification_steps, agentic_steps, completion_steps,
                          stages, max_iterations, provider, model, model_overrides,
                          log_source_selection, prompt_template,
                          workflow_architecture, workflow_config,
                          auto_include_contexts,
                          acceptance_criteria, quality_report, dependency_graph, cost_annotations,
                          ai_reviewed, is_favorite, created_at, updated_at, deleted_at,
                          metadata, sync_status, parent_workflow_id
                   FROM unified_workflows
                   WHERE deleted_at IS NULL
                     AND (example_status = 'active' OR is_favorite = true)
                   ORDER BY updated_at DESC
                   LIMIT $1"#,
                &[&limit_i64],
            )
            .await
            .map_err(|e| format!("PG search_unified_workflows_for_examples: {}", e))?;

        // Use the same row mapping as other workflow queries
        Ok(rows.into_iter().filter_map(|r| {
            // Minimal mapping via row_to_workflow macro
            match serde_json::json!({
                "id": r.get::<_, String>(0),
                "name": r.get::<_, String>(1),
                "description": r.get::<_, Option<String>>(2).unwrap_or_default(),
                "category": r.get::<_, Option<String>>(3).unwrap_or_else(|| "general".to_string()),
            }) {
                val => serde_json::from_value::<crate::unified_workflows::UnifiedWorkflow>(val).ok()
            }
        }).collect())
    }

    /// Toggle favorite status. Returns the new is_favorite value.
    /// Alias: `toggle_unified_workflow_favorite` delegates here.
    pub async fn toggle_favorite(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let new_val = qontinui_db::queries::workflows::toggle_favorite()
            .bind(&conn, &id)
            .one()
            .await
            .map_err(|e| format!("PG toggle_favorite: {}", e))?;
        Ok(new_val)
    }

    /// Get workflows pending sync to backend.
    pub async fn get_pending_sync_workflows(&self) -> Result<Vec<UnifiedWorkflow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::workflows::get_pending_sync_workflows()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG get_pending_sync_workflows: {}", e))?;

        Ok(rows.into_iter().map(|r| row_to_workflow!(r)).collect())
    }

    /// Clear sync_pending flag for a workflow.
    pub async fn clear_sync_pending(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::workflows::clear_sync_pending()
            .bind(&conn, &id)
            .await
            .map_err(|e| format!("PG clear_sync_pending: {}", e))?;
        Ok(())
    }

    /// Set sync_pending flag for a workflow.
    pub async fn set_sync_pending(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::workflows::set_sync_pending()
            .bind(&conn, &id)
            .await
            .map_err(|e| format!("PG set_sync_pending: {}", e))?;
        Ok(())
    }

    /// List all slash command workflow sources.
    pub async fn list_slash_command_sources(
        &self,
    ) -> Result<Vec<(String, String, String)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::workflows::list_slash_command_sources()
            .bind(&conn)
            .all()
            .await
            .map_err(|e| format!("PG list_slash_command_sources: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| (r.id, r.name, r.source_file_path))
            .collect())
    }

    /// Toggle the is_favorite flag on a unified workflow (alias for toggle_favorite).
    pub async fn toggle_unified_workflow_favorite(&self, id: &str) -> Result<bool, String> {
        self.toggle_favorite(id).await
    }

    /// Get workflow execution stats by workflow name.
    pub async fn get_workflow_stats(
        &self,
        workflow_name: &str,
    ) -> Result<(u32, u32, u32, Option<String>, Option<String>, Option<i64>), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = qontinui_db::queries::workflows::get_workflow_stats()
            .bind(&conn, &workflow_name)
            .one()
            .await
            .map_err(|e| format!("PG get_workflow_stats: {}", e))?;

        Ok((
            row.total_runs as u32,
            row.success_count as u32,
            row.failure_count as u32,
            if row.last_run_at.timestamp() == 0 {
                None
            } else {
                Some(row.last_run_at.to_rfc3339())
            },
            non_empty(row.last_run_status),
            if row.avg_duration_ms == 0.0 {
                None
            } else {
                Some(row.avg_duration_ms as i64)
            },
        ))
    }

    /// Update example_status for a workflow.
    pub async fn update_example_status(
        &self,
        workflow_id: &str,
        status: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE unified_workflows SET example_status = $1 WHERE id = $2",
            &[&status, &workflow_id],
        )
        .await
        .map_err(|e| format!("PG update_example_status: {}", e))?;
        tracing::info!(
            "Updated workflow '{}' example_status to '{}'",
            workflow_id,
            status
        );
        Ok(())
    }

    /// Count workflows matching a given category.
    pub async fn count_workflows_by_category(&self, category: &str) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_one(
                "SELECT COUNT(*) FROM unified_workflows WHERE category = $1",
                &[&category],
            )
            .await
            .map_err(|e| format!("PG count_workflows_by_category: {}", e))?;
        Ok(row.get(0))
    }
}
