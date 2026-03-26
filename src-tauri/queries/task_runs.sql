--- Task run core CRUD operations.

--! create_task_run (prompt?, task_type?, max_sessions?, execution_steps_json?, log_sources_json?, config_id?, workflow_name?, workflow_id?, workflow_type?, parent_task_run_id?, root_task_run_id?, workspace_id?, triggered_by?, bridge_id?, is_reflection?, reflection_source_task_run_id?, is_follow_up?, follow_up_source_task_run_id?, is_fixer?, fixer_source_task_run_id?, runner_port?)
INSERT INTO task_runs (
    id, task_name, prompt, task_type, status, sessions_count, max_sessions,
    auto_continue, output_log, execution_steps_json, log_sources_json,
    config_id, workflow_name, workflow_id, workflow_type,
    parent_task_run_id, root_task_run_id, depth,
    workspace_id, triggered_by, bridge_id,
    is_reflection, reflection_source_task_run_id,
    is_follow_up, follow_up_source_task_run_id,
    is_fixer, fixer_source_task_run_id,
    is_meta_optimizer, runner_port
) VALUES (
    :id, :task_name, :prompt, :task_type, 'running', 0, :max_sessions,
    :auto_continue, '', :execution_steps_json, :log_sources_json,
    :config_id, :workflow_name, :workflow_id, :workflow_type,
    :parent_task_run_id, :root_task_run_id, :depth,
    :workspace_id, :triggered_by, :bridge_id,
    :is_reflection, :reflection_source_task_run_id,
    :is_follow_up, :follow_up_source_task_run_id,
    :is_fixer, :fixer_source_task_run_id,
    :is_meta_optimizer, :runner_port
)
RETURNING id;

--! get_task_run
SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions,
       error_message, auto_continue,
       execution_steps_json, log_sources_json, config_id, workflow_name, workflow_id,
       COALESCE(summary, ai_summary) as summary, ai_summary, goal_achieved,
       remaining_work, summary_generated_at, transition_history_json, workflow_type,
       workspace_id, triggered_by,
       parent_task_run_id, root_task_run_id, depth, bridge_id, result_data,
       COALESCE(is_reflection, false) as is_reflection, reflection_source_task_run_id,
       COALESCE(is_follow_up, false) as is_follow_up, follow_up_source_task_run_id,
       COALESCE(is_fixer, false) as is_fixer, fixer_source_task_run_id,
       COALESCE(is_meta_optimizer, false) as is_meta_optimizer,
       created_at, updated_at, completed_at
FROM task_runs
WHERE id = :id;

--! get_recent_task_runs (runner_port?)
SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions,
       error_message, auto_continue,
       config_id, workflow_name, workflow_id,
       COALESCE(summary, ai_summary) as summary, ai_summary, goal_achieved,
       remaining_work, summary_generated_at,
       workspace_id, triggered_by,
       created_at, updated_at, completed_at
FROM task_runs
WHERE (workflow_type IS NULL OR workflow_type != 'chat')
  AND (:runner_port::integer IS NULL OR runner_port IS NULL OR runner_port = :runner_port)
ORDER BY updated_at DESC
LIMIT :max_results;

--! update_task_run_status
UPDATE task_runs SET status = :status, updated_at = NOW()
WHERE id = :id
RETURNING id;

--! complete_task_run
UPDATE task_runs SET status = 'complete', updated_at = NOW(), completed_at = NOW()
WHERE id = :id
RETURNING id;

--! fail_task_run
UPDATE task_runs SET status = 'failed', error_message = :error_message,
       updated_at = NOW(), completed_at = NOW()
WHERE id = :id
RETURNING id;

--! stop_task_run
UPDATE task_runs SET status = 'stopped', error_message = :reason,
       updated_at = NOW(), completed_at = NOW()
WHERE id = :id
RETURNING id;

--! delete_task_run
DELETE FROM task_runs WHERE id = :id RETURNING id;

--! update_task_summary (summary?, goal_achieved?, remaining_work?)
UPDATE task_runs SET
    summary = :summary,
    ai_summary = :summary,
    goal_achieved = :goal_achieved,
    remaining_work = :remaining_work,
    summary_generated_at = :summary_generated_at,
    updated_at = NOW()
WHERE id = :id
RETURNING id;

--! get_running_task_runs (runner_port?)
SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions,
       error_message, auto_continue,
       config_id, workflow_name, workflow_id, workflow_type,
       workspace_id, triggered_by,
       parent_task_run_id, root_task_run_id, depth, bridge_id,
       COALESCE(is_reflection, false) as is_reflection, reflection_source_task_run_id,
       COALESCE(is_follow_up, false) as is_follow_up, follow_up_source_task_run_id,
       COALESCE(is_fixer, false) as is_fixer, fixer_source_task_run_id,
       COALESCE(is_meta_optimizer, false) as is_meta_optimizer,
       runner_port,
       created_at, updated_at, completed_at
FROM task_runs
WHERE status = 'running'
  AND (:runner_port::integer IS NULL OR runner_port IS NULL OR runner_port = :runner_port)
ORDER BY created_at DESC;

--! append_task_output
UPDATE task_runs SET output_log = output_log || :output, updated_at = NOW()
WHERE id = :id;

--! update_task_name
UPDATE task_runs SET task_name = :task_name, updated_at = NOW()
WHERE id = :id
RETURNING id;

--! increment_sessions_count
UPDATE task_runs SET sessions_count = sessions_count + 1, updated_at = NOW()
WHERE id = :id;

--! update_task_result_data
UPDATE task_runs SET result_data = :result_data, updated_at = NOW()
WHERE id = :id
RETURNING id;

--! get_recent_task_runs_filtered (runner_port?, workflow_type?)
SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions,
       error_message, auto_continue,
       config_id, workflow_name, workflow_id,
       COALESCE(summary, ai_summary) as summary, ai_summary, goal_achieved,
       remaining_work, summary_generated_at,
       workspace_id, triggered_by,
       created_at, updated_at, completed_at
FROM task_runs
WHERE (:workflow_type::text IS NULL OR workflow_type = :workflow_type)
  AND (workflow_type IS NULL OR workflow_type != 'chat')
  AND (:runner_port::integer IS NULL OR runner_port IS NULL OR runner_port = :runner_port)
ORDER BY updated_at DESC
LIMIT :max_results;

--! get_task_output
SELECT output_log FROM task_runs WHERE id = :id;
