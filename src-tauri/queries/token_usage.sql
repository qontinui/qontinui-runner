--- Phase token usage CRUD operations.

--: PhaseTokenUsageRow(stage_index?, iteration?, model_used?, provider_used?, duration_ms?)

--! create_phase_token_usage
INSERT INTO phase_token_usage
    (task_run_id, phase, stage_index, iteration, model_used, provider_used,
     input_tokens, output_tokens, cost_cents, duration_ms)
VALUES (:task_run_id, :phase, :stage_index, :iteration, :model_used, :provider_used,
        :input_tokens, :output_tokens, :cost_cents, :duration_ms);

--! get_phase_token_usage : PhaseTokenUsageRow
SELECT phase, stage_index, iteration, model_used, provider_used,
       input_tokens, output_tokens, cost_cents, duration_ms, created_at
FROM phase_token_usage
WHERE task_run_id = :task_run_id
ORDER BY created_at ASC;

--! get_iteration_token_totals
SELECT COALESCE(SUM(input_tokens), 0) as total_input,
       COALESCE(SUM(output_tokens), 0) as total_output
FROM phase_token_usage
WHERE task_run_id = :task_run_id AND iteration = :iteration;

--! update_task_run_token_totals
UPDATE task_runs SET
    total_input_tokens = COALESCE((SELECT SUM(input_tokens) FROM phase_token_usage WHERE task_run_id = :task_run_id), 0),
    total_output_tokens = COALESCE((SELECT SUM(output_tokens) FROM phase_token_usage WHERE task_run_id = :task_run_id), 0),
    total_cost_cents = COALESCE((SELECT SUM(cost_cents) FROM phase_token_usage WHERE task_run_id = :task_run_id), 0)
WHERE id = :task_run_id;
