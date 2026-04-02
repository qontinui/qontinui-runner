--- Meta-optimizer recommendation operations: CRUD, status tracking.

--! get_recommendation
SELECT id, optimizer_type, recommendation_type, target_agent,
       title, description, current_value, recommended_value,
       evidence, confidence, status, COALESCE(applied_at, NOW()) as applied_at,
       COALESCE(outcome_after_apply, '') as outcome_after_apply, COALESCE(optimizer_run_id, '') as optimizer_run_id,
       created_at, COALESCE(content_hash, '') as content_hash, COALESCE(eval_result_id, '') as eval_result_id,
       COALESCE(eval_status, '') as eval_status
FROM meta_optimizer_recommendations
WHERE id = :id;

--! update_recommendation_status
UPDATE meta_optimizer_recommendations
SET status = :status, applied_at = NOW()
WHERE id = :id
RETURNING id, optimizer_type, recommendation_type, status, applied_at;

--! get_recommendation_applied_at
SELECT COALESCE(applied_at, NOW()) as applied_at
FROM meta_optimizer_recommendations
WHERE id = :id;
