--- Durable workflow queue operations (Inngest-inspired).

--! queue_insert (workflow_name, error_message?, task_run_id?)
INSERT INTO queued_workflows (id, workflow_id, workflow_name, queued_at, priority, status, error_message, task_run_id, retry_count, max_retries)
VALUES (:id, :workflow_id, :workflow_name, NOW(), :priority, 'pending', :error_message, :task_run_id, 0, :max_retries)
ON CONFLICT(id) DO UPDATE SET
    status = EXCLUDED.status,
    updated_at = NOW()
RETURNING id;

--! queue_get
SELECT id, workflow_id, workflow_name, queued_at, priority, status,
       COALESCE(started_at, NOW()) as started_at, COALESCE(completed_at, NOW()) as completed_at,
       updated_at, COALESCE(error_message, '') as error_message, COALESCE(task_run_id, '') as task_run_id,
       retry_count, max_retries
FROM queued_workflows
WHERE id = :id;

--! queue_load_pending
SELECT id, workflow_id, workflow_name, queued_at, priority, status,
       COALESCE(started_at, NOW()) as started_at, COALESCE(completed_at, NOW()) as completed_at,
       updated_at, COALESCE(error_message, '') as error_message, COALESCE(task_run_id, '') as task_run_id,
       retry_count, max_retries
FROM queued_workflows
WHERE status IN ('pending', 'running')
ORDER BY priority DESC, queued_at ASC;

--! queue_update_status
UPDATE queued_workflows
SET status = :status, updated_at = NOW()
WHERE id = :id
RETURNING id;

--! queue_set_started
UPDATE queued_workflows
SET started_at = NOW(), status = 'running', updated_at = NOW()
WHERE id = :id AND started_at IS NULL
RETURNING id;

--! queue_set_completed
UPDATE queued_workflows
SET completed_at = NOW(), status = :status, updated_at = NOW()
WHERE id = :id
RETURNING id;

--! queue_set_error
UPDATE queued_workflows
SET error_message = :error_message, updated_at = NOW()
WHERE id = :id
RETURNING id;

--! queue_set_task_run_id
UPDATE queued_workflows
SET task_run_id = :task_run_id, updated_at = NOW()
WHERE id = :id
RETURNING id;

--! queue_increment_retry
UPDATE queued_workflows
SET retry_count = retry_count + 1, status = 'pending', error_message = NULL, updated_at = NOW()
WHERE id = :id
RETURNING retry_count;

--! queue_recover_crashed
UPDATE queued_workflows
SET status = 'pending', started_at = NULL, updated_at = NOW()
WHERE status = 'running'
RETURNING id;

--! queue_cleanup
DELETE FROM queued_workflows
WHERE status IN ('completed', 'failed', 'cancelled')
  AND completed_at < :cutoff
RETURNING id;
