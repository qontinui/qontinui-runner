--- Task knowledge operations: create, search, embedding storage.

--! create_task_knowledge (evidence?, related_files?)
INSERT INTO task_knowledge (
    id, task_run_id, category, agent_type, iteration,
    content, evidence, confidence, related_files, created_at
) VALUES (
    :id, :task_run_id, :category, :agent_type, :iteration,
    :content, :evidence, :confidence, :related_files, NOW()
)
RETURNING id;

--! get_task_knowledge
SELECT id, task_run_id, category, agent_type, iteration,
       content, evidence, confidence, related_files,
       is_resolved, created_at
FROM task_knowledge
WHERE id = :id;

--! list_task_knowledge_by_category (category?)
SELECT id, task_run_id, category, agent_type, iteration,
       content, evidence, confidence, related_files,
       is_resolved, content_embedding, created_at
FROM task_knowledge
WHERE (:category::TEXT IS NULL OR category = :category)
  AND content_embedding IS NOT NULL
ORDER BY created_at DESC
LIMIT :max_results;

--! store_knowledge_embedding
UPDATE task_knowledge
SET content_embedding = :embedding
WHERE id = :id;

--! resolve_task_knowledge (resolution_notes?)
UPDATE task_knowledge
SET is_resolved = true,
    resolution_notes = :resolution_notes,
    resolved_at = NOW()
WHERE id = :id;
