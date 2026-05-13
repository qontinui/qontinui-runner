--- Activity Timeline CRUD and search operations (screenpipe-inspired capture history).
--- (smoke-test for clorinde auto-regen workflow happy path — PR #116 acceptance criterion #4)

--- Row types with nullable field annotations (? suffix = Option<T> in Rust).
--: SearchTimelineRow(app_name?, window_title?, url?, screenshot_path?, element_count?, confidence?)
--: SearchTimelineFilteredRow(app_name?, window_title?, url?, screenshot_path?, element_count?, confidence?)
--: GetTimelineRangeRow(app_name?, window_title?, url?, screenshot_path?, element_count?, confidence?)
--: GetTimelineEntryRow(app_name?, window_title?, url?, task_run_id?, screenshot_path?, element_count?, confidence?, metadata_json?)
--: GetTimelineByTaskRunRow(app_name?, window_title?, url?, screenshot_path?, element_count?, confidence?)
--: GetTimelineStatsRow(latest_capture?)

--! insert_activity_entry (app_name?, window_title?, url?, task_run_id?, screenshot_path?, element_count?, confidence?, metadata_json?)
INSERT INTO activity_timeline
    (text_content, content_hash, source_type, capture_mode,
     app_name, window_title, url, task_run_id, screenshot_path,
     element_count, confidence, metadata_json)
VALUES (:text_content, :content_hash, :source_type, :capture_mode,
        :app_name, :window_title, :url, :task_run_id, :screenshot_path,
        :element_count, :confidence, :metadata_json)
RETURNING id;

--! find_recent_duplicate
SELECT id, duplicate_count
FROM activity_timeline
WHERE content_hash = :content_hash
  AND NOT is_deleted
  AND created_at > NOW() - INTERVAL '30 seconds'
LIMIT 1;

--! increment_duplicate_count
UPDATE activity_timeline SET duplicate_count = duplicate_count + 1
WHERE id = :id;

--! search_timeline : SearchTimelineRow
SELECT id, LEFT(text_content, 500) as text_preview, source_type, capture_mode,
       app_name, window_title, url, screenshot_path, element_count, confidence,
       created_at,
       ts_rank(to_tsvector('english', text_content),
               plainto_tsquery('english', :query)) as rank
FROM activity_timeline
WHERE NOT is_deleted
  AND to_tsvector('english', text_content) @@ plainto_tsquery('english', :query)
ORDER BY rank DESC
LIMIT :max_results;

--! search_timeline_filtered (app_name?, source_type?, task_run_id?) : SearchTimelineFilteredRow
SELECT id, LEFT(text_content, 500) as text_preview, source_type, capture_mode,
       app_name, window_title, url, screenshot_path, element_count, confidence,
       created_at,
       ts_rank(to_tsvector('english', text_content),
               plainto_tsquery('english', :query)) as rank
FROM activity_timeline
WHERE NOT is_deleted
  AND to_tsvector('english', text_content) @@ plainto_tsquery('english', :query)
  AND (:app_name::text IS NULL OR app_name = :app_name)
  AND (:source_type::text IS NULL OR source_type = :source_type)
  AND (:task_run_id::text IS NULL OR task_run_id = :task_run_id)
ORDER BY rank DESC
LIMIT :max_results;

--! get_timeline_range : GetTimelineRangeRow
SELECT id, LEFT(text_content, 500) as text_preview, source_type, capture_mode,
       app_name, window_title, url, screenshot_path, element_count, confidence,
       created_at
FROM activity_timeline
WHERE NOT is_deleted
  AND created_at >= :start_time
  AND created_at <= :end_time
ORDER BY created_at DESC
LIMIT :max_results;

--! get_timeline_entry : GetTimelineEntryRow
SELECT id, text_content, content_hash, source_type, capture_mode,
       app_name, window_title, url, task_run_id, screenshot_path,
       element_count, confidence, metadata_json, duplicate_count,
       is_deleted, created_at
FROM activity_timeline
WHERE id = :id AND NOT is_deleted;

--! get_timeline_by_task_run : GetTimelineByTaskRunRow
SELECT id, LEFT(text_content, 500) as text_preview, source_type, capture_mode,
       app_name, window_title, url, screenshot_path, element_count, confidence,
       created_at
FROM activity_timeline
WHERE task_run_id = :task_run_id AND NOT is_deleted
ORDER BY created_at ASC;

--! soft_delete_timeline_entry
UPDATE activity_timeline SET is_deleted = true
WHERE id = :id
RETURNING id;

--! get_timeline_stats : GetTimelineStatsRow
SELECT source_type, capture_mode,
       COUNT(*)::bigint as count,
       MAX(created_at) as latest_capture
FROM activity_timeline
WHERE NOT is_deleted
GROUP BY source_type, capture_mode
ORDER BY count DESC;

--! cleanup_old_entries
DELETE FROM activity_timeline
WHERE created_at < NOW() - make_interval(days => :retention_days)
  AND is_deleted;
