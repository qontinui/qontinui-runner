--- Deferred question operations for non-blocking human-in-the-loop feedback.

--! insert_deferred_question (auto_decision_detail?, git_checkpoint?)
INSERT INTO deferred_questions (id, task_run_id, iteration, question, context_json, auto_decision_type, auto_decision_detail, confidence, risk_level, git_checkpoint)
VALUES (:id, :task_run_id, :iteration, :question, :context_json, :auto_decision_type, :auto_decision_detail, :confidence, :risk_level, :git_checkpoint)
RETURNING id;

--! review_deferred_question (reviewer_comment?)
UPDATE deferred_questions SET status = :status, reviewer_comment = :reviewer_comment, reviewed_at = NOW()
WHERE id = :id
RETURNING id;

--! update_contingent_iterations
UPDATE deferred_questions SET contingent_iterations = :contingent_iterations
WHERE id = :id
RETURNING id;

--! get_deferred_questions_for_task_run
SELECT id, task_run_id, iteration, question, context_json,
       auto_decision_type, COALESCE(auto_decision_detail, '') as auto_decision_detail,
       confidence, risk_level, status,
       COALESCE(git_checkpoint, '') as git_checkpoint,
       contingent_iterations,
       COALESCE(reviewer_comment, '') as reviewer_comment,
       created_at, COALESCE(reviewed_at, NOW()) as reviewed_at
FROM deferred_questions
WHERE task_run_id = :task_run_id
ORDER BY created_at ASC;

--! get_deferred_questions_by_status
SELECT id, task_run_id, iteration, question, context_json,
       auto_decision_type, COALESCE(auto_decision_detail, '') as auto_decision_detail,
       confidence, risk_level, status,
       COALESCE(git_checkpoint, '') as git_checkpoint,
       contingent_iterations,
       COALESCE(reviewer_comment, '') as reviewer_comment,
       created_at, COALESCE(reviewed_at, NOW()) as reviewed_at
FROM deferred_questions
WHERE task_run_id = :task_run_id AND status = :status
ORDER BY created_at ASC;

--! get_deferred_question_by_id
SELECT id, task_run_id, iteration, question, context_json,
       auto_decision_type, COALESCE(auto_decision_detail, '') as auto_decision_detail,
       confidence, risk_level, status,
       COALESCE(git_checkpoint, '') as git_checkpoint,
       contingent_iterations,
       COALESCE(reviewer_comment, '') as reviewer_comment,
       created_at, COALESCE(reviewed_at, NOW()) as reviewed_at
FROM deferred_questions
WHERE id = :id;

--! count_pending_deferred_questions
SELECT COUNT(*) as count
FROM deferred_questions
WHERE task_run_id = :task_run_id AND status = 'pending';
