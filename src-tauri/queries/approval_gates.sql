--- Approval gate operations for human-in-the-loop workflows.

--! insert_approval_gate
INSERT INTO approval_gates (id, task_run_id, iteration, prompt, context_json, status)
VALUES (:id, :task_run_id, :iteration, :prompt, :context_json, 'pending')
RETURNING id;

--! resolve_approval_gate (comment?)
UPDATE approval_gates SET action = :action, comment = :comment, status = :status, resolved_at = NOW()
WHERE id = :id
RETURNING id;

--! get_approval_gates_for_task_run
SELECT id, task_run_id, iteration, prompt, context_json, action, comment, status, created_at, resolved_at
FROM approval_gates
WHERE task_run_id = :task_run_id
ORDER BY created_at ASC;
