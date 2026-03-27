-- Error Monitor queries for Clorinde.
--
-- NOTE: These queries are NOT currently used by Clorinde-generated code.
-- The PG module (src/database/pg/error_monitor.rs) uses raw conn.query() calls
-- because Clorinde cannot be regenerated right now. This file serves as the
-- canonical reference for when Clorinde is re-enabled.

--! get_unresolved_errors (task_run_id?)
--: ErrorEventRow(log_source_id?, task_run_id?, workflow_step_id?, log_timestamp?, error_type?, error_code?, stack_trace?, context_lines?, raw_entry?, file_path?, line_number?, column_number?, function_name?, finding_id?, resolved_by_task_run_id?, resolution_notes?, acknowledged_at?, resolved_at?, workflow_name?, trace_id?)
SELECT e.id, e.log_source_id, e.log_source_name, e.task_run_id, e.workflow_step_id,
       e.log_timestamp::TEXT, e.captured_at::TEXT, e.severity, e.error_type, e.error_code,
       e.message, e.stack_trace, e.context_lines, e.raw_entry,
       e.file_path, e.line_number, e.column_number, e.function_name,
       e.signature_hash, e.occurrence_count, e.first_seen_at::TEXT, e.last_seen_at::TEXT,
       e.status, e.finding_id, e.resolved_by_task_run_id, e.resolution_notes,
       e.acknowledged_at::TEXT, e.resolved_at::TEXT, tr.workflow_name, e.trace_id
FROM error_events e
LEFT JOIN task_runs tr ON e.task_run_id = tr.id
WHERE e.status IN ('new', 'acknowledged', 'in_progress', 'promoted')
  AND (:task_run_id::TEXT IS NULL OR e.task_run_id = :task_run_id)
ORDER BY
    CASE e.severity WHEN 'critical' THEN 0 WHEN 'error' THEN 1 ELSE 2 END,
    e.occurrence_count DESC,
    e.last_seen_at DESC
LIMIT :max_results;

--! get_unresolved_errors_for_task
--: ErrorEventRow(log_source_id?, task_run_id?, workflow_step_id?, log_timestamp?, error_type?, error_code?, stack_trace?, context_lines?, raw_entry?, file_path?, line_number?, column_number?, function_name?, finding_id?, resolved_by_task_run_id?, resolution_notes?, acknowledged_at?, resolved_at?, workflow_name?, trace_id?)
SELECT e.id, e.log_source_id, e.log_source_name, e.task_run_id, e.workflow_step_id,
       e.log_timestamp::TEXT, e.captured_at::TEXT, e.severity, e.error_type, e.error_code,
       e.message, e.stack_trace, e.context_lines, e.raw_entry,
       e.file_path, e.line_number, e.column_number, e.function_name,
       e.signature_hash, e.occurrence_count, e.first_seen_at::TEXT, e.last_seen_at::TEXT,
       e.status, e.finding_id, e.resolved_by_task_run_id, e.resolution_notes,
       e.acknowledged_at::TEXT, e.resolved_at::TEXT, tr.workflow_name, e.trace_id
FROM error_events e
LEFT JOIN task_runs tr ON e.task_run_id = tr.id
WHERE e.status IN ('new', 'acknowledged', 'in_progress', 'promoted')
  AND e.task_run_id = :task_run_id
ORDER BY
    CASE e.severity WHEN 'critical' THEN 0 WHEN 'error' THEN 1 ELSE 2 END,
    e.occurrence_count DESC,
    e.last_seen_at DESC
LIMIT :max_results;

--! get_error_summary (task_run_id?)
SELECT
    COUNT(*)::INTEGER as total,
    COUNT(*) FILTER (WHERE status = 'new')::INTEGER as new_count,
    COUNT(*) FILTER (WHERE status IN ('new', 'acknowledged', 'in_progress', 'promoted'))::INTEGER as unresolved_count,
    COUNT(*) FILTER (WHERE severity = 'critical' AND status IN ('new', 'acknowledged', 'in_progress', 'promoted'))::INTEGER as critical_count,
    COUNT(*) FILTER (WHERE severity = 'error' AND status IN ('new', 'acknowledged', 'in_progress', 'promoted'))::INTEGER as error_count,
    COUNT(*) FILTER (WHERE severity = 'warning' AND status IN ('new', 'acknowledged', 'in_progress', 'promoted'))::INTEGER as warning_count
FROM error_events
WHERE (:task_run_id::TEXT IS NULL OR task_run_id = :task_run_id);

--! get_error_summary_by_source (task_run_id?)
SELECT log_source_name, COUNT(*)::BIGINT as cnt
FROM error_events
WHERE log_source_name IS NOT NULL
  AND (:task_run_id::TEXT IS NULL OR task_run_id = :task_run_id)
GROUP BY log_source_name;

--! get_error_summary_by_type (task_run_id?)
SELECT error_type, COUNT(*)::BIGINT as cnt
FROM error_events
WHERE error_type IS NOT NULL
  AND (:task_run_id::TEXT IS NULL OR task_run_id = :task_run_id)
GROUP BY error_type;

--! get_error_summary_by_status (task_run_id?)
SELECT status, COUNT(*)::BIGINT as cnt
FROM error_events
WHERE status IS NOT NULL
  AND (:task_run_id::TEXT IS NULL OR task_run_id = :task_run_id)
GROUP BY status;

--! update_error_status (resolution_notes?, acknowledged_at?, resolved_at?)
UPDATE error_events SET
    status = :status,
    resolution_notes = COALESCE(:resolution_notes, resolution_notes),
    acknowledged_at = COALESCE(:acknowledged_at::TIMESTAMPTZ, acknowledged_at),
    resolved_at = COALESCE(:resolved_at::TIMESTAMPTZ, resolved_at)
WHERE id = :id;

--! mark_resolved_by_task (resolution_notes?)
UPDATE error_events SET
    status = 'resolved',
    resolved_by_task_run_id = :task_run_id,
    resolution_notes = :resolution_notes,
    resolved_at = NOW()
WHERE id = :id;

--! get_error_debug_context (task_run_id?)
--: ErrorEventRow(log_source_id?, task_run_id?, workflow_step_id?, log_timestamp?, error_type?, error_code?, stack_trace?, context_lines?, raw_entry?, file_path?, line_number?, column_number?, function_name?, finding_id?, resolved_by_task_run_id?, resolution_notes?, acknowledged_at?, resolved_at?, workflow_name?, trace_id?)
SELECT e.id, e.log_source_id, e.log_source_name, e.task_run_id, e.workflow_step_id,
       e.log_timestamp::TEXT, e.captured_at::TEXT, e.severity, e.error_type, e.error_code,
       e.message, e.stack_trace, e.context_lines, e.raw_entry,
       e.file_path, e.line_number, e.column_number, e.function_name,
       e.signature_hash, e.occurrence_count, e.first_seen_at::TEXT, e.last_seen_at::TEXT,
       e.status, e.finding_id, e.resolved_by_task_run_id, e.resolution_notes,
       e.acknowledged_at::TEXT, e.resolved_at::TEXT, tr.workflow_name, e.trace_id
FROM error_events e
LEFT JOIN task_runs tr ON e.task_run_id = tr.id
WHERE e.status IN ('new', 'acknowledged', 'in_progress', 'promoted')
  AND (:task_run_id::TEXT IS NULL OR e.task_run_id = :task_run_id)
ORDER BY
    CASE e.severity WHEN 'critical' THEN 0 WHEN 'error' THEN 1 ELSE 2 END,
    e.occurrence_count DESC,
    e.last_seen_at DESC
LIMIT 50;
