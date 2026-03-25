--- Task run event operations (highest write frequency).

--! create_task_run_event (event_subtype?, data?, workflow_name?, state_name?, action_id?, duration_ms?)
INSERT INTO task_run_events
    (task_run_id, event_type, event_subtype, message, data,
     workflow_name, state_name, action_id, timestamp, duration_ms)
VALUES (:task_run_id, :event_type, :event_subtype, :message, :data,
        :workflow_name, :state_name, :action_id, :timestamp, :duration_ms)
RETURNING id;

--! get_task_run_events_all
SELECT id, task_run_id, event_type, event_subtype, message, data,
       workflow_name, state_name, action_id, timestamp, duration_ms
FROM task_run_events
WHERE task_run_id = :task_run_id
ORDER BY timestamp ASC;

--! get_task_run_events_by_type
SELECT id, task_run_id, event_type, event_subtype, message, data,
       workflow_name, state_name, action_id, timestamp, duration_ms
FROM task_run_events
WHERE task_run_id = :task_run_id AND event_type = :event_type
ORDER BY timestamp ASC;

--! get_task_run_events_limited
SELECT id, task_run_id, event_type, event_subtype, message, data,
       workflow_name, state_name, action_id, timestamp, duration_ms
FROM task_run_events
WHERE task_run_id = :task_run_id
ORDER BY timestamp ASC
LIMIT :max_results;

--! delete_task_run_events
DELETE FROM task_run_events WHERE task_run_id = :task_run_id;

--! get_event_count_by_task_run
SELECT COUNT(*)::bigint as count
FROM task_run_events
WHERE task_run_id = :task_run_id;

--! create_task_run_screenshot (event_id?, template_name?, confidence?, match_location?, width?, height?, file_size_bytes?)
INSERT INTO task_run_screenshots
    (id, task_run_id, event_id, file_path, screenshot_type,
     template_name, confidence, match_location,
     width, height, file_size_bytes, created_at)
VALUES (:id, :task_run_id, :event_id, :file_path, :screenshot_type,
        :template_name, :confidence, :match_location,
        :width, :height, :file_size_bytes, NOW());

--! get_task_run_screenshots_all
SELECT id, task_run_id, event_id, file_path, screenshot_type,
       template_name, confidence, match_location,
       width, height, file_size_bytes, created_at
FROM task_run_screenshots
WHERE task_run_id = :task_run_id
ORDER BY created_at ASC;

--! get_task_run_screenshots_by_type
SELECT id, task_run_id, event_id, file_path, screenshot_type,
       template_name, confidence, match_location,
       width, height, file_size_bytes, created_at
FROM task_run_screenshots
WHERE task_run_id = :task_run_id AND screenshot_type = :screenshot_type
ORDER BY created_at ASC;

--! create_task_run_playwright_result (spec_file?, duration_ms?, stdout?, stderr?, console_output?, page_snapshot?, error_message?, failure_screenshot_path?)
INSERT INTO task_run_playwright_results
    (id, task_run_id, test_name, spec_file, status, duration_ms,
     stdout, stderr, console_output, page_snapshot,
     error_message, failure_screenshot_path,
     assertions_passed, assertions_failed, created_at)
VALUES (:id, :task_run_id, :test_name, :spec_file, :status, :duration_ms,
        :stdout, :stderr, :console_output, :page_snapshot,
        :error_message, :failure_screenshot_path,
        :assertions_passed, :assertions_failed, NOW());

--! get_task_run_playwright_results_all
SELECT id, task_run_id, test_name, spec_file, status, duration_ms,
       stdout, stderr, console_output, page_snapshot,
       error_message, failure_screenshot_path,
       assertions_passed, assertions_failed, created_at
FROM task_run_playwright_results
WHERE task_run_id = :task_run_id
ORDER BY created_at ASC;

--! create_task_run_api_request (step_name?, request_headers?, request_body?, status_text?, response_headers?, response_body?, response_size_bytes?, extractions?, assertions?, error_message?)
INSERT INTO task_run_api_requests
    (id, task_run_id, step_id, step_name,
     method, url, resolved_url, request_headers, request_body,
     status_code, status_text, response_headers, response_time_ms,
     response_body_type, response_body, response_size_bytes,
     extractions, assertions, success, error_message, created_at)
VALUES (:id, :task_run_id, :step_id, :step_name,
        :method, :url, :resolved_url, :request_headers, :request_body,
        :status_code, :status_text, :response_headers, :response_time_ms,
        :response_body_type, :response_body, :response_size_bytes,
        :extractions, :assertions, :success, :error_message, NOW());

--! get_task_run_api_requests_all
SELECT id, task_run_id, step_id, step_name,
       method, url, resolved_url, request_headers, request_body,
       status_code, status_text, response_headers, response_time_ms,
       response_body_type, response_body, response_size_bytes,
       extractions, assertions, success, error_message, created_at
FROM task_run_api_requests
WHERE task_run_id = :task_run_id
ORDER BY created_at ASC;

--! create_task_run_awas_step (step_id?, step_name?, url?, action_id?, parameters?, response_data?, error_message?, duration_ms?)
INSERT INTO task_run_awas_steps
    (id, task_run_id, step_id, step_name, step_type,
     url, action_id, parameters, response_data,
     success, error_message, duration_ms, created_at)
VALUES (:id, :task_run_id, :step_id, :step_name, :step_type,
        :url, :action_id, :parameters, :response_data,
        :success, :error_message, :duration_ms, NOW());

--! get_task_run_awas_steps
SELECT id, task_run_id, step_id, step_name, step_type,
       url, action_id, parameters, response_data,
       success, error_message, duration_ms, created_at
FROM task_run_awas_steps
WHERE task_run_id = :task_run_id
ORDER BY created_at ASC;
