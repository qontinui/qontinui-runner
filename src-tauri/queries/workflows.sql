--- Unified workflow CRUD operations.

--! list_unified_workflows
SELECT id, name, COALESCE(description, '') as description, COALESCE(category, 'general') as category,
       COALESCE(tags, '[]') as tags, COALESCE(setup_steps, '[]') as setup_steps,
       COALESCE(verification_steps, '[]') as verification_steps,
       COALESCE(agentic_steps, '[]') as agentic_steps, COALESCE(completion_steps, '[]') as completion_steps,
       COALESCE(max_iterations, 10) as max_iterations, COALESCE(provider, '') as provider, COALESCE(model, '') as model,
       skip_ai_summary, created_at, updated_at, COALESCE(log_source_selection, '"default"') as log_source_selection,
       COALESCE(context_ids, '[]') as context_ids, COALESCE(disabled_context_ids, '[]') as disabled_context_ids,
       COALESCE(auto_include_contexts, true) as auto_include_contexts, COALESCE(prompt_template, '') as prompt_template,
       COALESCE(log_watch_enabled, true) as log_watch_enabled, COALESCE(health_check_enabled, true) as health_check_enabled,
       COALESCE(health_check_urls, '[]') as health_check_urls, COALESCE(timeout_seconds, 0) as timeout_seconds,
       COALESCE(preflight_check_enabled, true) as preflight_check_enabled, COALESCE(generated_by_task_run_id, '') as generated_by_task_run_id,
       COALESCE(enable_sweep, false) as enable_sweep, COALESCE(max_sweep_iterations, 5) as max_sweep_iterations,
       COALESCE(stages, '[]') as stages, COALESCE(stop_on_failure, false) as stop_on_failure,
       COALESCE(reflection_mode, true) as reflection_mode, COALESCE(model_overrides, '{}') as model_overrides,
       COALESCE(approval_gate, false) as approval_gate,
       COALESCE(completion_prompts_first, false) as completion_prompts_first,
       COALESCE(is_favorite, false) as is_favorite,
       COALESCE(dependency_graph, '') as dependency_graph, COALESCE(cost_annotations, '') as cost_annotations,
       COALESCE(quality_report, '') as quality_report, COALESCE(acceptance_criteria, '') as acceptance_criteria,
       COALESCE(constraint_overrides, '{}') as constraint_overrides,
       COALESCE(ai_reviewed, true) as ai_reviewed, COALESCE(workflow_architecture, '') as workflow_architecture,
       COALESCE(rollback_policy, '') as rollback_policy,
       COALESCE(strict_cwd, false) as strict_cwd, COALESCE(tool_tags, '[]') as tool_tags,
       COALESCE(enforce_token_budget, false) as enforce_token_budget,
       COALESCE(flow_control_json, '') as flow_control_json, COALESCE(phase_timeouts_json, '') as phase_timeouts_json
FROM unified_workflows
ORDER BY is_favorite DESC, updated_at DESC;

--! get_unified_workflow
SELECT id, name, COALESCE(description, '') as description, COALESCE(category, 'general') as category,
       COALESCE(tags, '[]') as tags, COALESCE(setup_steps, '[]') as setup_steps,
       COALESCE(verification_steps, '[]') as verification_steps,
       COALESCE(agentic_steps, '[]') as agentic_steps, COALESCE(completion_steps, '[]') as completion_steps,
       COALESCE(max_iterations, 10) as max_iterations, COALESCE(provider, '') as provider, COALESCE(model, '') as model,
       skip_ai_summary, created_at, updated_at, COALESCE(log_source_selection, '"default"') as log_source_selection,
       COALESCE(context_ids, '[]') as context_ids, COALESCE(disabled_context_ids, '[]') as disabled_context_ids,
       COALESCE(auto_include_contexts, true) as auto_include_contexts, COALESCE(prompt_template, '') as prompt_template,
       COALESCE(log_watch_enabled, true) as log_watch_enabled, COALESCE(health_check_enabled, true) as health_check_enabled,
       COALESCE(health_check_urls, '[]') as health_check_urls, COALESCE(timeout_seconds, 0) as timeout_seconds,
       COALESCE(preflight_check_enabled, true) as preflight_check_enabled, COALESCE(generated_by_task_run_id, '') as generated_by_task_run_id,
       COALESCE(enable_sweep, false) as enable_sweep, COALESCE(max_sweep_iterations, 5) as max_sweep_iterations,
       COALESCE(stages, '[]') as stages, COALESCE(stop_on_failure, false) as stop_on_failure,
       COALESCE(reflection_mode, true) as reflection_mode, COALESCE(model_overrides, '{}') as model_overrides,
       COALESCE(approval_gate, false) as approval_gate,
       COALESCE(completion_prompts_first, false) as completion_prompts_first,
       COALESCE(is_favorite, false) as is_favorite,
       COALESCE(dependency_graph, '') as dependency_graph, COALESCE(cost_annotations, '') as cost_annotations,
       COALESCE(quality_report, '') as quality_report, COALESCE(acceptance_criteria, '') as acceptance_criteria,
       COALESCE(constraint_overrides, '{}') as constraint_overrides,
       COALESCE(ai_reviewed, true) as ai_reviewed, COALESCE(workflow_architecture, '') as workflow_architecture,
       COALESCE(rollback_policy, '') as rollback_policy,
       COALESCE(strict_cwd, false) as strict_cwd, COALESCE(tool_tags, '[]') as tool_tags,
       COALESCE(enforce_token_budget, false) as enforce_token_budget,
       COALESCE(flow_control_json, '') as flow_control_json, COALESCE(phase_timeouts_json, '') as phase_timeouts_json
FROM unified_workflows
WHERE id = :id;

--! get_unified_workflow_by_name
SELECT id, name, COALESCE(description, '') as description, COALESCE(category, 'general') as category,
       COALESCE(tags, '[]') as tags, COALESCE(setup_steps, '[]') as setup_steps,
       COALESCE(verification_steps, '[]') as verification_steps,
       COALESCE(agentic_steps, '[]') as agentic_steps, COALESCE(completion_steps, '[]') as completion_steps,
       COALESCE(max_iterations, 10) as max_iterations, COALESCE(provider, '') as provider, COALESCE(model, '') as model,
       skip_ai_summary, created_at, updated_at, COALESCE(log_source_selection, '"default"') as log_source_selection,
       COALESCE(context_ids, '[]') as context_ids, COALESCE(disabled_context_ids, '[]') as disabled_context_ids,
       COALESCE(auto_include_contexts, true) as auto_include_contexts, COALESCE(prompt_template, '') as prompt_template,
       COALESCE(log_watch_enabled, true) as log_watch_enabled, COALESCE(health_check_enabled, true) as health_check_enabled,
       COALESCE(health_check_urls, '[]') as health_check_urls, COALESCE(timeout_seconds, 0) as timeout_seconds,
       COALESCE(preflight_check_enabled, true) as preflight_check_enabled, COALESCE(generated_by_task_run_id, '') as generated_by_task_run_id,
       COALESCE(enable_sweep, false) as enable_sweep, COALESCE(max_sweep_iterations, 5) as max_sweep_iterations,
       COALESCE(stages, '[]') as stages, COALESCE(stop_on_failure, false) as stop_on_failure,
       COALESCE(reflection_mode, true) as reflection_mode, COALESCE(model_overrides, '{}') as model_overrides,
       COALESCE(approval_gate, false) as approval_gate,
       COALESCE(completion_prompts_first, false) as completion_prompts_first,
       COALESCE(is_favorite, false) as is_favorite,
       COALESCE(dependency_graph, '') as dependency_graph, COALESCE(cost_annotations, '') as cost_annotations,
       COALESCE(quality_report, '') as quality_report, COALESCE(acceptance_criteria, '') as acceptance_criteria,
       COALESCE(constraint_overrides, '{}') as constraint_overrides,
       COALESCE(ai_reviewed, true) as ai_reviewed, COALESCE(workflow_architecture, '') as workflow_architecture,
       COALESCE(rollback_policy, '') as rollback_policy,
       COALESCE(strict_cwd, false) as strict_cwd, COALESCE(tool_tags, '[]') as tool_tags,
       COALESCE(enforce_token_budget, false) as enforce_token_budget,
       COALESCE(flow_control_json, '') as flow_control_json, COALESCE(phase_timeouts_json, '') as phase_timeouts_json
FROM unified_workflows
WHERE name = :name
LIMIT 1;

--! create_unified_workflow (description?, provider?, model?, timeout_seconds?, prompt_template?, generated_by_task_run_id?, dependency_graph?, cost_annotations?, quality_report?, acceptance_criteria?, workflow_architecture?, rollback_policy?, flow_control_json?, phase_timeouts_json?)
INSERT INTO unified_workflows (
    id, name, description, category, tags, setup_steps, verification_steps,
    agentic_steps, completion_steps, max_iterations, timeout_seconds, provider, model,
    skip_ai_summary, log_source_selection,
    context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
    log_watch_enabled, health_check_enabled, health_check_urls, preflight_check_enabled,
    generated_by_task_run_id, enable_sweep, max_sweep_iterations,
    stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
    completion_prompts_first, dependency_graph, cost_annotations, quality_report,
    acceptance_criteria, constraint_overrides, ai_reviewed, workflow_architecture,
    strict_cwd, tool_tags, rollback_policy, enforce_token_budget,
    flow_control_json, phase_timeouts_json
) VALUES (
    :id, :name, :description, :category, :tags, :setup_steps, :verification_steps,
    :agentic_steps, :completion_steps, :max_iterations, :timeout_seconds, :provider, :model,
    :skip_ai_summary, :log_source_selection,
    :context_ids, :disabled_context_ids, :auto_include_contexts, :prompt_template,
    :log_watch_enabled, :health_check_enabled, :health_check_urls, :preflight_check_enabled,
    :generated_by_task_run_id, :enable_sweep, :max_sweep_iterations,
    :stages, :stop_on_failure, :reflection_mode, :model_overrides, :approval_gate,
    :completion_prompts_first, :dependency_graph, :cost_annotations, :quality_report,
    :acceptance_criteria, :constraint_overrides, :ai_reviewed, :workflow_architecture,
    :strict_cwd, :tool_tags, :rollback_policy, :enforce_token_budget,
    :flow_control_json, :phase_timeouts_json
)
RETURNING id;

--! update_unified_workflow (description?, provider?, model?, timeout_seconds?, prompt_template?, dependency_graph?, cost_annotations?, quality_report?, acceptance_criteria?, workflow_architecture?, rollback_policy?, flow_control_json?, phase_timeouts_json?)
UPDATE unified_workflows SET
    name = :name,
    description = :description,
    category = :category,
    tags = :tags,
    setup_steps = :setup_steps,
    verification_steps = :verification_steps,
    agentic_steps = :agentic_steps,
    completion_steps = :completion_steps,
    max_iterations = :max_iterations,
    timeout_seconds = :timeout_seconds,
    provider = :provider,
    model = :model,
    skip_ai_summary = :skip_ai_summary,
    updated_at = NOW(),
    log_source_selection = :log_source_selection,
    prompt_template = :prompt_template,
    context_ids = :context_ids,
    disabled_context_ids = :disabled_context_ids,
    auto_include_contexts = :auto_include_contexts,
    log_watch_enabled = :log_watch_enabled,
    health_check_enabled = :health_check_enabled,
    health_check_urls = :health_check_urls,
    preflight_check_enabled = :preflight_check_enabled,
    enable_sweep = :enable_sweep,
    max_sweep_iterations = :max_sweep_iterations,
    stages = :stages,
    stop_on_failure = :stop_on_failure,
    approval_gate = :approval_gate,
    reflection_mode = :reflection_mode,
    model_overrides = :model_overrides,
    completion_prompts_first = :completion_prompts_first,
    dependency_graph = :dependency_graph,
    cost_annotations = :cost_annotations,
    quality_report = :quality_report,
    acceptance_criteria = :acceptance_criteria,
    constraint_overrides = :constraint_overrides,
    ai_reviewed = :ai_reviewed,
    workflow_architecture = :workflow_architecture,
    strict_cwd = :strict_cwd,
    tool_tags = :tool_tags,
    rollback_policy = :rollback_policy,
    enforce_token_budget = :enforce_token_budget,
    flow_control_json = :flow_control_json,
    phase_timeouts_json = :phase_timeouts_json
WHERE id = :id
RETURNING id;

--! delete_unified_workflow
DELETE FROM unified_workflows WHERE id = :id RETURNING id;

--! search_unified_workflows
SELECT id, name, COALESCE(description, '') as description, COALESCE(category, 'general') as category,
       COALESCE(tags, '[]') as tags, COALESCE(setup_steps, '[]') as setup_steps,
       COALESCE(verification_steps, '[]') as verification_steps,
       COALESCE(agentic_steps, '[]') as agentic_steps, COALESCE(completion_steps, '[]') as completion_steps,
       COALESCE(max_iterations, 10) as max_iterations, COALESCE(provider, '') as provider, COALESCE(model, '') as model,
       skip_ai_summary, created_at, updated_at, COALESCE(log_source_selection, '"default"') as log_source_selection,
       COALESCE(context_ids, '[]') as context_ids, COALESCE(disabled_context_ids, '[]') as disabled_context_ids,
       COALESCE(auto_include_contexts, true) as auto_include_contexts, COALESCE(prompt_template, '') as prompt_template,
       COALESCE(log_watch_enabled, true) as log_watch_enabled, COALESCE(health_check_enabled, true) as health_check_enabled,
       COALESCE(health_check_urls, '[]') as health_check_urls, COALESCE(timeout_seconds, 0) as timeout_seconds,
       COALESCE(preflight_check_enabled, true) as preflight_check_enabled, COALESCE(generated_by_task_run_id, '') as generated_by_task_run_id,
       COALESCE(enable_sweep, false) as enable_sweep, COALESCE(max_sweep_iterations, 5) as max_sweep_iterations,
       COALESCE(stages, '[]') as stages, COALESCE(stop_on_failure, false) as stop_on_failure,
       COALESCE(reflection_mode, true) as reflection_mode, COALESCE(model_overrides, '{}') as model_overrides,
       COALESCE(approval_gate, false) as approval_gate,
       COALESCE(completion_prompts_first, false) as completion_prompts_first,
       COALESCE(is_favorite, false) as is_favorite,
       COALESCE(dependency_graph, '') as dependency_graph, COALESCE(cost_annotations, '') as cost_annotations,
       COALESCE(quality_report, '') as quality_report, COALESCE(acceptance_criteria, '') as acceptance_criteria,
       COALESCE(constraint_overrides, '{}') as constraint_overrides,
       COALESCE(ai_reviewed, true) as ai_reviewed, COALESCE(workflow_architecture, '') as workflow_architecture,
       COALESCE(rollback_policy, '') as rollback_policy,
       COALESCE(strict_cwd, false) as strict_cwd, COALESCE(tool_tags, '[]') as tool_tags,
       COALESCE(enforce_token_budget, false) as enforce_token_budget,
       COALESCE(flow_control_json, '') as flow_control_json, COALESCE(phase_timeouts_json, '') as phase_timeouts_json
FROM unified_workflows
WHERE to_tsvector('english', name || ' ' || COALESCE(description, '')) @@ plainto_tsquery('english', :query)
   OR name ILIKE '%' || :query || '%'
   OR category ILIKE '%' || :query || '%'
ORDER BY is_favorite DESC, updated_at DESC;

--! toggle_favorite
UPDATE unified_workflows
SET is_favorite = NOT is_favorite, updated_at = NOW()
WHERE id = :id
RETURNING is_favorite;

--! get_pending_sync_workflows
SELECT id, name, COALESCE(description, '') as description, COALESCE(category, 'general') as category,
       COALESCE(tags, '[]') as tags, COALESCE(setup_steps, '[]') as setup_steps,
       COALESCE(verification_steps, '[]') as verification_steps,
       COALESCE(agentic_steps, '[]') as agentic_steps, COALESCE(completion_steps, '[]') as completion_steps,
       COALESCE(max_iterations, 10) as max_iterations, COALESCE(provider, '') as provider, COALESCE(model, '') as model,
       skip_ai_summary, created_at, updated_at, COALESCE(log_source_selection, '"default"') as log_source_selection,
       COALESCE(context_ids, '[]') as context_ids, COALESCE(disabled_context_ids, '[]') as disabled_context_ids,
       COALESCE(auto_include_contexts, true) as auto_include_contexts, COALESCE(prompt_template, '') as prompt_template,
       COALESCE(log_watch_enabled, true) as log_watch_enabled, COALESCE(health_check_enabled, true) as health_check_enabled,
       COALESCE(health_check_urls, '[]') as health_check_urls, COALESCE(timeout_seconds, 0) as timeout_seconds,
       COALESCE(preflight_check_enabled, true) as preflight_check_enabled, COALESCE(generated_by_task_run_id, '') as generated_by_task_run_id,
       COALESCE(enable_sweep, false) as enable_sweep, COALESCE(max_sweep_iterations, 5) as max_sweep_iterations,
       COALESCE(stages, '[]') as stages, COALESCE(stop_on_failure, false) as stop_on_failure,
       COALESCE(reflection_mode, true) as reflection_mode, COALESCE(model_overrides, '{}') as model_overrides,
       COALESCE(approval_gate, false) as approval_gate,
       COALESCE(completion_prompts_first, false) as completion_prompts_first,
       COALESCE(is_favorite, false) as is_favorite,
       COALESCE(dependency_graph, '') as dependency_graph, COALESCE(cost_annotations, '') as cost_annotations,
       COALESCE(quality_report, '') as quality_report, COALESCE(acceptance_criteria, '') as acceptance_criteria,
       COALESCE(constraint_overrides, '{}') as constraint_overrides,
       COALESCE(ai_reviewed, true) as ai_reviewed, COALESCE(workflow_architecture, '') as workflow_architecture,
       COALESCE(rollback_policy, '') as rollback_policy,
       COALESCE(strict_cwd, false) as strict_cwd, COALESCE(tool_tags, '[]') as tool_tags,
       COALESCE(enforce_token_budget, false) as enforce_token_budget,
       COALESCE(flow_control_json, '') as flow_control_json, COALESCE(phase_timeouts_json, '') as phase_timeouts_json
FROM unified_workflows
WHERE sync_pending = true;

--! clear_sync_pending
UPDATE unified_workflows SET sync_pending = false WHERE id = :id;

--! set_sync_pending
UPDATE unified_workflows SET sync_pending = true, updated_at = NOW() WHERE id = :id;

--! list_slash_command_sources
SELECT id, name, COALESCE(source_file_path, '') as source_file_path FROM unified_workflows
WHERE source_file_path IS NOT NULL;

--! upsert_slash_command_workflow (description?, source_content_hash?)
INSERT INTO unified_workflows (
    id, name, description, category, setup_steps, agentic_steps, max_iterations,
    source_file_path, source_content_hash
) VALUES (
    :id, :name, :description, 'slash-command', '[]', :agentic_steps, 1,
    :source_file_path, :source_content_hash
)
ON CONFLICT(id) DO UPDATE SET
    name = EXCLUDED.name,
    description = EXCLUDED.description,
    agentic_steps = EXCLUDED.agentic_steps,
    source_content_hash = EXCLUDED.source_content_hash,
    updated_at = NOW()
RETURNING id;

--! get_workflow_stats
SELECT
    COUNT(*)::bigint as total_runs,
    COUNT(*) FILTER (WHERE status = 'complete')::bigint as success_count,
    COUNT(*) FILTER (WHERE status = 'failed')::bigint as failure_count,
    COALESCE(MAX(created_at), NOW()) as last_run_at,
    COALESCE((SELECT status FROM task_runs WHERE workflow_name = :workflow_name
     ORDER BY created_at DESC LIMIT 1), '') as last_run_status,
    COALESCE(AVG(CASE WHEN completed_at IS NOT NULL
        THEN EXTRACT(EPOCH FROM (completed_at - created_at)) * 1000
        END)::double precision, 0) as avg_duration_ms
FROM task_runs
WHERE workflow_name = :workflow_name;

--! update_slash_command_content (source_content_hash?)
UPDATE unified_workflows SET
    agentic_steps = :agentic_steps,
    source_content_hash = :source_content_hash,
    updated_at = NOW()
WHERE id = :id
RETURNING id;
