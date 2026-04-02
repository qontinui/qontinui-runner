--- Learning system operations: outcomes, patterns, stats.

--! record_learning_outcome (duration_secs?, iterations?, strategy?, tools_used?, files_modified?, error_type?, error_message?, feedback?, workflow_architecture?, step_count?, verification_step_count?, agentic_step_count?, total_tokens?, total_cost_usd?, technology_tags?, domain_tags?, complexity_tier?)
INSERT INTO learning_outcomes (
    id, task_id, status, duration_secs, iterations, strategy,
    tools_used, files_modified, error_type, error_message, feedback,
    workflow_architecture, step_count, verification_step_count,
    agentic_step_count, has_ui_bridge, total_tokens, total_cost_usd,
    technology_tags, domain_tags, complexity_tier
) VALUES (
    :id, :task_id, :status, :duration_secs, :iterations, :strategy,
    :tools_used, :files_modified, :error_type, :error_message, :feedback,
    :workflow_architecture, :step_count, :verification_step_count,
    :agentic_step_count, :has_ui_bridge, :total_tokens, :total_cost_usd,
    :technology_tags, :domain_tags, :complexity_tier
)
RETURNING id;

--! get_learning_outcomes
SELECT id, task_id, status, COALESCE(duration_secs, 0) as duration_secs,
       COALESCE(iterations, 0) as iterations, COALESCE(strategy, '') as strategy,
       COALESCE(tools_used, '') as tools_used, COALESCE(files_modified, '') as files_modified,
       COALESCE(error_type, '') as error_type, COALESCE(error_message, '') as error_message,
       COALESCE(feedback, '') as feedback, created_at,
       COALESCE(workflow_architecture, '') as workflow_architecture,
       COALESCE(step_count, 0) as step_count, COALESCE(verification_step_count, 0) as verification_step_count,
       COALESCE(agentic_step_count, 0) as agentic_step_count, COALESCE(has_ui_bridge, false) as has_ui_bridge,
       COALESCE(total_tokens, 0) as total_tokens, COALESCE(total_cost_usd, 0) as total_cost_usd,
       COALESCE(composite_agentic_score, 0) as composite_agentic_score,
       COALESCE(technology_tags, '') as technology_tags, COALESCE(domain_tags, '') as domain_tags,
       COALESCE(complexity_tier, '') as complexity_tier
FROM learning_outcomes
ORDER BY created_at DESC
LIMIT :max_results;

--! save_learning_pattern (context?)
INSERT INTO learning_patterns (id, pattern_type, description, confidence, occurrences, context)
VALUES (:id, :pattern_type, :description, :confidence, :occurrences, :context)
ON CONFLICT(id) DO UPDATE SET
    description = EXCLUDED.description,
    confidence = EXCLUDED.confidence,
    occurrences = EXCLUDED.occurrences,
    context = EXCLUDED.context,
    updated_at = NOW()
RETURNING id;

--! get_learning_patterns
SELECT id, pattern_type, description, confidence, occurrences, COALESCE(context, '') as context,
       created_at, updated_at
FROM learning_patterns
ORDER BY confidence DESC;

--! get_learning_outcomes_count
SELECT COUNT(*)::bigint as count FROM learning_outcomes;

--! get_learning_stats_summary
SELECT
    COUNT(*)::bigint as total,
    COUNT(*) FILTER (WHERE status = 'success')::bigint as successes,
    COUNT(*) FILTER (WHERE status = 'failure')::bigint as failures,
    COUNT(*) FILTER (WHERE status = 'partial')::bigint as partials,
    COALESCE(AVG(duration_secs), 0)::double precision as avg_duration,
    COALESCE(AVG(iterations)::double precision, 0)::double precision as avg_iterations,
    COALESCE(AVG(total_cost_usd), 0)::double precision as avg_cost
FROM learning_outcomes;
