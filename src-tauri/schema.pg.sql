-- Runner PostgreSQL schema (Clorinde source of truth).
-- Each table here is validated against all queries in queries/*.sql.
-- This file is loaded by `clorinde fresh` into a temporary database for validation.

-- Schema migrations (runner migration tracking — infrastructure, not domain)
CREATE TABLE IF NOT EXISTS schema_migrations (
    version     INTEGER PRIMARY KEY,
    description TEXT NOT NULL,
    applied_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

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
    summary_generated_at TIMESTAMPTZ,

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
    fix_attempts INTEGER DEFAULT 0,

    -- Review / CI
    is_review BOOLEAN DEFAULT false,
    blocks_parent BOOLEAN DEFAULT false,
    ci_auto_resumes INTEGER DEFAULT 0,

    -- Meta-optimizer
    is_meta_optimizer BOOLEAN DEFAULT false,

    -- Cross-iteration context
    iteration_history TEXT,

    -- Pipeline checkpoint
    pipeline_checkpoint TEXT,

    -- Durable execution
    iteration_diffs TEXT,
    iteration_commits TEXT,
    verification_passed BOOLEAN,

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
    cache_creation_tokens BIGINT NOT NULL DEFAULT 0,
    cache_read_tokens BIGINT NOT NULL DEFAULT 0,
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

    -- Inngest-inspired enrichment (v146)
    queue_wait_ms BIGINT,           -- Time spent waiting in the workflow queue
    retry_attempt INTEGER,          -- Retry attempt number (0 = first attempt)
    phase TEXT,                     -- Workflow phase (setup, verification, agentic, completion)
    iteration INTEGER,              -- Loop iteration number
    workflow_id TEXT,               -- Workflow definition that owns this span

    -- Token usage and cost tracking (v168)
    input_tokens INTEGER,
    output_tokens INTEGER,
    cost_cents INTEGER,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_spans_execution ON execution_spans(execution_id);
CREATE INDEX IF NOT EXISTS idx_spans_trace ON execution_spans(trace_id);
CREATE INDEX IF NOT EXISTS idx_spans_name ON execution_spans(name);

-- Execution state snapshots (v168)
CREATE TABLE IF NOT EXISTS execution_state_snapshots (
    id              BIGSERIAL PRIMARY KEY,
    execution_id    TEXT NOT NULL,
    span_id         TEXT NOT NULL DEFAULT '',
    snapshot_ts     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    state_type      TEXT NOT NULL,
    summary         TEXT,
    context_json    TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_ess_execution ON execution_state_snapshots(execution_id);
CREATE INDEX IF NOT EXISTS idx_ess_ts ON execution_state_snapshots(snapshot_ts);

-- Durable Workflow Queue (Inngest-inspired)
CREATE TABLE IF NOT EXISTS queued_workflows (
    id TEXT PRIMARY KEY,
    workflow_id TEXT NOT NULL,
    workflow_name TEXT NOT NULL,
    queued_at TIMESTAMPTZ NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'pending',
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    error_message TEXT,
    task_run_id TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    max_retries INTEGER NOT NULL DEFAULT 3
);

CREATE INDEX IF NOT EXISTS idx_queued_workflows_status ON queued_workflows(status);
CREATE INDEX IF NOT EXISTS idx_queued_workflows_priority ON queued_workflows(priority DESC, queued_at ASC);

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

-- Learning Outcomes (task execution results for meta-optimizer)
CREATE TABLE IF NOT EXISTS learning_outcomes (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    status TEXT NOT NULL,
    duration_secs DOUBLE PRECISION,
    iterations INTEGER,
    strategy TEXT,
    tools_used TEXT,
    files_modified TEXT,
    error_type TEXT,
    error_message TEXT,
    feedback TEXT,
    workflow_architecture TEXT,
    context_embedding BYTEA,
    step_count BIGINT,
    verification_step_count BIGINT,
    agentic_step_count BIGINT,
    has_ui_bridge BOOLEAN DEFAULT false,
    total_tokens BIGINT,
    total_cost_usd DOUBLE PRECISION,
    composite_agentic_score DOUBLE PRECISION,
    technology_tags TEXT,
    domain_tags TEXT,
    complexity_tier TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_lo_task_id ON learning_outcomes(task_id);
CREATE INDEX IF NOT EXISTS idx_lo_status ON learning_outcomes(status);
CREATE INDEX IF NOT EXISTS idx_lo_created_at ON learning_outcomes(created_at);
CREATE INDEX IF NOT EXISTS idx_lo_strategy ON learning_outcomes(strategy);

-- Learning Patterns (identified patterns from task analysis)
CREATE TABLE IF NOT EXISTS learning_patterns (
    id TEXT PRIMARY KEY,
    pattern_type TEXT NOT NULL,
    description TEXT NOT NULL,
    confidence DOUBLE PRECISION NOT NULL,
    occurrences INTEGER NOT NULL DEFAULT 1,
    context TEXT,
    description_embedding BYTEA,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_lp_type ON learning_patterns(pattern_type);
CREATE INDEX IF NOT EXISTS idx_lp_confidence ON learning_patterns(confidence);

-- Q-Routing Table (Q-learning state-action values for architecture routing)
CREATE TABLE IF NOT EXISTS q_routing_table (
    state_key TEXT NOT NULL,
    action TEXT NOT NULL,
    q_value DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    visit_count INTEGER NOT NULL DEFAULT 0,
    last_updated TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (state_key, action)
);

-- Q-Routing Overrides (manual locks: force a state to use a specific architecture)
CREATE TABLE IF NOT EXISTS q_routing_overrides (
    state_key TEXT PRIMARY KEY,
    forced_action TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Workflow Execution State (current state machine position)
CREATE TABLE IF NOT EXISTS workflow_execution_state (
    execution_id TEXT PRIMARY KEY REFERENCES task_runs(id) ON DELETE CASCADE,
    workflow_type TEXT NOT NULL,
    state_name TEXT NOT NULL,
    state_data TEXT,
    phase TEXT,
    iteration INTEGER,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_wes_type ON workflow_execution_state(workflow_type);
CREATE INDEX IF NOT EXISTS idx_wes_state ON workflow_execution_state(state_name);

-- Workflow Step Checkpoints (step-level checkpointing for resume)
CREATE TABLE IF NOT EXISTS workflow_step_checkpoints (
    id TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    workflow_type TEXT NOT NULL,
    phase TEXT NOT NULL,
    iteration INTEGER,
    step_index INTEGER NOT NULL,
    step_type TEXT NOT NULL,
    step_name TEXT,
    status TEXT NOT NULL,
    result_json TEXT,
    step_config_json TEXT,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    duration_ms INTEGER,
    error TEXT,
    stage_index INTEGER DEFAULT 0,
    UNIQUE(execution_id, phase, iteration, step_index, stage_index)
);

CREATE INDEX IF NOT EXISTS idx_wsc_execution ON workflow_step_checkpoints(execution_id);
CREATE INDEX IF NOT EXISTS idx_wsc_lookup ON workflow_step_checkpoints(execution_id, phase, iteration);
CREATE INDEX IF NOT EXISTS idx_wsc_status ON workflow_step_checkpoints(status);
CREATE INDEX IF NOT EXISTS idx_wsc_cursor ON workflow_step_checkpoints(execution_id, step_index);

-- Step Progress Markers (intra-step progress tracking)
CREATE TABLE IF NOT EXISTS step_progress_markers (
    id BIGSERIAL PRIMARY KEY,
    checkpoint_id TEXT NOT NULL REFERENCES workflow_step_checkpoints(id) ON DELETE CASCADE,
    marker_type TEXT NOT NULL,
    current_value INTEGER NOT NULL,
    total_value INTEGER,
    description TEXT,
    data_json TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_spm_checkpoint ON step_progress_markers(checkpoint_id);

-- UI Bridge Elements (element snapshots from pages)
CREATE TABLE IF NOT EXISTS ui_bridge_elements (
    id BIGSERIAL PRIMARY KEY,
    task_run_id BIGINT,
    timestamp BIGINT NOT NULL,
    element_id TEXT NOT NULL,
    tag_name TEXT,
    element_type TEXT,
    bounds TEXT,
    visible BOOLEAN DEFAULT true,
    enabled BOOLEAN DEFAULT true,
    focused BOOLEAN DEFAULT false,
    value TEXT,
    text_content TEXT,
    label TEXT,
    role TEXT,
    parent_element_id TEXT,
    page_url TEXT,
    selector TEXT,
    state_ids TEXT,
    metadata TEXT
);

CREATE INDEX IF NOT EXISTS idx_ube_task_run ON ui_bridge_elements(task_run_id);
CREATE INDEX IF NOT EXISTS idx_ube_element_id ON ui_bridge_elements(element_id);
CREATE INDEX IF NOT EXISTS idx_ube_timestamp ON ui_bridge_elements(timestamp);

-- UI Bridge Events (action and state change timeline)
CREATE TABLE IF NOT EXISTS ui_bridge_events (
    id BIGSERIAL PRIMARY KEY,
    task_run_id BIGINT,
    timestamp BIGINT NOT NULL,
    sequence BIGINT NOT NULL,
    event_type TEXT NOT NULL,
    element_id TEXT,
    state_id TEXT,
    transition_id TEXT,
    action TEXT,
    params TEXT,
    result TEXT,
    duration_ms DOUBLE PRECISION,
    success BOOLEAN DEFAULT true,
    error_message TEXT,
    metadata TEXT
);

CREATE INDEX IF NOT EXISTS idx_ubev_task_run ON ui_bridge_events(task_run_id);
CREATE INDEX IF NOT EXISTS idx_ubev_type ON ui_bridge_events(event_type);
CREATE INDEX IF NOT EXISTS idx_ubev_timestamp ON ui_bridge_events(timestamp);
CREATE INDEX IF NOT EXISTS idx_ubev_element ON ui_bridge_events(element_id);
CREATE INDEX IF NOT EXISTS idx_ubev_state ON ui_bridge_events(state_id);

-- UI Bridge Navigation History (path execution records)
CREATE TABLE IF NOT EXISTS ui_bridge_navigation_history (
    id BIGSERIAL PRIMARY KEY,
    task_run_id BIGINT,
    timestamp BIGINT NOT NULL,
    target_states TEXT NOT NULL,
    path_found BOOLEAN NOT NULL,
    transitions_planned TEXT,
    transitions_executed TEXT,
    total_cost DOUBLE PRECISION,
    duration_ms DOUBLE PRECISION,
    success BOOLEAN DEFAULT false,
    final_active_states TEXT,
    error_message TEXT
);

CREATE INDEX IF NOT EXISTS idx_ubnav_task_run ON ui_bridge_navigation_history(task_run_id);

-- Stall Events (stall detection and intervention tracking)
CREATE TABLE IF NOT EXISTS stall_events (
    id TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text,
    task_run_id TEXT NOT NULL,
    iteration INTEGER NOT NULL,
    pattern_type TEXT NOT NULL,
    pattern_details TEXT,
    action_count INTEGER,
    intervention_action TEXT,
    intervention_result TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_stall_task_run ON stall_events(task_run_id);
CREATE INDEX IF NOT EXISTS idx_stall_pattern ON stall_events(pattern_type);

-- Shell Commands (reusable command definitions)
CREATE TABLE IF NOT EXISTS shell_commands (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    command TEXT NOT NULL,
    working_directory TEXT,
    timeout_seconds INTEGER,
    fail_on_error BOOLEAN NOT NULL DEFAULT true,
    category TEXT DEFAULT 'general',
    tags TEXT DEFAULT '[]',
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_sc_category ON shell_commands(category);
CREATE INDEX IF NOT EXISTS idx_sc_enabled ON shell_commands(enabled);

-- Saved API Requests (reusable API request templates)
CREATE TABLE IF NOT EXISTS saved_api_requests (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    category TEXT DEFAULT 'general',
    tags TEXT DEFAULT '[]',
    method TEXT NOT NULL DEFAULT 'GET',
    url TEXT NOT NULL,
    headers TEXT DEFAULT '{}',
    body TEXT,
    body_content_type TEXT DEFAULT 'application/json',
    timeout_ms INTEGER DEFAULT 30000,
    follow_redirects BOOLEAN DEFAULT true,
    variable_extractions TEXT DEFAULT '[]',
    assertions TEXT DEFAULT '[]',
    credential_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_sar_category ON saved_api_requests(category);

-- MCP Servers (external tool server configurations)
CREATE TABLE IF NOT EXISTS mcp_servers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    transport TEXT NOT NULL,
    stdio_config TEXT,
    http_config TEXT,
    enabled BOOLEAN NOT NULL DEFAULT true,
    auto_start BOOLEAN NOT NULL DEFAULT false,
    timeout_seconds INTEGER NOT NULL DEFAULT 30,
    cached_tools TEXT,
    tools_cached_at TIMESTAMPTZ,
    connection_state TEXT NOT NULL DEFAULT 'disconnected',
    last_connected_at TIMESTAMPTZ,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_mcp_enabled ON mcp_servers(enabled);

-- Checks (verification check definitions)
CREATE TABLE IF NOT EXISTS checks (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    check_type TEXT NOT NULL,
    tool TEXT NOT NULL,
    command TEXT,
    working_directory TEXT,
    config_path TEXT,
    auto_fix BOOLEAN NOT NULL DEFAULT false,
    fail_on_warning BOOLEAN NOT NULL DEFAULT false,
    timeout_seconds INTEGER,
    is_critical BOOLEAN NOT NULL DEFAULT false,
    enabled BOOLEAN NOT NULL DEFAULT true,
    ai_generated BOOLEAN NOT NULL DEFAULT false,
    ai_generation_prompt TEXT,
    tags TEXT DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_checks_type ON checks(check_type);
CREATE INDEX IF NOT EXISTS idx_checks_tool ON checks(tool);
CREATE INDEX IF NOT EXISTS idx_checks_enabled ON checks(enabled);

-- Check Groups (organize checks into reusable groups)
CREATE TABLE IF NOT EXISTS check_groups (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    color TEXT,
    enabled BOOLEAN NOT NULL DEFAULT true,
    run_in_parallel BOOLEAN NOT NULL DEFAULT false,
    stop_on_failure BOOLEAN NOT NULL DEFAULT true,
    tags TEXT DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_cg_enabled ON check_groups(enabled);

-- Check Group Members (many-to-many)
CREATE TABLE IF NOT EXISTS check_group_members (
    id TEXT PRIMARY KEY,
    group_id TEXT NOT NULL REFERENCES check_groups(id) ON DELETE CASCADE,
    check_id TEXT NOT NULL REFERENCES checks(id) ON DELETE CASCADE,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(group_id, check_id)
);

CREATE INDEX IF NOT EXISTS idx_cgm_group ON check_group_members(group_id);
CREATE INDEX IF NOT EXISTS idx_cgm_check ON check_group_members(check_id);

-- User Skills (custom and auto-generated skill library)
CREATE TABLE IF NOT EXISTS user_skills (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    description TEXT DEFAULT '',
    category TEXT DEFAULT 'custom',
    tags TEXT DEFAULT '[]',
    icon TEXT DEFAULT 'puzzle',
    color TEXT DEFAULT 'gray',
    allowed_phases TEXT NOT NULL DEFAULT '["setup"]',
    parameters TEXT DEFAULT '[]',
    template TEXT NOT NULL,
    source TEXT NOT NULL DEFAULT 'user',
    version TEXT DEFAULT '1.0.0',
    author TEXT,
    checksum TEXT,
    depends_on TEXT DEFAULT '[]',
    usage_count BIGINT DEFAULT 0,
    approval_status TEXT,
    forked_from TEXT,
    source_fix_id TEXT,
    source_pattern_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_us_slug ON user_skills(slug);
CREATE INDEX IF NOT EXISTS idx_us_category ON user_skills(category);
CREATE INDEX IF NOT EXISTS idx_us_updated_at ON user_skills(updated_at);
CREATE INDEX IF NOT EXISTS idx_us_source ON user_skills(source);

-- Approval Gates (human-in-the-loop workflow approvals)
CREATE TABLE IF NOT EXISTS approval_gates (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    iteration INTEGER NOT NULL,
    prompt TEXT NOT NULL,
    context_json TEXT DEFAULT '{}',
    action TEXT,
    comment TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_ag_task_run_id ON approval_gates(task_run_id);
CREATE INDEX IF NOT EXISTS idx_ag_status ON approval_gates(status);

-- Deferred Questions (non-blocking human-in-the-loop feedback for autonomous workflows)
CREATE TABLE IF NOT EXISTS deferred_questions (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    iteration INTEGER NOT NULL,
    question TEXT NOT NULL,
    context_json TEXT DEFAULT '{}',
    auto_decision_type TEXT NOT NULL,
    auto_decision_detail TEXT,
    confidence DOUBLE PRECISION NOT NULL,
    risk_level TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    git_checkpoint TEXT,
    contingent_iterations TEXT DEFAULT '[]',
    reviewer_comment TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    reviewed_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_dq_task_run_id ON deferred_questions(task_run_id);
CREATE INDEX IF NOT EXISTS idx_dq_status ON deferred_questions(status);

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
    -- Temporal validity: when this observation was/is considered true
    valid_from TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    valid_until TIMESTAMPTZ,  -- NULL = still valid (current knowledge)
    superseded_by BIGINT REFERENCES observations(id),
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
-- Temporal validity indexes
CREATE INDEX IF NOT EXISTS idx_obs_valid_from ON observations(valid_from) WHERE NOT is_deleted;
CREATE INDEX IF NOT EXISTS idx_obs_valid_until ON observations(valid_until) WHERE NOT is_deleted;
CREATE INDEX IF NOT EXISTS idx_obs_superseded ON observations(superseded_by) WHERE superseded_by IS NOT NULL;

-- Memory consolidation columns (importance-weighted decay & mental model synthesis)
-- These support the consolidation service that synthesizes raw observations into mental models.
ALTER TABLE observations ADD COLUMN IF NOT EXISTS importance DOUBLE PRECISION NOT NULL DEFAULT 0.5;
ALTER TABLE observations ADD COLUMN IF NOT EXISTS last_accessed_at TIMESTAMPTZ;
ALTER TABLE observations ADD COLUMN IF NOT EXISTS access_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE observations ADD COLUMN IF NOT EXISTS decay_rate DOUBLE PRECISION NOT NULL DEFAULT 0.1;
ALTER TABLE observations ADD COLUMN IF NOT EXISTS consolidated_from BIGINT[];  -- IDs of source observations
ALTER TABLE observations ADD COLUMN IF NOT EXISTS is_mental_model BOOLEAN NOT NULL DEFAULT false;

CREATE INDEX IF NOT EXISTS idx_obs_importance ON observations(importance) WHERE NOT is_deleted;
CREATE INDEX IF NOT EXISTS idx_obs_mental_model ON observations(is_mental_model) WHERE NOT is_deleted AND is_mental_model;
CREATE INDEX IF NOT EXISTS idx_obs_last_accessed ON observations(last_accessed_at) WHERE NOT is_deleted;

-- Track consolidation runs
CREATE TABLE IF NOT EXISTS memory_consolidation_log (
    id BIGSERIAL PRIMARY KEY,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    observations_scanned INTEGER NOT NULL DEFAULT 0,
    groups_found INTEGER NOT NULL DEFAULT 0,
    models_created INTEGER NOT NULL DEFAULT 0,
    observations_merged INTEGER NOT NULL DEFAULT 0,
    observations_decayed INTEGER NOT NULL DEFAULT 0,
    observations_archived INTEGER NOT NULL DEFAULT 0,
    error TEXT
);

-- Observation History (tracks prior versions of topic-keyed observations)
CREATE TABLE IF NOT EXISTS observation_history (
    id BIGSERIAL PRIMARY KEY,
    observation_id BIGINT NOT NULL REFERENCES observations(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    valid_from TIMESTAMPTZ NOT NULL,
    valid_until TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    revision_number INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_obs_history_obs_id ON observation_history(observation_id);
CREATE INDEX IF NOT EXISTS idx_obs_history_valid ON observation_history(valid_from, valid_until);

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

-- Agentic Metric Scores (per-metric scores for task runs)
CREATE TABLE IF NOT EXISTS agentic_metric_scores (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    metric_type TEXT NOT NULL,
    score DOUBLE PRECISION NOT NULL,
    confidence DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    rationale TEXT,
    is_llm_judged BOOLEAN NOT NULL DEFAULT false,
    model_used TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_ams_task_run ON agentic_metric_scores(task_run_id);
CREATE INDEX IF NOT EXISTS idx_ams_metric ON agentic_metric_scores(metric_type);
CREATE UNIQUE INDEX IF NOT EXISTS idx_ams_unique ON agentic_metric_scores(task_run_id, metric_type);

-- Prompt Registry (versioned prompt variants per agent type)
CREATE TABLE IF NOT EXISTS prompt_registry (
    id TEXT PRIMARY KEY,
    agent_type TEXT NOT NULL,
    variant_name TEXT NOT NULL,
    prompt_content TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    is_active BOOLEAN NOT NULL DEFAULT false,
    source_recommendation_id TEXT,
    performance_metrics TEXT DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(agent_type, variant_name, version)
);

CREATE INDEX IF NOT EXISTS idx_pr_agent_type ON prompt_registry(agent_type);
CREATE INDEX IF NOT EXISTS idx_pr_active ON prompt_registry(agent_type, is_active);

-- Meta-Optimizer Recommendations (optimizer output — human-reviewed from UI)
CREATE TABLE IF NOT EXISTS meta_optimizer_recommendations (
    id TEXT PRIMARY KEY,
    optimizer_type TEXT NOT NULL,
    recommendation_type TEXT NOT NULL,
    target_agent TEXT,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    current_value TEXT DEFAULT '{}',
    recommended_value TEXT DEFAULT '{}',
    evidence TEXT DEFAULT '{}',
    confidence DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    status TEXT NOT NULL DEFAULT 'pending',
    applied_at TIMESTAMPTZ,
    outcome_after_apply TEXT,
    optimizer_run_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    content_hash TEXT,
    eval_result_id TEXT,
    eval_status TEXT
);

CREATE INDEX IF NOT EXISTS idx_mor_type ON meta_optimizer_recommendations(optimizer_type);
CREATE INDEX IF NOT EXISTS idx_mor_status ON meta_optimizer_recommendations(status);
CREATE INDEX IF NOT EXISTS idx_mor_run ON meta_optimizer_recommendations(optimizer_run_id);
CREATE INDEX IF NOT EXISTS idx_mor_content_hash ON meta_optimizer_recommendations(content_hash);

-- Canary Rollouts (gradual rollout of optimizer recommendations)
CREATE TABLE IF NOT EXISTS canary_rollouts (
    id TEXT PRIMARY KEY,
    recommendation_id TEXT NOT NULL,
    percentage BIGINT NOT NULL DEFAULT 10,
    status TEXT NOT NULL DEFAULT 'active',
    start_date TIMESTAMPTZ NOT NULL,
    end_date TIMESTAMPTZ,
    baseline_run_count BIGINT DEFAULT 0,
    canary_run_count BIGINT DEFAULT 0,
    baseline_metrics_json TEXT DEFAULT '{}',
    canary_metrics_json TEXT DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_cr_status ON canary_rollouts(status);
CREATE INDEX IF NOT EXISTS idx_cr_rec ON canary_rollouts(recommendation_id);

-- Canary Run Records (individual run results within a canary rollout)
CREATE TABLE IF NOT EXISTS canary_run_records (
    id TEXT PRIMARY KEY,
    canary_id TEXT NOT NULL,
    is_canary BOOLEAN NOT NULL,
    task_run_id TEXT,
    success BOOLEAN NOT NULL,
    cost_usd DOUBLE PRECISION DEFAULT 0.0,
    duration_ms DOUBLE PRECISION DEFAULT 0.0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_crr_canary ON canary_run_records(canary_id, is_canary);

-- Prompt Template Canaries (A/B testing for prompt template versions)
CREATE TABLE IF NOT EXISTS prompt_template_canaries (
    id TEXT PRIMARY KEY,
    template_id TEXT NOT NULL,
    baseline_version INTEGER NOT NULL,
    candidate_version INTEGER NOT NULL,
    traffic_percentage DOUBLE PRECISION NOT NULL DEFAULT 0.1,
    status TEXT NOT NULL DEFAULT 'active',
    baseline_metrics_json TEXT NOT NULL DEFAULT '{}',
    candidate_metrics_json TEXT NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ended_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_ptc_template ON prompt_template_canaries(template_id);
CREATE INDEX IF NOT EXISTS idx_ptc_status ON prompt_template_canaries(status);

-- ============================================================================
-- Task Knowledge (knowledge acquisition flywheel)
-- ============================================================================

-- Error Events (application log error detection)
CREATE TABLE IF NOT EXISTS error_events (
    id BIGSERIAL PRIMARY KEY,

    -- Source identification
    log_source_id BIGINT,
    log_source_name TEXT NOT NULL,

    -- Workflow context (optional - only set during workflow runs)
    task_run_id TEXT REFERENCES task_runs(id) ON DELETE SET NULL,
    workflow_step_id TEXT,

    -- Timing
    log_timestamp TIMESTAMPTZ,
    captured_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Error classification
    severity TEXT NOT NULL DEFAULT 'error',
    error_type TEXT,
    error_code TEXT,

    -- Error content
    message TEXT NOT NULL,
    stack_trace TEXT,
    context_lines TEXT,
    raw_entry TEXT,

    -- Location (if parseable from stack trace)
    file_path TEXT,
    line_number INTEGER,
    column_number INTEGER,
    function_name TEXT,

    -- Deduplication and tracking
    signature_hash TEXT NOT NULL,
    occurrence_count INTEGER DEFAULT 1,
    first_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Status lifecycle
    status TEXT DEFAULT 'new',

    -- Debug agent integration
    finding_id TEXT,  -- Soft FK to task_run_findings(id)
    resolved_by_task_run_id TEXT,
    resolved_by_fix_id TEXT,
    resolution_notes TEXT,

    -- Embedding vector for hybrid RAG search (384-dim MiniLM as f32)
    message_embedding BYTEA,

    -- Cross-service trace correlation
    trace_id TEXT,

    -- Status timestamps
    acknowledged_at TIMESTAMPTZ,
    resolved_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_error_events_log_source ON error_events(log_source_id);
CREATE INDEX IF NOT EXISTS idx_error_events_task_run ON error_events(task_run_id);
CREATE INDEX IF NOT EXISTS idx_error_events_signature ON error_events(signature_hash);
CREATE INDEX IF NOT EXISTS idx_error_events_status ON error_events(status);
CREATE INDEX IF NOT EXISTS idx_error_events_severity ON error_events(severity);
CREATE INDEX IF NOT EXISTS idx_error_events_captured ON error_events(captured_at DESC);
CREATE INDEX IF NOT EXISTS idx_error_events_last_seen ON error_events(last_seen_at DESC);
CREATE INDEX IF NOT EXISTS idx_error_events_source_name ON error_events(log_source_name);
CREATE INDEX IF NOT EXISTS idx_error_events_trace_id ON error_events(trace_id);

-- Task Run Findings (detected issues within a task run)
CREATE TABLE IF NOT EXISTS task_run_findings (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    category TEXT NOT NULL,
    severity TEXT NOT NULL,
    signature_hash TEXT,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    file_path TEXT,
    line_number INTEGER,
    column_number INTEGER,
    code_snippet TEXT,
    status TEXT NOT NULL DEFAULT 'detected',
    action_type TEXT NOT NULL DEFAULT 'auto_fix',
    resolution TEXT,
    detected_in_session INTEGER NOT NULL,
    resolved_in_session INTEGER,
    needs_input BOOLEAN DEFAULT false,
    question TEXT,
    input_options TEXT,
    user_response TEXT,
    title_embedding BYTEA,
    description_embedding BYTEA,
    reflection_fix_id TEXT,
    detected_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_findings_task_run ON task_run_findings(task_run_id);
CREATE INDEX IF NOT EXISTS idx_findings_status ON task_run_findings(status);
CREATE INDEX IF NOT EXISTS idx_findings_signature ON task_run_findings(signature_hash);
CREATE INDEX IF NOT EXISTS idx_findings_category ON task_run_findings(category);

CREATE TABLE IF NOT EXISTS task_knowledge (
    id                      TEXT PRIMARY KEY,
    task_run_id             TEXT NOT NULL,
    category                TEXT NOT NULL,
    agent_type              TEXT NOT NULL DEFAULT 'system',
    iteration               INTEGER NOT NULL DEFAULT 0,
    content                 TEXT NOT NULL,
    evidence                TEXT,
    confidence              TEXT NOT NULL DEFAULT 'medium',
    related_files           TEXT NOT NULL DEFAULT '[]',
    related_criterion_id    TEXT,
    is_resolved             BOOLEAN NOT NULL DEFAULT false,
    resolution_notes        TEXT,
    resolved_at             TIMESTAMPTZ,
    archived_at             TIMESTAMPTZ,
    summary_entry_id        TEXT,
    content_embedding       BYTEA,
    project_path            TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_tk_task_run ON task_knowledge(task_run_id);
CREATE INDEX IF NOT EXISTS idx_tk_category ON task_knowledge(category);
CREATE INDEX IF NOT EXISTS idx_tk_resolved ON task_knowledge(is_resolved) WHERE NOT is_resolved;
CREATE INDEX IF NOT EXISTS idx_tk_created ON task_knowledge(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_tk_project_path ON task_knowledge(project_path) WHERE project_path IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_tk_active ON task_knowledge(task_run_id, category) WHERE archived_at IS NULL;

-- ============================================================================
-- Knowledge Graph Tables (workflow improvement system)
-- ============================================================================

-- Workflow version history — tracks evolution across regenerations and reflection fixes
CREATE TABLE IF NOT EXISTS workflow_versions (
    id TEXT PRIMARY KEY,
    workflow_id TEXT NOT NULL REFERENCES unified_workflows(id) ON DELETE CASCADE,
    version_number INTEGER NOT NULL,
    parent_version_id TEXT REFERENCES workflow_versions(id) ON DELETE SET NULL,
    generation_task_run_id TEXT REFERENCES task_runs(id) ON DELETE SET NULL,
    workflow_json TEXT NOT NULL,
    diff_summary TEXT,
    diff_json TEXT,
    trigger TEXT NOT NULL DEFAULT 'manual',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(workflow_id, version_number)
);
CREATE INDEX IF NOT EXISTS idx_wv_workflow ON workflow_versions(workflow_id);
CREATE INDEX IF NOT EXISTS idx_wv_task_run ON workflow_versions(generation_task_run_id);

-- Step-level finding attribution — links findings to the steps that produced them
CREATE TABLE IF NOT EXISTS step_finding_links (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    step_name TEXT NOT NULL,
    step_index INTEGER NOT NULL,
    finding_id TEXT NOT NULL,
    link_type TEXT NOT NULL DEFAULT 'detected_during',
    confidence REAL NOT NULL DEFAULT 1.0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_sfl_task_run ON step_finding_links(task_run_id);
CREATE INDEX IF NOT EXISTS idx_sfl_step ON step_finding_links(step_name);
CREATE INDEX IF NOT EXISTS idx_sfl_finding ON step_finding_links(finding_id);

-- Per-step generation agent tracking — which pipeline agent created each step
CREATE TABLE IF NOT EXISTS step_provenance (
    id TEXT PRIMARY KEY,
    workflow_id TEXT NOT NULL REFERENCES unified_workflows(id) ON DELETE CASCADE,
    workflow_version_id TEXT REFERENCES workflow_versions(id) ON DELETE SET NULL,
    step_name TEXT NOT NULL,
    step_index INTEGER NOT NULL,
    phase TEXT NOT NULL,
    generating_agent TEXT NOT NULL,
    generation_iteration INTEGER,
    original_step_json TEXT,
    final_step_json TEXT,
    ui_bridge_event_ids TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_sp_workflow ON step_provenance(workflow_id);
CREATE INDEX IF NOT EXISTS idx_sp_agent ON step_provenance(generating_agent);

-- Generation pipeline telemetry — per-phase events with timing and validation data
CREATE TABLE IF NOT EXISTS generation_pipeline_events (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    workflow_id TEXT REFERENCES unified_workflows(id) ON DELETE SET NULL,
    event_type TEXT NOT NULL,
    phase TEXT,
    iteration INTEGER,
    payload TEXT,
    duration_ms BIGINT,
    token_count BIGINT,
    validation_errors_before INTEGER,
    validation_errors_after INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_gpe_task_run ON generation_pipeline_events(task_run_id);
CREATE INDEX IF NOT EXISTS idx_gpe_type ON generation_pipeline_events(event_type);
CREATE INDEX IF NOT EXISTS idx_gpe_phase ON generation_pipeline_events(phase);

-- Rule influence tracking — which generation rules were active during each generation
CREATE TABLE IF NOT EXISTS rule_influence_log (
    id TEXT PRIMARY KEY,
    rule_id TEXT NOT NULL,
    task_run_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    workflow_id TEXT REFERENCES unified_workflows(id) ON DELETE SET NULL,
    influence_type TEXT NOT NULL DEFAULT 'loaded',
    evidence TEXT,
    phase TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_ril_rule ON rule_influence_log(rule_id);
CREATE INDEX IF NOT EXISTS idx_ril_task_run ON rule_influence_log(task_run_id);
CREATE INDEX IF NOT EXISTS idx_ril_influence ON rule_influence_log(influence_type);

-- Cross-run pattern detection — recurring findings and fix oscillations
CREATE TABLE IF NOT EXISTS cross_run_patterns (
    id TEXT PRIMARY KEY,
    pattern_type TEXT NOT NULL,
    signature_hash TEXT NOT NULL,
    workflow_name TEXT,
    occurrence_count INTEGER NOT NULL DEFAULT 1,
    first_seen_task_run_id TEXT,
    last_seen_task_run_id TEXT,
    first_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    affected_components TEXT,
    pattern_data TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    resolved_by_fix_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(pattern_type, signature_hash)
);
CREATE INDEX IF NOT EXISTS idx_crp_type ON cross_run_patterns(pattern_type);
CREATE INDEX IF NOT EXISTS idx_crp_workflow ON cross_run_patterns(workflow_name);
CREATE INDEX IF NOT EXISTS idx_crp_status ON cross_run_patterns(status);
CREATE INDEX IF NOT EXISTS idx_crp_signature ON cross_run_patterns(signature_hash);

-- Prompt Evolution (meta-prompt optimizer history)
CREATE TABLE IF NOT EXISTS prompt_evolution (
    id TEXT PRIMARY KEY,
    agent_type TEXT NOT NULL,
    parent_variant_id TEXT,
    variant_id TEXT NOT NULL,
    recommendation_id TEXT,
    critique TEXT,
    changes_summary TEXT,
    canary_verdict TEXT,
    score_before DOUBLE PRECISION,
    score_after DOUBLE PRECISION,
    baseline_prompt_hash TEXT,
    consecutive_rejections INTEGER DEFAULT 0,
    beam_run_id TEXT,
    generation INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_pe_agent ON prompt_evolution(agent_type);
CREATE INDEX IF NOT EXISTS idx_pe_verdict ON prompt_evolution(agent_type, canary_verdict);
CREATE INDEX IF NOT EXISTS idx_pe_variant ON prompt_evolution(variant_id);

-- Workflow Verification Phase Results
CREATE TABLE IF NOT EXISTS workflow_verification_phase_results (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    iteration INTEGER NOT NULL,
    all_passed BOOLEAN NOT NULL,
    total_steps INTEGER NOT NULL,
    passed_steps INTEGER NOT NULL,
    failed_steps INTEGER NOT NULL,
    skipped_steps INTEGER NOT NULL,
    total_duration_ms BIGINT NOT NULL,
    critical_failure BOOLEAN NOT NULL DEFAULT false,
    result_json TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_wf_ver_phase_task_run ON workflow_verification_phase_results(task_run_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_wf_ver_phase_unique ON workflow_verification_phase_results(task_run_id, iteration);

-- Workflow AI Sessions
CREATE TABLE IF NOT EXISTS workflow_ai_sessions (
    id BIGSERIAL PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    iteration INTEGER NOT NULL,
    phase TEXT NOT NULL,
    stage_index INTEGER,
    claude_cli_session_id TEXT,
    session_started_at TIMESTAMPTZ NOT NULL,
    session_completed_at TIMESTAMPTZ,
    output_length INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'running',
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_wf_ai_sessions_task_run ON workflow_ai_sessions(task_run_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_wf_ai_sessions_unique
    ON workflow_ai_sessions(task_run_id, iteration, phase, COALESCE(stage_index, -1));

-- Worktrees
CREATE TABLE IF NOT EXISTS worktrees (
    id TEXT PRIMARY KEY,
    worktree_path TEXT NOT NULL,
    branch_name TEXT NOT NULL,
    source_branch TEXT NOT NULL,
    source_commit TEXT NOT NULL,
    repo_path TEXT NOT NULL,
    task_run_id TEXT,
    workflow_name TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_worktrees_status ON worktrees(status);
CREATE INDEX IF NOT EXISTS idx_worktrees_task_run ON worktrees(task_run_id);

-- Workflow Constraint Results
CREATE TABLE IF NOT EXISTS workflow_constraint_results (
    id BIGSERIAL PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    iteration INTEGER NOT NULL,
    constraint_id TEXT NOT NULL,
    constraint_name TEXT NOT NULL,
    passed BOOLEAN NOT NULL,
    severity TEXT NOT NULL,
    violations_json TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_wf_constraint_task_run ON workflow_constraint_results(task_run_id);
CREATE INDEX IF NOT EXISTS idx_wf_constraint_iteration ON workflow_constraint_results(iteration);

-- Sessions (orchestration session tracking)
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    session_type TEXT NOT NULL,
    name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'starting',
    current_phase INTEGER NOT NULL DEFAULT 0,
    total_phases INTEGER NOT NULL DEFAULT 0,
    completed BOOLEAN NOT NULL DEFAULT false,
    restart_permitted BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    error_message TEXT,
    custom_data TEXT DEFAULT '{}',
    activity_log TEXT DEFAULT '[]',
    run_id TEXT,
    workflow_name TEXT
);
CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);
CREATE INDEX IF NOT EXISTS idx_sessions_workflow_name ON sessions(workflow_name);
CREATE INDEX IF NOT EXISTS idx_sessions_run_id ON sessions(run_id);
CREATE INDEX IF NOT EXISTS idx_sessions_created_at ON sessions(created_at);

-- Session Events
CREATE TABLE IF NOT EXISTS session_events (
    id BIGSERIAL PRIMARY KEY,
    session_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    message TEXT NOT NULL,
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_session_events_session_id ON session_events(session_id);

-- Task Run Mobile State
CREATE TABLE IF NOT EXISTS task_run_mobile_state (
    id BIGSERIAL PRIMARY KEY,
    task_run_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    device_id TEXT,
    device_type TEXT,
    device_model TEXT,
    app_package TEXT,
    app_activity TEXT,
    app_state TEXT,
    metro_connected BOOLEAN DEFAULT false,
    bundle_status TEXT,
    last_reload_type TEXT,
    last_reload_time TEXT,
    screenshot_path TEXT,
    logcat_path TEXT,
    has_errors BOOLEAN DEFAULT false,
    error_summary TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_mobile_state_task_run ON task_run_mobile_state(task_run_id);
CREATE INDEX IF NOT EXISTS idx_mobile_state_timestamp ON task_run_mobile_state(timestamp);

-- Task Run Mobile Logs
CREATE TABLE IF NOT EXISTS task_run_mobile_logs (
    id BIGSERIAL PRIMARY KEY,
    task_run_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    mobile_state_id BIGINT,
    log_source TEXT NOT NULL,
    log_level TEXT,
    log_tag TEXT,
    message TEXT NOT NULL,
    raw_line TEXT,
    data TEXT,
    error_type TEXT,
    error_code TEXT,
    stack_trace TEXT,
    file_path TEXT,
    line_number INTEGER,
    column_number INTEGER,
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    device_timestamp TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_mobile_logs_task_run ON task_run_mobile_logs(task_run_id);
CREATE INDEX IF NOT EXISTS idx_mobile_logs_source ON task_run_mobile_logs(log_source);

-- Prompts
CREATE TABLE IF NOT EXISTS prompts (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    category TEXT NOT NULL DEFAULT 'general',
    content TEXT NOT NULL,
    variables TEXT NOT NULL DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_prompts_name ON prompts(name);

-- Verification Tests
CREATE TABLE IF NOT EXISTS verification_tests (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    workflow_id TEXT,
    test_type TEXT NOT NULL DEFAULT 'python_script',
    command TEXT,
    expected_exit_code INTEGER DEFAULT 0,
    expected_output TEXT,
    timeout_seconds INTEGER DEFAULT 60,
    enabled BOOLEAN NOT NULL DEFAULT true,
    tags TEXT DEFAULT '[]',
    -- Rich-test columns added in v7 migration (Bug 9)
    category TEXT,
    playwright_code TEXT,
    vision_config TEXT,
    python_code TEXT,
    repo_test_config TEXT,
    success_criteria TEXT,
    config TEXT NOT NULL DEFAULT '{}',
    is_critical BOOLEAN NOT NULL DEFAULT false,
    ai_generated BOOLEAN NOT NULL DEFAULT false,
    ai_generation_prompt TEXT,
    creation_analysis TEXT,
    source_file TEXT,
    last_exported_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_verification_tests_workflow ON verification_tests(workflow_id);
CREATE INDEX IF NOT EXISTS idx_verification_tests_category ON verification_tests(category) WHERE category IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_verification_tests_enabled ON verification_tests(enabled) WHERE enabled;

-- Task Hooks
CREATE TABLE IF NOT EXISTS task_hooks (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    hook_type TEXT NOT NULL,
    trigger_event TEXT NOT NULL,
    command TEXT NOT NULL,
    working_directory TEXT,
    timeout_seconds INTEGER DEFAULT 30,
    enabled BOOLEAN NOT NULL DEFAULT true,
    workflow_filter TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_task_hooks_trigger ON task_hooks(trigger_event);

-- Scheduled Tasks
CREATE TABLE IF NOT EXISTS scheduled_tasks (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    enabled BOOLEAN NOT NULL DEFAULT true,
    schedule_type TEXT NOT NULL DEFAULT 'cron',
    schedule_value TEXT NOT NULL DEFAULT '',
    task_config TEXT NOT NULL DEFAULT '{}',
    skip_if_completed BOOLEAN NOT NULL DEFAULT false,
    auto_fix_on_failure BOOLEAN NOT NULL DEFAULT false,
    success_criteria TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    modified_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    next_run TIMESTAMPTZ,
    last_run_id TEXT,
    condition_status TEXT
);
CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_enabled ON scheduled_tasks(enabled);

-- Active Workflows (checkpoint storage)
CREATE TABLE IF NOT EXISTS active_workflows (
    id              BIGSERIAL PRIMARY KEY,
    workflow_name   TEXT NOT NULL UNIQUE,
    checkpoint_data TEXT NOT NULL,
    run_id          TEXT NOT NULL,
    phase_field     TEXT NOT NULL DEFAULT 'current_phase',
    completion_value INTEGER NOT NULL DEFAULT 1,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed       BOOLEAN NOT NULL DEFAULT false
);

-- Orchestrator Flows
CREATE TABLE IF NOT EXISTS orchestrator_flows (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    description     TEXT,
    steps           TEXT NOT NULL DEFAULT '[]',
    start_step      TEXT,
    timeout_secs    INTEGER,
    inputs          TEXT,
    outputs         TEXT,
    tags            TEXT DEFAULT '[]',
    version         TEXT NOT NULL DEFAULT '1.0.0',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_orch_flows_name ON orchestrator_flows(name);

-- Flow Executions
CREATE TABLE IF NOT EXISTS flow_executions (
    instance_id     TEXT PRIMARY KEY,
    flow_id         TEXT NOT NULL,
    current_step    TEXT,
    status          TEXT NOT NULL DEFAULT 'pending',
    context         TEXT,
    history         TEXT,
    error           TEXT,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at    TIMESTAMPTZ,
    FOREIGN KEY (flow_id) REFERENCES orchestrator_flows(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_flow_exec_flow ON flow_executions(flow_id);
CREATE INDEX IF NOT EXISTS idx_flow_exec_status ON flow_executions(status);

-- Flow Versions
CREATE TABLE IF NOT EXISTS flow_versions (
    id              TEXT PRIMARY KEY,
    flow_id         TEXT NOT NULL,
    version         INTEGER NOT NULL,
    definition      TEXT NOT NULL,
    message         TEXT,
    created_by      TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (flow_id) REFERENCES orchestrator_flows(id) ON DELETE CASCADE,
    UNIQUE(flow_id, version)
);
CREATE INDEX IF NOT EXISTS idx_flow_versions_flow_id ON flow_versions(flow_id);
CREATE INDEX IF NOT EXISTS idx_flow_versions_flow_version ON flow_versions(flow_id, version);

-- Orchestrator Checkpoints
CREATE TABLE IF NOT EXISTS orchestrator_checkpoints (
    id              TEXT PRIMARY KEY,
    task_id         TEXT NOT NULL,
    iteration       INTEGER NOT NULL DEFAULT 0,
    trigger         TEXT NOT NULL,
    state           TEXT NOT NULL DEFAULT '{}',
    name            TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_orch_checkpoints_task ON orchestrator_checkpoints(task_id);
CREATE INDEX IF NOT EXISTS idx_orch_checkpoints_task_iter ON orchestrator_checkpoints(task_id, iteration);

-- API Surface Snapshots — stores scan results for diff comparison
CREATE TABLE IF NOT EXISTS api_surface_snapshots (
    id              BIGSERIAL PRIMARY KEY,
    scan_json       TEXT NOT NULL,
    summary         TEXT NOT NULL,
    total_endpoints INTEGER NOT NULL,
    orphan_count    INTEGER NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_api_surface_snapshots_created ON api_surface_snapshots(created_at DESC);

-- Decision Trail (architectural decision history)
CREATE TABLE IF NOT EXISTS decisions (
    id              TEXT PRIMARY KEY,
    timestamp       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    scale           TEXT NOT NULL,                          -- 'tactical' | 'strategic'
    category        TEXT NOT NULL,                          -- 'architecture' | 'technology' | 'design' | 'integration' | 'performance' | 'security' | 'ux' | 'data-model'
    status          TEXT NOT NULL DEFAULT 'active',         -- 'active' | 'superseded' | 'reversed'
    title           TEXT NOT NULL,
    summary         TEXT NOT NULL,
    rationale       TEXT NOT NULL,
    alternatives_json       TEXT NOT NULL DEFAULT '[]',
    tradeoffs_json          TEXT NOT NULL DEFAULT '[]',
    triggered_by            TEXT,
    inspiration_json        TEXT,
    related_decisions_json  TEXT NOT NULL DEFAULT '[]',
    affected_files_json     TEXT NOT NULL DEFAULT '[]',
    affected_endpoints_json TEXT NOT NULL DEFAULT '[]',
    affected_tables_json    TEXT NOT NULL DEFAULT '[]',
    created_by              TEXT,
    superseded_by           TEXT,
    tags_json               TEXT NOT NULL DEFAULT '[]',
    is_deleted              BOOLEAN NOT NULL DEFAULT false,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_dec_timestamp ON decisions(timestamp);
CREATE INDEX IF NOT EXISTS idx_dec_category ON decisions(category);
CREATE INDEX IF NOT EXISTS idx_dec_scale ON decisions(scale);
CREATE INDEX IF NOT EXISTS idx_dec_status ON decisions(status) WHERE status = 'active';
CREATE INDEX IF NOT EXISTS idx_dec_fts ON decisions
    USING GIN (to_tsvector('english', title || ' ' || summary || ' ' || rationale))
    WHERE NOT is_deleted;

-- Concept Summaries (high-level feature narratives)
CREATE TABLE IF NOT EXISTS concept_summaries (
    id                      TEXT PRIMARY KEY,
    name                    TEXT NOT NULL,
    tagline                 TEXT NOT NULL,
    description             TEXT NOT NULL,
    inspiration_json        TEXT,
    benefits_json           TEXT NOT NULL DEFAULT '[]',
    components_json         TEXT NOT NULL DEFAULT '[]',
    related_decisions_json  TEXT NOT NULL DEFAULT '[]',
    metrics_json            TEXT,
    is_deleted              BOOLEAN NOT NULL DEFAULT false,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_cs_fts ON concept_summaries
    USING GIN (to_tsvector('english', name || ' ' || tagline || ' ' || description))
    WHERE NOT is_deleted;

-- Development Intelligence (coverage, complexity, and feature health trend tracking)
CREATE TABLE IF NOT EXISTS development_intelligence (
    id BIGSERIAL PRIMARY KEY,
    project_path TEXT NOT NULL,
    analysis_type TEXT NOT NULL,  -- 'coverage' | 'complexity' | 'health'
    page_route TEXT NOT NULL,
    score DOUBLE PRECISION,
    details_json TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_di_project ON development_intelligence(project_path, analysis_type);
CREATE INDEX IF NOT EXISTS idx_di_created ON development_intelligence(created_at);

-- Cached App Specs (discovered application specifications)
CREATE TABLE IF NOT EXISTS cached_app_specs (
    id TEXT PRIMARY KEY,
    app_url TEXT NOT NULL,
    app_name TEXT NOT NULL,
    spec_id TEXT NOT NULL,
    spec_json TEXT NOT NULL,
    discovered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    page_url TEXT
);
CREATE INDEX IF NOT EXISTS idx_cached_specs_app ON cached_app_specs(app_url);

-- Canvas Panels (task run visualization panels)
CREATE TABLE IF NOT EXISTS canvas_panels (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    component TEXT NOT NULL,
    title TEXT NOT NULL,
    data_json TEXT NOT NULL,
    priority INTEGER DEFAULT 50,
    size TEXT DEFAULT 'normal',
    group_name TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_canvas_panels_task_run_id ON canvas_panels(task_run_id);

-- Runner Instances (multi-instance coordination)
CREATE TABLE IF NOT EXISTS runner_instances (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    port           INTEGER NOT NULL UNIQUE,
    hostname       TEXT NOT NULL DEFAULT 'localhost',
    is_primary     BOOLEAN NOT NULL DEFAULT FALSE,
    pid            INTEGER,
    status         TEXT NOT NULL DEFAULT 'starting',
    last_heartbeat TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    running_tasks  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_ri_port ON runner_instances(port);
CREATE INDEX IF NOT EXISTS idx_ri_status ON runner_instances(status);
CREATE INDEX IF NOT EXISTS idx_ri_heartbeat ON runner_instances(last_heartbeat);

-- Process Sessions (managed process lifecycle tracking)
CREATE TABLE IF NOT EXISTS process_sessions (
    id TEXT PRIMARY KEY,
    process_config_id TEXT NOT NULL,
    process_name TEXT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    stopped_at TIMESTAMPTZ,
    exit_code INTEGER,
    state TEXT NOT NULL DEFAULT 'running',
    error_count INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_process_sessions_config_id ON process_sessions(process_config_id);
CREATE INDEX IF NOT EXISTS idx_process_sessions_started_at ON process_sessions(started_at);

-- Process Session Output (stdout/stderr lines from managed processes)
CREATE TABLE IF NOT EXISTS process_session_output (
    id BIGSERIAL PRIMARY KEY,
    session_id TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    stream TEXT NOT NULL,
    line TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES process_sessions(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_process_session_output_session ON process_session_output(session_id);

-- State Machine Configs (state machine definitions for UI automation)
CREATE TABLE IF NOT EXISTS state_machine_configs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL DEFAULT 'default',
    description TEXT,
    render_count INTEGER NOT NULL DEFAULT 0,
    element_count INTEGER NOT NULL DEFAULT 0,
    include_html_ids BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- State Machine States (individual states within a state machine config)
CREATE TABLE IF NOT EXISTS state_machine_states (
    id TEXT PRIMARY KEY,
    config_id TEXT NOT NULL REFERENCES state_machine_configs(id) ON DELETE CASCADE,
    state_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    element_ids TEXT NOT NULL DEFAULT '[]',
    render_ids TEXT NOT NULL DEFAULT '[]',
    confidence DOUBLE PRECISION NOT NULL DEFAULT 0.9,
    acceptance_criteria TEXT NOT NULL DEFAULT '[]',
    extra_metadata TEXT NOT NULL DEFAULT '{}',
    domain_knowledge TEXT NOT NULL DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_sm_states_config_id ON state_machine_states(config_id);

-- State Machine Transitions (transitions between states)
CREATE TABLE IF NOT EXISTS state_machine_transitions (
    id TEXT PRIMARY KEY,
    config_id TEXT NOT NULL REFERENCES state_machine_configs(id) ON DELETE CASCADE,
    transition_id TEXT NOT NULL,
    name TEXT NOT NULL,
    from_states TEXT NOT NULL DEFAULT '[]',
    activate_states TEXT NOT NULL DEFAULT '[]',
    exit_states TEXT NOT NULL DEFAULT '[]',
    actions TEXT NOT NULL DEFAULT '[]',
    path_cost DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    stays_visible BOOLEAN NOT NULL DEFAULT FALSE,
    extra_metadata TEXT NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_sm_transitions_config_id ON state_machine_transitions(config_id);

-- UI Bridge Integrations (projects with source integration)
CREATE TABLE IF NOT EXISTS ui_bridge_integrations (
    id TEXT PRIMARY KEY,
    project_path TEXT NOT NULL,
    label TEXT,
    framework TEXT,
    integration_type TEXT NOT NULL,
    sdk_version TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    target_url TEXT,
    last_health_check INTEGER,
    element_count INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ui_bridge_integrations_status ON ui_bridge_integrations(status);
CREATE INDEX IF NOT EXISTS idx_ui_bridge_integrations_type ON ui_bridge_integrations(integration_type);

-- UI Bridge States (registered UI states for navigation)
CREATE TABLE IF NOT EXISTS ui_bridge_states (
    id BIGSERIAL PRIMARY KEY,
    state_id TEXT UNIQUE NOT NULL,
    name TEXT NOT NULL,
    elements TEXT,
    blocking INTEGER DEFAULT 0,
    blocks TEXT,
    group_id TEXT,
    path_cost DOUBLE PRECISION DEFAULT 1.0,
    is_active INTEGER DEFAULT 0,
    active_when TEXT,
    metadata TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_ui_bridge_states_state_id ON ui_bridge_states(state_id);
CREATE INDEX IF NOT EXISTS idx_ui_bridge_states_group ON ui_bridge_states(group_id);
CREATE INDEX IF NOT EXISTS idx_ui_bridge_states_active ON ui_bridge_states(is_active);

-- State Machine Capture Screenshots (full-page screenshots for state view)
CREATE TABLE IF NOT EXISTS sm_capture_screenshots (
    id TEXT PRIMARY KEY,
    config_id TEXT NOT NULL REFERENCES state_machine_configs(id) ON DELETE CASCADE,
    capture_index INTEGER NOT NULL,
    screenshot_webp BYTEA NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    element_bounds_json TEXT NOT NULL DEFAULT '{}',
    fingerprint_hashes_json TEXT NOT NULL DEFAULT '[]',
    captured_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_sm_screenshots_config ON sm_capture_screenshots(config_id);

-- State Machine Element Thumbnails (per-element cropped thumbnails)
CREATE TABLE IF NOT EXISTS sm_element_thumbnails (
    config_id TEXT NOT NULL REFERENCES state_machine_configs(id) ON DELETE CASCADE,
    fingerprint_hash TEXT NOT NULL,
    thumbnail_base64 TEXT NOT NULL,
    PRIMARY KEY (config_id, fingerprint_hash)
);
CREATE INDEX IF NOT EXISTS idx_sm_thumbnails_config ON sm_element_thumbnails(config_id);

-- Log Sources (application log files to monitor for errors)
CREATE TABLE IF NOT EXISTS log_sources (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT,
    path TEXT NOT NULL,
    path_type TEXT DEFAULT 'file',
    format TEXT DEFAULT 'plaintext',
    parser TEXT DEFAULT 'generic',
    timestamp_pattern TEXT,
    timezone TEXT DEFAULT 'local',
    error_patterns TEXT,
    warning_patterns TEXT,
    ignore_patterns TEXT,
    enabled BOOLEAN DEFAULT true,
    poll_interval_ms INTEGER DEFAULT 5000,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_log_sources_name ON log_sources(name);
CREATE INDEX IF NOT EXISTS idx_log_sources_enabled ON log_sources(enabled);

-- Recordings (browser interaction recording sessions)
CREATE TABLE IF NOT EXISTS recordings (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    base_url TEXT NOT NULL,
    action_count INTEGER DEFAULT 0,
    status TEXT DEFAULT 'recording',
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    duration_ms INTEGER,
    browser_info TEXT,
    tab_id INTEGER,
    tags TEXT DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_recordings_status ON recordings(status);
CREATE INDEX IF NOT EXISTS idx_recordings_created_at ON recordings(created_at);
CREATE INDEX IF NOT EXISTS idx_recordings_base_url ON recordings(base_url);

-- Recording Actions (individual captured user interactions)
CREATE TABLE IF NOT EXISTS recording_actions (
    id TEXT PRIMARY KEY,
    recording_id TEXT NOT NULL,
    sequence_number INTEGER NOT NULL,
    action_type TEXT NOT NULL,
    url TEXT NOT NULL,
    page_title TEXT,
    target_json TEXT NOT NULL,
    action_data_json TEXT,
    screenshot_path TEXT,
    timestamp TEXT NOT NULL,
    duration_ms INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (recording_id) REFERENCES recordings(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_recording_actions_recording_id ON recording_actions(recording_id);
CREATE INDEX IF NOT EXISTS idx_recording_actions_sequence ON recording_actions(recording_id, sequence_number);
CREATE INDEX IF NOT EXISTS idx_recording_actions_action_type ON recording_actions(action_type);

-- Recording Exports (generated scripts from recordings)
CREATE TABLE IF NOT EXISTS recording_exports (
    id TEXT PRIMARY KEY,
    recording_id TEXT NOT NULL,
    export_format TEXT NOT NULL,
    script_content TEXT NOT NULL,
    file_name TEXT NOT NULL,
    options_json TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (recording_id) REFERENCES recordings(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_recording_exports_recording_id ON recording_exports(recording_id);
CREATE INDEX IF NOT EXISTS idx_recording_exports_format ON recording_exports(export_format);

-- Convergence Snapshots (workflow convergence tracking over time)
CREATE TABLE IF NOT EXISTS convergence_snapshots (
    id TEXT PRIMARY KEY,
    workflow_name TEXT NOT NULL,
    project_path TEXT,
    scope TEXT NOT NULL DEFAULT 'workflow',
    convergence_score DOUBLE PRECISION NOT NULL,
    consecutive_clean_runs INTEGER NOT NULL,
    novelty_score DOUBLE PRECISION NOT NULL,
    effective_fix_rate DOUBLE PRECISION NOT NULL,
    change_velocity DOUBLE PRECISION NOT NULL,
    total_fixes INTEGER NOT NULL,
    effective_fixes INTEGER NOT NULL,
    snapshot_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_convergence_workflow ON convergence_snapshots(workflow_name);
CREATE INDEX IF NOT EXISTS idx_convergence_project ON convergence_snapshots(project_path);
CREATE INDEX IF NOT EXISTS idx_convergence_scope ON convergence_snapshots(scope);

-- Scheduler Settings (singleton scheduler configuration)
CREATE TABLE IF NOT EXISTS scheduler_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    enabled BOOLEAN NOT NULL DEFAULT true,
    max_concurrent INTEGER NOT NULL DEFAULT 1,
    default_auto_fix_on_failure BOOLEAN NOT NULL DEFAULT false,
    timezone TEXT
);

-- =============================================================================
-- Generation Rules
-- =============================================================================
CREATE TABLE IF NOT EXISTS generation_rules (
    id TEXT PRIMARY KEY,
    agent TEXT NOT NULL,
    section TEXT NOT NULL,
    rule_number INTEGER NOT NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    condition TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    provenance TEXT NOT NULL DEFAULT 'seed',
    source_fix_id TEXT,
    confidence DOUBLE PRECISION DEFAULT 1.0,
    auto_generated_at TIMESTAMPTZ,
    evidence_count INTEGER DEFAULT 0,
    severity TEXT NOT NULL DEFAULT 'normal',
    failure_count INTEGER NOT NULL DEFAULT 0,
    examples_json TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_generation_rules_agent ON generation_rules(agent);
CREATE INDEX IF NOT EXISTS idx_generation_rules_status ON generation_rules(status);
CREATE INDEX IF NOT EXISTS idx_generation_rules_agent_section ON generation_rules(agent, section, rule_number);
CREATE INDEX IF NOT EXISTS idx_generation_rules_severity ON generation_rules(severity);

-- =============================================================================
-- Generation Pipeline Artifacts
-- =============================================================================
CREATE TABLE IF NOT EXISTS generation_pipeline_artifacts (
    id TEXT PRIMARY KEY,
    workflow_id TEXT,
    task_run_id TEXT,
    description TEXT NOT NULL,
    category TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    investigation_duration_ms INTEGER,
    investigation_enriched_description TEXT,
    discovery_duration_ms INTEGER,
    builder_duration_ms INTEGER,
    autofix_duration_ms INTEGER,
    verification_duration_ms INTEGER,
    hardener_duration_ms INTEGER,
    total_duration_ms INTEGER,
    discovery_calls TEXT,
    builder_raw_output TEXT,
    builder_parsed_json TEXT,
    autofix_diff TEXT,
    verification_iterations TEXT,
    fixer_snapshots TEXT,
    hardening_summary TEXT,
    hardened_json TEXT,
    final_json TEXT,
    validation_errors TEXT,
    specification_duration_ms INTEGER,
    specification_criteria TEXT,
    specification_prompt TEXT,
    builder_prompt TEXT,
    verification_prompts TEXT,
    hardener_prompt TEXT,
    revision_duration_ms INTEGER DEFAULT NULL,
    quality_report TEXT DEFAULT NULL,
    revision_cycles INTEGER DEFAULT NULL,
    confidence_score DOUBLE PRECISION DEFAULT NULL,
    success BOOLEAN NOT NULL DEFAULT true,
    error_message TEXT,
    model_used TEXT
);
CREATE INDEX IF NOT EXISTS idx_pipeline_artifacts_workflow ON generation_pipeline_artifacts(workflow_id);
CREATE INDEX IF NOT EXISTS idx_pipeline_artifacts_created ON generation_pipeline_artifacts(created_at);

-- =============================================================================
-- Golden Datasets
-- =============================================================================
CREATE TABLE IF NOT EXISTS golden_datasets (
    id TEXT PRIMARY KEY,
    agent_type TEXT NOT NULL,
    name TEXT NOT NULL,
    entries_json TEXT NOT NULL DEFAULT '[]',
    entry_count INTEGER DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_golden_agent ON golden_datasets(agent_type);

-- =============================================================================
-- Eval Specs
-- =============================================================================
CREATE TABLE IF NOT EXISTS eval_specs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    target_agent TEXT,
    spec_json TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- =============================================================================
-- Eval Results
-- =============================================================================
CREATE TABLE IF NOT EXISTS eval_results (
    id TEXT PRIMARY KEY,
    spec_id TEXT NOT NULL,
    recommendation_id TEXT,
    status TEXT NOT NULL DEFAULT 'running',
    result_json TEXT NOT NULL DEFAULT '{}',
    p_value DOUBLE PRECISION,
    trials_run INTEGER DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_eval_results_spec ON eval_results(spec_id);
CREATE INDEX IF NOT EXISTS idx_eval_results_rec ON eval_results(recommendation_id);

-- =============================================================================
-- Pipeline Agent Traces
-- =============================================================================
CREATE TABLE IF NOT EXISTS pipeline_agent_traces (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    agent_type TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    input_snapshot TEXT NOT NULL DEFAULT '{}',
    output_snapshot TEXT NOT NULL DEFAULT '{}',
    config_json TEXT NOT NULL DEFAULT '{}',
    duration_ms INTEGER NOT NULL DEFAULT 0,
    tokens_in INTEGER NOT NULL DEFAULT 0,
    tokens_out INTEGER NOT NULL DEFAULT 0,
    cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    downstream_success BOOLEAN,
    output_quality_score DOUBLE PRECISION,
    parent_span_id TEXT,
    span_type TEXT DEFAULT 'agent',
    guardrail_results_json TEXT,
    handoff_context_json TEXT,
    schema_valid_first_attempt BOOLEAN,
    validation_retries INTEGER,
    validation_error_summary TEXT,
    coercions_applied TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_pipeline_agent_traces_task_run ON pipeline_agent_traces(task_run_id);
CREATE INDEX IF NOT EXISTS idx_pipeline_agent_traces_agent_type ON pipeline_agent_traces(agent_type);
CREATE INDEX IF NOT EXISTS idx_pipeline_agent_traces_run_id ON pipeline_agent_traces(run_id);
CREATE INDEX IF NOT EXISTS idx_pipeline_traces_parent_span ON pipeline_agent_traces(parent_span_id);
CREATE INDEX IF NOT EXISTS idx_pipeline_traces_span_type ON pipeline_agent_traces(span_type);

-- =============================================================================
-- Meta-Optimizer Runs
-- =============================================================================
CREATE TABLE IF NOT EXISTS meta_optimizer_runs (
    id TEXT PRIMARY KEY,
    optimizer_type TEXT NOT NULL,
    trigger_type TEXT NOT NULL DEFAULT 'threshold',
    runs_analyzed INTEGER NOT NULL DEFAULT 0,
    recommendations_produced INTEGER NOT NULL DEFAULT 0,
    task_run_id TEXT,
    status TEXT NOT NULL DEFAULT 'running',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_meta_optimizer_runs_type ON meta_optimizer_runs(optimizer_type);

-- =============================================================================
-- Meta-Optimizer Snapshots
-- =============================================================================
CREATE TABLE IF NOT EXISTS meta_optimizer_snapshots (
    id TEXT PRIMARY KEY,
    snapshot_type TEXT NOT NULL,
    period_start TEXT NOT NULL,
    period_end TEXT NOT NULL,
    metrics_json TEXT NOT NULL,
    breakdown_json TEXT DEFAULT '{}',
    recommendation_id TEXT,
    runs_included INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_meta_optimizer_snapshots_type ON meta_optimizer_snapshots(snapshot_type);
CREATE INDEX IF NOT EXISTS idx_meta_optimizer_snapshots_rec ON meta_optimizer_snapshots(recommendation_id);

-- =============================================================================
-- Reflection Fixes
-- =============================================================================
CREATE TABLE IF NOT EXISTS reflection_fixes (
    id TEXT PRIMARY KEY,
    source_task_run_id TEXT NOT NULL,
    reflection_task_run_id TEXT NOT NULL,
    source_finding_id TEXT,
    source_knowledge_id TEXT,
    fix_type TEXT NOT NULL,
    fix_description TEXT NOT NULL,
    file_changed TEXT,
    old_value TEXT,
    new_value TEXT,
    confidence TEXT NOT NULL DEFAULT 'medium',
    content_hash TEXT,
    status TEXT NOT NULL DEFAULT 'applied',
    effectiveness TEXT,
    effectiveness_evidence TEXT,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    evaluated_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    source_agent TEXT,
    reasoning TEXT,
    alternatives_considered TEXT,
    reflection_scope TEXT DEFAULT 'workflow',
    project_path TEXT,
    target_component TEXT,
    reuse_count INTEGER DEFAULT 0,
    applicability_context TEXT,
    fix_description_embedding BYTEA,
    FOREIGN KEY (source_task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (reflection_task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (source_finding_id) REFERENCES task_run_findings(id) ON DELETE SET NULL,
    FOREIGN KEY (source_knowledge_id) REFERENCES task_knowledge(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_reflection_fixes_source ON reflection_fixes(source_task_run_id);
CREATE INDEX IF NOT EXISTS idx_reflection_fixes_reflection ON reflection_fixes(reflection_task_run_id);
CREATE INDEX IF NOT EXISTS idx_reflection_fixes_content_hash ON reflection_fixes(content_hash);
CREATE INDEX IF NOT EXISTS idx_reflection_fixes_status ON reflection_fixes(status);
CREATE INDEX IF NOT EXISTS idx_reflection_fixes_effectiveness ON reflection_fixes(effectiveness);
CREATE INDEX IF NOT EXISTS idx_reflection_fixes_applied_at ON reflection_fixes(applied_at);
CREATE INDEX IF NOT EXISTS idx_reflection_fixes_source_agent ON reflection_fixes(source_agent);
CREATE INDEX IF NOT EXISTS idx_reflection_fixes_project ON reflection_fixes(project_path);
CREATE INDEX IF NOT EXISTS idx_reflection_fixes_scope ON reflection_fixes(reflection_scope);

-- =============================================================================
-- Fix Applications
-- =============================================================================
CREATE TABLE IF NOT EXISTS fix_applications (
    id TEXT PRIMARY KEY,
    fix_id TEXT NOT NULL,
    task_run_id TEXT NOT NULL,
    error_signature_hash TEXT,
    outcome TEXT DEFAULT 'pending',
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    evaluated_at TIMESTAMPTZ,
    FOREIGN KEY (fix_id) REFERENCES reflection_fixes(id) ON DELETE CASCADE,
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_fix_applications_fix ON fix_applications(fix_id);
CREATE INDEX IF NOT EXISTS idx_fix_applications_task ON fix_applications(task_run_id);
CREATE INDEX IF NOT EXISTS idx_fix_applications_sig ON fix_applications(error_signature_hash);

-- =============================================================================
-- Workflow Generation Feedback
-- =============================================================================
CREATE TABLE IF NOT EXISTS workflow_generation_feedback (
    id TEXT PRIMARY KEY,
    workflow_id TEXT NOT NULL,
    task_run_id TEXT,
    feedback_type TEXT NOT NULL,
    edited_field TEXT,
    old_value TEXT,
    new_value TEXT,
    delete_reason TEXT,
    rating INTEGER,
    rating_comment TEXT,
    workflow_category TEXT,
    workflow_description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (workflow_id) REFERENCES unified_workflows(id) ON DELETE CASCADE,
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_wgf_workflow_id ON workflow_generation_feedback(workflow_id);
CREATE INDEX IF NOT EXISTS idx_wgf_task_run_id ON workflow_generation_feedback(task_run_id);
CREATE INDEX IF NOT EXISTS idx_wgf_feedback_type ON workflow_generation_feedback(feedback_type);
CREATE INDEX IF NOT EXISTS idx_wgf_created_at ON workflow_generation_feedback(created_at);

-- =============================================================================
-- Known Issues
-- =============================================================================
CREATE TABLE IF NOT EXISTS known_issues (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    category TEXT NOT NULL DEFAULT 'other',
    scope_type TEXT NOT NULL DEFAULT 'global',
    scope_value TEXT,
    scope_tags TEXT DEFAULT '[]',
    detection_method TEXT NOT NULL DEFAULT 'ai_judgment',
    detection_config TEXT DEFAULT '{}',
    pattern_template_id TEXT,
    reproduction_context TEXT,
    trigger_conditions TEXT DEFAULT '[]',
    severity TEXT NOT NULL DEFAULT 'medium',
    status TEXT NOT NULL DEFAULT 'active',
    confidence DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    provenance TEXT NOT NULL DEFAULT 'manual',
    source_finding_ids TEXT DEFAULT '[]',
    source_task_run_id TEXT,
    verification_hint TEXT,
    verification_step_template TEXT,
    times_detected INTEGER DEFAULT 1,
    times_checked INTEGER DEFAULT 0,
    last_detected_at TIMESTAMPTZ,
    last_checked_at TIMESTAMPTZ,
    resolved_at TIMESTAMPTZ,
    description_embedding BYTEA,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (source_task_run_id) REFERENCES task_runs(id) ON DELETE SET NULL
    -- pattern_template_id is a soft FK to issue_pattern_templates(id)
);
CREATE INDEX IF NOT EXISTS idx_known_issues_category ON known_issues(category);
CREATE INDEX IF NOT EXISTS idx_known_issues_scope_type ON known_issues(scope_type);
CREATE INDEX IF NOT EXISTS idx_known_issues_status ON known_issues(status);
CREATE INDEX IF NOT EXISTS idx_known_issues_severity ON known_issues(severity);
CREATE INDEX IF NOT EXISTS idx_known_issues_scope_value ON known_issues(scope_value);
CREATE INDEX IF NOT EXISTS idx_known_issues_scope_compound ON known_issues(scope_type, scope_value, status);

-- =============================================================================
-- Issue Pattern Templates
-- =============================================================================
CREATE TABLE IF NOT EXISTS issue_pattern_templates (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    category TEXT NOT NULL,
    detection_type TEXT NOT NULL,
    step_template TEXT,
    ai_prompt_template TEXT,
    parameters TEXT NOT NULL DEFAULT '[]',
    built_in BOOLEAN NOT NULL DEFAULT false,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_ipt_category ON issue_pattern_templates(category);
CREATE INDEX IF NOT EXISTS idx_ipt_status ON issue_pattern_templates(status);

-- =============================================================================
-- Pending Discoveries
-- =============================================================================
CREATE TABLE IF NOT EXISTS pending_discoveries (
    id TEXT PRIMARY KEY,
    payload TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_attempt TEXT,
    attempt_count INTEGER DEFAULT 0,
    error TEXT
);
CREATE INDEX IF NOT EXISTS idx_pending_discoveries_created_at ON pending_discoveries(created_at);
CREATE INDEX IF NOT EXISTS idx_pending_discoveries_attempt_count ON pending_discoveries(attempt_count);

-- =============================================================================
-- Step Type Knowledge
-- =============================================================================
CREATE TABLE IF NOT EXISTS step_type_knowledge (
    id TEXT PRIMARY KEY,
    step_type TEXT NOT NULL,
    layer TEXT NOT NULL DEFAULT 'universal',
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'active',
    provenance TEXT NOT NULL DEFAULT 'seed',
    source_fix_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (source_fix_id) REFERENCES reflection_fixes(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_stk_step_type ON step_type_knowledge(step_type);
CREATE INDEX IF NOT EXISTS idx_stk_layer ON step_type_knowledge(layer);
CREATE INDEX IF NOT EXISTS idx_stk_composite ON step_type_knowledge(step_type, layer, status);

-- =============================================================================
-- Task Knowledge Summaries
-- =============================================================================
CREATE TABLE IF NOT EXISTS task_knowledge_summaries (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    category TEXT NOT NULL,
    summary TEXT NOT NULL,
    covered_iterations TEXT NOT NULL,
    item_count INTEGER NOT NULL,
    original_tokens INTEGER,
    compressed_tokens INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_task_knowledge_summaries_task_run_id ON task_knowledge_summaries(task_run_id);
CREATE INDEX IF NOT EXISTS idx_task_knowledge_summaries_category ON task_knowledge_summaries(category);

-- =============================================================================
-- Batch 3: Remaining missing tables (18 of 19; workflow_generation_feedback already exists)
-- =============================================================================

-- GUI Lock (singleton row)
CREATE TABLE IF NOT EXISTS gui_lock (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    holder_session_id TEXT,
    acquired_at TIMESTAMPTZ,
    FOREIGN KEY (holder_session_id) REFERENCES sessions(id) ON DELETE SET NULL
);

-- AI Workflows (legacy workflow definitions)
CREATE TABLE IF NOT EXISTS ai_workflows (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    config TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Executions (workflow execution records)
CREATE TABLE IF NOT EXISTS executions (
    id TEXT PRIMARY KEY,
    workflow_name TEXT,
    config_path TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ended_at TIMESTAMPTZ,
    status TEXT NOT NULL,
    success BOOLEAN,
    result_data TEXT,
    error_message TEXT
);
CREATE INDEX IF NOT EXISTS idx_executions_started_at ON executions(started_at);
CREATE INDEX IF NOT EXISTS idx_executions_workflow_name ON executions(workflow_name);

-- Config Statistics (aggregated statistics per config)
CREATE TABLE IF NOT EXISTS config_statistics (
    id TEXT PRIMARY KEY,
    config_id TEXT NOT NULL UNIQUE,
    config_hash TEXT,
    total_runs INTEGER DEFAULT 0,
    successful_runs INTEGER DEFAULT 0,
    failed_runs INTEGER DEFAULT 0,
    timeout_runs INTEGER DEFAULT 0,
    avg_duration_ms INTEGER,
    recent_success_rate DOUBLE PRECISION,
    recent_avg_duration_ms INTEGER,
    transition_stats TEXT,
    template_stats TEXT,
    state_stats TEXT,
    error_patterns TEXT,
    flaky_transitions TEXT,
    flaky_templates TEXT,
    first_run_at TIMESTAMPTZ,
    last_run_at TIMESTAMPTZ,
    last_updated_at TIMESTAMPTZ,
    FOREIGN KEY (config_id) REFERENCES configs(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_config_statistics_config_id ON config_statistics(config_id);

-- Causal Events (cause-effect relationship graph)
CREATE TABLE IF NOT EXISTS causal_events (
    id TEXT PRIMARY KEY,
    cause_event_type TEXT NOT NULL,
    cause_event_id TEXT NOT NULL,
    effect_event_type TEXT NOT NULL,
    effect_event_id TEXT NOT NULL,
    relationship TEXT NOT NULL,
    confidence TEXT NOT NULL DEFAULT 'high',
    source TEXT NOT NULL DEFAULT 'automated',
    task_run_id TEXT,
    workflow_name TEXT,
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_causal_cause ON causal_events(cause_event_type, cause_event_id);
CREATE INDEX IF NOT EXISTS idx_causal_effect ON causal_events(effect_event_type, effect_event_id);
CREATE INDEX IF NOT EXISTS idx_causal_workflow ON causal_events(workflow_name);
CREATE INDEX IF NOT EXISTS idx_causal_task_run ON causal_events(task_run_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_causal_dedup ON causal_events(cause_event_type, cause_event_id, effect_event_type, effect_event_id);

-- Architecture Components (component-level aggregated data)
CREATE TABLE IF NOT EXISTS architecture_components (
    id TEXT PRIMARY KEY,
    workflow_name TEXT NOT NULL,
    component_path TEXT NOT NULL,
    component_type TEXT NOT NULL DEFAULT 'file',
    fix_count INTEGER NOT NULL DEFAULT 0,
    error_count INTEGER NOT NULL DEFAULT 0,
    causal_involvement_count INTEGER NOT NULL DEFAULT 0,
    effective_fix_count INTEGER NOT NULL DEFAULT 0,
    ineffective_fix_count INTEGER NOT NULL DEFAULT 0,
    health_score DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    change_velocity DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    last_activity_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(workflow_name, component_path)
);
CREATE INDEX IF NOT EXISTS idx_arch_comp_workflow ON architecture_components(workflow_name);
CREATE INDEX IF NOT EXISTS idx_arch_comp_health ON architecture_components(health_score);

-- Component Relationships (inter-component dependency graph)
CREATE TABLE IF NOT EXISTS component_relationships (
    id TEXT PRIMARY KEY,
    workflow_name TEXT NOT NULL,
    source_component TEXT NOT NULL,
    target_component TEXT NOT NULL,
    relationship_type TEXT NOT NULL,
    strength INTEGER NOT NULL DEFAULT 1,
    last_seen_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(workflow_name, source_component, target_component, relationship_type)
);
CREATE INDEX IF NOT EXISTS idx_comp_rel_workflow ON component_relationships(workflow_name);
CREATE INDEX IF NOT EXISTS idx_comp_rel_source ON component_relationships(source_component);

-- Component Health Snapshots (temporal health trends)
CREATE TABLE IF NOT EXISTS component_health_snapshots (
    id TEXT PRIMARY KEY,
    workflow_name TEXT NOT NULL,
    component_path TEXT NOT NULL,
    health_score DOUBLE PRECISION NOT NULL,
    fix_count INTEGER NOT NULL DEFAULT 0,
    effective_fix_count INTEGER NOT NULL DEFAULT 0,
    change_velocity DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    snapshot_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_comp_health_snap_wf ON component_health_snapshots(workflow_name);
CREATE INDEX IF NOT EXISTS idx_comp_health_snap_comp ON component_health_snapshots(workflow_name, component_path);
CREATE INDEX IF NOT EXISTS idx_comp_health_snap_at ON component_health_snapshots(snapshot_at);

-- Agentic Metric Baselines (learned baselines per workflow)
CREATE TABLE IF NOT EXISTS agentic_metric_baselines (
    id TEXT PRIMARY KEY,
    workflow_id TEXT,
    metric_type TEXT NOT NULL,
    baseline_value TEXT NOT NULL,
    sample_count INTEGER NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_baselines_unique ON agentic_metric_baselines(workflow_id, metric_type);

-- Spec Compliance Results (spec assertion tracking)
CREATE TABLE IF NOT EXISTS spec_compliance_results (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    spec_id TEXT,
    iteration INTEGER NOT NULL,
    overall_score DOUBLE PRECISION NOT NULL,
    raw_pass_rate DOUBLE PRECISION NOT NULL,
    critical_passed INTEGER NOT NULL DEFAULT 0,
    critical_total INTEGER NOT NULL DEFAULT 0,
    warning_passed INTEGER NOT NULL DEFAULT 0,
    warning_total INTEGER NOT NULL DEFAULT 0,
    info_passed INTEGER NOT NULL DEFAULT 0,
    info_total INTEGER NOT NULL DEFAULT 0,
    assertions_passed INTEGER NOT NULL,
    assertions_total INTEGER NOT NULL,
    group_scores_json TEXT NOT NULL DEFAULT '[]',
    assertion_details_json TEXT NOT NULL DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_spec_compliance_task ON spec_compliance_results(task_run_id);
CREATE INDEX IF NOT EXISTS idx_spec_compliance_score ON spec_compliance_results(overall_score);
CREATE INDEX IF NOT EXISTS idx_spec_compliance_spec ON spec_compliance_results(spec_id);

-- Spec Accuracy Results (spec quality analysis)
CREATE TABLE IF NOT EXISTS spec_accuracy_results (
    id TEXT PRIMARY KEY,
    spec_id TEXT NOT NULL,
    analysis_type TEXT NOT NULL,
    score DOUBLE PRECISION NOT NULL,
    detail_json TEXT NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_spec_accuracy_spec ON spec_accuracy_results(spec_id);
CREATE INDEX IF NOT EXISTS idx_spec_accuracy_type ON spec_accuracy_results(analysis_type);

-- Artifacts (UI Bridge IPC artifact persistence)
CREATE TABLE IF NOT EXISTS artifacts (
    artifact_id TEXT PRIMARY KEY,
    source_json TEXT NOT NULL,
    result_json TEXT NOT NULL,
    environment_json TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    passed INTEGER
);
CREATE INDEX IF NOT EXISTS idx_artifacts_created_at ON artifacts(created_at);
CREATE INDEX IF NOT EXISTS idx_artifacts_passed ON artifacts(passed);

-- Robustness Reports (prompt robustness evaluation)
CREATE TABLE IF NOT EXISTS robustness_reports (
    id TEXT PRIMARY KEY,
    prompt_variant_id TEXT,
    recommendation_id TEXT,
    total_tests INTEGER NOT NULL,
    passed INTEGER NOT NULL,
    failed INTEGER NOT NULL,
    report_json TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_robustness_variant ON robustness_reports(prompt_variant_id);

-- =============================================================================
-- Batch: 19 missing tables migrated from SQLite schema
-- =============================================================================

-- Model Profiles (per-model capability/cost metadata)
CREATE TABLE IF NOT EXISTS model_profiles (
    id TEXT PRIMARY KEY,
    model_id TEXT NOT NULL UNIQUE,
    profile_json TEXT NOT NULL,
    trial_count INTEGER DEFAULT 0,
    last_updated TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_model_profiles_model ON model_profiles(model_id);

-- Check Results (execution results for individual checks)
CREATE TABLE IF NOT EXISTS check_results (
    id TEXT PRIMARY KEY,
    check_id TEXT NOT NULL,
    task_run_id TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    duration_ms INTEGER,
    output TEXT,
    error_message TEXT,
    issues_found INTEGER DEFAULT 0,
    issues_fixed INTEGER DEFAULT 0,
    files_checked INTEGER DEFAULT 0,
    structured_output TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (check_id) REFERENCES checks(id) ON DELETE CASCADE,
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_check_results_check_id ON check_results(check_id);
CREATE INDEX IF NOT EXISTS idx_check_results_task_run_id ON check_results(task_run_id);
CREATE INDEX IF NOT EXISTS idx_check_results_status ON check_results(status);
CREATE INDEX IF NOT EXISTS idx_check_results_created_at ON check_results(created_at);

-- Shell Command Results (execution results for shell commands)
CREATE TABLE IF NOT EXISTS shell_command_results (
    id TEXT PRIMARY KEY,
    shell_command_id TEXT NOT NULL,
    task_run_id TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    exit_code INTEGER,
    stdout TEXT,
    stderr TEXT,
    duration_ms INTEGER,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (shell_command_id) REFERENCES shell_commands(id) ON DELETE CASCADE,
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_shell_command_results_shell_command_id ON shell_command_results(shell_command_id);
CREATE INDEX IF NOT EXISTS idx_shell_command_results_task_run_id ON shell_command_results(task_run_id);
CREATE INDEX IF NOT EXISTS idx_shell_command_results_status ON shell_command_results(status);
CREATE INDEX IF NOT EXISTS idx_shell_command_results_created_at ON shell_command_results(created_at);

-- Test Associations (link tests to configs/workflows)
CREATE TABLE IF NOT EXISTS test_associations (
    id TEXT PRIMARY KEY,
    test_id TEXT NOT NULL,
    config_id TEXT,
    workflow_name TEXT,
    trigger_point TEXT NOT NULL,
    action_id TEXT,
    execution_order INTEGER DEFAULT 0,
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (test_id) REFERENCES verification_tests(id) ON DELETE CASCADE,
    FOREIGN KEY (config_id) REFERENCES configs(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_test_associations_test_id ON test_associations(test_id);
CREATE INDEX IF NOT EXISTS idx_test_associations_config_id ON test_associations(config_id);

-- Test Results (execution results for verification tests)
CREATE TABLE IF NOT EXISTS test_results (
    id TEXT PRIMARY KEY,
    test_id TEXT NOT NULL,
    task_run_id TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    duration_ms INTEGER,
    output TEXT,
    error_message TEXT,
    structured_output TEXT,
    assertions_passed INTEGER DEFAULT 0,
    assertions_failed INTEGER DEFAULT 0,
    screenshots TEXT DEFAULT '[]',
    visual_evidence TEXT,
    ai_analysis TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (test_id) REFERENCES verification_tests(id) ON DELETE CASCADE,
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_test_results_test_id ON test_results(test_id);
CREATE INDEX IF NOT EXISTS idx_test_results_task_run_id ON test_results(task_run_id);
CREATE INDEX IF NOT EXISTS idx_test_results_status ON test_results(status);

-- Verification Plans (orchestrator architecture, created by Planning Agent)
CREATE TABLE IF NOT EXISTS verification_plans (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    plan_json TEXT NOT NULL,
    goal_summary TEXT NOT NULL,
    criteria_count INTEGER NOT NULL DEFAULT 0,
    has_ai_criteria BOOLEAN NOT NULL DEFAULT false,
    replan_reason TEXT,
    previous_version_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (previous_version_id) REFERENCES verification_plans(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_verification_plans_task_run_id ON verification_plans(task_run_id);
CREATE INDEX IF NOT EXISTS idx_verification_plans_version ON verification_plans(version);
CREATE INDEX IF NOT EXISTS idx_verification_plans_created_at ON verification_plans(created_at);

-- Orchestrator Verification Results (per-criterion results per iteration)
CREATE TABLE IF NOT EXISTS orchestrator_verification_results (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    plan_id TEXT NOT NULL,
    iteration INTEGER NOT NULL,
    criterion_id TEXT NOT NULL,
    criterion_type TEXT NOT NULL,
    passed BOOLEAN NOT NULL,
    is_critical BOOLEAN NOT NULL DEFAULT true,
    confidence TEXT,
    observations TEXT DEFAULT '[]',
    issues TEXT DEFAULT '[]',
    suggestions TEXT DEFAULT '[]',
    raw_output TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (plan_id) REFERENCES verification_plans(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_orch_ver_results_task_run_id ON orchestrator_verification_results(task_run_id);
CREATE INDEX IF NOT EXISTS idx_orch_ver_results_plan_id ON orchestrator_verification_results(plan_id);
CREATE INDEX IF NOT EXISTS idx_orch_ver_results_iteration ON orchestrator_verification_results(iteration);
CREATE INDEX IF NOT EXISTS idx_orch_ver_results_passed ON orchestrator_verification_results(passed);
CREATE INDEX IF NOT EXISTS idx_orch_ver_results_criterion_id ON orchestrator_verification_results(criterion_id);

-- Orchestration Loop Configs (saved loop configuration presets)
CREATE TABLE IF NOT EXISTS orchestration_loop_configs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    is_favorite BOOLEAN DEFAULT false,
    config_json TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_ol_configs_favorite ON orchestration_loop_configs(is_favorite);
CREATE INDEX IF NOT EXISTS idx_ol_configs_updated ON orchestration_loop_configs(updated_at);

-- Comparison Runs (A/B workflow comparison experiments)
CREATE TABLE IF NOT EXISTS comparison_runs (
    id TEXT PRIMARY KEY,
    workflow_id TEXT NOT NULL,
    variation_type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'running',
    entries_json TEXT NOT NULL DEFAULT '[]',
    report TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_comparison_runs_workflow ON comparison_runs(workflow_id);
CREATE INDEX IF NOT EXISTS idx_comparison_runs_status ON comparison_runs(status);

-- Task Run Automation (workflow automation execution details)
CREATE TABLE IF NOT EXISTS task_run_automation (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    workflow_name TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ended_at TIMESTAMPTZ,
    duration_ms INTEGER,
    automation_status TEXT NOT NULL DEFAULT 'running',
    success BOOLEAN,
    error_type TEXT,
    error_message TEXT,
    actions_summary TEXT,
    states_visited TEXT,
    transitions_executed TEXT,
    template_matches TEXT,
    anomalies TEXT,
    iteration_number INTEGER NOT NULL DEFAULT 1,
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_task_run_automation_task_run_id ON task_run_automation(task_run_id);
CREATE INDEX IF NOT EXISTS idx_task_run_automation_started_at ON task_run_automation(started_at);
CREATE INDEX IF NOT EXISTS idx_task_run_automation_status ON task_run_automation(automation_status);

-- Task Run MCP Calls (MCP tool invocations within task runs)
CREATE TABLE IF NOT EXISTS task_run_mcp_calls (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    step_id TEXT NOT NULL,
    step_name TEXT,
    server_id TEXT NOT NULL,
    server_name TEXT,
    tool_name TEXT NOT NULL,
    arguments TEXT,
    resolved_arguments TEXT,
    response TEXT,
    response_type TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    extractions TEXT,
    assertions TEXT,
    success BOOLEAN NOT NULL,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (server_id) REFERENCES mcp_servers(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_task_run_mcp_calls_task_run_id ON task_run_mcp_calls(task_run_id);
CREATE INDEX IF NOT EXISTS idx_task_run_mcp_calls_server_id ON task_run_mcp_calls(server_id);
CREATE INDEX IF NOT EXISTS idx_task_run_mcp_calls_step_id ON task_run_mcp_calls(step_id);
CREATE INDEX IF NOT EXISTS idx_task_run_mcp_calls_created_at ON task_run_mcp_calls(created_at);
CREATE INDEX IF NOT EXISTS idx_task_run_mcp_calls_success ON task_run_mcp_calls(success);

-- Task Run Output Chunks (streaming output segments for task runs)
CREATE TABLE IF NOT EXISTS task_run_output_chunks (
    id BIGSERIAL PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    chunk_sequence INTEGER NOT NULL,
    content TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_chunks_task_run ON task_run_output_chunks(task_run_id, chunk_sequence);

-- Trigger History (event log for workflow triggers)
CREATE TABLE IF NOT EXISTS trigger_history (
    id TEXT PRIMARY KEY,
    trigger_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    event_data TEXT DEFAULT '{}',
    action TEXT NOT NULL,
    task_run_id TEXT,
    error_message TEXT,
    triggered_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
    -- trigger_id is a soft FK to workflow_triggers(id)
);
CREATE INDEX IF NOT EXISTS idx_trigger_history_trigger_id ON trigger_history(trigger_id);
CREATE INDEX IF NOT EXISTS idx_trigger_history_triggered_at ON trigger_history(triggered_at);

-- Scheduler History (execution log for scheduled tasks).
-- PK is execution_id (not id): runtime code in database/pg/scheduler.rs
-- and database/pg/mod.rs ensure_tables() both use execution_id. Prior
-- versions of this file had `id TEXT PRIMARY KEY`, which matched neither
-- the runtime code nor the live DB — a documentation bug that was
-- corrected as part of the 2026-04-08 drift audit.
CREATE TABLE IF NOT EXISTS scheduler_history (
    execution_id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    session_id TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ended_at TIMESTAMPTZ,
    status TEXT NOT NULL DEFAULT 'pending',
    success BOOLEAN NOT NULL DEFAULT false,
    error_message TEXT,
    triggered_auto_fix BOOLEAN NOT NULL DEFAULT false,
    auto_fix_session_id TEXT,
    FOREIGN KEY (task_id) REFERENCES scheduled_tasks(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_scheduler_history_task_id ON scheduler_history(task_id);
CREATE INDEX IF NOT EXISTS idx_scheduler_history_started_at ON scheduler_history(started_at);
CREATE INDEX IF NOT EXISTS idx_scheduler_history_status ON scheduler_history(status);

-- Workflow Triggers (event-driven workflow execution)
CREATE TABLE IF NOT EXISTS workflow_triggers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    trigger_type TEXT NOT NULL,
    trigger_config TEXT NOT NULL,
    workflow_id TEXT NOT NULL,
    workflow_overrides TEXT,
    conditions TEXT DEFAULT '[]',
    debounce_ms BIGINT DEFAULT 1000,
    cooldown_seconds BIGINT DEFAULT 60,
    max_concurrent INTEGER DEFAULT 1,
    retry_count INTEGER DEFAULT 0,
    retry_delay_seconds BIGINT DEFAULT 30,
    enabled BOOLEAN DEFAULT true,
    last_triggered_at TIMESTAMPTZ,
    last_execution_id TEXT,
    trigger_count BIGINT DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (workflow_id) REFERENCES unified_workflows(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_workflow_triggers_type ON workflow_triggers(trigger_type);
CREATE INDEX IF NOT EXISTS idx_workflow_triggers_enabled ON workflow_triggers(enabled);

-- Workflow Variables (session-scoped variables for API request substitution)
CREATE TABLE IF NOT EXISTS workflow_variables (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    variable_name TEXT NOT NULL,
    variable_value TEXT NOT NULL,
    source TEXT NOT NULL,
    source_step_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    UNIQUE(task_run_id, variable_name)
);
CREATE INDEX IF NOT EXISTS idx_workflow_variables_task_run_id ON workflow_variables(task_run_id);
CREATE INDEX IF NOT EXISTS idx_workflow_variables_name ON workflow_variables(variable_name);

-- Spec Versions (version history for spec documents)
CREATE TABLE IF NOT EXISTS spec_versions (
    id TEXT PRIMARY KEY,
    spec_id TEXT NOT NULL,
    version_number INTEGER NOT NULL,
    content_hash TEXT NOT NULL,
    spec_json TEXT NOT NULL,
    change_summary TEXT,
    change_type TEXT NOT NULL DEFAULT 'manual',
    parent_version_id TEXT,
    assertion_count INTEGER NOT NULL,
    group_count INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_spec_versions_spec ON spec_versions(spec_id);
CREATE INDEX IF NOT EXISTS idx_spec_versions_hash ON spec_versions(content_hash);
CREATE UNIQUE INDEX IF NOT EXISTS idx_spec_versions_num ON spec_versions(spec_id, version_number);

-- UI Bridge Transitions
CREATE TABLE IF NOT EXISTS ui_bridge_transitions (
    id BIGSERIAL PRIMARY KEY,
    transition_id TEXT UNIQUE NOT NULL,
    name TEXT NOT NULL,
    from_states TEXT NOT NULL,
    activate_states TEXT NOT NULL,
    exit_states TEXT,
    activate_groups TEXT,
    exit_groups TEXT,
    actions TEXT,
    path_cost DOUBLE PRECISION DEFAULT 1.0,
    stays_visible BOOLEAN DEFAULT false,
    metadata TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_ubt_transition_id ON ui_bridge_transitions(transition_id);

-- Entailment Cache (PG persistence for evaluation entailment cache)
CREATE TABLE IF NOT EXISTS entailment_cache (
    criterion_hash BIGINT NOT NULL,
    step_hash BIGINT NOT NULL,
    score FLOAT8 NOT NULL,
    explanation TEXT,
    tier TEXT,
    cached_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (criterion_hash, step_hash)
);
CREATE INDEX IF NOT EXISTS idx_entailment_cache_cached_at ON entailment_cache(cached_at);

-- World State Verifier shadow-mode disagreements.
-- Populated when the agentic loop runs with QONTINUI_WORLD_STATE_VERIFIER=shadow
-- (or the equivalent persisted setting) and the WSM/text verifier verdicts differ.
-- Consumed by the Settings → World State Verifier calibration view.
CREATE TABLE IF NOT EXISTS wsv_shadow_disagreements (
    id BIGSERIAL PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    iteration INTEGER NOT NULL,
    text_status TEXT NOT NULL,
    wsm_status TEXT NOT NULL,
    text_confidence DOUBLE PRECISION NOT NULL,
    wsm_confidence DOUBLE PRECISION NOT NULL,
    intent TEXT NOT NULL,
    wsm_observations TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_wsv_disagreements_task_run
    ON wsv_shadow_disagreements(task_run_id);
CREATE INDEX IF NOT EXISTS idx_wsv_disagreements_created_at
    ON wsv_shadow_disagreements(created_at DESC);

-- PRM Training Exports
CREATE TABLE IF NOT EXISTS prm_training_exports (
    id TEXT PRIMARY KEY,
    export_format TEXT NOT NULL DEFAULT 'jsonl',
    total_examples INTEGER NOT NULL DEFAULT 0,
    passed_count INTEGER NOT NULL DEFAULT 0,
    failed_count INTEGER NOT NULL DEFAULT 0,
    fixed_count INTEGER NOT NULL DEFAULT 0,
    runs_processed INTEGER NOT NULL DEFAULT 0,
    file_path TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_prm_exports_created ON prm_training_exports(created_at);

-- Playbook Entries (Adaptive Learning)
CREATE TABLE IF NOT EXISTS playbook_entries (
    id TEXT PRIMARY KEY,
    lesson TEXT NOT NULL,
    category TEXT NOT NULL,
    domain TEXT,
    severity TEXT NOT NULL DEFAULT 'minor',
    source_run_id TEXT NOT NULL,
    source_step_id TEXT,
    positive INTEGER NOT NULL DEFAULT 1,
    times_applied INTEGER NOT NULL DEFAULT 0,
    times_helped INTEGER NOT NULL DEFAULT 0,
    embedding BYTEA,
    status TEXT NOT NULL DEFAULT 'staged',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_playbook_entries_domain ON playbook_entries(domain);
CREATE INDEX IF NOT EXISTS idx_playbook_entries_status ON playbook_entries(status);
CREATE INDEX IF NOT EXISTS idx_playbook_entries_severity ON playbook_entries(severity);

-- Curated Examples (Adaptive Learning)
CREATE TABLE IF NOT EXISTS curated_examples (
    id TEXT PRIMARY KEY,
    domain TEXT NOT NULL,
    criterion_description TEXT NOT NULL,
    steps_json TEXT NOT NULL,
    quality_score DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    execution_verified INTEGER NOT NULL DEFAULT 0,
    times_used INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_curated_examples_domain ON curated_examples(domain);
CREATE INDEX IF NOT EXISTS idx_curated_examples_quality ON curated_examples(quality_score);

-- Template Performance
CREATE TABLE IF NOT EXISTS template_performance (
    template_id TEXT PRIMARY KEY,
    template_name TEXT NOT NULL,
    source TEXT NOT NULL DEFAULT 'manual',
    success_count INTEGER NOT NULL DEFAULT 0,
    failure_count INTEGER NOT NULL DEFAULT 0,
    total_quality_score DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    last_used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Template Lifecycle Events
CREATE TABLE IF NOT EXISTS template_lifecycle_events (
    id TEXT PRIMARY KEY,
    template_id TEXT NOT NULL,
    action TEXT NOT NULL,
    old_source TEXT NOT NULL,
    new_source TEXT NOT NULL,
    confidence_at_transition DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_lifecycle_events_template ON template_lifecycle_events(template_id);

-- GEPA Optimization Runs
CREATE TABLE IF NOT EXISTS gepa_optimization_runs (
    id TEXT PRIMARY KEY,
    domain TEXT NOT NULL,
    old_instructions TEXT NOT NULL,
    new_instructions TEXT,
    old_score DOUBLE PRECISION,
    new_score DOUBLE PRECISION,
    improvement DOUBLE PRECISION,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_gepa_runs_domain ON gepa_optimization_runs(domain);
CREATE INDEX IF NOT EXISTS idx_gepa_runs_created ON gepa_optimization_runs(created_at);

-- Step Templates (exploration-based generation)
CREATE TABLE IF NOT EXISTS step_templates (
    id TEXT PRIMARY KEY,
    domain TEXT NOT NULL,
    pattern_description TEXT NOT NULL,
    template_steps_json TEXT NOT NULL,
    parameters_json TEXT NOT NULL DEFAULT '[]',
    success_count INTEGER NOT NULL DEFAULT 0,
    failure_count INTEGER NOT NULL DEFAULT 0,
    confidence DOUBLE PRECISION NOT NULL DEFAULT 0.5,
    source TEXT NOT NULL DEFAULT 'seeded',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_step_templates_domain ON step_templates(domain);

-- Exploration Stats
CREATE TABLE IF NOT EXISTS exploration_stats (
    id TEXT PRIMARY KEY,
    workflow_id TEXT,
    task_run_id TEXT,
    total_candidates INTEGER NOT NULL DEFAULT 0,
    search_depth INTEGER NOT NULL DEFAULT 0,
    search_duration_ms INTEGER NOT NULL DEFAULT 0,
    best_score DOUBLE PRECISION,
    strategy_used TEXT,
    score_progression TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_exploration_stats_workflow ON exploration_stats(workflow_id);

-- =============================================================================
-- Iteration Logs (per-iteration provider/model tracking)
-- =============================================================================
CREATE TABLE IF NOT EXISTS iteration_logs (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    iteration INTEGER NOT NULL DEFAULT 0,
    provider_used TEXT,
    model_used TEXT,
    duration_ms INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_iteration_logs_task_run ON iteration_logs(task_run_id);
CREATE INDEX IF NOT EXISTS idx_iteration_logs_provider ON iteration_logs(provider_used) WHERE provider_used IS NOT NULL;

-- =============================================================================
-- Online Learning: Performance Drift Detection
-- =============================================================================
CREATE TABLE IF NOT EXISTS performance_drift_signals (
    id              TEXT PRIMARY KEY,
    detector_type   TEXT NOT NULL,           -- 'adwin' | 'ddm' | 'page_hinkley'
    metric_name     TEXT NOT NULL,           -- 'composite_score' | 'success_rate' | 'cost_usd' | 'duration_secs'
    context_key     TEXT NOT NULL DEFAULT '',-- domain:complexity:has_ui (matches Q-Router state)
    drift_level     TEXT NOT NULL,           -- 'warning' | 'drift'
    pre_drift_mean  DOUBLE PRECISION,
    post_drift_mean DOUBLE PRECISION,
    window_size     BIGINT,
    acknowledged    BOOLEAN NOT NULL DEFAULT false,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_drift_context ON performance_drift_signals(context_key, metric_name);
CREATE INDEX IF NOT EXISTS idx_drift_unack ON performance_drift_signals(acknowledged) WHERE acknowledged = false;

CREATE TABLE IF NOT EXISTS drift_detector_state (
    detector_id     TEXT PRIMARY KEY,        -- 'adwin:composite_score:backend:moderate:no_ui'
    detector_type   TEXT NOT NULL,
    state_json      TEXT NOT NULL,           -- serialized detector state
    last_updated    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- =============================================================================
-- Online Learning: Contextual Bandit Model Routing
-- =============================================================================
CREATE TABLE IF NOT EXISTS model_routing_table (
    context_key     TEXT NOT NULL,
    model_id        TEXT NOT NULL,
    q_value         DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    visit_count     INTEGER NOT NULL DEFAULT 0,
    sum_of_squares  DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    last_updated    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (context_key, model_id)
);

CREATE TABLE IF NOT EXISTS model_routing_overrides (
    context_key     TEXT PRIMARY KEY,
    forced_model    TEXT NOT NULL,
    reason          TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS model_routing_decisions (
    id              TEXT PRIMARY KEY,
    task_run_id     TEXT NOT NULL,
    context_key     TEXT NOT NULL,
    model_selected  TEXT NOT NULL,
    source          TEXT NOT NULL,            -- 'bandit' | 'fallback' | 'override'
    exploration     BOOLEAN NOT NULL DEFAULT false,
    reward          DOUBLE PRECISION,         -- filled post-run
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_mrd_task_run ON model_routing_decisions(task_run_id);
CREATE INDEX IF NOT EXISTS idx_mrd_model ON model_routing_decisions(model_selected);

-- =============================================================================
-- Online Learning: Experience Summaries
-- =============================================================================
CREATE TABLE IF NOT EXISTS experience_summaries (
    id                      TEXT PRIMARY KEY,
    task_run_id             TEXT NOT NULL,
    domain                  TEXT NOT NULL,
    complexity_tier         TEXT NOT NULL,
    outcome                 TEXT NOT NULL,
    key_decisions_json      TEXT NOT NULL DEFAULT '[]',
    failure_points_json     TEXT NOT NULL DEFAULT '[]',
    effective_patterns_json TEXT NOT NULL DEFAULT '[]',
    embedding               BYTEA,
    similarity_cluster      TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_exp_domain ON experience_summaries(domain);
CREATE INDEX IF NOT EXISTS idx_exp_outcome ON experience_summaries(outcome);

-- =============================================================================
-- Online Learning: Step Credit Assignments (ADCA)
-- =============================================================================
CREATE TABLE IF NOT EXISTS step_credit_assignments (
    id                          TEXT PRIMARY KEY,
    task_run_id                 TEXT NOT NULL,
    step_index                  INTEGER NOT NULL,
    step_type                   TEXT NOT NULL,
    agent_type                  TEXT,
    raw_credit                  DOUBLE PRECISION NOT NULL,
    normalized_credit           DOUBLE PRECISION NOT NULL,
    temporal_proximity          DOUBLE PRECISION,
    output_utilization          DOUBLE PRECISION,
    confidence_delta_signal     DOUBLE PRECISION,
    downstream_success_signal   DOUBLE PRECISION,
    cost_efficiency_signal      DOUBLE PRECISION,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_sca_task_run ON step_credit_assignments(task_run_id);
CREATE INDEX IF NOT EXISTS idx_sca_step_type ON step_credit_assignments(step_type);

-- =============================================================================
-- Online Learning: Evolvable Strategy Bank
-- =============================================================================
CREATE TABLE IF NOT EXISTS strategy_bank (
    id                  TEXT PRIMARY KEY,
    name                TEXT NOT NULL,
    description         TEXT NOT NULL,
    applicability_json  TEXT NOT NULL,
    components_json     TEXT NOT NULL,
    stats_json          TEXT NOT NULL DEFAULT '{}',
    provenance_json     TEXT NOT NULL,
    status              TEXT NOT NULL DEFAULT 'candidate',  -- candidate | active | degraded | retired
    parent_strategy_id  TEXT REFERENCES strategy_bank(id) ON DELETE SET NULL,
    embedding           BYTEA,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_strategy_status ON strategy_bank(status);
CREATE INDEX IF NOT EXISTS idx_strategy_parent ON strategy_bank(parent_strategy_id);

-- =============================================================================
-- Security Audit Events
-- =============================================================================
CREATE TABLE IF NOT EXISTS security_audit_events (
    id          TEXT PRIMARY KEY,
    timestamp   TEXT NOT NULL,
    task_run_id TEXT,
    step_name   TEXT,
    workflow_id TEXT,
    event_type  TEXT NOT NULL,
    action      TEXT NOT NULL,
    decision    TEXT NOT NULL,
    reason      TEXT,
    metadata    TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_sec_audit_task_run
    ON security_audit_events(task_run_id) WHERE task_run_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_sec_audit_type
    ON security_audit_events(event_type);
CREATE INDEX IF NOT EXISTS idx_sec_audit_decision
    ON security_audit_events(decision);
CREATE INDEX IF NOT EXISTS idx_sec_audit_created
    ON security_audit_events(created_at);

-- =============================================================================
-- Phase Model Routing (Q-learning state for model tier selection)
-- =============================================================================
CREATE TABLE IF NOT EXISTS phase_model_routing (
    state_key    TEXT NOT NULL,
    phase        TEXT NOT NULL,
    model_tier   TEXT NOT NULL,
    q_value      DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    visit_count  INTEGER NOT NULL DEFAULT 0,
    last_updated TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (state_key, phase, model_tier)
);
CREATE INDEX IF NOT EXISTS idx_phase_model_routing_state
    ON phase_model_routing(state_key);

-- Phase 1A: Add model_used tracking to learning_outcomes
ALTER TABLE learning_outcomes ADD COLUMN IF NOT EXISTS model_used TEXT;

-- ============================================================================
-- Restate Durable Execution Tables
-- ============================================================================

-- Maps task_run executions to Restate workflow invocations
CREATE TABLE IF NOT EXISTS restate_workflow_executions (
    execution_id TEXT PRIMARY KEY REFERENCES task_runs(id) ON DELETE CASCADE,
    restate_workflow_id TEXT NOT NULL,
    restate_invocation_id TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    launched_via_restate BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_rwe_status ON restate_workflow_executions(status);
CREATE INDEX IF NOT EXISTS idx_rwe_restate_wf ON restate_workflow_executions(restate_workflow_id);

-- Pending awakeables for external resolution (approval gates, deferred HITL)
CREATE TABLE IF NOT EXISTS restate_awakeables (
    awakeable_id TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    awakeable_type TEXT NOT NULL,
    type_data TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_ra_execution ON restate_awakeables(execution_id);
CREATE INDEX IF NOT EXISTS idx_ra_status ON restate_awakeables(status);

-- ============================================================
-- Memory query cache (for tiered reasoning results)
-- ============================================================
CREATE TABLE IF NOT EXISTS memory_query_cache (
    id BIGSERIAL PRIMARY KEY,
    query_hash TEXT NOT NULL,
    reasoning_level TEXT NOT NULL,
    result_json TEXT NOT NULL,
    hit_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_mqc_hash_level ON memory_query_cache(query_hash, reasoning_level);
CREATE INDEX IF NOT EXISTS idx_mqc_expires ON memory_query_cache(expires_at);

-- ============================================================
-- Contradiction resolutions (Honcho-inspired contradiction handling)
-- ============================================================
CREATE TABLE IF NOT EXISTS contradiction_resolutions (
    id BIGSERIAL PRIMARY KEY,
    observation_a_id BIGINT NOT NULL REFERENCES observations(id),
    observation_b_id BIGINT NOT NULL REFERENCES observations(id),
    resolution_type TEXT NOT NULL,
    winner_id BIGINT REFERENCES observations(id),
    loser_id BIGINT REFERENCES observations(id),
    confidence DOUBLE PRECISION NOT NULL DEFAULT 0.5,
    rationale TEXT NOT NULL,
    evidence_json TEXT,
    resolved_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_by TEXT NOT NULL DEFAULT 'system',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_cr_obs_a ON contradiction_resolutions(observation_a_id);
CREATE INDEX IF NOT EXISTS idx_cr_obs_b ON contradiction_resolutions(observation_b_id);
CREATE INDEX IF NOT EXISTS idx_cr_resolved ON contradiction_resolutions(resolved_at);

-- ============================================================
-- Entity profiles (Honcho-inspired evolving representations)
-- ============================================================
CREATE TABLE IF NOT EXISTS entity_profiles (
    id BIGSERIAL PRIMARY KEY,
    entity_kind TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    entity_label TEXT NOT NULL,
    profile_summary TEXT NOT NULL,
    profile_detail TEXT,
    topic_key TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    importance DOUBLE PRECISION NOT NULL DEFAULT 0.5,
    decay_rate DOUBLE PRECISION NOT NULL DEFAULT 0.02,
    access_count INTEGER NOT NULL DEFAULT 0,
    last_accessed_at TIMESTAMPTZ,
    revision_count INTEGER NOT NULL DEFAULT 1,
    source_observation_ids BIGINT[],
    source_finding_ids TEXT[],
    source_fix_ids TEXT[],
    source_cross_run_pattern_ids TEXT[],
    valid_from TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    valid_until TIMESTAMPTZ,
    superseded_by BIGINT REFERENCES entity_profiles(id),
    is_deleted BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_ep_entity ON entity_profiles(entity_kind, entity_id) WHERE NOT is_deleted;
CREATE INDEX IF NOT EXISTS idx_ep_topic_key ON entity_profiles(topic_key) WHERE NOT is_deleted;
CREATE INDEX IF NOT EXISTS idx_ep_importance ON entity_profiles(importance) WHERE NOT is_deleted;
CREATE INDEX IF NOT EXISTS idx_ep_fts ON entity_profiles USING GIN (to_tsvector('english', entity_label || ' ' || profile_summary)) WHERE NOT is_deleted;

-- ============================================================
-- Reasoning traces (Honcho-inspired formal reasoning)
-- ============================================================
CREATE TABLE IF NOT EXISTS reasoning_traces (
    id BIGSERIAL PRIMARY KEY,
    reasoning_type TEXT NOT NULL,
    premise_ids BIGINT[] NOT NULL,
    conclusion TEXT NOT NULL,
    confidence DOUBLE PRECISION NOT NULL DEFAULT 0.5,
    evidence_json TEXT,
    created_observation_id BIGINT REFERENCES observations(id),
    dreamer_run_id BIGINT,
    is_valid BOOLEAN NOT NULL DEFAULT true,
    invalidated_by BIGINT REFERENCES reasoning_traces(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_rt_type ON reasoning_traces(reasoning_type);
CREATE INDEX IF NOT EXISTS idx_rt_run ON reasoning_traces(dreamer_run_id);
CREATE INDEX IF NOT EXISTS idx_rt_created ON reasoning_traces(created_at);
CREATE INDEX IF NOT EXISTS idx_rt_valid ON reasoning_traces(is_valid) WHERE is_valid;

ALTER TABLE memory_consolidation_log ADD COLUMN IF NOT EXISTS is_dreamer BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE memory_consolidation_log ADD COLUMN IF NOT EXISTS inductive_traces INTEGER NOT NULL DEFAULT 0;
ALTER TABLE memory_consolidation_log ADD COLUMN IF NOT EXISTS deductive_traces INTEGER NOT NULL DEFAULT 0;
ALTER TABLE memory_consolidation_log ADD COLUMN IF NOT EXISTS abductive_traces INTEGER NOT NULL DEFAULT 0;

-- ============================================================
-- Working representations (pre-computed context snapshots)
-- ============================================================
CREATE TABLE IF NOT EXISTS working_representations (
    id BIGSERIAL PRIMARY KEY,
    task_run_id TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    observations_json TEXT NOT NULL DEFAULT '[]',
    cross_run_patterns_json TEXT NOT NULL DEFAULT '[]',
    entity_profiles_json TEXT NOT NULL DEFAULT '[]',
    recent_findings_json TEXT NOT NULL DEFAULT '[]',
    recent_fixes_json TEXT NOT NULL DEFAULT '[]',
    applicable_skills_json TEXT NOT NULL DEFAULT '[]',
    workflow_id TEXT,
    workflow_name TEXT,
    total_items INTEGER NOT NULL DEFAULT 0,
    build_duration_ms BIGINT,
    built_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL,
    is_stale BOOLEAN NOT NULL DEFAULT false,
    UNIQUE(task_run_id)
);
CREATE INDEX IF NOT EXISTS idx_wr_task_run ON working_representations(task_run_id);
CREATE INDEX IF NOT EXISTS idx_wr_expires ON working_representations(expires_at);

-- ============================================================
-- Tables previously declared only in database/pg/mod.rs ensure_tables().
-- Hoisted into schema.pg.sql on 2026-04-08 as part of the drift audit
-- so Clorinde validation sees the same surface runtime expects. These
-- have no explicit FOREIGN KEYs (logical references documented in
-- comments) which matches the mod.rs declarations.
-- ============================================================

-- Span events (OpenTelemetry-style trace records for generator agents)
CREATE TABLE IF NOT EXISTS span_events (
    id              TEXT PRIMARY KEY,
    execution_id    TEXT NOT NULL,
    trace_id        TEXT NOT NULL,
    agent_type      TEXT NOT NULL,
    event_type      TEXT NOT NULL,
    step_index      INTEGER NOT NULL DEFAULT 0,
    metric_name     TEXT,
    reward_value    DOUBLE PRECISION,
    data_key        TEXT,
    data_json       TEXT,
    role            TEXT,
    content         TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_span_events_exec ON span_events(execution_id);
CREATE INDEX IF NOT EXISTS idx_span_events_trace ON span_events(trace_id);
CREATE INDEX IF NOT EXISTS idx_span_events_type ON span_events(event_type);

-- Duel pools / candidates / results (Copeland-style tournament evolution)
CREATE TABLE IF NOT EXISTS duel_pools (
    id              TEXT PRIMARY KEY,
    agent_type      TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'active',
    generation      INTEGER NOT NULL DEFAULT 0,
    config_json     TEXT NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at    TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_dp_agent ON duel_pools(agent_type);
CREATE INDEX IF NOT EXISTS idx_dp_status ON duel_pools(status);

CREATE TABLE IF NOT EXISTS duel_candidates (
    id              TEXT PRIMARY KEY,
    pool_id         TEXT NOT NULL,
    prompt_content  TEXT NOT NULL,
    variant_id      TEXT,
    generation      INTEGER NOT NULL DEFAULT 0,
    parent_id       TEXT,
    copeland_score  DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    alpha           DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    beta            DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    status          TEXT NOT NULL DEFAULT 'active',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_dc_pool ON duel_candidates(pool_id);
CREATE INDEX IF NOT EXISTS idx_dc_status ON duel_candidates(pool_id, status);

CREATE TABLE IF NOT EXISTS duel_results (
    id                  TEXT PRIMARY KEY,
    pool_id             TEXT NOT NULL,
    candidate_a_id      TEXT NOT NULL,
    candidate_b_id      TEXT NOT NULL,
    winner_id           TEXT NOT NULL,
    judge_rationale     TEXT,
    position_swapped    BOOLEAN NOT NULL DEFAULT false,
    confidence          DOUBLE PRECISION NOT NULL DEFAULT 0.5,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_dr_pool ON duel_results(pool_id);

-- Beam search runs / candidates (prompt evolution via beam search)
CREATE TABLE IF NOT EXISTS beam_search_runs (
    id              TEXT PRIMARY KEY,
    agent_type      TEXT NOT NULL,
    pool_id         TEXT,
    config_json     TEXT NOT NULL,
    generation      INTEGER NOT NULL DEFAULT 0,
    status          TEXT NOT NULL DEFAULT 'running',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at    TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_bsr_agent ON beam_search_runs(agent_type);
CREATE INDEX IF NOT EXISTS idx_bsr_pool ON beam_search_runs(pool_id);

CREATE TABLE IF NOT EXISTS beam_candidates (
    id              TEXT PRIMARY KEY,
    beam_run_id     TEXT NOT NULL,
    parent_id       TEXT,
    prompt_content  TEXT NOT NULL,
    critique        TEXT,
    changes_summary TEXT,
    generation      INTEGER NOT NULL DEFAULT 0,
    thinking_style  TEXT,
    variant_id      TEXT,
    status          TEXT NOT NULL DEFAULT 'active',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_bc_run ON beam_candidates(beam_run_id);
CREATE INDEX IF NOT EXISTS idx_bc_gen ON beam_candidates(beam_run_id, generation);

-- Resource version history (append-only versioned resource snapshots)
CREATE TABLE IF NOT EXISTS resource_versions (
    id              TEXT PRIMARY KEY,
    resource_type   TEXT NOT NULL,
    resource_key    TEXT NOT NULL,
    version         BIGINT NOT NULL,
    content_hash    TEXT NOT NULL,
    content         TEXT NOT NULL,
    metadata_json   TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (resource_type, resource_key, version)
);
CREATE INDEX IF NOT EXISTS idx_rv_resource ON resource_versions(resource_type, resource_key);
CREATE INDEX IF NOT EXISTS idx_rv_latest ON resource_versions(resource_type, resource_key, version DESC);
CREATE INDEX IF NOT EXISTS idx_rv_hash ON resource_versions(content_hash);

-- PR watcher state (GitHub PR auto-resume tracking)
CREATE TABLE IF NOT EXISTS pr_watch_state (
    id                  TEXT PRIMARY KEY,
    task_run_id         TEXT NOT NULL,
    pr_number           BIGINT NOT NULL,
    repo_full_name      TEXT NOT NULL,
    head_sha            TEXT NOT NULL DEFAULT '',
    workflow_id         TEXT NOT NULL DEFAULT '',
    last_checks_status  TEXT NOT NULL DEFAULT 'pending',
    last_review_status  TEXT NOT NULL DEFAULT 'pending',
    auto_resume_count   INTEGER NOT NULL DEFAULT 0,
    max_auto_resumes    INTEGER NOT NULL DEFAULT 10,
    github_token        TEXT NOT NULL DEFAULT '',
    auto_resume_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    completed_at        TIMESTAMPTZ,
    completion_reason   TEXT,
    last_polled_at      TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(task_run_id, pr_number)
);
CREATE INDEX IF NOT EXISTS idx_prw_active ON pr_watch_state(completed_at) WHERE completed_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_prw_task_run ON pr_watch_state(task_run_id);

-- Learned patterns (online learning distilled problem/solution pairs)
CREATE TABLE IF NOT EXISTS learned_patterns (
    id TEXT PRIMARY KEY,
    problem_hash TEXT NOT NULL UNIQUE,
    trigger_keywords JSONB NOT NULL,
    problem_description TEXT NOT NULL,
    solution_description TEXT NOT NULL,
    confidence DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    sample_count INTEGER NOT NULL DEFAULT 0,
    project_path TEXT,
    workflow_name TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_learned_patterns_confidence ON learned_patterns(confidence DESC);
CREATE INDEX IF NOT EXISTS idx_learned_patterns_workflow ON learned_patterns(workflow_name);
CREATE INDEX IF NOT EXISTS idx_learned_patterns_keywords_gin ON learned_patterns USING GIN(trigger_keywords);

-- Ticket system integration (external ticket provider mapping)
CREATE TABLE IF NOT EXISTS ticket_task_mapping (
    id TEXT PRIMARY KEY,
    ticket_source TEXT NOT NULL,
    ticket_external_id TEXT NOT NULL,
    ticket_url TEXT NOT NULL,
    task_run_id TEXT NOT NULL,
    workflow_id TEXT NOT NULL,
    sync_status TEXT NOT NULL DEFAULT 'synced',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(ticket_source, ticket_external_id)
);
CREATE INDEX IF NOT EXISTS idx_ticket_task_mapping_task ON ticket_task_mapping(task_run_id);

CREATE TABLE IF NOT EXISTS ticket_provider_configs (
    id TEXT PRIMARY KEY,
    workflow_id TEXT NOT NULL UNIQUE,
    source TEXT NOT NULL,
    config_json TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_ticket_provider_configs_workflow ON ticket_provider_configs(workflow_id);

-- UI Bridge visual regression baselines (persistent store for PgBaselineStore)
CREATE TABLE IF NOT EXISTS ui_bridge_baselines (
    id              TEXT PRIMARY KEY,
    target_scope    TEXT NOT NULL,
    fingerprint     TEXT,
    png_bytes       BYTEA NOT NULL,
    width           INTEGER NOT NULL,
    height          INTEGER NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    metadata_json   TEXT,
    ttl_days        INTEGER
);
CREATE INDEX IF NOT EXISTS idx_ui_bridge_baselines_target ON ui_bridge_baselines(target_scope);
