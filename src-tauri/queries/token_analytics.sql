--- Token usage analytics queries for the LLM Observability dashboard.

--! get_daily_cost
SELECT date(created_at)::text as day,
       COALESCE(SUM(cost_cents), 0)::bigint as total_cost_cents,
       COALESCE(SUM(input_tokens), 0)::bigint as total_input_tokens,
       COALESCE(SUM(output_tokens), 0)::bigint as total_output_tokens,
       COUNT(*)::bigint as call_count
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days)
GROUP BY date(created_at)
ORDER BY day ASC;

--! get_cost_by_model
SELECT model_used,
       COALESCE(SUM(cost_cents), 0)::bigint as total_cost_cents,
       COALESCE(SUM(input_tokens), 0)::bigint as total_input_tokens,
       COALESCE(SUM(output_tokens), 0)::bigint as total_output_tokens,
       COUNT(*)::bigint as call_count,
       COALESCE(AVG(duration_ms), 0)::bigint as avg_duration_ms
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days)
  AND model_used IS NOT NULL
GROUP BY model_used
ORDER BY SUM(cost_cents) DESC;

--! get_cost_by_phase
SELECT phase,
       COALESCE(SUM(cost_cents), 0)::bigint as total_cost_cents,
       COALESCE(SUM(input_tokens), 0)::bigint as total_input_tokens,
       COALESCE(SUM(output_tokens), 0)::bigint as total_output_tokens
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days)
GROUP BY phase
ORDER BY SUM(cost_cents) DESC;

--! get_provider_latency
SELECT provider_used,
       COALESCE(AVG(duration_ms), 0)::bigint as avg_duration_ms,
       MIN(duration_ms) as min_duration_ms,
       MAX(duration_ms) as max_duration_ms,
       COUNT(*)::bigint as call_count
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days)
  AND provider_used IS NOT NULL
  AND duration_ms IS NOT NULL
GROUP BY provider_used;

--! get_task_run_costs
SELECT task_run_id,
       COALESCE(SUM(cost_cents), 0)::bigint as total_cost_cents,
       COALESCE(SUM(input_tokens), 0)::bigint as total_input_tokens,
       COALESCE(SUM(output_tokens), 0)::bigint as total_output_tokens,
       COUNT(*)::bigint as call_count,
       MIN(created_at)::text as started_at
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days)
GROUP BY task_run_id
ORDER BY SUM(cost_cents) DESC
LIMIT :max_results;

--! get_token_usage_totals
SELECT COALESCE(SUM(cost_cents), 0)::bigint as total_cost_cents,
       COALESCE(SUM(input_tokens), 0)::bigint as total_input_tokens,
       COALESCE(SUM(output_tokens), 0)::bigint as total_output_tokens,
       COUNT(*)::bigint as total_calls,
       COALESCE(AVG(duration_ms), 0)::bigint as avg_duration_ms
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days);

--! get_unique_models_count
SELECT COUNT(DISTINCT model_used)::bigint as count
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days)
  AND model_used IS NOT NULL;

--! get_unique_providers_count
SELECT COUNT(DISTINCT provider_used)::bigint as count
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days)
  AND provider_used IS NOT NULL;

--! get_cost_by_target_app
SELECT COALESCE(target_app, '(no app)') as target_app,
       COALESCE(SUM(cost_cents), 0)::bigint as total_cost_cents,
       COALESCE(SUM(input_tokens), 0)::bigint as total_input_tokens,
       COALESCE(SUM(output_tokens), 0)::bigint as total_output_tokens,
       COUNT(*)::bigint as call_count,
       COALESCE(AVG(duration_ms), 0)::bigint as avg_duration_ms
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days)
  AND target_app IS NOT NULL
GROUP BY target_app
ORDER BY SUM(cost_cents) DESC;

--! get_cost_by_target_page
SELECT COALESCE(target_page_url, '(no page)') as target_page_url,
       COALESCE(SUM(cost_cents), 0)::bigint as total_cost_cents,
       COALESCE(SUM(input_tokens), 0)::bigint as total_input_tokens,
       COALESCE(SUM(output_tokens), 0)::bigint as total_output_tokens,
       COUNT(*)::bigint as call_count
FROM phase_token_usage
WHERE created_at >= NOW() - make_interval(days => :days)
  AND target_page_url IS NOT NULL
GROUP BY target_page_url
ORDER BY SUM(cost_cents) DESC
LIMIT 50;
