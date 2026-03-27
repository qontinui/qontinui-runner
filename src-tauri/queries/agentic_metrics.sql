--- Agentic metric score operations: per-run scores, aggregates, trends.

--! get_scores_for_run
SELECT id, task_run_id, metric_type, score, confidence, rationale,
       is_llm_judged, model_used, created_at
FROM agentic_metric_scores
WHERE task_run_id = :task_run_id
ORDER BY metric_type;

--! get_metric_aggregates
SELECT metric_type,
       AVG(score) as avg_score,
       MIN(score) as min_score,
       MAX(score) as max_score,
       COUNT(*)::bigint as sample_count
FROM agentic_metric_scores
WHERE created_at >= :since
GROUP BY metric_type
ORDER BY metric_type;

--! get_latest_task_run_id
SELECT task_run_id
FROM agentic_metric_scores
ORDER BY created_at DESC
LIMIT 1;

--! has_scores
SELECT COUNT(*)::bigint as count
FROM agentic_metric_scores
WHERE task_run_id = :task_run_id;

--! get_composite_score_trend
SELECT DATE_TRUNC('day', created_at) as day,
       AVG(score) as avg_score,
       COUNT(DISTINCT task_run_id)::bigint as run_count
FROM agentic_metric_scores
WHERE created_at >= :since
GROUP BY DATE_TRUNC('day', created_at)
ORDER BY day;
