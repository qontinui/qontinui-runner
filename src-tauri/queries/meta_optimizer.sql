--- Meta-optimizer recommendation operations: CRUD, status tracking.

--! get_recommendation
SELECT id, optimizer_type, recommendation_type, target_agent,
       title, description, current_value, recommended_value,
       evidence, confidence, status, applied_at,
       outcome_after_apply, optimizer_run_id,
       created_at, content_hash, eval_result_id, eval_status
FROM meta_optimizer_recommendations
WHERE id = :id;

--! update_recommendation_status
UPDATE meta_optimizer_recommendations
SET status = :status, applied_at = NOW()
WHERE id = :id
RETURNING id, optimizer_type, recommendation_type, status, applied_at;

--! get_recommendation_applied_at
SELECT applied_at
FROM meta_optimizer_recommendations
WHERE id = :id;
