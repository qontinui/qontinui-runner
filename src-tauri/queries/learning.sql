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
SELECT id, task_id, status, duration_secs, iterations, strategy,
       tools_used, files_modified, error_type, error_message, feedback, created_at,
       workflow_architecture, step_count, verification_step_count,
       agentic_step_count, has_ui_bridge,
       total_tokens, total_cost_usd, composite_agentic_score,
       technology_tags, domain_tags, complexity_tier
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
SELECT id, pattern_type, description, confidence, occurrences, context,
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
