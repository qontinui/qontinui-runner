--- Token usage analytics queries for the LLM Observability dashboard.

--! get_daily_cost
SELECT date(created_at)::text as day,
       COALESCE(SUM(cost_cents), 0) as total_cost_cents,
       COALESCE(SUM(input_tokens), 0) as total_input_tokens,
       COALESCE(SUM(output_tokens), 0) as total_output_tokens,
       COUNT(*) as call_count
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days)
GROUP BY date(created_at)
ORDER BY day ASC;

--! get_cost_by_model
SELECT model_used,
       COALESCE(SUM(cost_cents), 0) as total_cost_cents,
       COALESCE(SUM(input_tokens), 0) as total_input_tokens,
       COALESCE(SUM(output_tokens), 0) as total_output_tokens,
       COUNT(*) as call_count,
       AVG(duration_ms)::bigint as avg_duration_ms
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days)
  AND model_used IS NOT NULL
GROUP BY model_used
ORDER BY SUM(cost_cents) DESC;

--! get_cost_by_phase
SELECT phase,
       COALESCE(SUM(cost_cents), 0) as total_cost_cents,
       COALESCE(SUM(input_tokens), 0) as total_input_tokens,
       COALESCE(SUM(output_tokens), 0) as total_output_tokens
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days)
GROUP BY phase
ORDER BY SUM(cost_cents) DESC;

--! get_provider_latency
SELECT provider_used,
       AVG(duration_ms)::bigint as avg_duration_ms,
       MIN(duration_ms) as min_duration_ms,
       MAX(duration_ms) as max_duration_ms,
       COUNT(*) as call_count
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days)
  AND provider_used IS NOT NULL
  AND duration_ms IS NOT NULL
GROUP BY provider_used;

--! get_task_run_costs
SELECT task_run_id,
       COALESCE(SUM(cost_cents), 0) as total_cost_cents,
       COALESCE(SUM(input_tokens), 0) as total_input_tokens,
       COALESCE(SUM(output_tokens), 0) as total_output_tokens,
       COUNT(*) as call_count,
       MIN(created_at)::text as started_at
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days)
GROUP BY task_run_id
ORDER BY SUM(cost_cents) DESC
LIMIT :max_results;

--! get_token_usage_totals
SELECT COALESCE(SUM(cost_cents), 0) as total_cost_cents,
       COALESCE(SUM(input_tokens), 0) as total_input_tokens,
       COALESCE(SUM(output_tokens), 0) as total_output_tokens,
       COUNT(*) as total_calls,
       AVG(duration_ms) as avg_duration_ms
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days);

--! get_unique_models_count
SELECT COUNT(DISTINCT model_used) as count
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days)
  AND model_used IS NOT NULL;

--! get_unique_providers_count
SELECT COUNT(DISTINCT provider_used) as count
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days)
  AND provider_used IS NOT NULL;
