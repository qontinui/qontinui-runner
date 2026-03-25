-- Runner PostgreSQL schema (Clorinde source of truth).
-- Each table here is validated against all queries in queries/*.sql.
-- This file is loaded by `clorinde fresh` into a temporary database for validation.

-- Stub for FK constraint (full task_runs migration comes later)
CREATE TABLE IF NOT EXISTS task_runs (
    id TEXT PRIMARY KEY,
    total_input_tokens BIGINT DEFAULT 0,
    total_output_tokens BIGINT DEFAULT 0,
    total_cost_cents BIGINT DEFAULT 0
);

CREATE TABLE IF NOT EXISTS phase_token_usage (
    id BIGSERIAL PRIMARY KEY,
    task_run_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    phase TEXT NOT NULL,
    stage_index INTEGER,
    iteration INTEGER,
    model_used TEXT,
    provider_used TEXT,
    input_tokens BIGINT NOT NULL DEFAULT 0,
    output_tokens BIGINT NOT NULL DEFAULT 0,
    cost_cents BIGINT NOT NULL DEFAULT 0,
    duration_ms BIGINT,
    target_app TEXT,
    target_page_url TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_ptu_task_run ON phase_token_usage(task_run_id);
CREATE INDEX IF NOT EXISTS idx_ptu_created_at ON phase_token_usage(created_at);
CREATE INDEX IF NOT EXISTS idx_ptu_target_app ON phase_token_usage(target_app) WHERE target_app IS NOT NULL;

-- Task Run Events (highest write frequency — unified event log)
CREATE TABLE IF NOT EXISTS task_run_events (
    id BIGSERIAL PRIMARY KEY,
    task_run_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,
    event_subtype TEXT,
    message TEXT NOT NULL,
    data TEXT,
    workflow_name TEXT,
    state_name TEXT,
    action_id TEXT,
    timestamp TEXT NOT NULL,
    duration_ms BIGINT
);

CREATE INDEX IF NOT EXISTS idx_tre_task_run_id ON task_run_events(task_run_id);
CREATE INDEX IF NOT EXISTS idx_tre_event_type ON task_run_events(event_type);
CREATE INDEX IF NOT EXISTS idx_tre_timestamp ON task_run_events(timestamp);
CREATE INDEX IF NOT EXISTS idx_tre_subtype ON task_run_events(event_subtype);
CREATE INDEX IF NOT EXISTS idx_tre_workflow ON task_run_events(workflow_name);

-- Task Run Screenshots
CREATE TABLE IF NOT EXISTS task_run_screenshots (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    event_id BIGINT REFERENCES task_run_events(id) ON DELETE SET NULL,
    file_path TEXT NOT NULL,
    screenshot_type TEXT NOT NULL,
    template_name TEXT,
    confidence DOUBLE PRECISION,
    match_location TEXT,
    width INTEGER,
    height INTEGER,
    file_size_bytes BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_trs_task_run_id ON task_run_screenshots(task_run_id);
CREATE INDEX IF NOT EXISTS idx_trs_type ON task_run_screenshots(screenshot_type);

-- Task Run Playwright Results
CREATE TABLE IF NOT EXISTS task_run_playwright_results (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    test_name TEXT NOT NULL,
    spec_file TEXT,
    status TEXT NOT NULL,
    duration_ms BIGINT,
    stdout TEXT,
    stderr TEXT,
    console_output TEXT,
    page_snapshot TEXT,
    error_message TEXT,
    failure_screenshot_path TEXT,
    assertions_passed INTEGER DEFAULT 0,
    assertions_failed INTEGER DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_trp_task_run_id ON task_run_playwright_results(task_run_id);
CREATE INDEX IF NOT EXISTS idx_trp_status ON task_run_playwright_results(status);

-- Task Run API Requests
CREATE TABLE IF NOT EXISTS task_run_api_requests (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    step_id TEXT NOT NULL,
    step_name TEXT,
    method TEXT NOT NULL,
    url TEXT NOT NULL,
    resolved_url TEXT NOT NULL,
    request_headers TEXT,
    request_body TEXT,
    status_code INTEGER NOT NULL,
    status_text TEXT,
    response_headers TEXT,
    response_time_ms BIGINT NOT NULL,
    response_body_type TEXT NOT NULL,
    response_body TEXT,
    response_size_bytes BIGINT,
    extractions TEXT,
    assertions TEXT,
    success BOOLEAN NOT NULL,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_trar_task_run_id ON task_run_api_requests(task_run_id);
CREATE INDEX IF NOT EXISTS idx_trar_step_id ON task_run_api_requests(step_id);
CREATE INDEX IF NOT EXISTS idx_trar_created_at ON task_run_api_requests(created_at);

-- Task Run AWAS Steps
CREATE TABLE IF NOT EXISTS task_run_awas_steps (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    step_id TEXT,
    step_name TEXT,
    step_type TEXT NOT NULL,
    url TEXT,
    action_id TEXT,
    parameters TEXT,
    response_data TEXT,
    success BOOLEAN NOT NULL DEFAULT true,
    error_message TEXT,
    duration_ms BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_traw_task_run_id ON task_run_awas_steps(task_run_id);
CREATE INDEX IF NOT EXISTS idx_traw_step_type ON task_run_awas_steps(step_type);

-- Execution Spans (OpenTelemetry-compatible trace spans)
CREATE TABLE IF NOT EXISTS execution_spans (
    id BIGSERIAL PRIMARY KEY,
    execution_id TEXT REFERENCES task_runs(id) ON DELETE CASCADE,
    trace_id TEXT NOT NULL,
    span_id TEXT NOT NULL,
    parent_span_id TEXT,
    name TEXT NOT NULL,
    start_ts TEXT NOT NULL,
    end_ts TEXT,
    duration_ms BIGINT,
    attributes TEXT,
    success BOOLEAN DEFAULT true,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_spans_execution ON execution_spans(execution_id);
CREATE INDEX IF NOT EXISTS idx_spans_trace ON execution_spans(trace_id);
CREATE INDEX IF NOT EXISTS idx_spans_name ON execution_spans(name);
