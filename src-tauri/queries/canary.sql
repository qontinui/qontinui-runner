--- Canary rollout operations: rollouts, run records, prompt template canaries.

--! start_canary
INSERT INTO canary_rollouts (
    id, recommendation_id, percentage, status, start_date,
    baseline_metrics_json, canary_metrics_json
) VALUES (
    :id, :recommendation_id, :percentage, 'active', NOW(),
    :baseline_metrics_json, :canary_metrics_json
)
RETURNING id, recommendation_id, percentage, status, start_date, created_at;

--! get_active_canaries
SELECT id, recommendation_id, percentage, status, start_date, end_date,
       baseline_run_count, canary_run_count,
       baseline_metrics_json, canary_metrics_json, created_at
FROM canary_rollouts
WHERE status = 'active'
ORDER BY created_at DESC;

--! get_canary_history
SELECT cr.id, cr.recommendation_id, cr.percentage, cr.status,
       cr.start_date, cr.end_date,
       cr.baseline_run_count, cr.canary_run_count,
       cr.baseline_metrics_json, cr.canary_metrics_json,
       cr.created_at,
       mor.title as recommendation_title,
       mor.optimizer_type, mor.target_agent
FROM canary_rollouts cr
LEFT JOIN meta_optimizer_recommendations mor ON mor.id = cr.recommendation_id
ORDER BY cr.created_at DESC
LIMIT :max_results;

--! get_canary_metrics
SELECT id, baseline_run_count, canary_run_count,
       baseline_metrics_json, canary_metrics_json
FROM canary_rollouts
WHERE id = :id;

--! update_canary_metrics
UPDATE canary_rollouts
SET baseline_run_count = :baseline_run_count,
    canary_run_count = :canary_run_count,
    baseline_metrics_json = :baseline_metrics_json,
    canary_metrics_json = :canary_metrics_json
WHERE id = :id
RETURNING id, baseline_run_count, canary_run_count;

--! record_canary_run (task_run_id?, cost_usd?, duration_ms?)
INSERT INTO canary_run_records (
    id, canary_id, is_canary, task_run_id, success, cost_usd, duration_ms
) VALUES (
    :id, :canary_id, :is_canary, :task_run_id, :success, :cost_usd, :duration_ms
)
RETURNING id, canary_id, is_canary, created_at;

--! promote_canary
UPDATE canary_rollouts
SET status = 'promoted', end_date = NOW()
WHERE id = :id
RETURNING id, recommendation_id, status, end_date;

--! rollback_canary
UPDATE canary_rollouts
SET status = 'rolled_back', end_date = NOW()
WHERE id = :id
RETURNING id, recommendation_id, status, end_date;

--! create_template_canary
INSERT INTO prompt_template_canaries (
    id, template_id, baseline_version, candidate_version,
    traffic_percentage, status, baseline_metrics_json, candidate_metrics_json
) VALUES (
    :id, :template_id, :baseline_version, :candidate_version,
    :traffic_percentage, 'active', :baseline_metrics_json, :candidate_metrics_json
)
RETURNING id, template_id, baseline_version, candidate_version, status, created_at;

--! get_template_canary
SELECT id, template_id, baseline_version, candidate_version,
       traffic_percentage, status,
       baseline_metrics_json, candidate_metrics_json,
       created_at, ended_at
FROM prompt_template_canaries
WHERE id = :id;

--! update_template_canary_metrics
UPDATE prompt_template_canaries
SET baseline_metrics_json = :baseline_metrics_json,
    candidate_metrics_json = :candidate_metrics_json
WHERE id = :id
RETURNING id, template_id, status;

--! get_active_template_canary
SELECT id, template_id, baseline_version, candidate_version,
       traffic_percentage, status,
       baseline_metrics_json, candidate_metrics_json,
       created_at, ended_at
FROM prompt_template_canaries
WHERE template_id = :template_id AND status = 'active'
LIMIT 1;
