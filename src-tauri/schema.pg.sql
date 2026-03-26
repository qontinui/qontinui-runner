-- Runner PostgreSQL schema (Clorinde source of truth).
-- Each table here is validated against all queries in queries/*.sql.
-- This file is loaded by `clorinde fresh` into a temporary database for validation.

-- Task Runs (central execution record — FK target for all event/usage tables)
CREATE TABLE IF NOT EXISTS task_runs (
    id TEXT PRIMARY KEY,
    task_name TEXT NOT NULL,
    prompt TEXT,
    task_type TEXT NOT NULL DEFAULT 'task',
    status TEXT NOT NULL DEFAULT 'running',

    -- Session tracking
    sessions_count INTEGER NOT NULL DEFAULT 0,
    max_sessions INTEGER,
    auto_continue BOOLEAN NOT NULL DEFAULT true,

    -- Output
    output_log TEXT DEFAULT '',
    error_message TEXT,

    -- Execution configuration
    execution_steps_json TEXT,
    log_sources_json TEXT,

    -- Config linkage
    config_id TEXT,
    workflow_name TEXT,
    workflow_id TEXT,

    -- Summary
    summary TEXT,
    ai_summary TEXT,
    goal_achieved BOOLEAN,
    remaining_work TEXT,
    summary_generated_at TEXT,

    -- Runtime context
    runtime_context_json TEXT,
    transition_history_json TEXT,

    -- Hierarchy
    parent_task_run_id TEXT REFERENCES task_runs(id) ON DELETE SET NULL,
    root_task_run_id TEXT,
    depth INTEGER DEFAULT 0,

    -- Multi-bridge
    bridge_id TEXT,

    -- Workflow type
    workflow_type TEXT,

    -- Structured result data
    result_data TEXT,

    -- Web integration
    workspace_id TEXT,
    triggered_by TEXT,

    -- Embeddings (BYTEA in PG)
    prompt_embedding BYTEA,
    summary_embedding BYTEA,

    -- Reflection
    is_reflection BOOLEAN DEFAULT false,
    reflection_source_task_run_id TEXT,

    -- Follow-up
    is_follow_up BOOLEAN DEFAULT false,
    follow_up_source_task_run_id TEXT,

    -- Runner port
    runner_port INTEGER,

    -- Fixer
    is_fixer BOOLEAN DEFAULT false,
    fixer_source_task_run_id TEXT,

    -- Meta-optimizer
    is_meta_optimizer BOOLEAN DEFAULT false,

    -- Cross-iteration context
    iteration_history TEXT,

    -- Pipeline checkpoint
    pipeline_checkpoint TEXT,

    -- Durable execution
    iteration_diffs TEXT,
    iteration_commits TEXT,

    -- Token totals (aggregated from phase_token_usage)
    total_input_tokens BIGINT DEFAULT 0,
    total_output_tokens BIGINT DEFAULT 0,
    total_cost_cents BIGINT DEFAULT 0,

    -- Timestamps
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_tr_status ON task_runs(status);
CREATE INDEX IF NOT EXISTS idx_tr_created_at ON task_runs(created_at);
CREATE INDEX IF NOT EXISTS idx_tr_task_type ON task_runs(task_type);
CREATE INDEX IF NOT EXISTS idx_tr_config_id ON task_runs(config_id);
CREATE INDEX IF NOT EXISTS idx_tr_parent ON task_runs(parent_task_run_id);
CREATE INDEX IF NOT EXISTS idx_tr_root ON task_runs(root_task_run_id);
CREATE INDEX IF NOT EXISTS idx_tr_bridge_id ON task_runs(bridge_id);
CREATE INDEX IF NOT EXISTS idx_tr_runner_port ON task_runs(runner_port);
CREATE INDEX IF NOT EXISTS idx_tr_workflow_id ON task_runs(workflow_id);

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

-- Unified Workflows (workflow definitions — core CRUD table)
CREATE TABLE IF NOT EXISTS unified_workflows (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT DEFAULT '',
    category TEXT DEFAULT 'general',
    tags TEXT DEFAULT '[]',

    -- Phase steps (JSON arrays)
    setup_steps TEXT DEFAULT '[]',
    verification_steps TEXT DEFAULT '[]',
    agentic_steps TEXT DEFAULT '[]',
    completion_steps TEXT DEFAULT '[]',

    -- Agentic configuration
    max_iterations BIGINT DEFAULT 10,
    provider TEXT,
    model TEXT,
    skip_ai_summary BOOLEAN NOT NULL DEFAULT false,
    timeout_seconds BIGINT,
    prompt_template TEXT,

    -- Context configuration
    context_ids TEXT DEFAULT '[]',
    disabled_context_ids TEXT DEFAULT '[]',
    auto_include_contexts BOOLEAN DEFAULT true,

    -- Log configuration
    log_watch_enabled BOOLEAN DEFAULT true,
    log_source_selection TEXT DEFAULT '"default"',

    -- Health check configuration
    health_check_enabled BOOLEAN DEFAULT true,
    health_check_urls TEXT DEFAULT '[]',

    -- Pre-flight check
    preflight_check_enabled BOOLEAN DEFAULT true,

    -- Completion sweep
    enable_sweep BOOLEAN DEFAULT false,
    max_sweep_iterations BIGINT DEFAULT 5,

    -- Multi-stage
    stages TEXT DEFAULT '[]',
    stop_on_failure BOOLEAN DEFAULT false,
    constraint_overrides TEXT DEFAULT '{}',
    approval_gate BOOLEAN DEFAULT false,
    reflection_mode BOOLEAN DEFAULT true,
    completion_prompts_first BOOLEAN NOT NULL DEFAULT false,
    model_overrides TEXT DEFAULT '{}',

    -- Generation tracking
    generated_by_task_run_id TEXT REFERENCES task_runs(id) ON DELETE SET NULL,

    -- Embedding (BYTEA in PG, BLOB in SQLite)
    description_embedding BYTEA,

    -- Example library
    example_status TEXT DEFAULT 'pending',

    -- Sync
    sync_pending BOOLEAN DEFAULT false,

    -- Favorites
    is_favorite BOOLEAN DEFAULT false,

    -- Quality metadata
    dependency_graph TEXT,
    cost_annotations TEXT,
    quality_report TEXT,
    acceptance_criteria TEXT,
    ai_reviewed BOOLEAN DEFAULT true,

    -- Architecture
    workflow_architecture TEXT,

    -- Slash command tracking
    source_file_path TEXT,
    source_content_hash TEXT,

    -- Durable execution
    rollback_policy TEXT DEFAULT 'none',

    -- CWD and tool filtering
    strict_cwd BOOLEAN DEFAULT false,
    tool_tags TEXT DEFAULT '[]',

    -- Token budget
    enforce_token_budget BOOLEAN DEFAULT false,

    -- Flow control (v147)
    flow_control_json TEXT,
    phase_timeouts_json TEXT,

    -- Timestamps
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_uw_category ON unified_workflows(category);
CREATE INDEX IF NOT EXISTS idx_uw_updated_at ON unified_workflows(updated_at);
CREATE INDEX IF NOT EXISTS idx_uw_name ON unified_workflows(name);
CREATE INDEX IF NOT EXISTS idx_uw_example_status ON unified_workflows(example_status);
CREATE INDEX IF NOT EXISTS idx_uw_sync_pending ON unified_workflows(sync_pending);
CREATE INDEX IF NOT EXISTS idx_uw_is_favorite ON unified_workflows(is_favorite);
CREATE INDEX IF NOT EXISTS idx_uw_source_file ON unified_workflows(source_file_path);
-- Full-text search on name + description
CREATE INDEX IF NOT EXISTS idx_uw_fts ON unified_workflows
    USING GIN (to_tsvector('english', name || ' ' || COALESCE(description, '')));

-- Settings (key-value store with JSON values)
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,  -- JSON value
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Config storage (imported projects, loaded config files)
CREATE TABLE IF NOT EXISTS configs (
    id TEXT PRIMARY KEY,           -- Config ID (project_id for web imports, hash for files)
    name TEXT NOT NULL,            -- Display name
    config_json TEXT NOT NULL,     -- Full QontinuiConfig as JSON
    source_type TEXT NOT NULL,     -- 'web' or 'file'
    source_path TEXT,              -- File path if source_type='file'
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_configs_name ON configs(name);

-- Observations (Engram-inspired persistent memory for cross-session knowledge)
CREATE TABLE IF NOT EXISTS observations (
    id BIGSERIAL PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    -- Type classification: decision, architecture, bugfix, pattern, learning, discovery
    observation_type TEXT NOT NULL,
    -- Scope: project, personal, global
    scope TEXT NOT NULL DEFAULT 'project',
    -- Stable topic key for upsert semantics (e.g. "architecture/auth-model")
    topic_key TEXT,
    -- SHA-256 of normalized content for deduplication
    content_hash TEXT NOT NULL,
    revision_count INTEGER NOT NULL DEFAULT 1,
    duplicate_count INTEGER NOT NULL DEFAULT 0,
    -- Optional FK links
    project_id TEXT,
    workflow_id TEXT,
    task_run_id TEXT REFERENCES task_runs(id) ON DELETE SET NULL,
    session_id TEXT,
    is_deleted BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_obs_topic_key ON observations(topic_key) WHERE topic_key IS NOT NULL AND NOT is_deleted;
CREATE INDEX IF NOT EXISTS idx_obs_content_hash ON observations(content_hash);
CREATE INDEX IF NOT EXISTS idx_obs_project_type ON observations(project_id, observation_type) WHERE NOT is_deleted;
CREATE INDEX IF NOT EXISTS idx_obs_scope ON observations(scope) WHERE NOT is_deleted;
CREATE INDEX IF NOT EXISTS idx_obs_created_at ON observations(created_at);
CREATE INDEX IF NOT EXISTS idx_obs_task_run ON observations(task_run_id) WHERE task_run_id IS NOT NULL;
-- Full-text search index (title + content combined)
CREATE INDEX IF NOT EXISTS idx_obs_fts ON observations
    USING GIN (to_tsvector('english', title || ' ' || content))
    WHERE NOT is_deleted;

-- Activity Timeline (screenpipe-inspired searchable capture history)
-- Stores extracted text from all capture sources (UI Bridge snapshots, OCR, accessibility trees)
-- with full-text search for debugging and reactive watchers.
CREATE TABLE IF NOT EXISTS activity_timeline (
    id              BIGSERIAL PRIMARY KEY,
    -- Extracted text content (a11y tree text, OCR output, or UI Bridge element text)
    text_content    TEXT NOT NULL,
    -- SHA-256 of normalized text for consecutive-frame deduplication
    content_hash    TEXT NOT NULL,
    -- Source classification
    source_type     TEXT NOT NULL,       -- 'accessibility', 'ocr', 'ui_bridge'
    capture_mode    TEXT NOT NULL,       -- 'white_box' (UI Bridge) or 'black_box' (HAL)
    -- Window/app context at capture time
    app_name        TEXT,
    window_title    TEXT,
    url             TEXT,
    -- Linkage
    task_run_id     TEXT REFERENCES task_runs(id) ON DELETE SET NULL,
    screenshot_path TEXT,
    -- Capture quality metrics
    element_count   INTEGER,
    confidence      DOUBLE PRECISION,
    -- Extensible metadata (page type, form count, modal state, etc.)
    metadata_json   TEXT,
    -- Dedup tracking: how many consecutive identical frames were suppressed
    duplicate_count INTEGER NOT NULL DEFAULT 0,
    -- Lifecycle
    is_deleted      BOOLEAN NOT NULL DEFAULT false,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Deduplication: fast lookup for recent identical content
CREATE INDEX IF NOT EXISTS idx_at_content_hash ON activity_timeline(content_hash);
-- Time-range queries (most common query pattern)
CREATE INDEX IF NOT EXISTS idx_at_created_at ON activity_timeline(created_at);
-- Filter by task run
CREATE INDEX IF NOT EXISTS idx_at_task_run ON activity_timeline(task_run_id) WHERE task_run_id IS NOT NULL;
-- Filter by application
CREATE INDEX IF NOT EXISTS idx_at_app_name ON activity_timeline(app_name) WHERE app_name IS NOT NULL;
-- Filter by source type
CREATE INDEX IF NOT EXISTS idx_at_source_type ON activity_timeline(source_type) WHERE NOT is_deleted;
-- Full-text search (the core feature — enables natural language queries over screen history)
CREATE INDEX IF NOT EXISTS idx_at_fts ON activity_timeline
    USING GIN (to_tsvector('english', text_content))
    WHERE NOT is_deleted;

-- Watchers (screenpipe-inspired scheduled reactive AI agents)
-- Each watcher runs on a schedule, queries the activity timeline, reasons with AI,
-- and optionally triggers an action (run workflow, create observation, notify).
CREATE TABLE IF NOT EXISTS watchers (
    id                  TEXT PRIMARY KEY,
    name                TEXT NOT NULL,
    -- Schedule: ScheduleExpression as JSON (cron, interval, once, condition)
    schedule_json       TEXT NOT NULL,
    -- Activity timeline FTS query string
    timeline_query      TEXT NOT NULL,
    -- Optional filters for timeline search
    app_name_filter     TEXT,
    source_type_filter  TEXT,
    -- How far back to search (PostgreSQL interval, e.g. '15 minutes', '1 hour')
    lookback_window     TEXT NOT NULL DEFAULT '15 minutes',
    -- AI prompt template with {{results}}, {{result_count}}, {{query}} placeholders
    reasoning_prompt    TEXT NOT NULL,
    -- WatcherAction as JSON (RunWorkflow, Notify, CreateObservation, LogOnly)
    action_json         TEXT NOT NULL,
    -- Lifecycle
    enabled             BOOLEAN NOT NULL DEFAULT true,
    last_run_at         TIMESTAMPTZ,
    last_result_json    TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_watchers_enabled ON watchers(enabled) WHERE enabled;
