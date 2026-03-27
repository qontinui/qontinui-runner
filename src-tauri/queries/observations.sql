--- Observation CRUD and search operations (Engram-inspired persistent memory).
--- Now with temporal validity tracking (valid_from/valid_until/superseded_by).

--: GetObservation(topic_key?, project_id?, workflow_id?, task_run_id?, session_id?, valid_until?, superseded_by?)
--: ObservationPreview(topic_key?, project_id?, valid_until?, superseded_by?)
--: ObservationSearchRow(topic_key?, project_id?, valid_until?, superseded_by?)
--: ObservationHistoryRow()
--: TemporalSearchRow(topic_key?, project_id?, valid_until?, superseded_by?)
--: WeeklyTrendRow()
--: TopicRevisionRow(topic_key?)

--! save_observation (topic_key?, project_id?, workflow_id?, task_run_id?, session_id?)
INSERT INTO observations
    (title, content, observation_type, scope, topic_key, content_hash,
     project_id, workflow_id, task_run_id, session_id, valid_from)
VALUES (:title, :content, :observation_type, :scope, :topic_key, :content_hash,
        :project_id, :workflow_id, :task_run_id, :session_id, NOW())
RETURNING id;

--! snapshot_before_upsert (topic_key?)
INSERT INTO observation_history (observation_id, title, content, content_hash, valid_from, valid_until, revision_number)
SELECT id, title, content, content_hash, valid_from, NOW(), revision_count
FROM observations
WHERE topic_key = :topic_key AND topic_key IS NOT NULL AND NOT is_deleted;

--! upsert_observation_by_topic_key (project_id?, workflow_id?, task_run_id?, session_id?)
INSERT INTO observations
    (title, content, observation_type, scope, topic_key, content_hash,
     project_id, workflow_id, task_run_id, session_id, valid_from)
VALUES (:title, :content, :observation_type, :scope, :topic_key, :content_hash,
        :project_id, :workflow_id, :task_run_id, :session_id, NOW())
ON CONFLICT (topic_key) WHERE topic_key IS NOT NULL AND NOT is_deleted
DO UPDATE SET
    title = EXCLUDED.title,
    content = EXCLUDED.content,
    content_hash = EXCLUDED.content_hash,
    observation_type = EXCLUDED.observation_type,
    revision_count = observations.revision_count + 1,
    valid_from = NOW(),
    updated_at = NOW()
RETURNING id;

--! get_observation : GetObservation
SELECT id, title, content, observation_type, scope, topic_key, content_hash,
       revision_count, duplicate_count, project_id, workflow_id, task_run_id,
       session_id, is_deleted, valid_from, valid_until, superseded_by, created_at, updated_at
FROM observations
WHERE id = :id AND NOT is_deleted;

--! search_observations : ObservationSearchRow
SELECT id, title, LEFT(content, 300) as content_preview, observation_type, scope,
       topic_key, revision_count, project_id,
       valid_from, valid_until, superseded_by,
       created_at, updated_at,
       ts_rank(to_tsvector('english', title || ' ' || content),
               plainto_tsquery('english', :query)) as rank
FROM observations
WHERE NOT is_deleted
  AND to_tsvector('english', title || ' ' || content) @@ plainto_tsquery('english', :query)
ORDER BY rank DESC
LIMIT :max_results;

--! search_observations_by_project : ObservationSearchRow
SELECT id, title, LEFT(content, 300) as content_preview, observation_type, scope,
       topic_key, revision_count, project_id,
       valid_from, valid_until, superseded_by,
       created_at, updated_at,
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
       topic_key, revision_count, project_id,
       valid_from, valid_until, superseded_by,
       created_at, updated_at
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
       topic_key, revision_count, project_id,
       valid_from, valid_until, superseded_by,
       created_at, updated_at
FROM observations
WHERE task_run_id = :task_run_id AND NOT is_deleted
ORDER BY created_at ASC;

--! get_all_observations_full : GetObservation
SELECT id, title, content, observation_type, scope, topic_key, content_hash,
       revision_count, duplicate_count, project_id, workflow_id, task_run_id,
       session_id, is_deleted, valid_from, valid_until, superseded_by, created_at, updated_at
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

--- Phase 3: Supersession ---

--! supersede_observation
UPDATE observations SET
    superseded_by = :new_observation_id,
    valid_until = NOW(),
    updated_at = NOW()
WHERE id = :id AND NOT is_deleted
RETURNING id;

--- Phase 4: Temporal query support ---

--! search_observations_temporal (query?, from_time?, to_time?, as_of?) : TemporalSearchRow
SELECT id, title, LEFT(content, 300) as content_preview, observation_type, scope,
       topic_key, revision_count, project_id,
       valid_from, valid_until, superseded_by,
       created_at, updated_at,
       ts_rank(to_tsvector('english', title || ' ' || content),
               plainto_tsquery('english', COALESCE(:query, ''))) as rank
FROM observations
WHERE NOT is_deleted
  AND (:query::text IS NULL OR to_tsvector('english', title || ' ' || content) @@ plainto_tsquery('english', :query))
  AND (:from_time::timestamptz IS NULL OR valid_from >= :from_time)
  AND (:to_time::timestamptz IS NULL OR valid_from <= :to_time)
  AND (:as_of::timestamptz IS NULL OR (valid_from <= :as_of AND (valid_until IS NULL OR valid_until >= :as_of)))
ORDER BY
  CASE WHEN valid_until IS NULL THEN 0 ELSE 1 END,
  valid_from DESC
LIMIT :max_results;

--- Phase 5: Temporal aggregation queries ---

--! observation_weekly_trend : WeeklyTrendRow
SELECT observation_type,
       date_trunc('week', created_at)::timestamptz as week_start,
       COUNT(*)::bigint as count
FROM observations
WHERE NOT is_deleted
  AND created_at >= NOW() - (:weeks || ' weeks')::interval
GROUP BY observation_type, date_trunc('week', created_at)
ORDER BY week_start DESC, count DESC;

--! most_revised_topics : TopicRevisionRow
SELECT t.topic_key, t.title, t.revisions
FROM (
  SELECT DISTINCT ON (topic_key) topic_key, title, revision_count::bigint as revisions
  FROM observations
  WHERE NOT is_deleted
    AND topic_key IS NOT NULL
    AND updated_at >= :from_time
    AND updated_at <= :to_time
  ORDER BY topic_key, updated_at DESC
) t
ORDER BY t.revisions DESC
LIMIT :max_results;

--! get_observation_history : ObservationHistoryRow
SELECT id, observation_id, title, LEFT(content, 500) as content_preview,
       content_hash, valid_from, valid_until, revision_number, created_at
FROM observation_history
WHERE observation_id = :observation_id
ORDER BY revision_number DESC;

--! snapshot_observations_at : GetObservation
SELECT id, title, content, observation_type, scope, topic_key, content_hash,
       revision_count, duplicate_count, project_id, workflow_id, task_run_id,
       session_id, is_deleted, valid_from, valid_until, superseded_by, created_at, updated_at
FROM observations
WHERE NOT is_deleted
  AND valid_from <= :as_of
  AND (valid_until IS NULL OR valid_until >= :as_of)
ORDER BY valid_from DESC
LIMIT :max_results;

-- ============================================================================
-- Memory Consolidation queries
-- ============================================================================

--: ConsolidationObservation(topic_key?, project_id?, workflow_id?, task_run_id?, session_id?, last_accessed_at?, consolidated_from?)

--! get_observations_for_consolidation : ConsolidationObservation
SELECT id, title, content, observation_type, scope, topic_key, content_hash,
       revision_count, duplicate_count, importance, access_count, decay_rate,
       is_mental_model, consolidated_from, project_id, workflow_id, task_run_id,
       session_id, last_accessed_at, created_at, updated_at
FROM observations
WHERE NOT is_deleted AND NOT is_mental_model
ORDER BY importance DESC, updated_at DESC
LIMIT :max_results;

--! get_mental_models : ConsolidationObservation
SELECT id, title, content, observation_type, scope, topic_key, content_hash,
       revision_count, duplicate_count, importance, access_count, decay_rate,
       is_mental_model, consolidated_from, project_id, workflow_id, task_run_id,
       session_id, last_accessed_at, created_at, updated_at
FROM observations
WHERE NOT is_deleted AND is_mental_model
ORDER BY importance DESC, updated_at DESC
LIMIT :max_results;

--! update_observation_importance
UPDATE observations
SET importance = :importance,
    decay_rate = :decay_rate,
    updated_at = NOW()
WHERE id = :id AND NOT is_deleted
RETURNING id;

--! record_observation_access
UPDATE observations
SET last_accessed_at = NOW(),
    access_count = access_count + 1,
    updated_at = NOW()
WHERE id = :id AND NOT is_deleted
RETURNING id;

--! save_mental_model (topic_key?, project_id?, workflow_id?, task_run_id?, session_id?, consolidated_from?)
INSERT INTO observations
    (title, content, observation_type, scope, topic_key, content_hash,
     importance, decay_rate, is_mental_model, consolidated_from,
     project_id, workflow_id, task_run_id, session_id, valid_from)
VALUES (:title, :content, :observation_type, :scope, :topic_key, :content_hash,
        :importance, :decay_rate, true, :consolidated_from,
        :project_id, :workflow_id, :task_run_id, :session_id, NOW())
RETURNING id;

--! reduce_observation_importance
UPDATE observations
SET importance = importance * :factor,
    updated_at = NOW()
WHERE id = :id AND NOT is_deleted
RETURNING id;

--! decay_and_archive_observations
UPDATE observations
SET is_deleted = true, updated_at = NOW()
WHERE NOT is_deleted
  AND NOT is_mental_model
  AND importance * EXP(-decay_rate * EXTRACT(EPOCH FROM (NOW() - COALESCE(last_accessed_at, updated_at))) / 86400.0) < :retention_threshold
RETURNING id;

--! decay_and_archive_mental_models
UPDATE observations
SET is_deleted = true, updated_at = NOW()
WHERE NOT is_deleted
  AND is_mental_model
  AND importance * EXP(-decay_rate * EXTRACT(EPOCH FROM (NOW() - COALESCE(last_accessed_at, updated_at))) / 86400.0) < :retention_threshold
RETURNING id;

--! get_observations_by_topic_prefix : ConsolidationObservation
SELECT id, title, content, observation_type, scope, topic_key, content_hash,
       revision_count, duplicate_count, importance, access_count, decay_rate,
       is_mental_model, consolidated_from, project_id, workflow_id, task_run_id,
       session_id, last_accessed_at, created_at, updated_at
FROM observations
WHERE NOT is_deleted AND NOT is_mental_model
  AND topic_key IS NOT NULL
  AND topic_key LIKE :prefix || '%'
ORDER BY importance DESC;

--! get_observations_by_type_for_consolidation : ConsolidationObservation
SELECT id, title, content, observation_type, scope, topic_key, content_hash,
       revision_count, duplicate_count, importance, access_count, decay_rate,
       is_mental_model, consolidated_from, project_id, workflow_id, task_run_id,
       session_id, last_accessed_at, created_at, updated_at
FROM observations
WHERE NOT is_deleted AND NOT is_mental_model
  AND observation_type = :observation_type
ORDER BY importance DESC
LIMIT :max_results;

--! get_decay_preview
SELECT id, title, observation_type, importance, decay_rate, is_mental_model,
       COALESCE(last_accessed_at, updated_at) as last_activity,
       importance * EXP(-decay_rate * EXTRACT(EPOCH FROM (NOW() - COALESCE(last_accessed_at, updated_at))) / 86400.0) as current_retention
FROM observations
WHERE NOT is_deleted
ORDER BY current_retention ASC
LIMIT :max_results;

--: ConsolidationLogRow(completed_at?, error?)

--! insert_consolidation_log
INSERT INTO memory_consolidation_log (started_at)
VALUES (NOW())
RETURNING id;

--! complete_consolidation_log (error?)
UPDATE memory_consolidation_log
SET completed_at = NOW(),
    observations_scanned = :observations_scanned,
    groups_found = :groups_found,
    models_created = :models_created,
    observations_merged = :observations_merged,
    observations_decayed = :observations_decayed,
    observations_archived = :observations_archived,
    error = :error
WHERE id = :id;

--! get_consolidation_log : ConsolidationLogRow
SELECT id, started_at, completed_at, observations_scanned, groups_found,
       models_created, observations_merged, observations_decayed,
       observations_archived, error
FROM memory_consolidation_log
ORDER BY started_at DESC
LIMIT :max_results;

--: LastConsolidationRow(last_run?)

--! get_last_consolidation_time : LastConsolidationRow
SELECT MAX(started_at) as last_run
FROM memory_consolidation_log
WHERE completed_at IS NOT NULL AND error IS NULL;

--: MemoryHealthRow(avg_importance?, avg_access_count?)

--! get_memory_health_stats : MemoryHealthRow
SELECT
    COUNT(*) FILTER (WHERE NOT is_mental_model)::bigint as total_observations,
    COUNT(*) FILTER (WHERE is_mental_model)::bigint as total_mental_models,
    COUNT(*) FILTER (WHERE importance * EXP(-decay_rate * EXTRACT(EPOCH FROM (NOW() - COALESCE(last_accessed_at, updated_at))) / 86400.0) < 0.05)::bigint as decay_queue_size,
    AVG(importance) as avg_importance,
    AVG(access_count)::double precision as avg_access_count
FROM observations
WHERE NOT is_deleted;
