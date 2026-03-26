--- Watcher CRUD operations (screenpipe-inspired scheduled reactive agents).

--- Row types with nullable field annotations (? suffix = Option<T> in Rust).
--: WatcherRow(app_name_filter?, source_type_filter?, last_run_at?, last_result_json?)
--: ListWatcherRow(app_name_filter?, source_type_filter?, last_run_at?, last_result_json?)
--: ListEnabledWatcherRow(app_name_filter?, source_type_filter?, last_run_at?, last_result_json?)

--! create_watcher (app_name_filter?, source_type_filter?)
INSERT INTO watchers
    (id, name, schedule_json, timeline_query, app_name_filter, source_type_filter,
     lookback_window, reasoning_prompt, action_json)
VALUES (:id, :name, :schedule_json, :timeline_query, :app_name_filter, :source_type_filter,
        :lookback_window, :reasoning_prompt, :action_json)
RETURNING id;

--! get_watcher : WatcherRow
SELECT id, name, schedule_json, timeline_query, app_name_filter, source_type_filter,
       lookback_window, reasoning_prompt, action_json, enabled,
       last_run_at, last_result_json, created_at, updated_at
FROM watchers
WHERE id = :id;

--! list_watchers : ListWatcherRow
SELECT id, name, schedule_json, timeline_query, app_name_filter, source_type_filter,
       lookback_window, reasoning_prompt, action_json, enabled,
       last_run_at, last_result_json, created_at, updated_at
FROM watchers
ORDER BY created_at DESC;

--! list_enabled_watchers : ListEnabledWatcherRow
SELECT id, name, schedule_json, timeline_query, app_name_filter, source_type_filter,
       lookback_window, reasoning_prompt, action_json, enabled,
       last_run_at, last_result_json, created_at, updated_at
FROM watchers
WHERE enabled = true
ORDER BY created_at ASC;

--! update_watcher (name?, schedule_json?, timeline_query?, app_name_filter?, source_type_filter?, lookback_window?, reasoning_prompt?, action_json?, enabled?)
UPDATE watchers SET
    name = COALESCE(:name, name),
    schedule_json = COALESCE(:schedule_json, schedule_json),
    timeline_query = COALESCE(:timeline_query, timeline_query),
    app_name_filter = COALESCE(:app_name_filter, app_name_filter),
    source_type_filter = COALESCE(:source_type_filter, source_type_filter),
    lookback_window = COALESCE(:lookback_window, lookback_window),
    reasoning_prompt = COALESCE(:reasoning_prompt, reasoning_prompt),
    action_json = COALESCE(:action_json, action_json),
    enabled = COALESCE(:enabled, enabled),
    updated_at = NOW()
WHERE id = :id
RETURNING id;

--! record_watcher_run (last_result_json?)
UPDATE watchers SET
    last_run_at = NOW(),
    last_result_json = :last_result_json,
    updated_at = NOW()
WHERE id = :id;

--! delete_watcher
DELETE FROM watchers WHERE id = :id RETURNING id;

--! set_watcher_enabled
UPDATE watchers SET enabled = :enabled, updated_at = NOW()
WHERE id = :id
RETURNING id;
