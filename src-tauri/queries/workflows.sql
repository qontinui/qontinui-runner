--- Unified workflow CRUD operations.

--! list_unified_workflows
SELECT id, name, description, category, tags, setup_steps, verification_steps,
       agentic_steps, completion_steps, max_iterations, provider, model,
       skip_ai_summary, created_at, updated_at, log_source_selection,
       context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
       log_watch_enabled, health_check_enabled, health_check_urls, timeout_seconds,
       preflight_check_enabled, generated_by_task_run_id, enable_sweep, max_sweep_iterations,
       stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
       completion_prompts_first, is_favorite, dependency_graph, cost_annotations,
       quality_report, acceptance_criteria, constraint_overrides,
       COALESCE(ai_reviewed, true) as ai_reviewed, workflow_architecture, rollback_policy,
       strict_cwd, tool_tags, enforce_token_budget,
       flow_control_json, phase_timeouts_json
FROM unified_workflows
ORDER BY is_favorite DESC, updated_at DESC;

--! get_unified_workflow
SELECT id, name, description, category, tags, setup_steps, verification_steps,
       agentic_steps, completion_steps, max_iterations, provider, model,
       skip_ai_summary, created_at, updated_at, log_source_selection,
       context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
       log_watch_enabled, health_check_enabled, health_check_urls, timeout_seconds,
       preflight_check_enabled, generated_by_task_run_id, enable_sweep, max_sweep_iterations,
       stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
       completion_prompts_first, is_favorite, dependency_graph, cost_annotations,
       quality_report, acceptance_criteria, constraint_overrides,
       COALESCE(ai_reviewed, true) as ai_reviewed, workflow_architecture, rollback_policy,
       strict_cwd, tool_tags, enforce_token_budget,
       flow_control_json, phase_timeouts_json
FROM unified_workflows
WHERE id = :id;

--! get_unified_workflow_by_name
SELECT id, name, description, category, tags, setup_steps, verification_steps,
       agentic_steps, completion_steps, max_iterations, provider, model,
       skip_ai_summary, created_at, updated_at, log_source_selection,
       context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
       log_watch_enabled, health_check_enabled, health_check_urls, timeout_seconds,
       preflight_check_enabled, generated_by_task_run_id, enable_sweep, max_sweep_iterations,
       stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
       completion_prompts_first, is_favorite, dependency_graph, cost_annotations,
       quality_report, acceptance_criteria, constraint_overrides,
       COALESCE(ai_reviewed, true) as ai_reviewed, workflow_architecture, rollback_policy,
       strict_cwd, tool_tags, enforce_token_budget,
       flow_control_json, phase_timeouts_json
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
SELECT id, name, description, category, tags, setup_steps, verification_steps,
       agentic_steps, completion_steps, max_iterations, provider, model,
       skip_ai_summary, created_at, updated_at, log_source_selection,
       context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
       log_watch_enabled, health_check_enabled, health_check_urls, timeout_seconds,
       preflight_check_enabled, generated_by_task_run_id, enable_sweep, max_sweep_iterations,
       stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
       completion_prompts_first, is_favorite, dependency_graph, cost_annotations,
       quality_report, acceptance_criteria, constraint_overrides,
       COALESCE(ai_reviewed, true) as ai_reviewed, workflow_architecture, rollback_policy,
       strict_cwd, tool_tags, enforce_token_budget,
       flow_control_json, phase_timeouts_json
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
SELECT id, name, description, category, tags, setup_steps, verification_steps,
       agentic_steps, completion_steps, max_iterations, provider, model,
       skip_ai_summary, created_at, updated_at, log_source_selection,
       context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
       log_watch_enabled, health_check_enabled, health_check_urls, timeout_seconds,
       preflight_check_enabled, generated_by_task_run_id, enable_sweep, max_sweep_iterations,
       stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
       completion_prompts_first, is_favorite, dependency_graph, cost_annotations,
       quality_report, acceptance_criteria, constraint_overrides,
       COALESCE(ai_reviewed, true) as ai_reviewed, workflow_architecture, rollback_policy,
       strict_cwd, tool_tags, enforce_token_budget,
       flow_control_json, phase_timeouts_json
FROM unified_workflows
WHERE sync_pending = true;

--! clear_sync_pending
UPDATE unified_workflows SET sync_pending = false WHERE id = :id;

--! set_sync_pending
UPDATE unified_workflows SET sync_pending = true, updated_at = NOW() WHERE id = :id;

--! list_slash_command_sources
SELECT id, name, source_file_path FROM unified_workflows
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

--! update_slash_command_content (source_content_hash?)
UPDATE unified_workflows SET
    agentic_steps = :agentic_steps,
    source_content_hash = :source_content_hash,
    updated_at = NOW()
WHERE id = :id
RETURNING id;
