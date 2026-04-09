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
SELECT id, task_name, COALESCE(prompt, '') as prompt, COALESCE(task_type, 'task') as task_type,
       COALESCE(status, 'running') as status, COALESCE(sessions_count, 0) as sessions_count,
       COALESCE(max_sessions, 0) as max_sessions,
       COALESCE(error_message, '') as error_message, COALESCE(auto_continue, true) as auto_continue,
       COALESCE(execution_steps_json, '') as execution_steps_json, COALESCE(log_sources_json, '') as log_sources_json,
       COALESCE(config_id, '') as config_id, COALESCE(workflow_name, '') as workflow_name, COALESCE(workflow_id, '') as workflow_id,
       COALESCE(summary, ai_summary, '') as summary, COALESCE(ai_summary, '') as ai_summary,
       COALESCE(goal_achieved, false) as goal_achieved,
       COALESCE(remaining_work, '') as remaining_work, COALESCE(summary_generated_at::TEXT, '') as summary_generated_at,
       COALESCE(transition_history_json, '') as transition_history_json,
       COALESCE(workflow_type, 'task') as workflow_type,
       COALESCE(workspace_id, '') as workspace_id, COALESCE(triggered_by, '') as triggered_by,
       COALESCE(parent_task_run_id, '') as parent_task_run_id, COALESCE(root_task_run_id, '') as root_task_run_id,
       COALESCE(depth, 0) as depth, COALESCE(bridge_id, '') as bridge_id, COALESCE(result_data, '') as result_data,
       COALESCE(is_reflection, false) as is_reflection, COALESCE(reflection_source_task_run_id, '') as reflection_source_task_run_id,
       COALESCE(is_follow_up, false) as is_follow_up, COALESCE(follow_up_source_task_run_id, '') as follow_up_source_task_run_id,
       COALESCE(is_fixer, false) as is_fixer, COALESCE(fixer_source_task_run_id, '') as fixer_source_task_run_id,
       COALESCE(is_meta_optimizer, false) as is_meta_optimizer,
       COALESCE(is_review, false) as is_review, COALESCE(blocks_parent, false) as blocks_parent,
       COALESCE(created_at, NOW()) as created_at, COALESCE(updated_at, NOW()) as updated_at, COALESCE(completed_at, NOW()) as completed_at
FROM task_runs
WHERE id = :id;

--! get_recent_task_runs (runner_port?)
SELECT id, task_name, COALESCE(prompt, '') as prompt, COALESCE(task_type, 'task') as task_type,
       COALESCE(status, 'running') as status, COALESCE(sessions_count, 0) as sessions_count,
       COALESCE(max_sessions, 0) as max_sessions,
       COALESCE(error_message, '') as error_message, COALESCE(auto_continue, true) as auto_continue,
       COALESCE(config_id, '') as config_id, COALESCE(workflow_name, '') as workflow_name, COALESCE(workflow_id, '') as workflow_id,
       COALESCE(summary, ai_summary, '') as summary, COALESCE(ai_summary, '') as ai_summary,
       COALESCE(goal_achieved, false) as goal_achieved,
       COALESCE(remaining_work, '') as remaining_work, COALESCE(summary_generated_at::TEXT, '') as summary_generated_at,
       COALESCE(workspace_id, '') as workspace_id, COALESCE(triggered_by, '') as triggered_by,
       COALESCE(created_at, NOW()) as created_at, COALESCE(updated_at, NOW()) as updated_at, COALESCE(completed_at, NOW()) as completed_at
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
SELECT id, task_name, COALESCE(prompt, '') as prompt, COALESCE(task_type, 'task') as task_type,
       COALESCE(status, 'running') as status, COALESCE(sessions_count, 0) as sessions_count,
       COALESCE(max_sessions, 0) as max_sessions,
       COALESCE(error_message, '') as error_message, COALESCE(auto_continue, true) as auto_continue,
       COALESCE(config_id, '') as config_id, COALESCE(workflow_name, '') as workflow_name, COALESCE(workflow_id, '') as workflow_id,
       COALESCE(workflow_type, 'task') as workflow_type,
       COALESCE(workspace_id, '') as workspace_id, COALESCE(triggered_by, '') as triggered_by,
       COALESCE(parent_task_run_id, '') as parent_task_run_id, COALESCE(root_task_run_id, '') as root_task_run_id,
       COALESCE(depth, 0) as depth, COALESCE(bridge_id, '') as bridge_id,
       COALESCE(is_reflection, false) as is_reflection, COALESCE(reflection_source_task_run_id, '') as reflection_source_task_run_id,
       COALESCE(is_follow_up, false) as is_follow_up, COALESCE(follow_up_source_task_run_id, '') as follow_up_source_task_run_id,
       COALESCE(is_fixer, false) as is_fixer, COALESCE(fixer_source_task_run_id, '') as fixer_source_task_run_id,
       COALESCE(is_meta_optimizer, false) as is_meta_optimizer,
       COALESCE(is_review, false) as is_review, COALESCE(blocks_parent, false) as blocks_parent,
       COALESCE(runner_port, 0) as runner_port,
       COALESCE(created_at, NOW()) as created_at, COALESCE(updated_at, NOW()) as updated_at, COALESCE(completed_at, NOW()) as completed_at
FROM task_runs
WHERE status = 'running'
  AND (:runner_port::integer IS NULL OR runner_port IS NULL OR runner_port = :runner_port)
ORDER BY created_at DESC;

--! get_resumable_task_runs_for_runner (runner_port)
-- Stricter variant of get_running_task_runs used by the startup-resume path.
-- Only returns rows whose runner_port matches exactly — does NOT include
-- runner_port IS NULL rows, so two runners restarting simultaneously cannot
-- both pick up the same orphan. NULL-port tasks are handled separately by
-- claim_orphaned_task_runs.
SELECT id, task_name, COALESCE(prompt, '') as prompt, COALESCE(task_type, 'task') as task_type,
       COALESCE(status, 'running') as status, COALESCE(sessions_count, 0) as sessions_count,
       COALESCE(max_sessions, 0) as max_sessions,
       COALESCE(error_message, '') as error_message, COALESCE(auto_continue, true) as auto_continue,
       COALESCE(config_id, '') as config_id, COALESCE(workflow_name, '') as workflow_name, COALESCE(workflow_id, '') as workflow_id,
       COALESCE(workflow_type, 'task') as workflow_type,
       COALESCE(workspace_id, '') as workspace_id, COALESCE(triggered_by, '') as triggered_by,
       COALESCE(parent_task_run_id, '') as parent_task_run_id, COALESCE(root_task_run_id, '') as root_task_run_id,
       COALESCE(depth, 0) as depth, COALESCE(bridge_id, '') as bridge_id,
       COALESCE(is_reflection, false) as is_reflection, COALESCE(reflection_source_task_run_id, '') as reflection_source_task_run_id,
       COALESCE(is_follow_up, false) as is_follow_up, COALESCE(follow_up_source_task_run_id, '') as follow_up_source_task_run_id,
       COALESCE(is_fixer, false) as is_fixer, COALESCE(fixer_source_task_run_id, '') as fixer_source_task_run_id,
       COALESCE(is_meta_optimizer, false) as is_meta_optimizer,
       COALESCE(is_review, false) as is_review, COALESCE(blocks_parent, false) as blocks_parent,
       COALESCE(runner_port, 0) as runner_port,
       COALESCE(created_at, NOW()) as created_at, COALESCE(updated_at, NOW()) as updated_at, COALESCE(completed_at, NOW()) as completed_at
FROM task_runs
WHERE status = 'running'
  AND runner_port = :runner_port
ORDER BY created_at DESC;

--! lease_task_for_resume (id, expected_updated_at)
-- Optimistic compare-and-set used to ensure exactly one resumer takes a task.
-- If two processes for the same runner race, only the one whose updated_at
-- read matches the current value will succeed; the other gets zero rows.
UPDATE task_runs
   SET updated_at = NOW()
 WHERE id = :id
   AND status = 'running'
   AND updated_at = :expected_updated_at
RETURNING id;

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
SELECT id, task_name, COALESCE(prompt, '') as prompt, COALESCE(task_type, 'task') as task_type,
       COALESCE(status, 'running') as status, COALESCE(sessions_count, 0) as sessions_count,
       COALESCE(max_sessions, 0) as max_sessions,
       COALESCE(error_message, '') as error_message, COALESCE(auto_continue, true) as auto_continue,
       COALESCE(config_id, '') as config_id, COALESCE(workflow_name, '') as workflow_name, COALESCE(workflow_id, '') as workflow_id,
       COALESCE(summary, ai_summary, '') as summary, COALESCE(ai_summary, '') as ai_summary,
       COALESCE(goal_achieved, false) as goal_achieved,
       COALESCE(remaining_work, '') as remaining_work, COALESCE(summary_generated_at::TEXT, '') as summary_generated_at,
       COALESCE(workspace_id, '') as workspace_id, COALESCE(triggered_by, '') as triggered_by,
       COALESCE(created_at, NOW()) as created_at, COALESCE(updated_at, NOW()) as updated_at, COALESCE(completed_at, NOW()) as completed_at
FROM task_runs
WHERE (:workflow_type::text IS NULL OR workflow_type = :workflow_type)
  AND (workflow_type IS NULL OR workflow_type != 'chat')
  AND (:runner_port::integer IS NULL OR runner_port IS NULL OR runner_port = :runner_port)
ORDER BY updated_at DESC
LIMIT :max_results;

--! get_task_output
SELECT COALESCE(output_log, '') as output_log FROM task_runs WHERE id = :id;
