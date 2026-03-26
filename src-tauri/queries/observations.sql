--- Observation CRUD and search operations (Engram-inspired persistent memory).

--: GetObservation(topic_key?, project_id?, workflow_id?, task_run_id?, session_id?)
--: ObservationPreview(topic_key?, project_id?)
--: ObservationSearchRow(topic_key?, project_id?)

--! save_observation (topic_key?, project_id?, workflow_id?, task_run_id?, session_id?)
INSERT INTO observations
    (title, content, observation_type, scope, topic_key, content_hash,
     project_id, workflow_id, task_run_id, session_id)
VALUES (:title, :content, :observation_type, :scope, :topic_key, :content_hash,
        :project_id, :workflow_id, :task_run_id, :session_id)
RETURNING id;

--! upsert_observation_by_topic_key (project_id?, workflow_id?, task_run_id?, session_id?)
INSERT INTO observations
    (title, content, observation_type, scope, topic_key, content_hash,
     project_id, workflow_id, task_run_id, session_id)
VALUES (:title, :content, :observation_type, :scope, :topic_key, :content_hash,
        :project_id, :workflow_id, :task_run_id, :session_id)
ON CONFLICT (topic_key) WHERE topic_key IS NOT NULL AND NOT is_deleted
DO UPDATE SET
    title = EXCLUDED.title,
    content = EXCLUDED.content,
    content_hash = EXCLUDED.content_hash,
    observation_type = EXCLUDED.observation_type,
    revision_count = observations.revision_count + 1,
    updated_at = NOW()
RETURNING id;

--! get_observation : GetObservation
SELECT id, title, content, observation_type, scope, topic_key, content_hash,
       revision_count, duplicate_count, project_id, workflow_id, task_run_id,
       session_id, is_deleted, created_at, updated_at
FROM observations
WHERE id = :id AND NOT is_deleted;

--! search_observations : ObservationSearchRow
SELECT id, title, LEFT(content, 300) as content_preview, observation_type, scope,
       topic_key, revision_count, project_id, created_at, updated_at,
       ts_rank(to_tsvector('english', title || ' ' || content),
               plainto_tsquery('english', :query)) as rank
FROM observations
WHERE NOT is_deleted
  AND to_tsvector('english', title || ' ' || content) @@ plainto_tsquery('english', :query)
ORDER BY rank DESC
LIMIT :max_results;

--! search_observations_by_project : ObservationSearchRow
SELECT id, title, LEFT(content, 300) as content_preview, observation_type, scope,
       topic_key, revision_count, project_id, created_at, updated_at,
       ts_rank(to_tsvector('english', title || ' ' || content),
               plainto_tsquery('english', :query)) as rank
FROM observations
WHERE NOT is_deleted
  AND project_id = :project_id
  AND to_tsvector('english', title || ' ' || content) @@ plainto_tsquery('english', :query)
ORDER BY rank DESC
LIMIT :max_results;

--! find_duplicate
SELECT id, duplicate_count
FROM observations
WHERE content_hash = :content_hash
  AND NOT is_deleted
  AND created_at > NOW() - INTERVAL '15 minutes'
LIMIT 1;

--! increment_duplicate_count
UPDATE observations SET duplicate_count = duplicate_count + 1
WHERE id = :id;

--! get_project_context (observation_type?) : ObservationPreview
SELECT id, title, LEFT(content, 300) as content_preview, observation_type, scope,
       topic_key, revision_count, project_id, created_at, updated_at
FROM observations
WHERE NOT is_deleted
  AND (project_id = :project_id OR scope = 'global')
  AND (:observation_type::text IS NULL OR observation_type = :observation_type)
ORDER BY updated_at DESC
LIMIT :max_results;

--! update_observation (title?, content?, observation_type?, content_hash?)
UPDATE observations SET
    title = COALESCE(:title, title),
    content = COALESCE(:content, content),
    observation_type = COALESCE(:observation_type, observation_type),
    content_hash = COALESCE(:content_hash, content_hash),
    updated_at = NOW()
WHERE id = :id AND NOT is_deleted
RETURNING id;

--! soft_delete_observation
UPDATE observations SET is_deleted = true, updated_at = NOW()
WHERE id = :id
RETURNING id;

--! get_observations_by_task_run : ObservationPreview
SELECT id, title, LEFT(content, 300) as content_preview, observation_type, scope,
       topic_key, revision_count, project_id, created_at, updated_at
FROM observations
WHERE task_run_id = :task_run_id AND NOT is_deleted
ORDER BY created_at ASC;

--! get_all_observations_full : GetObservation
SELECT id, title, content, observation_type, scope, topic_key, content_hash,
       revision_count, duplicate_count, project_id, workflow_id, task_run_id,
       session_id, is_deleted, created_at, updated_at
FROM observations
WHERE NOT is_deleted
ORDER BY updated_at DESC
LIMIT :max_results;

--! cleanup_stale_observations
UPDATE observations SET is_deleted = true, updated_at = NOW()
WHERE NOT is_deleted
  AND revision_count <= :max_revision_count
  AND duplicate_count = 0
  AND updated_at < NOW() - (:retention_days || ' days')::interval
RETURNING id;

--! get_observation_stats
SELECT observation_type,
       COUNT(*)::bigint as count,
       MAX(updated_at) as latest_updated
FROM observations
WHERE NOT is_deleted
GROUP BY observation_type
ORDER BY count DESC;
