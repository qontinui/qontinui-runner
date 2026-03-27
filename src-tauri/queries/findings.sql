--- Task run findings operations.

--! update_finding_status (resolution?, resolved_in_session?)
UPDATE task_run_findings SET
    status = :status,
    resolution = COALESCE(:resolution, resolution),
    resolved_in_session = COALESCE(:resolved_in_session, resolved_in_session),
    resolved_at = CASE WHEN :status IN ('resolved', 'wont_fix', 'deferred') THEN NOW() ELSE resolved_at END,
    updated_at = NOW()
WHERE id = :id
RETURNING id;
