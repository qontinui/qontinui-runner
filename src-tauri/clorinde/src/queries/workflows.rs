// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct CreateUnifiedWorkflowParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::StringSql,
    T8: crate::StringSql,
    T9: crate::StringSql,
    T10: crate::StringSql,
    T11: crate::StringSql,
    T12: crate::StringSql,
    T13: crate::StringSql,
    T14: crate::StringSql,
    T15: crate::StringSql,
    T16: crate::StringSql,
    T17: crate::StringSql,
    T18: crate::StringSql,
    T19: crate::StringSql,
    T20: crate::StringSql,
    T21: crate::StringSql,
    T22: crate::StringSql,
    T23: crate::StringSql,
    T24: crate::StringSql,
    T25: crate::StringSql,
    T26: crate::StringSql,
    T27: crate::StringSql,
    T28: crate::StringSql,
    T29: crate::StringSql,
> {
    pub id: T1,
    pub name: T2,
    pub description: Option<T3>,
    pub category: T4,
    pub tags: T5,
    pub setup_steps: T6,
    pub verification_steps: T7,
    pub agentic_steps: T8,
    pub completion_steps: T9,
    pub max_iterations: i64,
    pub timeout_seconds: Option<i64>,
    pub provider: Option<T10>,
    pub model: Option<T11>,
    pub skip_ai_summary: bool,
    pub log_source_selection: T12,
    pub context_ids: T13,
    pub disabled_context_ids: T14,
    pub auto_include_contexts: bool,
    pub prompt_template: Option<T15>,
    pub log_watch_enabled: bool,
    pub health_check_enabled: bool,
    pub health_check_urls: T16,
    pub preflight_check_enabled: bool,
    pub generated_by_task_run_id: Option<T17>,
    pub enable_sweep: bool,
    pub max_sweep_iterations: i64,
    pub stages: T18,
    pub stop_on_failure: bool,
    pub reflection_mode: bool,
    pub model_overrides: T19,
    pub approval_gate: bool,
    pub completion_prompts_first: bool,
    pub dependency_graph: Option<T20>,
    pub cost_annotations: Option<T21>,
    pub quality_report: Option<T22>,
    pub acceptance_criteria: Option<T23>,
    pub constraint_overrides: T24,
    pub ai_reviewed: bool,
    pub workflow_architecture: Option<T25>,
    pub strict_cwd: bool,
    pub tool_tags: T26,
    pub rollback_policy: Option<T27>,
    pub enforce_token_budget: bool,
    pub flow_control_json: Option<T28>,
    pub phase_timeouts_json: Option<T29>,
}
#[derive(Debug)]
pub struct UpdateUnifiedWorkflowParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::StringSql,
    T8: crate::StringSql,
    T9: crate::StringSql,
    T10: crate::StringSql,
    T11: crate::StringSql,
    T12: crate::StringSql,
    T13: crate::StringSql,
    T14: crate::StringSql,
    T15: crate::StringSql,
    T16: crate::StringSql,
    T17: crate::StringSql,
    T18: crate::StringSql,
    T19: crate::StringSql,
    T20: crate::StringSql,
    T21: crate::StringSql,
    T22: crate::StringSql,
    T23: crate::StringSql,
    T24: crate::StringSql,
    T25: crate::StringSql,
    T26: crate::StringSql,
    T27: crate::StringSql,
    T28: crate::StringSql,
> {
    pub name: T1,
    pub description: Option<T2>,
    pub category: T3,
    pub tags: T4,
    pub setup_steps: T5,
    pub verification_steps: T6,
    pub agentic_steps: T7,
    pub completion_steps: T8,
    pub max_iterations: i64,
    pub timeout_seconds: Option<i64>,
    pub provider: Option<T9>,
    pub model: Option<T10>,
    pub skip_ai_summary: bool,
    pub log_source_selection: T11,
    pub prompt_template: Option<T12>,
    pub context_ids: T13,
    pub disabled_context_ids: T14,
    pub auto_include_contexts: bool,
    pub log_watch_enabled: bool,
    pub health_check_enabled: bool,
    pub health_check_urls: T15,
    pub preflight_check_enabled: bool,
    pub enable_sweep: bool,
    pub max_sweep_iterations: i64,
    pub stages: T16,
    pub stop_on_failure: bool,
    pub approval_gate: bool,
    pub reflection_mode: bool,
    pub model_overrides: T17,
    pub completion_prompts_first: bool,
    pub dependency_graph: Option<T18>,
    pub cost_annotations: Option<T19>,
    pub quality_report: Option<T20>,
    pub acceptance_criteria: Option<T21>,
    pub constraint_overrides: T22,
    pub ai_reviewed: bool,
    pub workflow_architecture: Option<T23>,
    pub strict_cwd: bool,
    pub tool_tags: T24,
    pub rollback_policy: Option<T25>,
    pub enforce_token_budget: bool,
    pub flow_control_json: Option<T26>,
    pub phase_timeouts_json: Option<T27>,
    pub id: T28,
}
#[derive(Debug)]
pub struct UpsertSlashCommandWorkflowParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
> {
    pub id: T1,
    pub name: T2,
    pub description: Option<T3>,
    pub agentic_steps: T4,
    pub source_file_path: T5,
    pub source_content_hash: Option<T6>,
}
#[derive(Debug)]
pub struct UpdateSlashCommandContentParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
> {
    pub agentic_steps: T1,
    pub source_content_hash: Option<T2>,
    pub id: T3,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListUnifiedWorkflows {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub tags: String,
    pub setup_steps: String,
    pub verification_steps: String,
    pub agentic_steps: String,
    pub completion_steps: String,
    pub max_iterations: i64,
    pub provider: String,
    pub model: String,
    pub skip_ai_summary: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub log_source_selection: String,
    pub context_ids: String,
    pub disabled_context_ids: String,
    pub auto_include_contexts: bool,
    pub prompt_template: String,
    pub log_watch_enabled: bool,
    pub health_check_enabled: bool,
    pub health_check_urls: String,
    pub timeout_seconds: i64,
    pub preflight_check_enabled: bool,
    pub generated_by_task_run_id: String,
    pub enable_sweep: bool,
    pub max_sweep_iterations: i64,
    pub stages: String,
    pub stop_on_failure: bool,
    pub reflection_mode: bool,
    pub model_overrides: String,
    pub approval_gate: bool,
    pub completion_prompts_first: bool,
    pub is_favorite: bool,
    pub dependency_graph: String,
    pub cost_annotations: String,
    pub quality_report: String,
    pub acceptance_criteria: String,
    pub constraint_overrides: String,
    pub ai_reviewed: bool,
    pub workflow_architecture: String,
    pub rollback_policy: String,
    pub strict_cwd: bool,
    pub tool_tags: String,
    pub enforce_token_budget: bool,
    pub flow_control_json: String,
    pub phase_timeouts_json: String,
}
pub struct ListUnifiedWorkflowsBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub category: &'a str,
    pub tags: &'a str,
    pub setup_steps: &'a str,
    pub verification_steps: &'a str,
    pub agentic_steps: &'a str,
    pub completion_steps: &'a str,
    pub max_iterations: i64,
    pub provider: &'a str,
    pub model: &'a str,
    pub skip_ai_summary: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub log_source_selection: &'a str,
    pub context_ids: &'a str,
    pub disabled_context_ids: &'a str,
    pub auto_include_contexts: bool,
    pub prompt_template: &'a str,
    pub log_watch_enabled: bool,
    pub health_check_enabled: bool,
    pub health_check_urls: &'a str,
    pub timeout_seconds: i64,
    pub preflight_check_enabled: bool,
    pub generated_by_task_run_id: &'a str,
    pub enable_sweep: bool,
    pub max_sweep_iterations: i64,
    pub stages: &'a str,
    pub stop_on_failure: bool,
    pub reflection_mode: bool,
    pub model_overrides: &'a str,
    pub approval_gate: bool,
    pub completion_prompts_first: bool,
    pub is_favorite: bool,
    pub dependency_graph: &'a str,
    pub cost_annotations: &'a str,
    pub quality_report: &'a str,
    pub acceptance_criteria: &'a str,
    pub constraint_overrides: &'a str,
    pub ai_reviewed: bool,
    pub workflow_architecture: &'a str,
    pub rollback_policy: &'a str,
    pub strict_cwd: bool,
    pub tool_tags: &'a str,
    pub enforce_token_budget: bool,
    pub flow_control_json: &'a str,
    pub phase_timeouts_json: &'a str,
}
impl<'a> From<ListUnifiedWorkflowsBorrowed<'a>> for ListUnifiedWorkflows {
    fn from(
        ListUnifiedWorkflowsBorrowed {
            id,
            name,
            description,
            category,
            tags,
            setup_steps,
            verification_steps,
            agentic_steps,
            completion_steps,
            max_iterations,
            provider,
            model,
            skip_ai_summary,
            created_at,
            updated_at,
            log_source_selection,
            context_ids,
            disabled_context_ids,
            auto_include_contexts,
            prompt_template,
            log_watch_enabled,
            health_check_enabled,
            health_check_urls,
            timeout_seconds,
            preflight_check_enabled,
            generated_by_task_run_id,
            enable_sweep,
            max_sweep_iterations,
            stages,
            stop_on_failure,
            reflection_mode,
            model_overrides,
            approval_gate,
            completion_prompts_first,
            is_favorite,
            dependency_graph,
            cost_annotations,
            quality_report,
            acceptance_criteria,
            constraint_overrides,
            ai_reviewed,
            workflow_architecture,
            rollback_policy,
            strict_cwd,
            tool_tags,
            enforce_token_budget,
            flow_control_json,
            phase_timeouts_json,
        }: ListUnifiedWorkflowsBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            category: category.into(),
            tags: tags.into(),
            setup_steps: setup_steps.into(),
            verification_steps: verification_steps.into(),
            agentic_steps: agentic_steps.into(),
            completion_steps: completion_steps.into(),
            max_iterations,
            provider: provider.into(),
            model: model.into(),
            skip_ai_summary,
            created_at,
            updated_at,
            log_source_selection: log_source_selection.into(),
            context_ids: context_ids.into(),
            disabled_context_ids: disabled_context_ids.into(),
            auto_include_contexts,
            prompt_template: prompt_template.into(),
            log_watch_enabled,
            health_check_enabled,
            health_check_urls: health_check_urls.into(),
            timeout_seconds,
            preflight_check_enabled,
            generated_by_task_run_id: generated_by_task_run_id.into(),
            enable_sweep,
            max_sweep_iterations,
            stages: stages.into(),
            stop_on_failure,
            reflection_mode,
            model_overrides: model_overrides.into(),
            approval_gate,
            completion_prompts_first,
            is_favorite,
            dependency_graph: dependency_graph.into(),
            cost_annotations: cost_annotations.into(),
            quality_report: quality_report.into(),
            acceptance_criteria: acceptance_criteria.into(),
            constraint_overrides: constraint_overrides.into(),
            ai_reviewed,
            workflow_architecture: workflow_architecture.into(),
            rollback_policy: rollback_policy.into(),
            strict_cwd,
            tool_tags: tool_tags.into(),
            enforce_token_budget,
            flow_control_json: flow_control_json.into(),
            phase_timeouts_json: phase_timeouts_json.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetUnifiedWorkflow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub tags: String,
    pub setup_steps: String,
    pub verification_steps: String,
    pub agentic_steps: String,
    pub completion_steps: String,
    pub max_iterations: i64,
    pub provider: String,
    pub model: String,
    pub skip_ai_summary: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub log_source_selection: String,
    pub context_ids: String,
    pub disabled_context_ids: String,
    pub auto_include_contexts: bool,
    pub prompt_template: String,
    pub log_watch_enabled: bool,
    pub health_check_enabled: bool,
    pub health_check_urls: String,
    pub timeout_seconds: i64,
    pub preflight_check_enabled: bool,
    pub generated_by_task_run_id: String,
    pub enable_sweep: bool,
    pub max_sweep_iterations: i64,
    pub stages: String,
    pub stop_on_failure: bool,
    pub reflection_mode: bool,
    pub model_overrides: String,
    pub approval_gate: bool,
    pub completion_prompts_first: bool,
    pub is_favorite: bool,
    pub dependency_graph: String,
    pub cost_annotations: String,
    pub quality_report: String,
    pub acceptance_criteria: String,
    pub constraint_overrides: String,
    pub ai_reviewed: bool,
    pub workflow_architecture: String,
    pub rollback_policy: String,
    pub strict_cwd: bool,
    pub tool_tags: String,
    pub enforce_token_budget: bool,
    pub flow_control_json: String,
    pub phase_timeouts_json: String,
}
pub struct GetUnifiedWorkflowBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub category: &'a str,
    pub tags: &'a str,
    pub setup_steps: &'a str,
    pub verification_steps: &'a str,
    pub agentic_steps: &'a str,
    pub completion_steps: &'a str,
    pub max_iterations: i64,
    pub provider: &'a str,
    pub model: &'a str,
    pub skip_ai_summary: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub log_source_selection: &'a str,
    pub context_ids: &'a str,
    pub disabled_context_ids: &'a str,
    pub auto_include_contexts: bool,
    pub prompt_template: &'a str,
    pub log_watch_enabled: bool,
    pub health_check_enabled: bool,
    pub health_check_urls: &'a str,
    pub timeout_seconds: i64,
    pub preflight_check_enabled: bool,
    pub generated_by_task_run_id: &'a str,
    pub enable_sweep: bool,
    pub max_sweep_iterations: i64,
    pub stages: &'a str,
    pub stop_on_failure: bool,
    pub reflection_mode: bool,
    pub model_overrides: &'a str,
    pub approval_gate: bool,
    pub completion_prompts_first: bool,
    pub is_favorite: bool,
    pub dependency_graph: &'a str,
    pub cost_annotations: &'a str,
    pub quality_report: &'a str,
    pub acceptance_criteria: &'a str,
    pub constraint_overrides: &'a str,
    pub ai_reviewed: bool,
    pub workflow_architecture: &'a str,
    pub rollback_policy: &'a str,
    pub strict_cwd: bool,
    pub tool_tags: &'a str,
    pub enforce_token_budget: bool,
    pub flow_control_json: &'a str,
    pub phase_timeouts_json: &'a str,
}
impl<'a> From<GetUnifiedWorkflowBorrowed<'a>> for GetUnifiedWorkflow {
    fn from(
        GetUnifiedWorkflowBorrowed {
            id,
            name,
            description,
            category,
            tags,
            setup_steps,
            verification_steps,
            agentic_steps,
            completion_steps,
            max_iterations,
            provider,
            model,
            skip_ai_summary,
            created_at,
            updated_at,
            log_source_selection,
            context_ids,
            disabled_context_ids,
            auto_include_contexts,
            prompt_template,
            log_watch_enabled,
            health_check_enabled,
            health_check_urls,
            timeout_seconds,
            preflight_check_enabled,
            generated_by_task_run_id,
            enable_sweep,
            max_sweep_iterations,
            stages,
            stop_on_failure,
            reflection_mode,
            model_overrides,
            approval_gate,
            completion_prompts_first,
            is_favorite,
            dependency_graph,
            cost_annotations,
            quality_report,
            acceptance_criteria,
            constraint_overrides,
            ai_reviewed,
            workflow_architecture,
            rollback_policy,
            strict_cwd,
            tool_tags,
            enforce_token_budget,
            flow_control_json,
            phase_timeouts_json,
        }: GetUnifiedWorkflowBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            category: category.into(),
            tags: tags.into(),
            setup_steps: setup_steps.into(),
            verification_steps: verification_steps.into(),
            agentic_steps: agentic_steps.into(),
            completion_steps: completion_steps.into(),
            max_iterations,
            provider: provider.into(),
            model: model.into(),
            skip_ai_summary,
            created_at,
            updated_at,
            log_source_selection: log_source_selection.into(),
            context_ids: context_ids.into(),
            disabled_context_ids: disabled_context_ids.into(),
            auto_include_contexts,
            prompt_template: prompt_template.into(),
            log_watch_enabled,
            health_check_enabled,
            health_check_urls: health_check_urls.into(),
            timeout_seconds,
            preflight_check_enabled,
            generated_by_task_run_id: generated_by_task_run_id.into(),
            enable_sweep,
            max_sweep_iterations,
            stages: stages.into(),
            stop_on_failure,
            reflection_mode,
            model_overrides: model_overrides.into(),
            approval_gate,
            completion_prompts_first,
            is_favorite,
            dependency_graph: dependency_graph.into(),
            cost_annotations: cost_annotations.into(),
            quality_report: quality_report.into(),
            acceptance_criteria: acceptance_criteria.into(),
            constraint_overrides: constraint_overrides.into(),
            ai_reviewed,
            workflow_architecture: workflow_architecture.into(),
            rollback_policy: rollback_policy.into(),
            strict_cwd,
            tool_tags: tool_tags.into(),
            enforce_token_budget,
            flow_control_json: flow_control_json.into(),
            phase_timeouts_json: phase_timeouts_json.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetUnifiedWorkflowByName {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub tags: String,
    pub setup_steps: String,
    pub verification_steps: String,
    pub agentic_steps: String,
    pub completion_steps: String,
    pub max_iterations: i64,
    pub provider: String,
    pub model: String,
    pub skip_ai_summary: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub log_source_selection: String,
    pub context_ids: String,
    pub disabled_context_ids: String,
    pub auto_include_contexts: bool,
    pub prompt_template: String,
    pub log_watch_enabled: bool,
    pub health_check_enabled: bool,
    pub health_check_urls: String,
    pub timeout_seconds: i64,
    pub preflight_check_enabled: bool,
    pub generated_by_task_run_id: String,
    pub enable_sweep: bool,
    pub max_sweep_iterations: i64,
    pub stages: String,
    pub stop_on_failure: bool,
    pub reflection_mode: bool,
    pub model_overrides: String,
    pub approval_gate: bool,
    pub completion_prompts_first: bool,
    pub is_favorite: bool,
    pub dependency_graph: String,
    pub cost_annotations: String,
    pub quality_report: String,
    pub acceptance_criteria: String,
    pub constraint_overrides: String,
    pub ai_reviewed: bool,
    pub workflow_architecture: String,
    pub rollback_policy: String,
    pub strict_cwd: bool,
    pub tool_tags: String,
    pub enforce_token_budget: bool,
    pub flow_control_json: String,
    pub phase_timeouts_json: String,
}
pub struct GetUnifiedWorkflowByNameBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub category: &'a str,
    pub tags: &'a str,
    pub setup_steps: &'a str,
    pub verification_steps: &'a str,
    pub agentic_steps: &'a str,
    pub completion_steps: &'a str,
    pub max_iterations: i64,
    pub provider: &'a str,
    pub model: &'a str,
    pub skip_ai_summary: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub log_source_selection: &'a str,
    pub context_ids: &'a str,
    pub disabled_context_ids: &'a str,
    pub auto_include_contexts: bool,
    pub prompt_template: &'a str,
    pub log_watch_enabled: bool,
    pub health_check_enabled: bool,
    pub health_check_urls: &'a str,
    pub timeout_seconds: i64,
    pub preflight_check_enabled: bool,
    pub generated_by_task_run_id: &'a str,
    pub enable_sweep: bool,
    pub max_sweep_iterations: i64,
    pub stages: &'a str,
    pub stop_on_failure: bool,
    pub reflection_mode: bool,
    pub model_overrides: &'a str,
    pub approval_gate: bool,
    pub completion_prompts_first: bool,
    pub is_favorite: bool,
    pub dependency_graph: &'a str,
    pub cost_annotations: &'a str,
    pub quality_report: &'a str,
    pub acceptance_criteria: &'a str,
    pub constraint_overrides: &'a str,
    pub ai_reviewed: bool,
    pub workflow_architecture: &'a str,
    pub rollback_policy: &'a str,
    pub strict_cwd: bool,
    pub tool_tags: &'a str,
    pub enforce_token_budget: bool,
    pub flow_control_json: &'a str,
    pub phase_timeouts_json: &'a str,
}
impl<'a> From<GetUnifiedWorkflowByNameBorrowed<'a>> for GetUnifiedWorkflowByName {
    fn from(
        GetUnifiedWorkflowByNameBorrowed {
            id,
            name,
            description,
            category,
            tags,
            setup_steps,
            verification_steps,
            agentic_steps,
            completion_steps,
            max_iterations,
            provider,
            model,
            skip_ai_summary,
            created_at,
            updated_at,
            log_source_selection,
            context_ids,
            disabled_context_ids,
            auto_include_contexts,
            prompt_template,
            log_watch_enabled,
            health_check_enabled,
            health_check_urls,
            timeout_seconds,
            preflight_check_enabled,
            generated_by_task_run_id,
            enable_sweep,
            max_sweep_iterations,
            stages,
            stop_on_failure,
            reflection_mode,
            model_overrides,
            approval_gate,
            completion_prompts_first,
            is_favorite,
            dependency_graph,
            cost_annotations,
            quality_report,
            acceptance_criteria,
            constraint_overrides,
            ai_reviewed,
            workflow_architecture,
            rollback_policy,
            strict_cwd,
            tool_tags,
            enforce_token_budget,
            flow_control_json,
            phase_timeouts_json,
        }: GetUnifiedWorkflowByNameBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            category: category.into(),
            tags: tags.into(),
            setup_steps: setup_steps.into(),
            verification_steps: verification_steps.into(),
            agentic_steps: agentic_steps.into(),
            completion_steps: completion_steps.into(),
            max_iterations,
            provider: provider.into(),
            model: model.into(),
            skip_ai_summary,
            created_at,
            updated_at,
            log_source_selection: log_source_selection.into(),
            context_ids: context_ids.into(),
            disabled_context_ids: disabled_context_ids.into(),
            auto_include_contexts,
            prompt_template: prompt_template.into(),
            log_watch_enabled,
            health_check_enabled,
            health_check_urls: health_check_urls.into(),
            timeout_seconds,
            preflight_check_enabled,
            generated_by_task_run_id: generated_by_task_run_id.into(),
            enable_sweep,
            max_sweep_iterations,
            stages: stages.into(),
            stop_on_failure,
            reflection_mode,
            model_overrides: model_overrides.into(),
            approval_gate,
            completion_prompts_first,
            is_favorite,
            dependency_graph: dependency_graph.into(),
            cost_annotations: cost_annotations.into(),
            quality_report: quality_report.into(),
            acceptance_criteria: acceptance_criteria.into(),
            constraint_overrides: constraint_overrides.into(),
            ai_reviewed,
            workflow_architecture: workflow_architecture.into(),
            rollback_policy: rollback_policy.into(),
            strict_cwd,
            tool_tags: tool_tags.into(),
            enforce_token_budget,
            flow_control_json: flow_control_json.into(),
            phase_timeouts_json: phase_timeouts_json.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SearchUnifiedWorkflows {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub tags: String,
    pub setup_steps: String,
    pub verification_steps: String,
    pub agentic_steps: String,
    pub completion_steps: String,
    pub max_iterations: i64,
    pub provider: String,
    pub model: String,
    pub skip_ai_summary: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub log_source_selection: String,
    pub context_ids: String,
    pub disabled_context_ids: String,
    pub auto_include_contexts: bool,
    pub prompt_template: String,
    pub log_watch_enabled: bool,
    pub health_check_enabled: bool,
    pub health_check_urls: String,
    pub timeout_seconds: i64,
    pub preflight_check_enabled: bool,
    pub generated_by_task_run_id: String,
    pub enable_sweep: bool,
    pub max_sweep_iterations: i64,
    pub stages: String,
    pub stop_on_failure: bool,
    pub reflection_mode: bool,
    pub model_overrides: String,
    pub approval_gate: bool,
    pub completion_prompts_first: bool,
    pub is_favorite: bool,
    pub dependency_graph: String,
    pub cost_annotations: String,
    pub quality_report: String,
    pub acceptance_criteria: String,
    pub constraint_overrides: String,
    pub ai_reviewed: bool,
    pub workflow_architecture: String,
    pub rollback_policy: String,
    pub strict_cwd: bool,
    pub tool_tags: String,
    pub enforce_token_budget: bool,
    pub flow_control_json: String,
    pub phase_timeouts_json: String,
}
pub struct SearchUnifiedWorkflowsBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub category: &'a str,
    pub tags: &'a str,
    pub setup_steps: &'a str,
    pub verification_steps: &'a str,
    pub agentic_steps: &'a str,
    pub completion_steps: &'a str,
    pub max_iterations: i64,
    pub provider: &'a str,
    pub model: &'a str,
    pub skip_ai_summary: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub log_source_selection: &'a str,
    pub context_ids: &'a str,
    pub disabled_context_ids: &'a str,
    pub auto_include_contexts: bool,
    pub prompt_template: &'a str,
    pub log_watch_enabled: bool,
    pub health_check_enabled: bool,
    pub health_check_urls: &'a str,
    pub timeout_seconds: i64,
    pub preflight_check_enabled: bool,
    pub generated_by_task_run_id: &'a str,
    pub enable_sweep: bool,
    pub max_sweep_iterations: i64,
    pub stages: &'a str,
    pub stop_on_failure: bool,
    pub reflection_mode: bool,
    pub model_overrides: &'a str,
    pub approval_gate: bool,
    pub completion_prompts_first: bool,
    pub is_favorite: bool,
    pub dependency_graph: &'a str,
    pub cost_annotations: &'a str,
    pub quality_report: &'a str,
    pub acceptance_criteria: &'a str,
    pub constraint_overrides: &'a str,
    pub ai_reviewed: bool,
    pub workflow_architecture: &'a str,
    pub rollback_policy: &'a str,
    pub strict_cwd: bool,
    pub tool_tags: &'a str,
    pub enforce_token_budget: bool,
    pub flow_control_json: &'a str,
    pub phase_timeouts_json: &'a str,
}
impl<'a> From<SearchUnifiedWorkflowsBorrowed<'a>> for SearchUnifiedWorkflows {
    fn from(
        SearchUnifiedWorkflowsBorrowed {
            id,
            name,
            description,
            category,
            tags,
            setup_steps,
            verification_steps,
            agentic_steps,
            completion_steps,
            max_iterations,
            provider,
            model,
            skip_ai_summary,
            created_at,
            updated_at,
            log_source_selection,
            context_ids,
            disabled_context_ids,
            auto_include_contexts,
            prompt_template,
            log_watch_enabled,
            health_check_enabled,
            health_check_urls,
            timeout_seconds,
            preflight_check_enabled,
            generated_by_task_run_id,
            enable_sweep,
            max_sweep_iterations,
            stages,
            stop_on_failure,
            reflection_mode,
            model_overrides,
            approval_gate,
            completion_prompts_first,
            is_favorite,
            dependency_graph,
            cost_annotations,
            quality_report,
            acceptance_criteria,
            constraint_overrides,
            ai_reviewed,
            workflow_architecture,
            rollback_policy,
            strict_cwd,
            tool_tags,
            enforce_token_budget,
            flow_control_json,
            phase_timeouts_json,
        }: SearchUnifiedWorkflowsBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            category: category.into(),
            tags: tags.into(),
            setup_steps: setup_steps.into(),
            verification_steps: verification_steps.into(),
            agentic_steps: agentic_steps.into(),
            completion_steps: completion_steps.into(),
            max_iterations,
            provider: provider.into(),
            model: model.into(),
            skip_ai_summary,
            created_at,
            updated_at,
            log_source_selection: log_source_selection.into(),
            context_ids: context_ids.into(),
            disabled_context_ids: disabled_context_ids.into(),
            auto_include_contexts,
            prompt_template: prompt_template.into(),
            log_watch_enabled,
            health_check_enabled,
            health_check_urls: health_check_urls.into(),
            timeout_seconds,
            preflight_check_enabled,
            generated_by_task_run_id: generated_by_task_run_id.into(),
            enable_sweep,
            max_sweep_iterations,
            stages: stages.into(),
            stop_on_failure,
            reflection_mode,
            model_overrides: model_overrides.into(),
            approval_gate,
            completion_prompts_first,
            is_favorite,
            dependency_graph: dependency_graph.into(),
            cost_annotations: cost_annotations.into(),
            quality_report: quality_report.into(),
            acceptance_criteria: acceptance_criteria.into(),
            constraint_overrides: constraint_overrides.into(),
            ai_reviewed,
            workflow_architecture: workflow_architecture.into(),
            rollback_policy: rollback_policy.into(),
            strict_cwd,
            tool_tags: tool_tags.into(),
            enforce_token_budget,
            flow_control_json: flow_control_json.into(),
            phase_timeouts_json: phase_timeouts_json.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetPendingSyncWorkflows {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub tags: String,
    pub setup_steps: String,
    pub verification_steps: String,
    pub agentic_steps: String,
    pub completion_steps: String,
    pub max_iterations: i64,
    pub provider: String,
    pub model: String,
    pub skip_ai_summary: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub log_source_selection: String,
    pub context_ids: String,
    pub disabled_context_ids: String,
    pub auto_include_contexts: bool,
    pub prompt_template: String,
    pub log_watch_enabled: bool,
    pub health_check_enabled: bool,
    pub health_check_urls: String,
    pub timeout_seconds: i64,
    pub preflight_check_enabled: bool,
    pub generated_by_task_run_id: String,
    pub enable_sweep: bool,
    pub max_sweep_iterations: i64,
    pub stages: String,
    pub stop_on_failure: bool,
    pub reflection_mode: bool,
    pub model_overrides: String,
    pub approval_gate: bool,
    pub completion_prompts_first: bool,
    pub is_favorite: bool,
    pub dependency_graph: String,
    pub cost_annotations: String,
    pub quality_report: String,
    pub acceptance_criteria: String,
    pub constraint_overrides: String,
    pub ai_reviewed: bool,
    pub workflow_architecture: String,
    pub rollback_policy: String,
    pub strict_cwd: bool,
    pub tool_tags: String,
    pub enforce_token_budget: bool,
    pub flow_control_json: String,
    pub phase_timeouts_json: String,
}
pub struct GetPendingSyncWorkflowsBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub category: &'a str,
    pub tags: &'a str,
    pub setup_steps: &'a str,
    pub verification_steps: &'a str,
    pub agentic_steps: &'a str,
    pub completion_steps: &'a str,
    pub max_iterations: i64,
    pub provider: &'a str,
    pub model: &'a str,
    pub skip_ai_summary: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub log_source_selection: &'a str,
    pub context_ids: &'a str,
    pub disabled_context_ids: &'a str,
    pub auto_include_contexts: bool,
    pub prompt_template: &'a str,
    pub log_watch_enabled: bool,
    pub health_check_enabled: bool,
    pub health_check_urls: &'a str,
    pub timeout_seconds: i64,
    pub preflight_check_enabled: bool,
    pub generated_by_task_run_id: &'a str,
    pub enable_sweep: bool,
    pub max_sweep_iterations: i64,
    pub stages: &'a str,
    pub stop_on_failure: bool,
    pub reflection_mode: bool,
    pub model_overrides: &'a str,
    pub approval_gate: bool,
    pub completion_prompts_first: bool,
    pub is_favorite: bool,
    pub dependency_graph: &'a str,
    pub cost_annotations: &'a str,
    pub quality_report: &'a str,
    pub acceptance_criteria: &'a str,
    pub constraint_overrides: &'a str,
    pub ai_reviewed: bool,
    pub workflow_architecture: &'a str,
    pub rollback_policy: &'a str,
    pub strict_cwd: bool,
    pub tool_tags: &'a str,
    pub enforce_token_budget: bool,
    pub flow_control_json: &'a str,
    pub phase_timeouts_json: &'a str,
}
impl<'a> From<GetPendingSyncWorkflowsBorrowed<'a>> for GetPendingSyncWorkflows {
    fn from(
        GetPendingSyncWorkflowsBorrowed {
            id,
            name,
            description,
            category,
            tags,
            setup_steps,
            verification_steps,
            agentic_steps,
            completion_steps,
            max_iterations,
            provider,
            model,
            skip_ai_summary,
            created_at,
            updated_at,
            log_source_selection,
            context_ids,
            disabled_context_ids,
            auto_include_contexts,
            prompt_template,
            log_watch_enabled,
            health_check_enabled,
            health_check_urls,
            timeout_seconds,
            preflight_check_enabled,
            generated_by_task_run_id,
            enable_sweep,
            max_sweep_iterations,
            stages,
            stop_on_failure,
            reflection_mode,
            model_overrides,
            approval_gate,
            completion_prompts_first,
            is_favorite,
            dependency_graph,
            cost_annotations,
            quality_report,
            acceptance_criteria,
            constraint_overrides,
            ai_reviewed,
            workflow_architecture,
            rollback_policy,
            strict_cwd,
            tool_tags,
            enforce_token_budget,
            flow_control_json,
            phase_timeouts_json,
        }: GetPendingSyncWorkflowsBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            category: category.into(),
            tags: tags.into(),
            setup_steps: setup_steps.into(),
            verification_steps: verification_steps.into(),
            agentic_steps: agentic_steps.into(),
            completion_steps: completion_steps.into(),
            max_iterations,
            provider: provider.into(),
            model: model.into(),
            skip_ai_summary,
            created_at,
            updated_at,
            log_source_selection: log_source_selection.into(),
            context_ids: context_ids.into(),
            disabled_context_ids: disabled_context_ids.into(),
            auto_include_contexts,
            prompt_template: prompt_template.into(),
            log_watch_enabled,
            health_check_enabled,
            health_check_urls: health_check_urls.into(),
            timeout_seconds,
            preflight_check_enabled,
            generated_by_task_run_id: generated_by_task_run_id.into(),
            enable_sweep,
            max_sweep_iterations,
            stages: stages.into(),
            stop_on_failure,
            reflection_mode,
            model_overrides: model_overrides.into(),
            approval_gate,
            completion_prompts_first,
            is_favorite,
            dependency_graph: dependency_graph.into(),
            cost_annotations: cost_annotations.into(),
            quality_report: quality_report.into(),
            acceptance_criteria: acceptance_criteria.into(),
            constraint_overrides: constraint_overrides.into(),
            ai_reviewed,
            workflow_architecture: workflow_architecture.into(),
            rollback_policy: rollback_policy.into(),
            strict_cwd,
            tool_tags: tool_tags.into(),
            enforce_token_budget,
            flow_control_json: flow_control_json.into(),
            phase_timeouts_json: phase_timeouts_json.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListSlashCommandSources {
    pub id: String,
    pub name: String,
    pub source_file_path: String,
}
pub struct ListSlashCommandSourcesBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub source_file_path: &'a str,
}
impl<'a> From<ListSlashCommandSourcesBorrowed<'a>> for ListSlashCommandSources {
    fn from(
        ListSlashCommandSourcesBorrowed {
            id,
            name,
            source_file_path,
        }: ListSlashCommandSourcesBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            source_file_path: source_file_path.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetWorkflowStats {
    pub total_runs: i64,
    pub success_count: i64,
    pub failure_count: i64,
    pub last_run_at: chrono::DateTime<chrono::FixedOffset>,
    pub last_run_status: String,
    pub avg_duration_ms: f64,
}
pub struct GetWorkflowStatsBorrowed<'a> {
    pub total_runs: i64,
    pub success_count: i64,
    pub failure_count: i64,
    pub last_run_at: chrono::DateTime<chrono::FixedOffset>,
    pub last_run_status: &'a str,
    pub avg_duration_ms: f64,
}
impl<'a> From<GetWorkflowStatsBorrowed<'a>> for GetWorkflowStats {
    fn from(
        GetWorkflowStatsBorrowed {
            total_runs,
            success_count,
            failure_count,
            last_run_at,
            last_run_status,
            avg_duration_ms,
        }: GetWorkflowStatsBorrowed<'a>,
    ) -> Self {
        Self {
            total_runs,
            success_count,
            failure_count,
            last_run_at,
            last_run_status: last_run_status.into(),
            avg_duration_ms,
        }
    }
}
use crate::client::async_::GenericClient;
use futures::{self, StreamExt, TryStreamExt};
pub struct ListUnifiedWorkflowsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<ListUnifiedWorkflowsBorrowed, tokio_postgres::Error>,
    mapper: fn(ListUnifiedWorkflowsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ListUnifiedWorkflowsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ListUnifiedWorkflowsBorrowed) -> R,
    ) -> ListUnifiedWorkflowsQuery<'c, 'a, 's, C, R, N> {
        ListUnifiedWorkflowsQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row =
            crate::client::async_::one(self.client, self.query, &self.params, self.cached).await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row =
            crate::client::async_::opt(self.client, self.query, &self.params, self.cached).await?;
        Ok(opt_row
            .map(|row| {
                let extracted = (self.extractor)(&row)?;
                Ok((self.mapper)(extracted))
            })
            .transpose()?)
    }
    pub async fn iter(
        self,
    ) -> Result<
        impl futures::Stream<Item = Result<T, tokio_postgres::Error>> + 'c,
        tokio_postgres::Error,
    > {
        let stream = crate::client::async_::raw(
            self.client,
            self.query,
            crate::slice_iter(&self.params),
            self.cached,
        )
        .await?;
        let mapped = stream
            .map(move |res| {
                res.and_then(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct GetUnifiedWorkflowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetUnifiedWorkflowBorrowed, tokio_postgres::Error>,
    mapper: fn(GetUnifiedWorkflowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetUnifiedWorkflowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetUnifiedWorkflowBorrowed) -> R,
    ) -> GetUnifiedWorkflowQuery<'c, 'a, 's, C, R, N> {
        GetUnifiedWorkflowQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row =
            crate::client::async_::one(self.client, self.query, &self.params, self.cached).await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row =
            crate::client::async_::opt(self.client, self.query, &self.params, self.cached).await?;
        Ok(opt_row
            .map(|row| {
                let extracted = (self.extractor)(&row)?;
                Ok((self.mapper)(extracted))
            })
            .transpose()?)
    }
    pub async fn iter(
        self,
    ) -> Result<
        impl futures::Stream<Item = Result<T, tokio_postgres::Error>> + 'c,
        tokio_postgres::Error,
    > {
        let stream = crate::client::async_::raw(
            self.client,
            self.query,
            crate::slice_iter(&self.params),
            self.cached,
        )
        .await?;
        let mapped = stream
            .map(move |res| {
                res.and_then(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct GetUnifiedWorkflowByNameQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetUnifiedWorkflowByNameBorrowed, tokio_postgres::Error>,
    mapper: fn(GetUnifiedWorkflowByNameBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetUnifiedWorkflowByNameQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetUnifiedWorkflowByNameBorrowed) -> R,
    ) -> GetUnifiedWorkflowByNameQuery<'c, 'a, 's, C, R, N> {
        GetUnifiedWorkflowByNameQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row =
            crate::client::async_::one(self.client, self.query, &self.params, self.cached).await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row =
            crate::client::async_::opt(self.client, self.query, &self.params, self.cached).await?;
        Ok(opt_row
            .map(|row| {
                let extracted = (self.extractor)(&row)?;
                Ok((self.mapper)(extracted))
            })
            .transpose()?)
    }
    pub async fn iter(
        self,
    ) -> Result<
        impl futures::Stream<Item = Result<T, tokio_postgres::Error>> + 'c,
        tokio_postgres::Error,
    > {
        let stream = crate::client::async_::raw(
            self.client,
            self.query,
            crate::slice_iter(&self.params),
            self.cached,
        )
        .await?;
        let mapped = stream
            .map(move |res| {
                res.and_then(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct StringQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<&str, tokio_postgres::Error>,
    mapper: fn(&str) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> StringQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(self, mapper: fn(&str) -> R) -> StringQuery<'c, 'a, 's, C, R, N> {
        StringQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row =
            crate::client::async_::one(self.client, self.query, &self.params, self.cached).await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row =
            crate::client::async_::opt(self.client, self.query, &self.params, self.cached).await?;
        Ok(opt_row
            .map(|row| {
                let extracted = (self.extractor)(&row)?;
                Ok((self.mapper)(extracted))
            })
            .transpose()?)
    }
    pub async fn iter(
        self,
    ) -> Result<
        impl futures::Stream<Item = Result<T, tokio_postgres::Error>> + 'c,
        tokio_postgres::Error,
    > {
        let stream = crate::client::async_::raw(
            self.client,
            self.query,
            crate::slice_iter(&self.params),
            self.cached,
        )
        .await?;
        let mapped = stream
            .map(move |res| {
                res.and_then(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct SearchUnifiedWorkflowsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<SearchUnifiedWorkflowsBorrowed, tokio_postgres::Error>,
    mapper: fn(SearchUnifiedWorkflowsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> SearchUnifiedWorkflowsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(SearchUnifiedWorkflowsBorrowed) -> R,
    ) -> SearchUnifiedWorkflowsQuery<'c, 'a, 's, C, R, N> {
        SearchUnifiedWorkflowsQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row =
            crate::client::async_::one(self.client, self.query, &self.params, self.cached).await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row =
            crate::client::async_::opt(self.client, self.query, &self.params, self.cached).await?;
        Ok(opt_row
            .map(|row| {
                let extracted = (self.extractor)(&row)?;
                Ok((self.mapper)(extracted))
            })
            .transpose()?)
    }
    pub async fn iter(
        self,
    ) -> Result<
        impl futures::Stream<Item = Result<T, tokio_postgres::Error>> + 'c,
        tokio_postgres::Error,
    > {
        let stream = crate::client::async_::raw(
            self.client,
            self.query,
            crate::slice_iter(&self.params),
            self.cached,
        )
        .await?;
        let mapped = stream
            .map(move |res| {
                res.and_then(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct BoolQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<bool, tokio_postgres::Error>,
    mapper: fn(bool) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> BoolQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(self, mapper: fn(bool) -> R) -> BoolQuery<'c, 'a, 's, C, R, N> {
        BoolQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row =
            crate::client::async_::one(self.client, self.query, &self.params, self.cached).await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row =
            crate::client::async_::opt(self.client, self.query, &self.params, self.cached).await?;
        Ok(opt_row
            .map(|row| {
                let extracted = (self.extractor)(&row)?;
                Ok((self.mapper)(extracted))
            })
            .transpose()?)
    }
    pub async fn iter(
        self,
    ) -> Result<
        impl futures::Stream<Item = Result<T, tokio_postgres::Error>> + 'c,
        tokio_postgres::Error,
    > {
        let stream = crate::client::async_::raw(
            self.client,
            self.query,
            crate::slice_iter(&self.params),
            self.cached,
        )
        .await?;
        let mapped = stream
            .map(move |res| {
                res.and_then(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct GetPendingSyncWorkflowsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetPendingSyncWorkflowsBorrowed, tokio_postgres::Error>,
    mapper: fn(GetPendingSyncWorkflowsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetPendingSyncWorkflowsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetPendingSyncWorkflowsBorrowed) -> R,
    ) -> GetPendingSyncWorkflowsQuery<'c, 'a, 's, C, R, N> {
        GetPendingSyncWorkflowsQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row =
            crate::client::async_::one(self.client, self.query, &self.params, self.cached).await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row =
            crate::client::async_::opt(self.client, self.query, &self.params, self.cached).await?;
        Ok(opt_row
            .map(|row| {
                let extracted = (self.extractor)(&row)?;
                Ok((self.mapper)(extracted))
            })
            .transpose()?)
    }
    pub async fn iter(
        self,
    ) -> Result<
        impl futures::Stream<Item = Result<T, tokio_postgres::Error>> + 'c,
        tokio_postgres::Error,
    > {
        let stream = crate::client::async_::raw(
            self.client,
            self.query,
            crate::slice_iter(&self.params),
            self.cached,
        )
        .await?;
        let mapped = stream
            .map(move |res| {
                res.and_then(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct ListSlashCommandSourcesQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<ListSlashCommandSourcesBorrowed, tokio_postgres::Error>,
    mapper: fn(ListSlashCommandSourcesBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ListSlashCommandSourcesQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ListSlashCommandSourcesBorrowed) -> R,
    ) -> ListSlashCommandSourcesQuery<'c, 'a, 's, C, R, N> {
        ListSlashCommandSourcesQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row =
            crate::client::async_::one(self.client, self.query, &self.params, self.cached).await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row =
            crate::client::async_::opt(self.client, self.query, &self.params, self.cached).await?;
        Ok(opt_row
            .map(|row| {
                let extracted = (self.extractor)(&row)?;
                Ok((self.mapper)(extracted))
            })
            .transpose()?)
    }
    pub async fn iter(
        self,
    ) -> Result<
        impl futures::Stream<Item = Result<T, tokio_postgres::Error>> + 'c,
        tokio_postgres::Error,
    > {
        let stream = crate::client::async_::raw(
            self.client,
            self.query,
            crate::slice_iter(&self.params),
            self.cached,
        )
        .await?;
        let mapped = stream
            .map(move |res| {
                res.and_then(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct GetWorkflowStatsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetWorkflowStatsBorrowed, tokio_postgres::Error>,
    mapper: fn(GetWorkflowStatsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetWorkflowStatsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetWorkflowStatsBorrowed) -> R,
    ) -> GetWorkflowStatsQuery<'c, 'a, 's, C, R, N> {
        GetWorkflowStatsQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row =
            crate::client::async_::one(self.client, self.query, &self.params, self.cached).await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row =
            crate::client::async_::opt(self.client, self.query, &self.params, self.cached).await?;
        Ok(opt_row
            .map(|row| {
                let extracted = (self.extractor)(&row)?;
                Ok((self.mapper)(extracted))
            })
            .transpose()?)
    }
    pub async fn iter(
        self,
    ) -> Result<
        impl futures::Stream<Item = Result<T, tokio_postgres::Error>> + 'c,
        tokio_postgres::Error,
    > {
        let stream = crate::client::async_::raw(
            self.client,
            self.query,
            crate::slice_iter(&self.params),
            self.cached,
        )
        .await?;
        let mapped = stream
            .map(move |res| {
                res.and_then(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct ListUnifiedWorkflowsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_unified_workflows() -> ListUnifiedWorkflowsStmt {
    ListUnifiedWorkflowsStmt(
        "SELECT id, name, COALESCE(description, '') as description, COALESCE(category, 'general') as category, COALESCE(tags, '[]') as tags, COALESCE(setup_steps, '[]') as setup_steps, COALESCE(verification_steps, '[]') as verification_steps, COALESCE(agentic_steps, '[]') as agentic_steps, COALESCE(completion_steps, '[]') as completion_steps, COALESCE(max_iterations, 10) as max_iterations, COALESCE(provider, '') as provider, COALESCE(model, '') as model, skip_ai_summary, created_at, updated_at, COALESCE(log_source_selection, '\"default\"') as log_source_selection, COALESCE(context_ids, '[]') as context_ids, COALESCE(disabled_context_ids, '[]') as disabled_context_ids, COALESCE(auto_include_contexts, true) as auto_include_contexts, COALESCE(prompt_template, '') as prompt_template, COALESCE(log_watch_enabled, true) as log_watch_enabled, COALESCE(health_check_enabled, true) as health_check_enabled, COALESCE(health_check_urls, '[]') as health_check_urls, COALESCE(timeout_seconds, 0) as timeout_seconds, COALESCE(preflight_check_enabled, true) as preflight_check_enabled, COALESCE(generated_by_task_run_id, '') as generated_by_task_run_id, COALESCE(enable_sweep, false) as enable_sweep, COALESCE(max_sweep_iterations, 5) as max_sweep_iterations, COALESCE(stages, '[]') as stages, COALESCE(stop_on_failure, false) as stop_on_failure, COALESCE(reflection_mode, true) as reflection_mode, COALESCE(model_overrides, '{}') as model_overrides, COALESCE(approval_gate, false) as approval_gate, COALESCE(completion_prompts_first, false) as completion_prompts_first, COALESCE(is_favorite, false) as is_favorite, COALESCE(dependency_graph, '') as dependency_graph, COALESCE(cost_annotations, '') as cost_annotations, COALESCE(quality_report, '') as quality_report, COALESCE(acceptance_criteria, '') as acceptance_criteria, COALESCE(constraint_overrides, '{}') as constraint_overrides, COALESCE(ai_reviewed, true) as ai_reviewed, COALESCE(workflow_architecture, '') as workflow_architecture, COALESCE(rollback_policy, '') as rollback_policy, COALESCE(strict_cwd, false) as strict_cwd, COALESCE(tool_tags, '[]') as tool_tags, COALESCE(enforce_token_budget, false) as enforce_token_budget, COALESCE(flow_control_json, '') as flow_control_json, COALESCE(phase_timeouts_json, '') as phase_timeouts_json FROM unified_workflows ORDER BY is_favorite DESC, updated_at DESC",
        None,
    )
}
impl ListUnifiedWorkflowsStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient>(
        &'s self,
        client: &'c C,
    ) -> ListUnifiedWorkflowsQuery<'c, 'a, 's, C, ListUnifiedWorkflows, 0> {
        ListUnifiedWorkflowsQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ListUnifiedWorkflowsBorrowed, tokio_postgres::Error> {
                Ok(ListUnifiedWorkflowsBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    description: row.try_get(2)?,
                    category: row.try_get(3)?,
                    tags: row.try_get(4)?,
                    setup_steps: row.try_get(5)?,
                    verification_steps: row.try_get(6)?,
                    agentic_steps: row.try_get(7)?,
                    completion_steps: row.try_get(8)?,
                    max_iterations: row.try_get(9)?,
                    provider: row.try_get(10)?,
                    model: row.try_get(11)?,
                    skip_ai_summary: row.try_get(12)?,
                    created_at: row.try_get(13)?,
                    updated_at: row.try_get(14)?,
                    log_source_selection: row.try_get(15)?,
                    context_ids: row.try_get(16)?,
                    disabled_context_ids: row.try_get(17)?,
                    auto_include_contexts: row.try_get(18)?,
                    prompt_template: row.try_get(19)?,
                    log_watch_enabled: row.try_get(20)?,
                    health_check_enabled: row.try_get(21)?,
                    health_check_urls: row.try_get(22)?,
                    timeout_seconds: row.try_get(23)?,
                    preflight_check_enabled: row.try_get(24)?,
                    generated_by_task_run_id: row.try_get(25)?,
                    enable_sweep: row.try_get(26)?,
                    max_sweep_iterations: row.try_get(27)?,
                    stages: row.try_get(28)?,
                    stop_on_failure: row.try_get(29)?,
                    reflection_mode: row.try_get(30)?,
                    model_overrides: row.try_get(31)?,
                    approval_gate: row.try_get(32)?,
                    completion_prompts_first: row.try_get(33)?,
                    is_favorite: row.try_get(34)?,
                    dependency_graph: row.try_get(35)?,
                    cost_annotations: row.try_get(36)?,
                    quality_report: row.try_get(37)?,
                    acceptance_criteria: row.try_get(38)?,
                    constraint_overrides: row.try_get(39)?,
                    ai_reviewed: row.try_get(40)?,
                    workflow_architecture: row.try_get(41)?,
                    rollback_policy: row.try_get(42)?,
                    strict_cwd: row.try_get(43)?,
                    tool_tags: row.try_get(44)?,
                    enforce_token_budget: row.try_get(45)?,
                    flow_control_json: row.try_get(46)?,
                    phase_timeouts_json: row.try_get(47)?,
                })
            },
            mapper: |it| ListUnifiedWorkflows::from(it),
        }
    }
}
pub struct GetUnifiedWorkflowStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_unified_workflow() -> GetUnifiedWorkflowStmt {
    GetUnifiedWorkflowStmt(
        "SELECT id, name, COALESCE(description, '') as description, COALESCE(category, 'general') as category, COALESCE(tags, '[]') as tags, COALESCE(setup_steps, '[]') as setup_steps, COALESCE(verification_steps, '[]') as verification_steps, COALESCE(agentic_steps, '[]') as agentic_steps, COALESCE(completion_steps, '[]') as completion_steps, COALESCE(max_iterations, 10) as max_iterations, COALESCE(provider, '') as provider, COALESCE(model, '') as model, skip_ai_summary, created_at, updated_at, COALESCE(log_source_selection, '\"default\"') as log_source_selection, COALESCE(context_ids, '[]') as context_ids, COALESCE(disabled_context_ids, '[]') as disabled_context_ids, COALESCE(auto_include_contexts, true) as auto_include_contexts, COALESCE(prompt_template, '') as prompt_template, COALESCE(log_watch_enabled, true) as log_watch_enabled, COALESCE(health_check_enabled, true) as health_check_enabled, COALESCE(health_check_urls, '[]') as health_check_urls, COALESCE(timeout_seconds, 0) as timeout_seconds, COALESCE(preflight_check_enabled, true) as preflight_check_enabled, COALESCE(generated_by_task_run_id, '') as generated_by_task_run_id, COALESCE(enable_sweep, false) as enable_sweep, COALESCE(max_sweep_iterations, 5) as max_sweep_iterations, COALESCE(stages, '[]') as stages, COALESCE(stop_on_failure, false) as stop_on_failure, COALESCE(reflection_mode, true) as reflection_mode, COALESCE(model_overrides, '{}') as model_overrides, COALESCE(approval_gate, false) as approval_gate, COALESCE(completion_prompts_first, false) as completion_prompts_first, COALESCE(is_favorite, false) as is_favorite, COALESCE(dependency_graph, '') as dependency_graph, COALESCE(cost_annotations, '') as cost_annotations, COALESCE(quality_report, '') as quality_report, COALESCE(acceptance_criteria, '') as acceptance_criteria, COALESCE(constraint_overrides, '{}') as constraint_overrides, COALESCE(ai_reviewed, true) as ai_reviewed, COALESCE(workflow_architecture, '') as workflow_architecture, COALESCE(rollback_policy, '') as rollback_policy, COALESCE(strict_cwd, false) as strict_cwd, COALESCE(tool_tags, '[]') as tool_tags, COALESCE(enforce_token_budget, false) as enforce_token_budget, COALESCE(flow_control_json, '') as flow_control_json, COALESCE(phase_timeouts_json, '') as phase_timeouts_json FROM unified_workflows WHERE id = $1",
        None,
    )
}
impl GetUnifiedWorkflowStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>(
        &'s self,
        client: &'c C,
        id: &'a T1,
    ) -> GetUnifiedWorkflowQuery<'c, 'a, 's, C, GetUnifiedWorkflow, 1> {
        GetUnifiedWorkflowQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetUnifiedWorkflowBorrowed, tokio_postgres::Error> {
                Ok(GetUnifiedWorkflowBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    description: row.try_get(2)?,
                    category: row.try_get(3)?,
                    tags: row.try_get(4)?,
                    setup_steps: row.try_get(5)?,
                    verification_steps: row.try_get(6)?,
                    agentic_steps: row.try_get(7)?,
                    completion_steps: row.try_get(8)?,
                    max_iterations: row.try_get(9)?,
                    provider: row.try_get(10)?,
                    model: row.try_get(11)?,
                    skip_ai_summary: row.try_get(12)?,
                    created_at: row.try_get(13)?,
                    updated_at: row.try_get(14)?,
                    log_source_selection: row.try_get(15)?,
                    context_ids: row.try_get(16)?,
                    disabled_context_ids: row.try_get(17)?,
                    auto_include_contexts: row.try_get(18)?,
                    prompt_template: row.try_get(19)?,
                    log_watch_enabled: row.try_get(20)?,
                    health_check_enabled: row.try_get(21)?,
                    health_check_urls: row.try_get(22)?,
                    timeout_seconds: row.try_get(23)?,
                    preflight_check_enabled: row.try_get(24)?,
                    generated_by_task_run_id: row.try_get(25)?,
                    enable_sweep: row.try_get(26)?,
                    max_sweep_iterations: row.try_get(27)?,
                    stages: row.try_get(28)?,
                    stop_on_failure: row.try_get(29)?,
                    reflection_mode: row.try_get(30)?,
                    model_overrides: row.try_get(31)?,
                    approval_gate: row.try_get(32)?,
                    completion_prompts_first: row.try_get(33)?,
                    is_favorite: row.try_get(34)?,
                    dependency_graph: row.try_get(35)?,
                    cost_annotations: row.try_get(36)?,
                    quality_report: row.try_get(37)?,
                    acceptance_criteria: row.try_get(38)?,
                    constraint_overrides: row.try_get(39)?,
                    ai_reviewed: row.try_get(40)?,
                    workflow_architecture: row.try_get(41)?,
                    rollback_policy: row.try_get(42)?,
                    strict_cwd: row.try_get(43)?,
                    tool_tags: row.try_get(44)?,
                    enforce_token_budget: row.try_get(45)?,
                    flow_control_json: row.try_get(46)?,
                    phase_timeouts_json: row.try_get(47)?,
                })
            },
            mapper: |it| GetUnifiedWorkflow::from(it),
        }
    }
}
pub struct GetUnifiedWorkflowByNameStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_unified_workflow_by_name() -> GetUnifiedWorkflowByNameStmt {
    GetUnifiedWorkflowByNameStmt(
        "SELECT id, name, COALESCE(description, '') as description, COALESCE(category, 'general') as category, COALESCE(tags, '[]') as tags, COALESCE(setup_steps, '[]') as setup_steps, COALESCE(verification_steps, '[]') as verification_steps, COALESCE(agentic_steps, '[]') as agentic_steps, COALESCE(completion_steps, '[]') as completion_steps, COALESCE(max_iterations, 10) as max_iterations, COALESCE(provider, '') as provider, COALESCE(model, '') as model, skip_ai_summary, created_at, updated_at, COALESCE(log_source_selection, '\"default\"') as log_source_selection, COALESCE(context_ids, '[]') as context_ids, COALESCE(disabled_context_ids, '[]') as disabled_context_ids, COALESCE(auto_include_contexts, true) as auto_include_contexts, COALESCE(prompt_template, '') as prompt_template, COALESCE(log_watch_enabled, true) as log_watch_enabled, COALESCE(health_check_enabled, true) as health_check_enabled, COALESCE(health_check_urls, '[]') as health_check_urls, COALESCE(timeout_seconds, 0) as timeout_seconds, COALESCE(preflight_check_enabled, true) as preflight_check_enabled, COALESCE(generated_by_task_run_id, '') as generated_by_task_run_id, COALESCE(enable_sweep, false) as enable_sweep, COALESCE(max_sweep_iterations, 5) as max_sweep_iterations, COALESCE(stages, '[]') as stages, COALESCE(stop_on_failure, false) as stop_on_failure, COALESCE(reflection_mode, true) as reflection_mode, COALESCE(model_overrides, '{}') as model_overrides, COALESCE(approval_gate, false) as approval_gate, COALESCE(completion_prompts_first, false) as completion_prompts_first, COALESCE(is_favorite, false) as is_favorite, COALESCE(dependency_graph, '') as dependency_graph, COALESCE(cost_annotations, '') as cost_annotations, COALESCE(quality_report, '') as quality_report, COALESCE(acceptance_criteria, '') as acceptance_criteria, COALESCE(constraint_overrides, '{}') as constraint_overrides, COALESCE(ai_reviewed, true) as ai_reviewed, COALESCE(workflow_architecture, '') as workflow_architecture, COALESCE(rollback_policy, '') as rollback_policy, COALESCE(strict_cwd, false) as strict_cwd, COALESCE(tool_tags, '[]') as tool_tags, COALESCE(enforce_token_budget, false) as enforce_token_budget, COALESCE(flow_control_json, '') as flow_control_json, COALESCE(phase_timeouts_json, '') as phase_timeouts_json FROM unified_workflows WHERE name = $1 LIMIT 1",
        None,
    )
}
impl GetUnifiedWorkflowByNameStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>(
        &'s self,
        client: &'c C,
        name: &'a T1,
    ) -> GetUnifiedWorkflowByNameQuery<'c, 'a, 's, C, GetUnifiedWorkflowByName, 1> {
        GetUnifiedWorkflowByNameQuery {
            client,
            params: [name],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetUnifiedWorkflowByNameBorrowed, tokio_postgres::Error> {
                Ok(GetUnifiedWorkflowByNameBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    description: row.try_get(2)?,
                    category: row.try_get(3)?,
                    tags: row.try_get(4)?,
                    setup_steps: row.try_get(5)?,
                    verification_steps: row.try_get(6)?,
                    agentic_steps: row.try_get(7)?,
                    completion_steps: row.try_get(8)?,
                    max_iterations: row.try_get(9)?,
                    provider: row.try_get(10)?,
                    model: row.try_get(11)?,
                    skip_ai_summary: row.try_get(12)?,
                    created_at: row.try_get(13)?,
                    updated_at: row.try_get(14)?,
                    log_source_selection: row.try_get(15)?,
                    context_ids: row.try_get(16)?,
                    disabled_context_ids: row.try_get(17)?,
                    auto_include_contexts: row.try_get(18)?,
                    prompt_template: row.try_get(19)?,
                    log_watch_enabled: row.try_get(20)?,
                    health_check_enabled: row.try_get(21)?,
                    health_check_urls: row.try_get(22)?,
                    timeout_seconds: row.try_get(23)?,
                    preflight_check_enabled: row.try_get(24)?,
                    generated_by_task_run_id: row.try_get(25)?,
                    enable_sweep: row.try_get(26)?,
                    max_sweep_iterations: row.try_get(27)?,
                    stages: row.try_get(28)?,
                    stop_on_failure: row.try_get(29)?,
                    reflection_mode: row.try_get(30)?,
                    model_overrides: row.try_get(31)?,
                    approval_gate: row.try_get(32)?,
                    completion_prompts_first: row.try_get(33)?,
                    is_favorite: row.try_get(34)?,
                    dependency_graph: row.try_get(35)?,
                    cost_annotations: row.try_get(36)?,
                    quality_report: row.try_get(37)?,
                    acceptance_criteria: row.try_get(38)?,
                    constraint_overrides: row.try_get(39)?,
                    ai_reviewed: row.try_get(40)?,
                    workflow_architecture: row.try_get(41)?,
                    rollback_policy: row.try_get(42)?,
                    strict_cwd: row.try_get(43)?,
                    tool_tags: row.try_get(44)?,
                    enforce_token_budget: row.try_get(45)?,
                    flow_control_json: row.try_get(46)?,
                    phase_timeouts_json: row.try_get(47)?,
                })
            },
            mapper: |it| GetUnifiedWorkflowByName::from(it),
        }
    }
}
pub struct CreateUnifiedWorkflowStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn create_unified_workflow() -> CreateUnifiedWorkflowStmt {
    CreateUnifiedWorkflowStmt(
        "INSERT INTO unified_workflows ( id, name, description, category, tags, setup_steps, verification_steps, agentic_steps, completion_steps, max_iterations, timeout_seconds, provider, model, skip_ai_summary, log_source_selection, context_ids, disabled_context_ids, auto_include_contexts, prompt_template, log_watch_enabled, health_check_enabled, health_check_urls, preflight_check_enabled, generated_by_task_run_id, enable_sweep, max_sweep_iterations, stages, stop_on_failure, reflection_mode, model_overrides, approval_gate, completion_prompts_first, dependency_graph, cost_annotations, quality_report, acceptance_criteria, constraint_overrides, ai_reviewed, workflow_architecture, strict_cwd, tool_tags, rollback_policy, enforce_token_budget, flow_control_json, phase_timeouts_json ) VALUES ( $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, $31, $32, $33, $34, $35, $36, $37, $38, $39, $40, $41, $42, $43, $44, $45 ) RETURNING id",
        None,
    )
}
impl CreateUnifiedWorkflowStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<
        'c,
        'a,
        's,
        C: GenericClient,
        T1: crate::StringSql,
        T2: crate::StringSql,
        T3: crate::StringSql,
        T4: crate::StringSql,
        T5: crate::StringSql,
        T6: crate::StringSql,
        T7: crate::StringSql,
        T8: crate::StringSql,
        T9: crate::StringSql,
        T10: crate::StringSql,
        T11: crate::StringSql,
        T12: crate::StringSql,
        T13: crate::StringSql,
        T14: crate::StringSql,
        T15: crate::StringSql,
        T16: crate::StringSql,
        T17: crate::StringSql,
        T18: crate::StringSql,
        T19: crate::StringSql,
        T20: crate::StringSql,
        T21: crate::StringSql,
        T22: crate::StringSql,
        T23: crate::StringSql,
        T24: crate::StringSql,
        T25: crate::StringSql,
        T26: crate::StringSql,
        T27: crate::StringSql,
        T28: crate::StringSql,
        T29: crate::StringSql,
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        name: &'a T2,
        description: &'a Option<T3>,
        category: &'a T4,
        tags: &'a T5,
        setup_steps: &'a T6,
        verification_steps: &'a T7,
        agentic_steps: &'a T8,
        completion_steps: &'a T9,
        max_iterations: &'a i64,
        timeout_seconds: &'a Option<i64>,
        provider: &'a Option<T10>,
        model: &'a Option<T11>,
        skip_ai_summary: &'a bool,
        log_source_selection: &'a T12,
        context_ids: &'a T13,
        disabled_context_ids: &'a T14,
        auto_include_contexts: &'a bool,
        prompt_template: &'a Option<T15>,
        log_watch_enabled: &'a bool,
        health_check_enabled: &'a bool,
        health_check_urls: &'a T16,
        preflight_check_enabled: &'a bool,
        generated_by_task_run_id: &'a Option<T17>,
        enable_sweep: &'a bool,
        max_sweep_iterations: &'a i64,
        stages: &'a T18,
        stop_on_failure: &'a bool,
        reflection_mode: &'a bool,
        model_overrides: &'a T19,
        approval_gate: &'a bool,
        completion_prompts_first: &'a bool,
        dependency_graph: &'a Option<T20>,
        cost_annotations: &'a Option<T21>,
        quality_report: &'a Option<T22>,
        acceptance_criteria: &'a Option<T23>,
        constraint_overrides: &'a T24,
        ai_reviewed: &'a bool,
        workflow_architecture: &'a Option<T25>,
        strict_cwd: &'a bool,
        tool_tags: &'a T26,
        rollback_policy: &'a Option<T27>,
        enforce_token_budget: &'a bool,
        flow_control_json: &'a Option<T28>,
        phase_timeouts_json: &'a Option<T29>,
    ) -> StringQuery<'c, 'a, 's, C, String, 45> {
        StringQuery {
            client,
            params: [
                id,
                name,
                description,
                category,
                tags,
                setup_steps,
                verification_steps,
                agentic_steps,
                completion_steps,
                max_iterations,
                timeout_seconds,
                provider,
                model,
                skip_ai_summary,
                log_source_selection,
                context_ids,
                disabled_context_ids,
                auto_include_contexts,
                prompt_template,
                log_watch_enabled,
                health_check_enabled,
                health_check_urls,
                preflight_check_enabled,
                generated_by_task_run_id,
                enable_sweep,
                max_sweep_iterations,
                stages,
                stop_on_failure,
                reflection_mode,
                model_overrides,
                approval_gate,
                completion_prompts_first,
                dependency_graph,
                cost_annotations,
                quality_report,
                acceptance_criteria,
                constraint_overrides,
                ai_reviewed,
                workflow_architecture,
                strict_cwd,
                tool_tags,
                rollback_policy,
                enforce_token_budget,
                flow_control_json,
                phase_timeouts_json,
            ],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
impl<
    'c,
    'a,
    's,
    C: GenericClient,
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::StringSql,
    T8: crate::StringSql,
    T9: crate::StringSql,
    T10: crate::StringSql,
    T11: crate::StringSql,
    T12: crate::StringSql,
    T13: crate::StringSql,
    T14: crate::StringSql,
    T15: crate::StringSql,
    T16: crate::StringSql,
    T17: crate::StringSql,
    T18: crate::StringSql,
    T19: crate::StringSql,
    T20: crate::StringSql,
    T21: crate::StringSql,
    T22: crate::StringSql,
    T23: crate::StringSql,
    T24: crate::StringSql,
    T25: crate::StringSql,
    T26: crate::StringSql,
    T27: crate::StringSql,
    T28: crate::StringSql,
    T29: crate::StringSql,
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        CreateUnifiedWorkflowParams<
            T1,
            T2,
            T3,
            T4,
            T5,
            T6,
            T7,
            T8,
            T9,
            T10,
            T11,
            T12,
            T13,
            T14,
            T15,
            T16,
            T17,
            T18,
            T19,
            T20,
            T21,
            T22,
            T23,
            T24,
            T25,
            T26,
            T27,
            T28,
            T29,
        >,
        StringQuery<'c, 'a, 's, C, String, 45>,
        C,
    > for CreateUnifiedWorkflowStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a CreateUnifiedWorkflowParams<
            T1,
            T2,
            T3,
            T4,
            T5,
            T6,
            T7,
            T8,
            T9,
            T10,
            T11,
            T12,
            T13,
            T14,
            T15,
            T16,
            T17,
            T18,
            T19,
            T20,
            T21,
            T22,
            T23,
            T24,
            T25,
            T26,
            T27,
            T28,
            T29,
        >,
    ) -> StringQuery<'c, 'a, 's, C, String, 45> {
        self.bind(
            client,
            &params.id,
            &params.name,
            &params.description,
            &params.category,
            &params.tags,
            &params.setup_steps,
            &params.verification_steps,
            &params.agentic_steps,
            &params.completion_steps,
            &params.max_iterations,
            &params.timeout_seconds,
            &params.provider,
            &params.model,
            &params.skip_ai_summary,
            &params.log_source_selection,
            &params.context_ids,
            &params.disabled_context_ids,
            &params.auto_include_contexts,
            &params.prompt_template,
            &params.log_watch_enabled,
            &params.health_check_enabled,
            &params.health_check_urls,
            &params.preflight_check_enabled,
            &params.generated_by_task_run_id,
            &params.enable_sweep,
            &params.max_sweep_iterations,
            &params.stages,
            &params.stop_on_failure,
            &params.reflection_mode,
            &params.model_overrides,
            &params.approval_gate,
            &params.completion_prompts_first,
            &params.dependency_graph,
            &params.cost_annotations,
            &params.quality_report,
            &params.acceptance_criteria,
            &params.constraint_overrides,
            &params.ai_reviewed,
            &params.workflow_architecture,
            &params.strict_cwd,
            &params.tool_tags,
            &params.rollback_policy,
            &params.enforce_token_budget,
            &params.flow_control_json,
            &params.phase_timeouts_json,
        )
    }
}
pub struct UpdateUnifiedWorkflowStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_unified_workflow() -> UpdateUnifiedWorkflowStmt {
    UpdateUnifiedWorkflowStmt(
        "UPDATE unified_workflows SET name = $1, description = $2, category = $3, tags = $4, setup_steps = $5, verification_steps = $6, agentic_steps = $7, completion_steps = $8, max_iterations = $9, timeout_seconds = $10, provider = $11, model = $12, skip_ai_summary = $13, updated_at = NOW(), log_source_selection = $14, prompt_template = $15, context_ids = $16, disabled_context_ids = $17, auto_include_contexts = $18, log_watch_enabled = $19, health_check_enabled = $20, health_check_urls = $21, preflight_check_enabled = $22, enable_sweep = $23, max_sweep_iterations = $24, stages = $25, stop_on_failure = $26, approval_gate = $27, reflection_mode = $28, model_overrides = $29, completion_prompts_first = $30, dependency_graph = $31, cost_annotations = $32, quality_report = $33, acceptance_criteria = $34, constraint_overrides = $35, ai_reviewed = $36, workflow_architecture = $37, strict_cwd = $38, tool_tags = $39, rollback_policy = $40, enforce_token_budget = $41, flow_control_json = $42, phase_timeouts_json = $43 WHERE id = $44 RETURNING id",
        None,
    )
}
impl UpdateUnifiedWorkflowStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<
        'c,
        'a,
        's,
        C: GenericClient,
        T1: crate::StringSql,
        T2: crate::StringSql,
        T3: crate::StringSql,
        T4: crate::StringSql,
        T5: crate::StringSql,
        T6: crate::StringSql,
        T7: crate::StringSql,
        T8: crate::StringSql,
        T9: crate::StringSql,
        T10: crate::StringSql,
        T11: crate::StringSql,
        T12: crate::StringSql,
        T13: crate::StringSql,
        T14: crate::StringSql,
        T15: crate::StringSql,
        T16: crate::StringSql,
        T17: crate::StringSql,
        T18: crate::StringSql,
        T19: crate::StringSql,
        T20: crate::StringSql,
        T21: crate::StringSql,
        T22: crate::StringSql,
        T23: crate::StringSql,
        T24: crate::StringSql,
        T25: crate::StringSql,
        T26: crate::StringSql,
        T27: crate::StringSql,
        T28: crate::StringSql,
    >(
        &'s self,
        client: &'c C,
        name: &'a T1,
        description: &'a Option<T2>,
        category: &'a T3,
        tags: &'a T4,
        setup_steps: &'a T5,
        verification_steps: &'a T6,
        agentic_steps: &'a T7,
        completion_steps: &'a T8,
        max_iterations: &'a i64,
        timeout_seconds: &'a Option<i64>,
        provider: &'a Option<T9>,
        model: &'a Option<T10>,
        skip_ai_summary: &'a bool,
        log_source_selection: &'a T11,
        prompt_template: &'a Option<T12>,
        context_ids: &'a T13,
        disabled_context_ids: &'a T14,
        auto_include_contexts: &'a bool,
        log_watch_enabled: &'a bool,
        health_check_enabled: &'a bool,
        health_check_urls: &'a T15,
        preflight_check_enabled: &'a bool,
        enable_sweep: &'a bool,
        max_sweep_iterations: &'a i64,
        stages: &'a T16,
        stop_on_failure: &'a bool,
        approval_gate: &'a bool,
        reflection_mode: &'a bool,
        model_overrides: &'a T17,
        completion_prompts_first: &'a bool,
        dependency_graph: &'a Option<T18>,
        cost_annotations: &'a Option<T19>,
        quality_report: &'a Option<T20>,
        acceptance_criteria: &'a Option<T21>,
        constraint_overrides: &'a T22,
        ai_reviewed: &'a bool,
        workflow_architecture: &'a Option<T23>,
        strict_cwd: &'a bool,
        tool_tags: &'a T24,
        rollback_policy: &'a Option<T25>,
        enforce_token_budget: &'a bool,
        flow_control_json: &'a Option<T26>,
        phase_timeouts_json: &'a Option<T27>,
        id: &'a T28,
    ) -> StringQuery<'c, 'a, 's, C, String, 44> {
        StringQuery {
            client,
            params: [
                name,
                description,
                category,
                tags,
                setup_steps,
                verification_steps,
                agentic_steps,
                completion_steps,
                max_iterations,
                timeout_seconds,
                provider,
                model,
                skip_ai_summary,
                log_source_selection,
                prompt_template,
                context_ids,
                disabled_context_ids,
                auto_include_contexts,
                log_watch_enabled,
                health_check_enabled,
                health_check_urls,
                preflight_check_enabled,
                enable_sweep,
                max_sweep_iterations,
                stages,
                stop_on_failure,
                approval_gate,
                reflection_mode,
                model_overrides,
                completion_prompts_first,
                dependency_graph,
                cost_annotations,
                quality_report,
                acceptance_criteria,
                constraint_overrides,
                ai_reviewed,
                workflow_architecture,
                strict_cwd,
                tool_tags,
                rollback_policy,
                enforce_token_budget,
                flow_control_json,
                phase_timeouts_json,
                id,
            ],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
impl<
    'c,
    'a,
    's,
    C: GenericClient,
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::StringSql,
    T8: crate::StringSql,
    T9: crate::StringSql,
    T10: crate::StringSql,
    T11: crate::StringSql,
    T12: crate::StringSql,
    T13: crate::StringSql,
    T14: crate::StringSql,
    T15: crate::StringSql,
    T16: crate::StringSql,
    T17: crate::StringSql,
    T18: crate::StringSql,
    T19: crate::StringSql,
    T20: crate::StringSql,
    T21: crate::StringSql,
    T22: crate::StringSql,
    T23: crate::StringSql,
    T24: crate::StringSql,
    T25: crate::StringSql,
    T26: crate::StringSql,
    T27: crate::StringSql,
    T28: crate::StringSql,
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        UpdateUnifiedWorkflowParams<
            T1,
            T2,
            T3,
            T4,
            T5,
            T6,
            T7,
            T8,
            T9,
            T10,
            T11,
            T12,
            T13,
            T14,
            T15,
            T16,
            T17,
            T18,
            T19,
            T20,
            T21,
            T22,
            T23,
            T24,
            T25,
            T26,
            T27,
            T28,
        >,
        StringQuery<'c, 'a, 's, C, String, 44>,
        C,
    > for UpdateUnifiedWorkflowStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateUnifiedWorkflowParams<
            T1,
            T2,
            T3,
            T4,
            T5,
            T6,
            T7,
            T8,
            T9,
            T10,
            T11,
            T12,
            T13,
            T14,
            T15,
            T16,
            T17,
            T18,
            T19,
            T20,
            T21,
            T22,
            T23,
            T24,
            T25,
            T26,
            T27,
            T28,
        >,
    ) -> StringQuery<'c, 'a, 's, C, String, 44> {
        self.bind(
            client,
            &params.name,
            &params.description,
            &params.category,
            &params.tags,
            &params.setup_steps,
            &params.verification_steps,
            &params.agentic_steps,
            &params.completion_steps,
            &params.max_iterations,
            &params.timeout_seconds,
            &params.provider,
            &params.model,
            &params.skip_ai_summary,
            &params.log_source_selection,
            &params.prompt_template,
            &params.context_ids,
            &params.disabled_context_ids,
            &params.auto_include_contexts,
            &params.log_watch_enabled,
            &params.health_check_enabled,
            &params.health_check_urls,
            &params.preflight_check_enabled,
            &params.enable_sweep,
            &params.max_sweep_iterations,
            &params.stages,
            &params.stop_on_failure,
            &params.approval_gate,
            &params.reflection_mode,
            &params.model_overrides,
            &params.completion_prompts_first,
            &params.dependency_graph,
            &params.cost_annotations,
            &params.quality_report,
            &params.acceptance_criteria,
            &params.constraint_overrides,
            &params.ai_reviewed,
            &params.workflow_architecture,
            &params.strict_cwd,
            &params.tool_tags,
            &params.rollback_policy,
            &params.enforce_token_budget,
            &params.flow_control_json,
            &params.phase_timeouts_json,
            &params.id,
        )
    }
}
pub struct DeleteUnifiedWorkflowStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn delete_unified_workflow() -> DeleteUnifiedWorkflowStmt {
    DeleteUnifiedWorkflowStmt(
        "DELETE FROM unified_workflows WHERE id = $1 RETURNING id",
        None,
    )
}
impl DeleteUnifiedWorkflowStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>(
        &'s self,
        client: &'c C,
        id: &'a T1,
    ) -> StringQuery<'c, 'a, 's, C, String, 1> {
        StringQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
pub struct SearchUnifiedWorkflowsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn search_unified_workflows() -> SearchUnifiedWorkflowsStmt {
    SearchUnifiedWorkflowsStmt(
        "SELECT id, name, COALESCE(description, '') as description, COALESCE(category, 'general') as category, COALESCE(tags, '[]') as tags, COALESCE(setup_steps, '[]') as setup_steps, COALESCE(verification_steps, '[]') as verification_steps, COALESCE(agentic_steps, '[]') as agentic_steps, COALESCE(completion_steps, '[]') as completion_steps, COALESCE(max_iterations, 10) as max_iterations, COALESCE(provider, '') as provider, COALESCE(model, '') as model, skip_ai_summary, created_at, updated_at, COALESCE(log_source_selection, '\"default\"') as log_source_selection, COALESCE(context_ids, '[]') as context_ids, COALESCE(disabled_context_ids, '[]') as disabled_context_ids, COALESCE(auto_include_contexts, true) as auto_include_contexts, COALESCE(prompt_template, '') as prompt_template, COALESCE(log_watch_enabled, true) as log_watch_enabled, COALESCE(health_check_enabled, true) as health_check_enabled, COALESCE(health_check_urls, '[]') as health_check_urls, COALESCE(timeout_seconds, 0) as timeout_seconds, COALESCE(preflight_check_enabled, true) as preflight_check_enabled, COALESCE(generated_by_task_run_id, '') as generated_by_task_run_id, COALESCE(enable_sweep, false) as enable_sweep, COALESCE(max_sweep_iterations, 5) as max_sweep_iterations, COALESCE(stages, '[]') as stages, COALESCE(stop_on_failure, false) as stop_on_failure, COALESCE(reflection_mode, true) as reflection_mode, COALESCE(model_overrides, '{}') as model_overrides, COALESCE(approval_gate, false) as approval_gate, COALESCE(completion_prompts_first, false) as completion_prompts_first, COALESCE(is_favorite, false) as is_favorite, COALESCE(dependency_graph, '') as dependency_graph, COALESCE(cost_annotations, '') as cost_annotations, COALESCE(quality_report, '') as quality_report, COALESCE(acceptance_criteria, '') as acceptance_criteria, COALESCE(constraint_overrides, '{}') as constraint_overrides, COALESCE(ai_reviewed, true) as ai_reviewed, COALESCE(workflow_architecture, '') as workflow_architecture, COALESCE(rollback_policy, '') as rollback_policy, COALESCE(strict_cwd, false) as strict_cwd, COALESCE(tool_tags, '[]') as tool_tags, COALESCE(enforce_token_budget, false) as enforce_token_budget, COALESCE(flow_control_json, '') as flow_control_json, COALESCE(phase_timeouts_json, '') as phase_timeouts_json FROM unified_workflows WHERE to_tsvector('english', name || ' ' || COALESCE(description, '')) @@ plainto_tsquery('english', $1) OR name ILIKE '%' || $1 || '%' OR category ILIKE '%' || $1 || '%' ORDER BY is_favorite DESC, updated_at DESC",
        None,
    )
}
impl SearchUnifiedWorkflowsStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>(
        &'s self,
        client: &'c C,
        query: &'a T1,
    ) -> SearchUnifiedWorkflowsQuery<'c, 'a, 's, C, SearchUnifiedWorkflows, 1> {
        SearchUnifiedWorkflowsQuery {
            client,
            params: [query],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<SearchUnifiedWorkflowsBorrowed, tokio_postgres::Error> {
                Ok(SearchUnifiedWorkflowsBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    description: row.try_get(2)?,
                    category: row.try_get(3)?,
                    tags: row.try_get(4)?,
                    setup_steps: row.try_get(5)?,
                    verification_steps: row.try_get(6)?,
                    agentic_steps: row.try_get(7)?,
                    completion_steps: row.try_get(8)?,
                    max_iterations: row.try_get(9)?,
                    provider: row.try_get(10)?,
                    model: row.try_get(11)?,
                    skip_ai_summary: row.try_get(12)?,
                    created_at: row.try_get(13)?,
                    updated_at: row.try_get(14)?,
                    log_source_selection: row.try_get(15)?,
                    context_ids: row.try_get(16)?,
                    disabled_context_ids: row.try_get(17)?,
                    auto_include_contexts: row.try_get(18)?,
                    prompt_template: row.try_get(19)?,
                    log_watch_enabled: row.try_get(20)?,
                    health_check_enabled: row.try_get(21)?,
                    health_check_urls: row.try_get(22)?,
                    timeout_seconds: row.try_get(23)?,
                    preflight_check_enabled: row.try_get(24)?,
                    generated_by_task_run_id: row.try_get(25)?,
                    enable_sweep: row.try_get(26)?,
                    max_sweep_iterations: row.try_get(27)?,
                    stages: row.try_get(28)?,
                    stop_on_failure: row.try_get(29)?,
                    reflection_mode: row.try_get(30)?,
                    model_overrides: row.try_get(31)?,
                    approval_gate: row.try_get(32)?,
                    completion_prompts_first: row.try_get(33)?,
                    is_favorite: row.try_get(34)?,
                    dependency_graph: row.try_get(35)?,
                    cost_annotations: row.try_get(36)?,
                    quality_report: row.try_get(37)?,
                    acceptance_criteria: row.try_get(38)?,
                    constraint_overrides: row.try_get(39)?,
                    ai_reviewed: row.try_get(40)?,
                    workflow_architecture: row.try_get(41)?,
                    rollback_policy: row.try_get(42)?,
                    strict_cwd: row.try_get(43)?,
                    tool_tags: row.try_get(44)?,
                    enforce_token_budget: row.try_get(45)?,
                    flow_control_json: row.try_get(46)?,
                    phase_timeouts_json: row.try_get(47)?,
                })
            },
            mapper: |it| SearchUnifiedWorkflows::from(it),
        }
    }
}
pub struct ToggleFavoriteStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn toggle_favorite() -> ToggleFavoriteStmt {
    ToggleFavoriteStmt(
        "UPDATE unified_workflows SET is_favorite = NOT is_favorite, updated_at = NOW() WHERE id = $1 RETURNING is_favorite",
        None,
    )
}
impl ToggleFavoriteStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>(
        &'s self,
        client: &'c C,
        id: &'a T1,
    ) -> BoolQuery<'c, 'a, 's, C, bool, 1> {
        BoolQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
pub struct GetPendingSyncWorkflowsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_pending_sync_workflows() -> GetPendingSyncWorkflowsStmt {
    GetPendingSyncWorkflowsStmt(
        "SELECT id, name, COALESCE(description, '') as description, COALESCE(category, 'general') as category, COALESCE(tags, '[]') as tags, COALESCE(setup_steps, '[]') as setup_steps, COALESCE(verification_steps, '[]') as verification_steps, COALESCE(agentic_steps, '[]') as agentic_steps, COALESCE(completion_steps, '[]') as completion_steps, COALESCE(max_iterations, 10) as max_iterations, COALESCE(provider, '') as provider, COALESCE(model, '') as model, skip_ai_summary, created_at, updated_at, COALESCE(log_source_selection, '\"default\"') as log_source_selection, COALESCE(context_ids, '[]') as context_ids, COALESCE(disabled_context_ids, '[]') as disabled_context_ids, COALESCE(auto_include_contexts, true) as auto_include_contexts, COALESCE(prompt_template, '') as prompt_template, COALESCE(log_watch_enabled, true) as log_watch_enabled, COALESCE(health_check_enabled, true) as health_check_enabled, COALESCE(health_check_urls, '[]') as health_check_urls, COALESCE(timeout_seconds, 0) as timeout_seconds, COALESCE(preflight_check_enabled, true) as preflight_check_enabled, COALESCE(generated_by_task_run_id, '') as generated_by_task_run_id, COALESCE(enable_sweep, false) as enable_sweep, COALESCE(max_sweep_iterations, 5) as max_sweep_iterations, COALESCE(stages, '[]') as stages, COALESCE(stop_on_failure, false) as stop_on_failure, COALESCE(reflection_mode, true) as reflection_mode, COALESCE(model_overrides, '{}') as model_overrides, COALESCE(approval_gate, false) as approval_gate, COALESCE(completion_prompts_first, false) as completion_prompts_first, COALESCE(is_favorite, false) as is_favorite, COALESCE(dependency_graph, '') as dependency_graph, COALESCE(cost_annotations, '') as cost_annotations, COALESCE(quality_report, '') as quality_report, COALESCE(acceptance_criteria, '') as acceptance_criteria, COALESCE(constraint_overrides, '{}') as constraint_overrides, COALESCE(ai_reviewed, true) as ai_reviewed, COALESCE(workflow_architecture, '') as workflow_architecture, COALESCE(rollback_policy, '') as rollback_policy, COALESCE(strict_cwd, false) as strict_cwd, COALESCE(tool_tags, '[]') as tool_tags, COALESCE(enforce_token_budget, false) as enforce_token_budget, COALESCE(flow_control_json, '') as flow_control_json, COALESCE(phase_timeouts_json, '') as phase_timeouts_json FROM unified_workflows WHERE sync_pending = true",
        None,
    )
}
impl GetPendingSyncWorkflowsStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient>(
        &'s self,
        client: &'c C,
    ) -> GetPendingSyncWorkflowsQuery<'c, 'a, 's, C, GetPendingSyncWorkflows, 0> {
        GetPendingSyncWorkflowsQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetPendingSyncWorkflowsBorrowed, tokio_postgres::Error> {
                Ok(GetPendingSyncWorkflowsBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    description: row.try_get(2)?,
                    category: row.try_get(3)?,
                    tags: row.try_get(4)?,
                    setup_steps: row.try_get(5)?,
                    verification_steps: row.try_get(6)?,
                    agentic_steps: row.try_get(7)?,
                    completion_steps: row.try_get(8)?,
                    max_iterations: row.try_get(9)?,
                    provider: row.try_get(10)?,
                    model: row.try_get(11)?,
                    skip_ai_summary: row.try_get(12)?,
                    created_at: row.try_get(13)?,
                    updated_at: row.try_get(14)?,
                    log_source_selection: row.try_get(15)?,
                    context_ids: row.try_get(16)?,
                    disabled_context_ids: row.try_get(17)?,
                    auto_include_contexts: row.try_get(18)?,
                    prompt_template: row.try_get(19)?,
                    log_watch_enabled: row.try_get(20)?,
                    health_check_enabled: row.try_get(21)?,
                    health_check_urls: row.try_get(22)?,
                    timeout_seconds: row.try_get(23)?,
                    preflight_check_enabled: row.try_get(24)?,
                    generated_by_task_run_id: row.try_get(25)?,
                    enable_sweep: row.try_get(26)?,
                    max_sweep_iterations: row.try_get(27)?,
                    stages: row.try_get(28)?,
                    stop_on_failure: row.try_get(29)?,
                    reflection_mode: row.try_get(30)?,
                    model_overrides: row.try_get(31)?,
                    approval_gate: row.try_get(32)?,
                    completion_prompts_first: row.try_get(33)?,
                    is_favorite: row.try_get(34)?,
                    dependency_graph: row.try_get(35)?,
                    cost_annotations: row.try_get(36)?,
                    quality_report: row.try_get(37)?,
                    acceptance_criteria: row.try_get(38)?,
                    constraint_overrides: row.try_get(39)?,
                    ai_reviewed: row.try_get(40)?,
                    workflow_architecture: row.try_get(41)?,
                    rollback_policy: row.try_get(42)?,
                    strict_cwd: row.try_get(43)?,
                    tool_tags: row.try_get(44)?,
                    enforce_token_budget: row.try_get(45)?,
                    flow_control_json: row.try_get(46)?,
                    phase_timeouts_json: row.try_get(47)?,
                })
            },
            mapper: |it| GetPendingSyncWorkflows::from(it),
        }
    }
}
pub struct ClearSyncPendingStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn clear_sync_pending() -> ClearSyncPendingStmt {
    ClearSyncPendingStmt(
        "UPDATE unified_workflows SET sync_pending = false WHERE id = $1",
        None,
    )
}
impl ClearSyncPendingStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub async fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>(
        &'s self,
        client: &'c C,
        id: &'a T1,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[id]).await
    }
}
pub struct SetSyncPendingStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn set_sync_pending() -> SetSyncPendingStmt {
    SetSyncPendingStmt(
        "UPDATE unified_workflows SET sync_pending = true, updated_at = NOW() WHERE id = $1",
        None,
    )
}
impl SetSyncPendingStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub async fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>(
        &'s self,
        client: &'c C,
        id: &'a T1,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[id]).await
    }
}
pub struct ListSlashCommandSourcesStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_slash_command_sources() -> ListSlashCommandSourcesStmt {
    ListSlashCommandSourcesStmt(
        "SELECT id, name, COALESCE(source_file_path, '') as source_file_path FROM unified_workflows WHERE source_file_path IS NOT NULL",
        None,
    )
}
impl ListSlashCommandSourcesStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient>(
        &'s self,
        client: &'c C,
    ) -> ListSlashCommandSourcesQuery<'c, 'a, 's, C, ListSlashCommandSources, 0> {
        ListSlashCommandSourcesQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ListSlashCommandSourcesBorrowed, tokio_postgres::Error> {
                Ok(ListSlashCommandSourcesBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    source_file_path: row.try_get(2)?,
                })
            },
            mapper: |it| ListSlashCommandSources::from(it),
        }
    }
}
pub struct UpsertSlashCommandWorkflowStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn upsert_slash_command_workflow() -> UpsertSlashCommandWorkflowStmt {
    UpsertSlashCommandWorkflowStmt(
        "INSERT INTO unified_workflows ( id, name, description, category, setup_steps, agentic_steps, max_iterations, source_file_path, source_content_hash ) VALUES ( $1, $2, $3, 'slash-command', '[]', $4, 1, $5, $6 ) ON CONFLICT(id) DO UPDATE SET name = EXCLUDED.name, description = EXCLUDED.description, agentic_steps = EXCLUDED.agentic_steps, source_content_hash = EXCLUDED.source_content_hash, updated_at = NOW() RETURNING id",
        None,
    )
}
impl UpsertSlashCommandWorkflowStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<
        'c,
        'a,
        's,
        C: GenericClient,
        T1: crate::StringSql,
        T2: crate::StringSql,
        T3: crate::StringSql,
        T4: crate::StringSql,
        T5: crate::StringSql,
        T6: crate::StringSql,
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        name: &'a T2,
        description: &'a Option<T3>,
        agentic_steps: &'a T4,
        source_file_path: &'a T5,
        source_content_hash: &'a Option<T6>,
    ) -> StringQuery<'c, 'a, 's, C, String, 6> {
        StringQuery {
            client,
            params: [
                id,
                name,
                description,
                agentic_steps,
                source_file_path,
                source_content_hash,
            ],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
impl<
    'c,
    'a,
    's,
    C: GenericClient,
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        UpsertSlashCommandWorkflowParams<T1, T2, T3, T4, T5, T6>,
        StringQuery<'c, 'a, 's, C, String, 6>,
        C,
    > for UpsertSlashCommandWorkflowStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpsertSlashCommandWorkflowParams<T1, T2, T3, T4, T5, T6>,
    ) -> StringQuery<'c, 'a, 's, C, String, 6> {
        self.bind(
            client,
            &params.id,
            &params.name,
            &params.description,
            &params.agentic_steps,
            &params.source_file_path,
            &params.source_content_hash,
        )
    }
}
pub struct GetWorkflowStatsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_workflow_stats() -> GetWorkflowStatsStmt {
    GetWorkflowStatsStmt(
        "SELECT COUNT(*)::bigint as total_runs, COUNT(*) FILTER (WHERE status = 'complete')::bigint as success_count, COUNT(*) FILTER (WHERE status = 'failed')::bigint as failure_count, COALESCE(MAX(created_at), NOW()) as last_run_at, COALESCE((SELECT status FROM task_runs WHERE workflow_name = $1 ORDER BY created_at DESC LIMIT 1), '') as last_run_status, COALESCE(AVG(CASE WHEN completed_at IS NOT NULL THEN EXTRACT(EPOCH FROM (completed_at - created_at)) * 1000 END)::double precision, 0) as avg_duration_ms FROM task_runs WHERE workflow_name = $1",
        None,
    )
}
impl GetWorkflowStatsStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>(
        &'s self,
        client: &'c C,
        workflow_name: &'a T1,
    ) -> GetWorkflowStatsQuery<'c, 'a, 's, C, GetWorkflowStats, 1> {
        GetWorkflowStatsQuery {
            client,
            params: [workflow_name],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetWorkflowStatsBorrowed, tokio_postgres::Error> {
                Ok(GetWorkflowStatsBorrowed {
                    total_runs: row.try_get(0)?,
                    success_count: row.try_get(1)?,
                    failure_count: row.try_get(2)?,
                    last_run_at: row.try_get(3)?,
                    last_run_status: row.try_get(4)?,
                    avg_duration_ms: row.try_get(5)?,
                })
            },
            mapper: |it| GetWorkflowStats::from(it),
        }
    }
}
pub struct UpdateSlashCommandContentStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_slash_command_content() -> UpdateSlashCommandContentStmt {
    UpdateSlashCommandContentStmt(
        "UPDATE unified_workflows SET agentic_steps = $1, source_content_hash = $2, updated_at = NOW() WHERE id = $3 RETURNING id",
        None,
    )
}
impl UpdateSlashCommandContentStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<
        'c,
        'a,
        's,
        C: GenericClient,
        T1: crate::StringSql,
        T2: crate::StringSql,
        T3: crate::StringSql,
    >(
        &'s self,
        client: &'c C,
        agentic_steps: &'a T1,
        source_content_hash: &'a Option<T2>,
        id: &'a T3,
    ) -> StringQuery<'c, 'a, 's, C, String, 3> {
        StringQuery {
            client,
            params: [agentic_steps, source_content_hash, id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql, T3: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        UpdateSlashCommandContentParams<T1, T2, T3>,
        StringQuery<'c, 'a, 's, C, String, 3>,
        C,
    > for UpdateSlashCommandContentStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateSlashCommandContentParams<T1, T2, T3>,
    ) -> StringQuery<'c, 'a, 's, C, String, 3> {
        self.bind(
            client,
            &params.agentic_steps,
            &params.source_content_hash,
            &params.id,
        )
    }
}
