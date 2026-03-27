--- Workflow execution state, step checkpoints, and progress markers.

--! save_workflow_execution_state (state_data?, phase?, iteration?)
INSERT INTO workflow_execution_state (execution_id, workflow_type, state_name, state_data, phase, iteration, updated_at)
VALUES (:execution_id, :workflow_type, :state_name, :state_data, :phase, :iteration, NOW())
ON CONFLICT(execution_id) DO UPDATE SET
    workflow_type = EXCLUDED.workflow_type,
    state_name = EXCLUDED.state_name,
    state_data = EXCLUDED.state_data,
    phase = EXCLUDED.phase,
    iteration = EXCLUDED.iteration,
    updated_at = NOW()
RETURNING execution_id;

--! get_workflow_execution_state
SELECT execution_id, workflow_type, state_name, state_data, phase, iteration, updated_at
FROM workflow_execution_state
WHERE execution_id = :execution_id;

--! delete_workflow_execution_state
DELETE FROM workflow_execution_state WHERE execution_id = :execution_id;

--! get_step_checkpoints_paginated_with_cursor
SELECT id, execution_id, workflow_type, phase, iteration, step_index,
       stage_index, step_type, step_name, status, result_json, step_config_json,
       started_at, completed_at, duration_ms, error
FROM workflow_step_checkpoints
WHERE execution_id = :execution_id AND step_index > :cursor
ORDER BY step_index ASC
LIMIT :max_results;

--! get_step_checkpoints_paginated_no_cursor
SELECT id, execution_id, workflow_type, phase, iteration, step_index,
       stage_index, step_type, step_name, status, result_json, step_config_json,
       started_at, completed_at, duration_ms, error
FROM workflow_step_checkpoints
WHERE execution_id = :execution_id
ORDER BY step_index ASC
LIMIT :max_results;

--! get_running_checkpoint_id
SELECT id FROM workflow_step_checkpoints
WHERE execution_id = :execution_id AND status = 'running'
ORDER BY step_index DESC
LIMIT 1;

--! get_latest_step_progress_marker (checkpoint_id?)
SELECT id, checkpoint_id, marker_type, current_value, total_value, description, data_json, created_at
FROM step_progress_markers
WHERE checkpoint_id = :checkpoint_id
ORDER BY id DESC
LIMIT 1;
