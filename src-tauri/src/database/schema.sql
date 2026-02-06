-- SQLite Schema for qontinui-runner
-- Version: 53
--
-- This schema provides persistent storage for task runs, settings,
-- prompts, and scheduler state.
--
-- Key concept: TaskRun is THE unified run concept for all execution.
-- GUI automation is one aspect of a task, not a separate system.
-- Every task runs until completion (marked by [TASK_COMPLETE]).
--
-- Version 12 migrated existing run_details to task_run_automation table.
-- Version 13 removes the deprecated run_details table.
-- Version 14 adds verification test infrastructure (verification_tests, test_results, test_associations).
-- Version 15 adds creation_analysis to verification_tests and visual_evidence to test_results.
-- Version 17 adds API request step support (api_credentials, api_request_logs, workflow_variables).
-- Version 18 adds saved_api_requests table for API Request Library.
-- Version 19 adds unified_workflows table for phase-based workflow builder.
-- Version 20 adds task_run_events, task_run_screenshots, task_run_playwright_results (hybrid logging).
-- Version 21 adds completion_steps and skip_ai_summary to unified_workflows.
-- Version 22 adds hybrid logging tables via migration (already in schema as of v20).
-- Version 23 adds task_knowledge_summaries (compression) and retry_state_json to task_runs.
-- Version 24 adds task_run_api_requests table for API request log migration.
-- Version 25 adds runtime_context_json to task_runs (execution context propagation) and task_hooks table (lifecycle hooks).
-- Version 26 adds task_run_awas_steps table (AWAS step execution results).
-- Version 27 adds code quality checks infrastructure (checks, check_results tables).
-- Version 28 adds performance optimization indexes for large datasets.
-- Version 29 adds shell_commands and shell_command_results tables for shell command library.
-- Version 30 adds mobile development feedback tables (task_run_mobile_state, task_run_mobile_logs).
-- Version 31 adds MCP integration tables (mcp_servers, task_run_mcp_calls).
-- Version 35 adds UI Bridge Inspector tables (ui_bridge_elements, ui_bridge_states, ui_bridge_transitions, ui_bridge_events, etc.).
-- Version 36 adds task hierarchy fields (parent_task_run_id, root_task_run_id, depth) for nested subtasks.
-- Version 41 adds log_watch_enabled to unified_workflows for automatic log error detection.
-- Version 42 adds health_check_enabled to unified_workflows for automatic server health checks.
-- Version 43 adds health_check_urls to unified_workflows for user-configurable health check URLs.
-- Version 44 adds error monitoring system (log_sources, error_events tables with FTS).
-- Version 45 adds bridge_id to task_runs for multi-bridge support.
-- Version 53 adds preflight_check_enabled to unified_workflows for automatic pre-flight environment checks.

-- Schema version tracking
CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);

-- Sessions & Checkpoints (unified)
-- Stores both active sessions and historical checkpoint data
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    session_type TEXT NOT NULL,  -- 'prompt_workflow', 'ai_builder', 'one_shot'
    name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'starting',  -- 'starting', 'running', 'completed', 'failed', 'stopped', 'waiting'
    current_phase INTEGER NOT NULL DEFAULT 0,
    total_phases INTEGER NOT NULL DEFAULT 0,
    completed BOOLEAN NOT NULL DEFAULT 0,
    restart_permitted BOOLEAN NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    completed_at TEXT,
    error_message TEXT,
    custom_data TEXT DEFAULT '{}',  -- JSON blob for extensible data
    activity_log TEXT DEFAULT '[]',  -- JSON array of activity strings
    run_id TEXT,  -- Groups sessions across continuations
    workflow_name TEXT  -- e.g., 'improve-all', 'find-misplaced'
);

CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);
CREATE INDEX IF NOT EXISTS idx_sessions_workflow_name ON sessions(workflow_name);
CREATE INDEX IF NOT EXISTS idx_sessions_run_id ON sessions(run_id);
CREATE INDEX IF NOT EXISTS idx_sessions_created_at ON sessions(created_at);

-- Session events (detailed history for debugging)
CREATE TABLE IF NOT EXISTS session_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    event_type TEXT NOT NULL,  -- 'started', 'phase_completed', 'error', 'checkpoint_updated', etc.
    message TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    data TEXT,  -- Optional JSON payload
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_session_events_session_id ON session_events(session_id);
CREATE INDEX IF NOT EXISTS idx_session_events_timestamp ON session_events(timestamp);

-- Active workflow configs (for cross-session continuation)
-- This replaces the active-workflow.json file
CREATE TABLE IF NOT EXISTS active_workflows (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workflow_name TEXT NOT NULL UNIQUE,  -- e.g., 'improve-all'
    checkpoint_data TEXT NOT NULL,  -- JSON: current_phase, repos_to_process, work_completed, etc.
    run_id TEXT NOT NULL,  -- UUID for this run
    phase_field TEXT NOT NULL DEFAULT 'current_phase',
    completion_value INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    completed BOOLEAN NOT NULL DEFAULT 0
);

-- GUI lock (singleton for exclusive access)
CREATE TABLE IF NOT EXISTS gui_lock (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    holder_session_id TEXT,
    acquired_at TEXT,
    FOREIGN KEY (holder_session_id) REFERENCES sessions(id) ON DELETE SET NULL
);

-- Scheduler tasks
CREATE TABLE IF NOT EXISTS scheduled_tasks (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    enabled BOOLEAN NOT NULL DEFAULT 1,
    schedule_type TEXT NOT NULL,  -- 'once', 'cron', 'interval'
    schedule_value TEXT NOT NULL,  -- ISO datetime, cron expr, or seconds
    task_config TEXT NOT NULL,  -- Full task configuration as JSON
    skip_if_completed BOOLEAN NOT NULL DEFAULT 0,
    auto_fix_on_failure BOOLEAN NOT NULL DEFAULT 0,
    success_criteria TEXT,
    created_at TEXT NOT NULL,
    modified_at TEXT NOT NULL,
    next_run TEXT,
    last_run_id TEXT
);

-- Scheduler execution history
CREATE TABLE IF NOT EXISTS scheduler_history (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    session_id TEXT,  -- If this triggered an AI session
    started_at TEXT NOT NULL,
    ended_at TEXT,
    status TEXT NOT NULL DEFAULT 'running',  -- 'running', 'completed', 'failed', 'skipped', 'cancelled'
    success BOOLEAN NOT NULL DEFAULT 0,
    error_message TEXT,
    triggered_auto_fix BOOLEAN NOT NULL DEFAULT 0,
    auto_fix_session_id TEXT,
    FOREIGN KEY (task_id) REFERENCES scheduled_tasks(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_scheduler_history_task_id ON scheduler_history(task_id);
CREATE INDEX IF NOT EXISTS idx_scheduler_history_started_at ON scheduler_history(started_at);

-- Scheduler settings (singleton)
CREATE TABLE IF NOT EXISTS scheduler_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    enabled BOOLEAN NOT NULL DEFAULT 1,
    max_concurrent INTEGER NOT NULL DEFAULT 1,
    default_auto_fix_on_failure BOOLEAN NOT NULL DEFAULT 0,
    timezone TEXT
);

-- Settings (key-value store with JSON values)
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,  -- JSON value
    updated_at TEXT NOT NULL
);

-- Prompts library
CREATE TABLE IF NOT EXISTS prompts (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    category TEXT,
    content TEXT NOT NULL,
    variables TEXT DEFAULT '[]',  -- JSON array of variable names
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_prompts_category ON prompts(category);

-- AI Workflows (legacy - kept for backward compatibility)
CREATE TABLE IF NOT EXISTS ai_workflows (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    config TEXT NOT NULL,  -- Full workflow config as JSON
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- Task Runs (unified task execution model)
-- TaskRun is THE single concept for all runs (AI, automation, or mixed).
-- GUI automation is one aspect of task execution, not a separate system.
-- Every task runs until [TASK_COMPLETE] marker is found in output.
CREATE TABLE IF NOT EXISTS task_runs (
    id TEXT PRIMARY KEY,
    task_name TEXT NOT NULL,
    prompt TEXT,  -- The task description/instructions (NULL for pure automation tasks)

    -- Task type: 'task' (default), 'automation', 'scheduled'
    task_type TEXT NOT NULL DEFAULT 'task',

    -- Status: 'running', 'complete', 'failed', 'stopped'
    status TEXT NOT NULL DEFAULT 'running',

    -- Session tracking (for AI-enabled tasks)
    sessions_count INTEGER NOT NULL DEFAULT 0,  -- How many Claude sessions spawned
    max_sessions INTEGER,  -- NULL = unlimited, otherwise max before giving up
    auto_continue BOOLEAN NOT NULL DEFAULT 1,  -- Per-run auto-continue setting

    -- Output
    output_log TEXT DEFAULT '',  -- Accumulated output with [SESSION_START:N] markers
    error_message TEXT,

    -- Execution configuration
    execution_steps_json TEXT,  -- JSON array of ExecutionStepConfig for re-execution on resume
    log_sources_json TEXT,  -- JSON array of LogSourceConfig for log capture during execution

    -- Config linkage (for automation-enabled tasks)
    config_id TEXT,  -- Foreign key to configs table (optional)
    workflow_name TEXT,  -- Workflow name being executed

    -- Summary (post-completion analysis)
    summary TEXT,  -- AI-generated paragraph summary of the task run (canonical)
    ai_summary TEXT,  -- Deprecated: kept for backward compatibility with COALESCE queries
    goal_achieved BOOLEAN,  -- Whether the stated goal was achieved
    remaining_work TEXT,  -- What remains to be done if goal was not achieved
    summary_generated_at TEXT,  -- Timestamp when the summary was generated

    -- Retry state (for retry with feedback injection)
    retry_state_json TEXT,  -- JSON: {attempt, last_error, error_history[], delay_history[]}

    -- Runtime context (for execution context propagation)
    runtime_context_json TEXT,  -- JSON: {variables, step_outputs, iteration}

    -- Orchestrator state transition history (for stage-based recap)
    transition_history_json TEXT,  -- JSON array of StateTransition objects

    -- Hierarchy (for nested task runs / subtasks)
    parent_task_run_id TEXT,  -- Parent task run ID (NULL for root-level tasks)
    root_task_run_id TEXT,    -- Root task run ID (top of hierarchy, same as id for root tasks)
    depth INTEGER DEFAULT 0,  -- Nesting depth (0 = root/top-level)

    -- Multi-bridge support (for concurrent execution)
    bridge_id TEXT,  -- Bridge ID handling this task (NULL for legacy single-bridge tasks)

    -- Workflow type ('unified', 'legacy_session', 'automation_only', or NULL for legacy)
    workflow_type TEXT,  -- Unified workflows should only have status modified by LoopController

    -- Web integration
    workspace_id TEXT,  -- Links task to a workspace/organization from qontinui-web
    triggered_by TEXT,  -- Identifies who/what triggered the task run

    -- Timestamps
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    completed_at TEXT,

    FOREIGN KEY (config_id) REFERENCES configs(id) ON DELETE SET NULL,
    FOREIGN KEY (parent_task_run_id) REFERENCES task_runs(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_task_runs_status ON task_runs(status);
CREATE INDEX IF NOT EXISTS idx_task_runs_created_at ON task_runs(created_at);
CREATE INDEX IF NOT EXISTS idx_task_runs_task_type ON task_runs(task_type);
CREATE INDEX IF NOT EXISTS idx_task_runs_config_id ON task_runs(config_id);
-- Hierarchy indexes for querying child/subtasks
CREATE INDEX IF NOT EXISTS idx_task_runs_parent_task_run_id ON task_runs(parent_task_run_id);
CREATE INDEX IF NOT EXISTS idx_task_runs_root_task_run_id ON task_runs(root_task_run_id);
-- Bridge ID index for multi-bridge queries
CREATE INDEX IF NOT EXISTS idx_task_runs_bridge_id ON task_runs(bridge_id);

-- Task Run Output Chunks (for efficient O(1) appending)
-- Instead of concatenating to output_log column (O(n)), we insert chunks (O(1))
-- Full output is reconstructed by joining chunks ordered by chunk_sequence
CREATE TABLE IF NOT EXISTS task_run_output_chunks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_run_id TEXT NOT NULL,
    chunk_sequence INTEGER NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_chunks_task_run ON task_run_output_chunks(task_run_id, chunk_sequence);

-- Task Run Findings (AI-detected issues tied to task runs)
-- Findings persist across sessions and are used for continuation context
CREATE TABLE IF NOT EXISTS task_run_findings (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,

    -- Finding identity (for deduplication)
    category TEXT NOT NULL,        -- 'code_bug', 'security', 'todo', etc.
    severity TEXT NOT NULL,        -- 'critical', 'high', 'medium', 'low', 'info'
    signature_hash TEXT,           -- Hash for deduplication across sessions

    -- Finding content
    title TEXT NOT NULL,
    description TEXT NOT NULL,

    -- Code context (optional)
    file_path TEXT,
    line_number INTEGER,
    column_number INTEGER,
    code_snippet TEXT,

    -- Lifecycle
    status TEXT NOT NULL DEFAULT 'detected',  -- 'detected', 'in_progress', 'needs_input', 'resolved', 'wont_fix', 'deferred'
    action_type TEXT NOT NULL DEFAULT 'auto_fix',  -- 'auto_fix', 'needs_user_input', 'informational'
    resolution TEXT,

    -- Session tracking
    detected_in_session INTEGER NOT NULL,
    resolved_in_session INTEGER,

    -- User interaction
    needs_input BOOLEAN DEFAULT 0,
    question TEXT,
    input_options TEXT,            -- JSON array of options
    user_response TEXT,

    -- Timestamps
    detected_at TEXT NOT NULL,
    resolved_at TEXT,
    updated_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_findings_task_run ON task_run_findings(task_run_id);
CREATE INDEX IF NOT EXISTS idx_findings_status ON task_run_findings(status);
CREATE INDEX IF NOT EXISTS idx_findings_signature ON task_run_findings(signature_hash);
CREATE INDEX IF NOT EXISTS idx_findings_category ON task_run_findings(category);

-- Task Run Automation (child table for automation metrics)
-- Stores automation execution data within a task run.
-- Some runs have ONLY automation, some have ONLY AI, some have BOTH.
CREATE TABLE IF NOT EXISTS task_run_automation (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,

    -- Workflow details
    workflow_name TEXT,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    duration_ms INTEGER,

    -- Status: 'running', 'success', 'failed', 'timeout', 'cancelled'
    automation_status TEXT NOT NULL DEFAULT 'running',
    success BOOLEAN,
    error_type TEXT,
    error_message TEXT,

    -- Metrics (same as current run_details)
    actions_summary TEXT,       -- JSON {"total": N, "success": N, "failed": N, "skipped": N}
    states_visited TEXT,        -- JSON array of state names
    transitions_executed TEXT,  -- JSON array of {from, to, action, success, duration_ms}
    template_matches TEXT,      -- JSON array of {template, count, avg_confidence, failures}
    anomalies TEXT,             -- JSON array for anomaly detection

    -- Iteration tracking
    iteration_number INTEGER NOT NULL DEFAULT 1,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_task_run_automation_task_run_id ON task_run_automation(task_run_id);
CREATE INDEX IF NOT EXISTS idx_task_run_automation_started_at ON task_run_automation(started_at);
CREATE INDEX IF NOT EXISTS idx_task_run_automation_status ON task_run_automation(automation_status);

-- Execution history (automation runs)
CREATE TABLE IF NOT EXISTS executions (
    id TEXT PRIMARY KEY,
    workflow_name TEXT,
    config_path TEXT,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    status TEXT NOT NULL,  -- 'running', 'completed', 'failed', 'stopped'
    success BOOLEAN,
    result_data TEXT,  -- JSON with execution results
    error_message TEXT
);

CREATE INDEX IF NOT EXISTS idx_executions_started_at ON executions(started_at);
CREATE INDEX IF NOT EXISTS idx_executions_workflow_name ON executions(workflow_name);

-- Configs storage (for auto-storing imported/loaded configs)
-- This enables the runner to track which configs have been loaded and allow quick switching.
CREATE TABLE IF NOT EXISTS configs (
    id TEXT PRIMARY KEY,           -- Config ID (project_id for web imports, hash for files)
    name TEXT NOT NULL,            -- Display name
    config_json TEXT NOT NULL,     -- Full QontinuiConfig as JSON
    source_type TEXT NOT NULL,     -- 'web' (from /rag/import) or 'file' (from /load-config)
    source_path TEXT,              -- File path if source_type='file'
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_configs_name ON configs(name);
CREATE INDEX IF NOT EXISTS idx_configs_updated_at ON configs(updated_at);

-- Config Statistics (Tier 4 - Aggregated statistics per config)
-- Stores computed statistics and patterns for AI debugging context
CREATE TABLE IF NOT EXISTS config_statistics (
    id TEXT PRIMARY KEY,
    config_id TEXT NOT NULL UNIQUE,
    config_hash TEXT,                -- Hash to detect config changes
    total_runs INTEGER DEFAULT 0,
    successful_runs INTEGER DEFAULT 0,
    failed_runs INTEGER DEFAULT 0,
    timeout_runs INTEGER DEFAULT 0,
    avg_duration_ms INTEGER,
    recent_success_rate REAL,        -- Success rate of last N runs (sliding window)
    recent_avg_duration_ms INTEGER,  -- Average duration of last N runs
    transition_stats TEXT,           -- JSON map {transition_key: {total, success, failure, avg_duration}}
    template_stats TEXT,             -- JSON map {template_name: {total, matches, failures, avg_confidence}}
    state_stats TEXT,                -- JSON map {state_name: {visits, avg_time_in_state, entry_failures}}
    error_patterns TEXT,             -- JSON map {error_type: {count, last_seen, contexts[]}}
    flaky_transitions TEXT,          -- JSON array of transition keys with high variance
    flaky_templates TEXT,            -- JSON array of template names with unreliable matching
    first_run_at TEXT,
    last_run_at TEXT,
    last_updated_at TEXT,
    FOREIGN KEY (config_id) REFERENCES configs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_config_statistics_config_id ON config_statistics(config_id);

-- Pending Discoveries (Discovery Push queue)
-- Stores discoveries awaiting sync to qontinui-web
CREATE TABLE IF NOT EXISTS pending_discoveries (
    id TEXT PRIMARY KEY,
    payload TEXT NOT NULL,        -- Full DiscoveryPayload as JSON
    created_at TEXT NOT NULL,
    last_attempt TEXT,            -- Last sync attempt timestamp
    attempt_count INTEGER DEFAULT 0,
    error TEXT                    -- Last error message from failed sync
);

CREATE INDEX IF NOT EXISTS idx_pending_discoveries_created_at ON pending_discoveries(created_at);
CREATE INDEX IF NOT EXISTS idx_pending_discoveries_attempt_count ON pending_discoveries(attempt_count);

-- Verification Tests (test definitions stored in runner)
-- Database-first storage with file import/export for version control
CREATE TABLE IF NOT EXISTS verification_tests (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,

    -- Test type: 'playwright_cdp', 'qontinui_vision', 'python_script', 'repository_test'
    test_type TEXT NOT NULL,

    -- Category for organization
    category TEXT,  -- 'visual', 'dom', 'network', 'data', 'log', 'layout', 'unit', 'integration', 'custom'

    -- Code/config storage (one of these based on test_type)
    playwright_code TEXT,      -- TypeScript code for playwright_cdp
    vision_config TEXT,        -- JSON config for qontinui_vision
    python_code TEXT,          -- Python code for python_script
    repo_test_config TEXT,     -- JSON config for repository_test

    -- Natural language description for AI generation
    success_criteria TEXT,

    -- Test configuration (JSON)
    config TEXT DEFAULT '{}',  -- timeout_seconds, cdp_port, env_vars, etc.

    timeout_seconds INTEGER,  -- NULL = no timeout (default)
    is_critical BOOLEAN NOT NULL DEFAULT 0,  -- If true, failure fails the task (default: false for iterative AI workflows)
    enabled BOOLEAN NOT NULL DEFAULT 1,

    -- AI generation tracking
    ai_generated BOOLEAN NOT NULL DEFAULT 0,
    ai_generation_prompt TEXT,

    -- Page analysis captured during test creation (for AI debugging)
    creation_analysis TEXT,   -- JSON: screenshot, annotated_screenshot, elements[], source, url, etc.

    -- Organization
    tags TEXT DEFAULT '[]',  -- JSON array

    -- File tracking (for import/export)
    source_file TEXT,         -- Original file path if imported
    last_exported_at TEXT,    -- Timestamp of last export

    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_verification_tests_test_type ON verification_tests(test_type);
CREATE INDEX IF NOT EXISTS idx_verification_tests_category ON verification_tests(category);
CREATE INDEX IF NOT EXISTS idx_verification_tests_enabled ON verification_tests(enabled);

-- Test Results (execution results linked to task runs)
CREATE TABLE IF NOT EXISTS test_results (
    id TEXT PRIMARY KEY,
    test_id TEXT NOT NULL,
    task_run_id TEXT,  -- Links to task_runs table

    -- Status: 'pending', 'running', 'passed', 'failed', 'skipped', 'error', 'timeout'
    status TEXT NOT NULL DEFAULT 'pending',

    started_at TEXT,
    completed_at TEXT,
    duration_ms INTEGER,

    -- Output
    output TEXT,              -- stdout/stderr combined
    error_message TEXT,       -- Error message if failed
    structured_output TEXT,   -- JSON: parsed assertions, metrics, coverage

    -- Assertions summary
    assertions_passed INTEGER DEFAULT 0,
    assertions_failed INTEGER DEFAULT 0,

    -- Screenshots (JSON array of paths)
    screenshots TEXT DEFAULT '[]',

    -- Visual evidence (annotated screenshots with assertion overlays)
    visual_evidence TEXT,     -- JSON: annotated_screenshot_base64, assertion_overlays[], etc.

    -- AI analysis
    ai_analysis TEXT,

    created_at TEXT NOT NULL,

    FOREIGN KEY (test_id) REFERENCES verification_tests(id) ON DELETE CASCADE,
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_test_results_test_id ON test_results(test_id);
CREATE INDEX IF NOT EXISTS idx_test_results_task_run_id ON test_results(task_run_id);
CREATE INDEX IF NOT EXISTS idx_test_results_status ON test_results(status);

-- Test Associations (link tests to configs/workflows)
CREATE TABLE IF NOT EXISTS test_associations (
    id TEXT PRIMARY KEY,
    test_id TEXT NOT NULL,
    config_id TEXT,           -- Links to configs table
    workflow_name TEXT,       -- Workflow name within config

    -- Trigger point
    trigger_point TEXT NOT NULL,  -- 'before_workflow', 'after_workflow', 'on_action', 'manual'
    action_id TEXT,               -- Specific action ID for 'on_action' trigger

    execution_order INTEGER DEFAULT 0,
    enabled BOOLEAN NOT NULL DEFAULT 1,

    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,

    FOREIGN KEY (test_id) REFERENCES verification_tests(id) ON DELETE CASCADE,
    FOREIGN KEY (config_id) REFERENCES configs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_test_associations_test_id ON test_associations(test_id);
CREATE INDEX IF NOT EXISTS idx_test_associations_config_id ON test_associations(config_id);

-- Verification Plans (orchestrator architecture)
-- Created by the Planning Agent at task start, may be revised on replan
CREATE TABLE IF NOT EXISTS verification_plans (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,

    -- Plan version (incremented on replan)
    version INTEGER NOT NULL DEFAULT 1,

    -- The complete plan as JSON (VerificationPlan struct)
    plan_json TEXT NOT NULL,

    -- Summary fields for quick access
    goal_summary TEXT NOT NULL,
    criteria_count INTEGER NOT NULL DEFAULT 0,
    has_ai_criteria BOOLEAN NOT NULL DEFAULT 0,

    -- Replan tracking
    replan_reason TEXT,           -- Why this version was created (null for v1)
    previous_version_id TEXT,     -- Link to previous version (null for v1)

    created_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (previous_version_id) REFERENCES verification_plans(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_verification_plans_task_run_id ON verification_plans(task_run_id);
CREATE INDEX IF NOT EXISTS idx_verification_plans_version ON verification_plans(version);

-- Task Knowledge (findings, observations, context across iterations)
-- Shared knowledge base for planning, worker, and verification agents
CREATE TABLE IF NOT EXISTS task_knowledge (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,

    -- Knowledge category
    category TEXT NOT NULL,       -- 'finding', 'root_cause', 'observation', 'hypothesis', 'solution', 'context'

    -- Which agent created this
    agent_type TEXT NOT NULL,     -- 'planning', 'worker', 'verification', 'system'

    -- Iteration when created
    iteration INTEGER NOT NULL DEFAULT 1,

    -- Content
    content TEXT NOT NULL,        -- The finding/observation text
    evidence TEXT,                -- Supporting evidence (file paths, log excerpts, etc.)
    confidence TEXT DEFAULT 'medium',  -- 'high', 'medium', 'low'

    -- Related entities
    related_files TEXT DEFAULT '[]',   -- JSON array of file paths
    related_criterion_id TEXT,         -- Links to success criterion if applicable

    -- Resolution tracking
    is_resolved BOOLEAN NOT NULL DEFAULT 0,
    resolution_notes TEXT,
    resolved_at TEXT,

    created_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_task_knowledge_task_run_id ON task_knowledge(task_run_id);
CREATE INDEX IF NOT EXISTS idx_task_knowledge_category ON task_knowledge(category);
CREATE INDEX IF NOT EXISTS idx_task_knowledge_is_resolved ON task_knowledge(is_resolved);

-- Task Knowledge Summaries (Memory Compression)
-- Stores compressed summaries of old knowledge entries to prevent context overflow
CREATE TABLE IF NOT EXISTS task_knowledge_summaries (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    category TEXT NOT NULL,           -- 'finding', 'observation', 'verification_feedback', 'solution'
    summary TEXT NOT NULL,            -- Compressed summary of multiple entries
    covered_iterations TEXT NOT NULL, -- JSON array of iteration numbers covered
    item_count INTEGER NOT NULL,      -- Number of items summarized
    original_tokens INTEGER,          -- Estimated token count before compression
    compressed_tokens INTEGER,        -- Estimated token count after compression
    created_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_task_knowledge_summaries_task_run_id ON task_knowledge_summaries(task_run_id);
CREATE INDEX IF NOT EXISTS idx_task_knowledge_summaries_category ON task_knowledge_summaries(category);

-- Verification Results (per-iteration, per-criterion results)
-- Tracks deterministic and AI verification outcomes
CREATE TABLE IF NOT EXISTS orchestrator_verification_results (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    plan_id TEXT NOT NULL,

    -- Which iteration this result is from
    iteration INTEGER NOT NULL,

    -- Which criterion was verified
    criterion_id TEXT NOT NULL,
    criterion_type TEXT NOT NULL,  -- 'deterministic', 'ai_evaluated'

    -- Result
    passed BOOLEAN NOT NULL,
    is_critical BOOLEAN NOT NULL DEFAULT 1,
    confidence TEXT,              -- For AI verification: 'high', 'medium', 'low'

    -- Details
    observations TEXT DEFAULT '[]',    -- JSON array of observations
    issues TEXT DEFAULT '[]',          -- JSON array of issues found
    suggestions TEXT DEFAULT '[]',     -- JSON array of fix suggestions
    raw_output TEXT,                   -- Full output from check/evaluation

    created_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (plan_id) REFERENCES verification_plans(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_orch_ver_results_task_run_id ON orchestrator_verification_results(task_run_id);
CREATE INDEX IF NOT EXISTS idx_orch_ver_results_plan_id ON orchestrator_verification_results(plan_id);
CREATE INDEX IF NOT EXISTS idx_orch_ver_results_iteration ON orchestrator_verification_results(iteration);
CREATE INDEX IF NOT EXISTS idx_orch_ver_results_passed ON orchestrator_verification_results(passed);

-- API Credentials (metadata only, secrets in secure storage)
-- Used for API request step authentication
CREATE TABLE IF NOT EXISTS api_credentials (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    credential_type TEXT NOT NULL,  -- 'bearer_token', 'basic_auth', 'api_key', 'oauth2'
    storage_type TEXT NOT NULL DEFAULT 'secure',  -- 'secure' (encrypted file) or 'session' (memory only)

    -- OAuth2 specific
    token_endpoint TEXT,    -- Token endpoint URL for refresh
    client_id TEXT,

    -- Timestamps
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    expires_at TEXT         -- When the credential expires (for tokens)
);

CREATE INDEX IF NOT EXISTS idx_api_credentials_name ON api_credentials(name);
CREATE INDEX IF NOT EXISTS idx_api_credentials_type ON api_credentials(credential_type);

-- API Request Logs (persisted for history, supplements JSONL for DB queries)
-- Stores API request execution results linked to task runs
CREATE TABLE IF NOT EXISTS api_request_logs (
    id TEXT PRIMARY KEY,
    task_run_id TEXT,
    step_id TEXT NOT NULL,
    step_name TEXT,

    -- Request info
    method TEXT NOT NULL,
    url TEXT NOT NULL,
    resolved_url TEXT NOT NULL,  -- URL after variable substitution

    -- Response info
    status_code INTEGER NOT NULL,
    response_time_ms INTEGER NOT NULL,
    response_body_type TEXT NOT NULL,  -- 'json', 'text', 'binary'
    response_file_path TEXT,           -- For binary responses saved to disk
    response_size_bytes INTEGER,

    -- Results
    success BOOLEAN NOT NULL,
    assertion_failures INTEGER DEFAULT 0,
    extractions_json TEXT,   -- JSON array of extraction results
    assertions_json TEXT,    -- JSON array of assertion results
    error TEXT,

    created_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_api_request_logs_task_run_id ON api_request_logs(task_run_id);
CREATE INDEX IF NOT EXISTS idx_api_request_logs_created_at ON api_request_logs(created_at);
CREATE INDEX IF NOT EXISTS idx_api_request_logs_step_id ON api_request_logs(step_id);

-- Workflow Variables (session-scoped variables for API request substitution)
-- Stores extracted values from API responses for use in subsequent steps
CREATE TABLE IF NOT EXISTS workflow_variables (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    variable_name TEXT NOT NULL,
    variable_value TEXT NOT NULL,
    source TEXT NOT NULL,          -- 'api_extraction', 'step_output', 'user_defined'
    source_step_id TEXT,           -- Step that created this variable
    created_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    UNIQUE(task_run_id, variable_name)  -- Each variable unique per task run
);

CREATE INDEX IF NOT EXISTS idx_workflow_variables_task_run_id ON workflow_variables(task_run_id);
CREATE INDEX IF NOT EXISTS idx_workflow_variables_name ON workflow_variables(variable_name);

-- Saved API Request Templates (Library)
-- Reusable API request configurations that can be inserted into workflows
CREATE TABLE IF NOT EXISTS saved_api_requests (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    category TEXT DEFAULT 'general',
    tags TEXT DEFAULT '[]',  -- JSON array

    -- Request configuration (matches ApiRequestStep fields)
    method TEXT NOT NULL DEFAULT 'GET',
    url TEXT NOT NULL,
    headers TEXT DEFAULT '{}',  -- JSON object {key: value}
    body TEXT,
    body_content_type TEXT DEFAULT 'application/json',
    timeout_ms INTEGER DEFAULT 30000,
    follow_redirects BOOLEAN DEFAULT 1,

    -- Variable extractions (JSON array of ApiVariableExtraction)
    variable_extractions TEXT DEFAULT '[]',
    -- Assertions (JSON array of ApiAssertion)
    assertions TEXT DEFAULT '[]',

    -- Credential reference
    credential_id TEXT,

    -- Timestamps
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,

    FOREIGN KEY (credential_id) REFERENCES api_credentials(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_saved_api_requests_category ON saved_api_requests(category);
CREATE INDEX IF NOT EXISTS idx_saved_api_requests_updated_at ON saved_api_requests(updated_at);
CREATE INDEX IF NOT EXISTS idx_saved_api_requests_name ON saved_api_requests(name);

-- =============================================================================
-- Unified Workflows (Phase-based workflow builder)
-- =============================================================================
-- New workflow format with three phases: setup, verification, agentic
-- Each phase contains an array of typed steps
CREATE TABLE IF NOT EXISTS unified_workflows (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT DEFAULT '',
    category TEXT DEFAULT 'general',
    tags TEXT DEFAULT '[]',  -- JSON array of strings

    -- Phase steps (JSON arrays)
    setup_steps TEXT DEFAULT '[]',         -- JSON array of SetupStep
    verification_steps TEXT DEFAULT '[]',   -- JSON array of VerificationStep
    agentic_steps TEXT DEFAULT '[]',        -- JSON array of AgenticStep

    -- Agentic configuration
    max_iterations INTEGER DEFAULT 10,
    provider TEXT,  -- 'claude_cli', 'gemini_api', etc.
    model TEXT,     -- Model identifier

    -- Log watch configuration
    log_watch_enabled INTEGER DEFAULT 1,  -- 1 = enabled (default), 0 = disabled

    -- Health check configuration
    health_check_enabled INTEGER DEFAULT 1,  -- 1 = enabled (default), 0 = disabled
    health_check_urls TEXT DEFAULT '[]',  -- JSON array of { name, url, expected_status, timeout_seconds, is_critical }

    -- Pre-flight check configuration
    preflight_check_enabled INTEGER DEFAULT 1,  -- 1 = enabled (default), 0 = disabled

    -- Timestamps
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_unified_workflows_category ON unified_workflows(category);
CREATE INDEX IF NOT EXISTS idx_unified_workflows_updated_at ON unified_workflows(updated_at);
CREATE INDEX IF NOT EXISTS idx_unified_workflows_name ON unified_workflows(name);

-- =============================================================================
-- Task Run Event Logs (Phase 10: Hybrid Event Logging)
-- =============================================================================
-- Unified event storage for all execution events (replaces JSONL files for historical queries)
-- JSONL files remain for real-time streaming, this table for post-execution persistence

-- Task Run Events (unifies runner-general.jsonl, runner-actions.jsonl, runner-image-recognition.jsonl)
CREATE TABLE IF NOT EXISTS task_run_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_run_id TEXT NOT NULL,

    -- Event classification
    event_type TEXT NOT NULL,           -- 'general', 'action', 'image_recognition', 'state_change', 'ai_output'
    event_subtype TEXT,                 -- 'start', 'complete', 'error', 'match', 'transition', etc.

    -- Content
    message TEXT NOT NULL,
    data TEXT,                          -- JSON payload (action details, match results, etc.)

    -- Context
    workflow_name TEXT,
    state_name TEXT,
    action_id TEXT,

    -- Timing
    timestamp TEXT NOT NULL,
    duration_ms INTEGER,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_task_run_events_task_run_id ON task_run_events(task_run_id);
CREATE INDEX IF NOT EXISTS idx_task_run_events_event_type ON task_run_events(event_type);
CREATE INDEX IF NOT EXISTS idx_task_run_events_timestamp ON task_run_events(timestamp);

-- Task Run Screenshots (from runner-image-recognition.jsonl annotated screenshots)
CREATE TABLE IF NOT EXISTS task_run_screenshots (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    event_id INTEGER,                   -- Links to task_run_events if applicable

    -- Screenshot info
    file_path TEXT NOT NULL,            -- Path to PNG file in .dev-logs/screenshots/
    screenshot_type TEXT NOT NULL,      -- 'annotated', 'raw', 'diff', 'failure'

    -- Context
    template_name TEXT,
    confidence REAL,
    match_location TEXT,                -- JSON {x, y, width, height}

    -- Metadata
    width INTEGER,
    height INTEGER,
    file_size_bytes INTEGER,

    created_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (event_id) REFERENCES task_run_events(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_task_run_screenshots_task_run_id ON task_run_screenshots(task_run_id);
CREATE INDEX IF NOT EXISTS idx_task_run_screenshots_type ON task_run_screenshots(screenshot_type);

-- Task Run Playwright Results (from runner-playwright.jsonl)
CREATE TABLE IF NOT EXISTS task_run_playwright_results (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,

    -- Test identification
    test_name TEXT NOT NULL,
    spec_file TEXT,

    -- Results
    status TEXT NOT NULL,               -- 'passed', 'failed', 'skipped', 'timeout'
    duration_ms INTEGER,

    -- Output
    stdout TEXT,
    stderr TEXT,
    console_output TEXT,                -- JSON array of console messages
    page_snapshot TEXT,                 -- YAML page snapshot

    -- Failure details
    error_message TEXT,
    failure_screenshot_path TEXT,

    -- Assertion summary
    assertions_passed INTEGER DEFAULT 0,
    assertions_failed INTEGER DEFAULT 0,

    created_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_task_run_playwright_task_run_id ON task_run_playwright_results(task_run_id);
CREATE INDEX IF NOT EXISTS idx_task_run_playwright_status ON task_run_playwright_results(status);

-- Task Run API Requests (from runner-api-requests.jsonl)
-- Stores API request execution results migrated from JSONL logs
CREATE TABLE IF NOT EXISTS task_run_api_requests (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,

    -- Step identification
    step_id TEXT NOT NULL,
    step_name TEXT,

    -- Request details
    method TEXT NOT NULL,
    url TEXT NOT NULL,
    resolved_url TEXT NOT NULL,           -- URL after variable substitution
    request_headers TEXT,                 -- JSON object {header: value}
    request_body TEXT,

    -- Response details
    status_code INTEGER NOT NULL,
    status_text TEXT,
    response_headers TEXT,                -- JSON object {header: value}
    response_time_ms INTEGER NOT NULL,

    -- Response body handling
    response_body_type TEXT NOT NULL,     -- 'json', 'text', 'binary'
    response_body TEXT,
    response_size_bytes INTEGER,

    -- Variable extractions (JSON array of extraction results)
    extractions TEXT,                     -- JSON array of {variable_name, json_path, extracted_value, success, error}

    -- Assertion results (JSON array of assertion results)
    assertions TEXT,                      -- JSON array of {assertion_type, expected, actual, passed, error}

    -- Overall result
    success BOOLEAN NOT NULL,
    error_message TEXT,

    created_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_task_run_api_requests_task_run_id ON task_run_api_requests(task_run_id);
CREATE INDEX IF NOT EXISTS idx_task_run_api_requests_step_id ON task_run_api_requests(step_id);
CREATE INDEX IF NOT EXISTS idx_task_run_api_requests_created_at ON task_run_api_requests(created_at);
CREATE INDEX IF NOT EXISTS idx_task_run_api_requests_success ON task_run_api_requests(success);

-- =============================================================================
-- Task Hooks (Lifecycle hooks for execution events)
-- =============================================================================
-- Stores hook definitions that trigger on specific execution events
CREATE TABLE IF NOT EXISTS task_hooks (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,

    -- Hook trigger: 'pre_execution', 'post_execution', 'on_error', 'on_verification_fail', 'on_complete', 'pre_iteration', 'post_iteration'
    trigger TEXT NOT NULL,

    -- Hook action configuration
    action_type TEXT NOT NULL,          -- 'command', 'webhook', 'log', 'notification'
    action_config TEXT NOT NULL,        -- JSON: {command, url, headers, body, message, etc.}

    -- Execution settings
    enabled BOOLEAN DEFAULT 1,
    execution_order INTEGER DEFAULT 0,  -- Lower = executes earlier
    continue_on_failure BOOLEAN DEFAULT 1,

    -- Optional conditions (JSON array of {variable, operator, value})
    conditions TEXT DEFAULT '[]',

    -- Scope: NULL = global, or specific task_run_id for task-specific hooks
    task_run_id TEXT,

    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_task_hooks_trigger ON task_hooks(trigger);
CREATE INDEX IF NOT EXISTS idx_task_hooks_enabled ON task_hooks(enabled);
CREATE INDEX IF NOT EXISTS idx_task_hooks_task_run_id ON task_hooks(task_run_id);

-- =============================================================================
-- Task Run AWAS Steps (AWAS step execution results)
-- =============================================================================
-- Stores execution results from AWAS (Automated Web Agent System) steps
-- including discovery, execution, action listing, and element extraction
CREATE TABLE IF NOT EXISTS task_run_awas_steps (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,

    -- Step identification
    step_id TEXT,                           -- Optional step ID from workflow
    step_name TEXT,                         -- Optional human-readable step name
    step_type TEXT NOT NULL,                -- 'awas_discover', 'awas_execute', 'awas_check_support', 'awas_list_actions', 'awas_extract_elements'

    -- Context
    url TEXT,                               -- URL where the step was executed

    -- Execution parameters
    action_id TEXT,                         -- For awas_execute: the action that was executed
    parameters TEXT,                        -- JSON: step-specific parameters

    -- Response data
    response_data TEXT,                     -- JSON: contains manifest, actions, elements, etc. depending on step_type

    -- Results
    success INTEGER NOT NULL DEFAULT 0,
    error_message TEXT,
    duration_ms INTEGER,

    created_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_task_run_awas_steps_task_run_id ON task_run_awas_steps(task_run_id);
CREATE INDEX IF NOT EXISTS idx_task_run_awas_steps_step_type ON task_run_awas_steps(step_type);
CREATE INDEX IF NOT EXISTS idx_task_run_awas_steps_created_at ON task_run_awas_steps(created_at);

-- =============================================================================
-- Orchestrator Learning System (Version 26)
-- =============================================================================
-- Learning outcomes, patterns, checkpoints, and flows for AI orchestration

-- Learning Outcomes: Records task outcomes for learning system
CREATE TABLE IF NOT EXISTS learning_outcomes (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    status TEXT NOT NULL,  -- 'success', 'failure', 'partial'
    duration_secs REAL,
    iterations INTEGER,
    strategy TEXT,
    tools_used TEXT,  -- JSON array
    files_modified TEXT,  -- JSON array
    error_type TEXT,
    error_message TEXT,
    feedback TEXT,  -- JSON array
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_learning_outcomes_task_id ON learning_outcomes(task_id);
CREATE INDEX IF NOT EXISTS idx_learning_outcomes_status ON learning_outcomes(status);
CREATE INDEX IF NOT EXISTS idx_learning_outcomes_created_at ON learning_outcomes(created_at);

-- Learning Patterns: Identified patterns from task analysis
CREATE TABLE IF NOT EXISTS learning_patterns (
    id TEXT PRIMARY KEY,
    pattern_type TEXT NOT NULL,  -- 'success', 'failure', 'tool_usage', etc.
    description TEXT NOT NULL,
    confidence REAL NOT NULL,
    occurrences INTEGER NOT NULL DEFAULT 1,
    context TEXT,  -- JSON with additional context
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_learning_patterns_type ON learning_patterns(pattern_type);
CREATE INDEX IF NOT EXISTS idx_learning_patterns_confidence ON learning_patterns(confidence);

-- Orchestrator Checkpoints: State snapshots for time-travel debugging
CREATE TABLE IF NOT EXISTS orchestrator_checkpoints (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    iteration INTEGER NOT NULL,
    trigger TEXT NOT NULL,  -- 'automatic', 'manual', 'iteration_boundary', etc.
    state TEXT NOT NULL,  -- JSON serialized StateSnapshot
    name TEXT,  -- Optional user-provided name
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_orchestrator_checkpoints_task_id ON orchestrator_checkpoints(task_id);
CREATE INDEX IF NOT EXISTS idx_orchestrator_checkpoints_task_iteration ON orchestrator_checkpoints(task_id, iteration);

-- Orchestrator Flows: Flow definitions for deterministic workflows
CREATE TABLE IF NOT EXISTS orchestrator_flows (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    steps TEXT NOT NULL,  -- JSON object of step definitions
    start_step TEXT,
    timeout_secs INTEGER,
    inputs TEXT,  -- JSON array of input definitions
    outputs TEXT,  -- JSON array of output definitions
    tags TEXT,  -- JSON array
    version TEXT NOT NULL DEFAULT '1.0.0',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_orchestrator_flows_name ON orchestrator_flows(name);

-- Flow Executions: Runtime state for flow execution
CREATE TABLE IF NOT EXISTS flow_executions (
    instance_id TEXT PRIMARY KEY,
    flow_id TEXT NOT NULL,
    current_step TEXT,
    status TEXT NOT NULL DEFAULT 'pending',  -- 'pending', 'running', 'waiting_for_input', 'completed', 'failed', 'cancelled'
    context TEXT,  -- JSON object of flow variables
    history TEXT,  -- JSON array of step executions
    error TEXT,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    FOREIGN KEY (flow_id) REFERENCES orchestrator_flows(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_flow_executions_flow_id ON flow_executions(flow_id);
CREATE INDEX IF NOT EXISTS idx_flow_executions_status ON flow_executions(status);

-- Flow Versions: Version history for flow definitions (Version 46)
CREATE TABLE IF NOT EXISTS flow_versions (
    id TEXT PRIMARY KEY,
    flow_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    definition TEXT NOT NULL,       -- Full flow JSON snapshot
    message TEXT,                   -- Version description/commit message
    created_by TEXT,                -- User or system that created this version
    created_at TEXT NOT NULL,
    FOREIGN KEY (flow_id) REFERENCES orchestrator_flows(id) ON DELETE CASCADE,
    UNIQUE(flow_id, version)
);
CREATE INDEX IF NOT EXISTS idx_flow_versions_flow_id ON flow_versions(flow_id);
CREATE INDEX IF NOT EXISTS idx_flow_versions_flow_version ON flow_versions(flow_id, version);

-- =============================================================================
-- Code Quality Checks (Version 27)
-- =============================================================================
-- Check definitions stored in runner for code quality validation

CREATE TABLE IF NOT EXISTS checks (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    check_type TEXT NOT NULL,         -- 'lint', 'format', 'typecheck', 'custom_command'
    tool TEXT NOT NULL,               -- 'black', 'isort', 'ruff', 'mypy', 'eslint', 'prettier', etc.
    command TEXT,                     -- Custom command override
    working_directory TEXT,
    config_path TEXT,                 -- Path to config file (e.g., pyproject.toml)
    auto_fix BOOLEAN NOT NULL DEFAULT 0,
    fail_on_warning BOOLEAN NOT NULL DEFAULT 0,
    timeout_seconds INTEGER,  -- NULL = no timeout (default)
    is_critical BOOLEAN NOT NULL DEFAULT 0,
    enabled BOOLEAN NOT NULL DEFAULT 1,
    ai_generated BOOLEAN NOT NULL DEFAULT 0,
    ai_generation_prompt TEXT,
    tags TEXT DEFAULT '[]',           -- JSON array
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_checks_check_type ON checks(check_type);
CREATE INDEX IF NOT EXISTS idx_checks_tool ON checks(tool);
CREATE INDEX IF NOT EXISTS idx_checks_enabled ON checks(enabled);

-- Check Results (execution results linked to task runs)
CREATE TABLE IF NOT EXISTS check_results (
    id TEXT PRIMARY KEY,
    check_id TEXT NOT NULL,
    task_run_id TEXT,
    status TEXT NOT NULL DEFAULT 'pending',  -- 'pending', 'running', 'passed', 'failed', 'error', 'timeout'
    started_at TEXT,
    completed_at TEXT,
    duration_ms INTEGER,
    output TEXT,                      -- stdout/stderr combined
    error_message TEXT,
    issues_found INTEGER DEFAULT 0,
    issues_fixed INTEGER DEFAULT 0,
    files_checked INTEGER DEFAULT 0,
    structured_output TEXT,           -- JSON: parsed issues, file-by-file results
    created_at TEXT NOT NULL,
    FOREIGN KEY (check_id) REFERENCES checks(id) ON DELETE CASCADE,
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_check_results_check_id ON check_results(check_id);
CREATE INDEX IF NOT EXISTS idx_check_results_task_run_id ON check_results(task_run_id);
CREATE INDEX IF NOT EXISTS idx_check_results_status ON check_results(status);

-- Check Groups (organize checks into reusable groups)
CREATE TABLE IF NOT EXISTS check_groups (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    color TEXT,                       -- Color for UI display (e.g., 'purple', 'blue')
    enabled BOOLEAN NOT NULL DEFAULT 1,
    run_in_parallel BOOLEAN NOT NULL DEFAULT 0,  -- Run checks in parallel or sequential
    stop_on_failure BOOLEAN NOT NULL DEFAULT 1,  -- Stop running checks if one fails
    tags TEXT DEFAULT '[]',           -- JSON array
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_check_groups_enabled ON check_groups(enabled);

-- Check Group Members (many-to-many relationship)
CREATE TABLE IF NOT EXISTS check_group_members (
    id TEXT PRIMARY KEY,
    group_id TEXT NOT NULL,
    check_id TEXT NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    FOREIGN KEY (group_id) REFERENCES check_groups(id) ON DELETE CASCADE,
    FOREIGN KEY (check_id) REFERENCES checks(id) ON DELETE CASCADE,
    UNIQUE(group_id, check_id)
);

CREATE INDEX IF NOT EXISTS idx_check_group_members_group_id ON check_group_members(group_id);
CREATE INDEX IF NOT EXISTS idx_check_group_members_check_id ON check_group_members(check_id);

-- =============================================================================
-- Shell Commands Library (Version 29)
-- =============================================================================
-- Reusable shell command definitions and execution history

-- Shell Commands Library
CREATE TABLE IF NOT EXISTS shell_commands (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    command TEXT NOT NULL,
    working_directory TEXT,
    timeout_seconds INTEGER,  -- NULL = no timeout (default)
    fail_on_error BOOLEAN NOT NULL DEFAULT 1,
    category TEXT DEFAULT 'general',  -- 'git', 'npm', 'poetry', 'docker', 'general'
    tags TEXT DEFAULT '[]',           -- JSON array
    enabled BOOLEAN NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_shell_commands_category ON shell_commands(category);
CREATE INDEX IF NOT EXISTS idx_shell_commands_enabled ON shell_commands(enabled);
CREATE INDEX IF NOT EXISTS idx_shell_commands_name ON shell_commands(name);
CREATE INDEX IF NOT EXISTS idx_shell_commands_created_at ON shell_commands(created_at);
CREATE INDEX IF NOT EXISTS idx_shell_commands_updated_at ON shell_commands(updated_at);

-- Shell Command Results (execution history)
CREATE TABLE IF NOT EXISTS shell_command_results (
    id TEXT PRIMARY KEY,
    shell_command_id TEXT NOT NULL,
    task_run_id TEXT,
    status TEXT NOT NULL DEFAULT 'pending',  -- 'pending', 'running', 'success', 'failed', 'error', 'timeout'
    exit_code INTEGER,
    stdout TEXT,
    stderr TEXT,
    duration_ms INTEGER,
    started_at TEXT,
    completed_at TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY (shell_command_id) REFERENCES shell_commands(id) ON DELETE CASCADE,
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_shell_command_results_shell_command_id ON shell_command_results(shell_command_id);
CREATE INDEX IF NOT EXISTS idx_shell_command_results_task_run_id ON shell_command_results(task_run_id);
CREATE INDEX IF NOT EXISTS idx_shell_command_results_status ON shell_command_results(status);
CREATE INDEX IF NOT EXISTS idx_shell_command_results_created_at ON shell_command_results(created_at);

-- =============================================================================
-- MOBILE DEVELOPMENT FEEDBACK (Version 30)
-- =============================================================================

-- Task Run Mobile State: Captures device/app state during mobile development
-- Used for AI feedback loop during qontinui-mobile development
CREATE TABLE IF NOT EXISTS task_run_mobile_state (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_run_id TEXT NOT NULL,
    timestamp TEXT NOT NULL,

    -- Device identification
    device_id TEXT,                     -- e.g., 'emulator-5554' or physical device serial
    device_type TEXT,                   -- 'emulator', 'physical'
    device_model TEXT,                  -- e.g., 'sdk_gphone64_x86_64', 'Pixel 7'

    -- App state
    app_package TEXT,                   -- e.g., 'io.qontinui.mobile'
    app_activity TEXT,                  -- Current activity/screen name
    app_state TEXT,                     -- 'foreground', 'background', 'not_running', 'crashed'

    -- Metro/Expo state
    metro_connected INTEGER DEFAULT 0,  -- 0 or 1
    bundle_status TEXT,                 -- 'bundling', 'ready', 'error'
    last_reload_type TEXT,              -- 'hot', 'full', NULL
    last_reload_time TEXT,

    -- Capture paths (relative to .dev-logs/mobile/)
    screenshot_path TEXT,               -- Path to screenshot PNG
    logcat_path TEXT,                   -- Path to logcat capture

    -- Error summary (if any errors detected)
    has_errors INTEGER DEFAULT 0,
    error_summary TEXT,                 -- Brief summary of detected errors

    created_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_mobile_state_task_run_id ON task_run_mobile_state(task_run_id);
CREATE INDEX IF NOT EXISTS idx_mobile_state_timestamp ON task_run_mobile_state(timestamp);
CREATE INDEX IF NOT EXISTS idx_mobile_state_device_id ON task_run_mobile_state(device_id);
CREATE INDEX IF NOT EXISTS idx_mobile_state_has_errors ON task_run_mobile_state(has_errors);

-- Task Run Mobile Logs: Stores parsed log entries from Metro, Logcat, etc.
-- Enables filtering and searching mobile-specific logs
CREATE TABLE IF NOT EXISTS task_run_mobile_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_run_id TEXT NOT NULL,
    mobile_state_id INTEGER,            -- Links to task_run_mobile_state if applicable

    -- Log classification
    log_source TEXT NOT NULL,           -- 'metro', 'logcat', 'expo', 'gradle', 'eas'
    log_level TEXT,                     -- 'error', 'warn', 'info', 'debug', 'verbose'
    log_tag TEXT,                       -- e.g., 'ReactNative', 'ReactNativeJS', 'Expo'

    -- Content
    message TEXT NOT NULL,
    raw_line TEXT,                      -- Original unparsed line
    data TEXT,                          -- JSON: additional structured data (stack trace, etc.)

    -- Error details (if log_level = 'error')
    error_type TEXT,                    -- 'js_error', 'native_crash', 'build_error', 'bundle_error'
    error_code TEXT,                    -- Error code if available
    stack_trace TEXT,                   -- Full stack trace if available
    file_path TEXT,                     -- Source file if available
    line_number INTEGER,                -- Line number if available
    column_number INTEGER,              -- Column number if available

    -- Timing
    timestamp TEXT NOT NULL,
    device_timestamp TEXT,              -- Timestamp from device (may differ from capture time)

    created_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (mobile_state_id) REFERENCES task_run_mobile_state(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_mobile_logs_task_run_id ON task_run_mobile_logs(task_run_id);
CREATE INDEX IF NOT EXISTS idx_mobile_logs_source ON task_run_mobile_logs(log_source);
CREATE INDEX IF NOT EXISTS idx_mobile_logs_level ON task_run_mobile_logs(log_level);
CREATE INDEX IF NOT EXISTS idx_mobile_logs_error_type ON task_run_mobile_logs(error_type);
CREATE INDEX IF NOT EXISTS idx_mobile_logs_timestamp ON task_run_mobile_logs(timestamp);
CREATE INDEX IF NOT EXISTS idx_mobile_logs_state_id ON task_run_mobile_logs(mobile_state_id);

-- =============================================================================
-- MCP (Model Context Protocol) Integration (Version 31)
-- =============================================================================
-- MCP server configuration and call execution history

-- MCP Server Configurations
-- Stores configuration for MCP servers that can be called from workflows
CREATE TABLE IF NOT EXISTS mcp_servers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,

    -- Transport configuration
    transport TEXT NOT NULL,            -- 'stdio', 'http'

    -- Stdio transport settings (JSON: {command, args, cwd, env})
    stdio_config TEXT,

    -- HTTP transport settings (JSON: {url, headers})
    http_config TEXT,

    -- Common settings
    enabled BOOLEAN NOT NULL DEFAULT 1,
    auto_start BOOLEAN NOT NULL DEFAULT 0,
    timeout_seconds INTEGER NOT NULL DEFAULT 30,

    -- Cached tool info (JSON array of tool definitions)
    cached_tools TEXT,
    tools_cached_at TEXT,

    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_mcp_servers_enabled ON mcp_servers(enabled);
CREATE INDEX IF NOT EXISTS idx_mcp_servers_name ON mcp_servers(name);

-- Task Run MCP Calls
-- Stores MCP tool call execution results (similar to task_run_api_requests)
CREATE TABLE IF NOT EXISTS task_run_mcp_calls (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,

    -- Step identification
    step_id TEXT NOT NULL,
    step_name TEXT,

    -- Server identification
    server_id TEXT NOT NULL,
    server_name TEXT,

    -- Tool call details
    tool_name TEXT NOT NULL,
    arguments TEXT,                     -- JSON: original arguments
    resolved_arguments TEXT,            -- JSON: arguments after variable substitution

    -- Response details
    response TEXT,                      -- JSON: tool response content
    response_type TEXT NOT NULL,        -- 'text', 'json', 'error'
    duration_ms INTEGER NOT NULL,

    -- Variable extractions (JSON array of extraction results)
    extractions TEXT,                   -- JSON array of {variable_name, json_path, extracted_value, success, error}

    -- Assertion results (JSON array of assertion results)
    assertions TEXT,                    -- JSON array of {assertion_type, expected, actual, passed, error}

    -- Overall result
    success BOOLEAN NOT NULL,
    error_message TEXT,

    created_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (server_id) REFERENCES mcp_servers(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_task_run_mcp_calls_task_run_id ON task_run_mcp_calls(task_run_id);
CREATE INDEX IF NOT EXISTS idx_task_run_mcp_calls_server_id ON task_run_mcp_calls(server_id);
CREATE INDEX IF NOT EXISTS idx_task_run_mcp_calls_step_id ON task_run_mcp_calls(step_id);
CREATE INDEX IF NOT EXISTS idx_task_run_mcp_calls_created_at ON task_run_mcp_calls(created_at);
CREATE INDEX IF NOT EXISTS idx_task_run_mcp_calls_success ON task_run_mcp_calls(success);

-- =============================================================================
-- Performance Optimization Indexes (Version 28)
-- =============================================================================
-- Additional indexes for frequently filtered/sorted columns

-- Learning outcomes: strategy filtering
CREATE INDEX IF NOT EXISTS idx_learning_outcomes_strategy ON learning_outcomes(strategy);

-- Learning patterns: updated_at ordering
CREATE INDEX IF NOT EXISTS idx_learning_patterns_updated_at ON learning_patterns(updated_at);

-- Orchestrator checkpoints: trigger and created_at filtering
CREATE INDEX IF NOT EXISTS idx_orchestrator_checkpoints_trigger ON orchestrator_checkpoints(trigger);
CREATE INDEX IF NOT EXISTS idx_orchestrator_checkpoints_created_at ON orchestrator_checkpoints(created_at);

-- Orchestrator flows: ordering support
CREATE INDEX IF NOT EXISTS idx_orchestrator_flows_created_at ON orchestrator_flows(created_at);
CREATE INDEX IF NOT EXISTS idx_orchestrator_flows_updated_at ON orchestrator_flows(updated_at);

-- Flow executions: started_at ordering
CREATE INDEX IF NOT EXISTS idx_flow_executions_started_at ON flow_executions(started_at);

-- Task knowledge: composite and iteration indexes
CREATE INDEX IF NOT EXISTS idx_task_knowledge_task_run_iteration ON task_knowledge(task_run_id, iteration);
CREATE INDEX IF NOT EXISTS idx_task_knowledge_iteration ON task_knowledge(iteration);

-- Orchestrator verification results: criterion queries
CREATE INDEX IF NOT EXISTS idx_orch_ver_results_criterion_id ON orchestrator_verification_results(criterion_id);

-- Task run events: event filtering
CREATE INDEX IF NOT EXISTS idx_task_run_events_subtype ON task_run_events(event_subtype);
CREATE INDEX IF NOT EXISTS idx_task_run_events_workflow ON task_run_events(workflow_name);

-- Checks: ordering support
CREATE INDEX IF NOT EXISTS idx_checks_created_at ON checks(created_at);
CREATE INDEX IF NOT EXISTS idx_checks_updated_at ON checks(updated_at);

-- Check results: created_at ordering
CREATE INDEX IF NOT EXISTS idx_check_results_created_at ON check_results(created_at);

-- Sessions: additional filtering
CREATE INDEX IF NOT EXISTS idx_sessions_completed ON sessions(completed);
CREATE INDEX IF NOT EXISTS idx_sessions_session_type ON sessions(session_type);

-- Scheduler history: status filtering
CREATE INDEX IF NOT EXISTS idx_scheduler_history_status ON scheduler_history(status);

-- Task runs: workflow_name filtering and updated_at ordering
CREATE INDEX IF NOT EXISTS idx_task_runs_workflow_name ON task_runs(workflow_name);
CREATE INDEX IF NOT EXISTS idx_task_runs_updated_at ON task_runs(updated_at);

-- Verification plans: created_at ordering
CREATE INDEX IF NOT EXISTS idx_verification_plans_created_at ON verification_plans(created_at);

-- =============================================================================
-- Context Management for Unified Workflows (Version 32)
-- =============================================================================
-- Adds context_ids, disabled_context_ids, and auto_include_contexts to unified_workflows

-- Add context_ids column (JSON array of context IDs explicitly added to the workflow)
-- Note: This is handled in migration code

-- Add disabled_context_ids column (JSON array of context IDs excluded from auto-include)
-- Note: This is handled in migration code

-- Add auto_include_contexts column (boolean, default true)
-- Note: This is handled in migration code

-- =============================================================================
-- UI Bridge Inspector (Version 35)
-- =============================================================================
-- Tables for storing UI Bridge element snapshots, states, transitions, and events
-- Used by the UI Bridge Inspector Panel for debugging and analysis

-- UI Bridge element snapshots (captured from pages)
CREATE TABLE IF NOT EXISTS ui_bridge_elements (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_run_id INTEGER REFERENCES task_runs(id) ON DELETE CASCADE,
    timestamp INTEGER NOT NULL,
    element_id TEXT NOT NULL,
    tag_name TEXT,
    element_type TEXT,  -- 'button', 'input', 'select', 'checkbox', 'link', 'form', 'custom'
    bounds TEXT,        -- JSON: {x, y, width, height, top, right, bottom, left}
    visible INTEGER DEFAULT 1,
    enabled INTEGER DEFAULT 1,
    focused INTEGER DEFAULT 0,
    value TEXT,
    text_content TEXT,
    label TEXT,
    parent_id TEXT,     -- Parent element's ui-id
    children TEXT,      -- JSON array of child ui-ids
    actions TEXT,       -- JSON array of available actions
    metadata TEXT       -- JSON for extensible data
);

CREATE INDEX IF NOT EXISTS idx_ui_bridge_elements_task_run ON ui_bridge_elements(task_run_id);
CREATE INDEX IF NOT EXISTS idx_ui_bridge_elements_element_id ON ui_bridge_elements(element_id);
CREATE INDEX IF NOT EXISTS idx_ui_bridge_elements_timestamp ON ui_bridge_elements(timestamp);

-- UI Bridge states (state machine definitions)
CREATE TABLE IF NOT EXISTS ui_bridge_states (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    state_id TEXT UNIQUE NOT NULL,
    name TEXT NOT NULL,
    elements TEXT,       -- JSON array of element IDs belonging to this state
    blocking INTEGER DEFAULT 0,
    blocks TEXT,         -- JSON array of blocked state IDs
    group_id TEXT,
    path_cost REAL DEFAULT 1.0,
    is_active INTEGER DEFAULT 0,
    active_when TEXT,    -- Optional JavaScript condition string
    metadata TEXT,       -- JSON for extensible data
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_ui_bridge_states_state_id ON ui_bridge_states(state_id);
CREATE INDEX IF NOT EXISTS idx_ui_bridge_states_group ON ui_bridge_states(group_id);
CREATE INDEX IF NOT EXISTS idx_ui_bridge_states_active ON ui_bridge_states(is_active);

-- UI Bridge state groups (atomic state collections)
CREATE TABLE IF NOT EXISTS ui_bridge_state_groups (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    group_id TEXT UNIQUE NOT NULL,
    name TEXT NOT NULL,
    states TEXT,         -- JSON array of state IDs in this group
    metadata TEXT,       -- JSON for extensible data
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_ui_bridge_state_groups_group_id ON ui_bridge_state_groups(group_id);

-- UI Bridge transitions (state transitions)
CREATE TABLE IF NOT EXISTS ui_bridge_transitions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    transition_id TEXT UNIQUE NOT NULL,
    name TEXT NOT NULL,
    from_states TEXT NOT NULL,     -- JSON array of precondition state IDs
    activate_states TEXT NOT NULL, -- JSON array of states to activate
    exit_states TEXT,              -- JSON array of states to deactivate
    activate_groups TEXT,          -- JSON array of groups to activate
    exit_groups TEXT,              -- JSON array of groups to deactivate
    actions TEXT,                  -- JSON array of workflow steps
    path_cost REAL DEFAULT 1.0,
    stays_visible INTEGER DEFAULT 0,
    metadata TEXT,                 -- JSON for extensible data
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_ui_bridge_transitions_transition_id ON ui_bridge_transitions(transition_id);

-- UI Bridge events (timeline of actions and state changes)
CREATE TABLE IF NOT EXISTS ui_bridge_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_run_id INTEGER REFERENCES task_runs(id) ON DELETE CASCADE,
    timestamp INTEGER NOT NULL,
    sequence INTEGER NOT NULL,     -- Order within the task run
    event_type TEXT NOT NULL,      -- 'element_registered', 'element_discovered', 'action_executed',
                                   -- 'state_changed', 'transition_executed', 'navigation_started',
                                   -- 'navigation_completed', 'path_found', 'error'
    element_id TEXT,
    state_id TEXT,
    transition_id TEXT,
    action TEXT,
    params TEXT,                   -- JSON action parameters
    result TEXT,                   -- JSON result data
    duration_ms REAL,
    success INTEGER DEFAULT 1,
    error_message TEXT,
    metadata TEXT                  -- JSON for extensible data
);

CREATE INDEX IF NOT EXISTS idx_ui_bridge_events_task_run ON ui_bridge_events(task_run_id);
CREATE INDEX IF NOT EXISTS idx_ui_bridge_events_type ON ui_bridge_events(event_type);
CREATE INDEX IF NOT EXISTS idx_ui_bridge_events_timestamp ON ui_bridge_events(timestamp);
CREATE INDEX IF NOT EXISTS idx_ui_bridge_events_element ON ui_bridge_events(element_id);
CREATE INDEX IF NOT EXISTS idx_ui_bridge_events_state ON ui_bridge_events(state_id);

-- UI Bridge navigation history (path execution records)
CREATE TABLE IF NOT EXISTS ui_bridge_navigation_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_run_id INTEGER REFERENCES task_runs(id) ON DELETE CASCADE,
    timestamp INTEGER NOT NULL,
    target_states TEXT NOT NULL,   -- JSON array of target state IDs
    path_found INTEGER NOT NULL,
    transitions_planned TEXT,      -- JSON array of transition IDs in path
    transitions_executed TEXT,     -- JSON array of actually executed transitions
    total_cost REAL,
    duration_ms REAL,
    success INTEGER DEFAULT 0,
    final_active_states TEXT,      -- JSON array of final active states
    error_message TEXT
);

CREATE INDEX IF NOT EXISTS idx_ui_bridge_nav_history_task_run ON ui_bridge_navigation_history(task_run_id);
CREATE INDEX IF NOT EXISTS idx_ui_bridge_nav_history_timestamp ON ui_bridge_navigation_history(timestamp);

-- =============================================================================
-- Error Monitoring System (Version 44)
-- =============================================================================
-- Application log monitoring for error collection and debug agent integration
-- Captures errors from user-configured log sources (applications being automated)

-- Log Sources: User-configured application log files to monitor
CREATE TABLE IF NOT EXISTS log_sources (
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    -- Identity
    name TEXT NOT NULL UNIQUE,              -- Display name (e.g., "api-server", "web-frontend")
    description TEXT,

    -- Location
    path TEXT NOT NULL,                     -- File path, glob pattern, or directory
    path_type TEXT DEFAULT 'file',          -- 'file', 'glob', 'directory'

    -- Parsing configuration
    format TEXT DEFAULT 'plaintext',        -- 'plaintext', 'json', 'jsonl'
    parser TEXT DEFAULT 'generic',          -- 'python', 'javascript', 'rust', 'generic'
    timestamp_pattern TEXT,                 -- Regex to extract timestamp from log lines
    timezone TEXT DEFAULT 'local',          -- Timezone for parsing timestamps

    -- Custom patterns (JSON arrays of regex patterns)
    error_patterns TEXT,                    -- Patterns to identify errors
    warning_patterns TEXT,                  -- Patterns to identify warnings
    ignore_patterns TEXT,                   -- Patterns to ignore

    -- Monitoring settings
    enabled INTEGER DEFAULT 1,              -- 0 = disabled, 1 = enabled
    poll_interval_ms INTEGER DEFAULT 5000,  -- How often to check for new content

    -- Timestamps
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_log_sources_name ON log_sources(name);
CREATE INDEX IF NOT EXISTS idx_log_sources_enabled ON log_sources(enabled);

-- Error Events: Captured errors from application logs
-- Persistent store that survives across workflow runs for pattern detection
CREATE TABLE IF NOT EXISTS error_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    -- Source identification
    log_source_id INTEGER REFERENCES log_sources(id) ON DELETE SET NULL,
    log_source_name TEXT NOT NULL,          -- Denormalized for quick access

    -- Workflow context (optional - only set during workflow runs)
    task_run_id TEXT REFERENCES task_runs(id) ON DELETE SET NULL,
    workflow_step_id TEXT,                  -- Which workflow step was executing

    -- Timing
    log_timestamp TEXT,                     -- Timestamp from the log entry itself
    captured_at TEXT NOT NULL DEFAULT (datetime('now')),

    -- Error classification
    severity TEXT NOT NULL DEFAULT 'error', -- 'critical', 'error', 'warning'
    error_type TEXT,                        -- 'TypeError', 'ConnectionError', etc.
    error_code TEXT,                        -- HTTP status, exit code, etc.

    -- Error content
    message TEXT NOT NULL,                  -- The error message
    stack_trace TEXT,                       -- Full stack trace if available
    context_lines TEXT,                     -- Surrounding log lines for context
    raw_entry TEXT,                         -- Original unparsed log content

    -- Location (if parseable from stack trace)
    file_path TEXT,                         -- Source file where error occurred
    line_number INTEGER,
    column_number INTEGER,
    function_name TEXT,

    -- Deduplication and tracking
    signature_hash TEXT NOT NULL,           -- Hash for deduplication (error_type + message + location)
    occurrence_count INTEGER DEFAULT 1,     -- Incremented on duplicates
    first_seen_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_seen_at TEXT NOT NULL DEFAULT (datetime('now')),

    -- Status lifecycle
    status TEXT DEFAULT 'new',              -- 'new', 'acknowledged', 'in_progress', 'promoted', 'ignored', 'resolved', 'wont_fix'

    -- Debug agent integration
    finding_id INTEGER REFERENCES task_run_findings(id) ON DELETE SET NULL,  -- Linked finding (if promoted)
    resolved_by_task_run_id TEXT,           -- Which workflow fixed this error
    resolution_notes TEXT,

    -- Status timestamps
    acknowledged_at TEXT,
    resolved_at TEXT
);

-- Indexes for efficient queries
CREATE INDEX IF NOT EXISTS idx_error_events_log_source ON error_events(log_source_id);
CREATE INDEX IF NOT EXISTS idx_error_events_task_run ON error_events(task_run_id);
CREATE INDEX IF NOT EXISTS idx_error_events_signature ON error_events(signature_hash);
CREATE INDEX IF NOT EXISTS idx_error_events_status ON error_events(status);
CREATE INDEX IF NOT EXISTS idx_error_events_severity ON error_events(severity);
CREATE INDEX IF NOT EXISTS idx_error_events_captured ON error_events(captured_at DESC);
CREATE INDEX IF NOT EXISTS idx_error_events_last_seen ON error_events(last_seen_at DESC);
CREATE INDEX IF NOT EXISTS idx_error_events_source_name ON error_events(log_source_name);

-- Full-text search for error messages and stack traces
CREATE VIRTUAL TABLE IF NOT EXISTS error_events_fts USING fts5(
    message,
    stack_trace,
    error_type,
    content='error_events',
    content_rowid='id'
);

-- Triggers to keep FTS index in sync
CREATE TRIGGER IF NOT EXISTS error_events_ai AFTER INSERT ON error_events BEGIN
    INSERT INTO error_events_fts(rowid, message, stack_trace, error_type)
    VALUES (new.id, new.message, new.stack_trace, new.error_type);
END;

CREATE TRIGGER IF NOT EXISTS error_events_ad AFTER DELETE ON error_events BEGIN
    INSERT INTO error_events_fts(error_events_fts, rowid, message, stack_trace, error_type)
    VALUES ('delete', old.id, old.message, old.stack_trace, old.error_type);
END;

CREATE TRIGGER IF NOT EXISTS error_events_au AFTER UPDATE ON error_events BEGIN
    INSERT INTO error_events_fts(error_events_fts, rowid, message, stack_trace, error_type)
    VALUES ('delete', old.id, old.message, old.stack_trace, old.error_type);
    INSERT INTO error_events_fts(rowid, message, stack_trace, error_type)
    VALUES (new.id, new.message, new.stack_trace, new.error_type);
END;

-- =============================================================================
-- Recording & Playback System (Version 46)
-- =============================================================================
-- Stores browser interaction recordings and enables script generation
-- Records user actions during manual navigation for automation replay

-- Recordings: Session containers for recorded actions
CREATE TABLE IF NOT EXISTS recordings (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    base_url TEXT NOT NULL,              -- Starting URL for the recording
    action_count INTEGER DEFAULT 0,

    -- Status: 'recording', 'completed', 'failed', 'cancelled'
    status TEXT DEFAULT 'recording',

    -- Timing
    started_at TEXT NOT NULL,
    completed_at TEXT,
    duration_ms INTEGER,

    -- Metadata
    browser_info TEXT,                   -- JSON: {browser, version, userAgent}
    tab_id INTEGER,                      -- Chrome tab ID during recording

    -- Tags for organization
    tags TEXT DEFAULT '[]',              -- JSON array of strings

    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_recordings_status ON recordings(status);
CREATE INDEX IF NOT EXISTS idx_recordings_created_at ON recordings(created_at);
CREATE INDEX IF NOT EXISTS idx_recordings_base_url ON recordings(base_url);

-- Recording Actions: Individual captured user interactions
CREATE TABLE IF NOT EXISTS recording_actions (
    id TEXT PRIMARY KEY,
    recording_id TEXT NOT NULL,
    sequence_number INTEGER NOT NULL,    -- Order of action in recording

    -- Action type: 'click', 'type', 'navigate', 'select', 'scroll', 'hover', 'keypress'
    action_type TEXT NOT NULL,

    -- Page context
    url TEXT NOT NULL,                   -- URL at time of action
    page_title TEXT,

    -- Target element information (JSON)
    target_json TEXT NOT NULL,           -- {uiId, tagName, selector, xpath, textContent, bbox, attributes}

    -- Action-specific data (JSON)
    action_data_json TEXT,               -- Click: {x, y, button, clickCount}
                                         -- Type: {value, inputType}
                                         -- Navigate: {fromUrl, toUrl, navigationType}
                                         -- Select: {value, selectedText, selectedIndex}
                                         -- Scroll: {deltaX, deltaY, scrollLeft, scrollTop}

    -- Screenshot reference (optional)
    screenshot_path TEXT,                -- Path to screenshot taken at this action

    -- Timing
    timestamp TEXT NOT NULL,
    duration_ms INTEGER,                 -- Time until next action (for playback timing)

    created_at TEXT NOT NULL DEFAULT (datetime('now')),

    FOREIGN KEY (recording_id) REFERENCES recordings(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_recording_actions_recording_id ON recording_actions(recording_id);
CREATE INDEX IF NOT EXISTS idx_recording_actions_sequence ON recording_actions(recording_id, sequence_number);
CREATE INDEX IF NOT EXISTS idx_recording_actions_action_type ON recording_actions(action_type);

-- Recording Exports: Track generated scripts from recordings
CREATE TABLE IF NOT EXISTS recording_exports (
    id TEXT PRIMARY KEY,
    recording_id TEXT NOT NULL,

    -- Export format: 'python', 'playwright', 'pytest', 'cypress'
    export_format TEXT NOT NULL,

    -- Generated content
    script_content TEXT NOT NULL,
    file_name TEXT NOT NULL,

    -- Export options used
    options_json TEXT,                   -- JSON: {wait_strategy, selector_priority, etc.}

    created_at TEXT NOT NULL DEFAULT (datetime('now')),

    FOREIGN KEY (recording_id) REFERENCES recordings(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_recording_exports_recording_id ON recording_exports(recording_id);
CREATE INDEX IF NOT EXISTS idx_recording_exports_format ON recording_exports(export_format);

-- =============================================================================
-- Workflow State Management (Version 48)
-- =============================================================================
-- Explicit workflow state tracking for resume capability
-- Used by Unified Workflow, Orchestrator, and GUI Automation workflows

-- Workflow Execution State: Explicit state tracking for workflows
-- This replaces implicit state inference from task_runs status fields
CREATE TABLE IF NOT EXISTS workflow_execution_state (
    execution_id TEXT PRIMARY KEY,          -- Same as task_run_id
    workflow_type TEXT NOT NULL,            -- 'unified', 'orchestrator', 'gui_automation'
    state_name TEXT NOT NULL,               -- Current state name (e.g., 'SetupRunning', 'VerificationRunning')
    state_data TEXT,                        -- JSON serialized state variant data
    phase TEXT,                             -- Current phase if applicable
    iteration INTEGER,                      -- Current iteration if applicable
    updated_at TEXT NOT NULL,
    FOREIGN KEY (execution_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_workflow_exec_state_type ON workflow_execution_state(workflow_type);
CREATE INDEX IF NOT EXISTS idx_workflow_exec_state_name ON workflow_execution_state(state_name);

-- Workflow Step Checkpoints: Step-level checkpointing for resume
-- When a workflow is interrupted mid-execution, this allows resuming from exact step
CREATE TABLE IF NOT EXISTS workflow_step_checkpoints (
    id TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL,             -- Same as task_run_id
    workflow_type TEXT NOT NULL,            -- 'unified', 'orchestrator', 'gui_automation'
    phase TEXT NOT NULL,                    -- 'setup', 'verification', 'agentic', 'completion'
    iteration INTEGER,                      -- Iteration number (for phases that repeat)
    step_index INTEGER NOT NULL,            -- Step index within the phase
    step_type TEXT NOT NULL,                -- 'playwright', 'automation', 'ai', 'python_script'
    step_name TEXT,                         -- Display name
    status TEXT NOT NULL,                   -- 'pending', 'running', 'success', 'failed', 'skipped'
    result_json TEXT,                       -- JSON serialized result (if completed)
    step_config_json TEXT,                  -- JSON serialized step configuration (single source of truth)
    started_at TEXT,
    completed_at TEXT,
    duration_ms INTEGER,
    error TEXT,                             -- Error message if failed
    FOREIGN KEY (execution_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    UNIQUE(execution_id, phase, iteration, step_index)
);

CREATE INDEX IF NOT EXISTS idx_step_checkpoints_execution ON workflow_step_checkpoints(execution_id);
CREATE INDEX IF NOT EXISTS idx_step_checkpoints_lookup ON workflow_step_checkpoints(execution_id, phase, iteration);
CREATE INDEX IF NOT EXISTS idx_step_checkpoints_status ON workflow_step_checkpoints(status);
CREATE INDEX IF NOT EXISTS idx_step_checkpoints_cursor ON workflow_step_checkpoints(execution_id, step_index);

-- Step Progress Markers: Intra-step progress tracking for long AI operations
-- Allows tracking progress within a step (e.g., "analyzed 50/100 files")
CREATE TABLE IF NOT EXISTS step_progress_markers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    checkpoint_id TEXT NOT NULL,              -- Reference to workflow_step_checkpoints.id
    marker_type TEXT NOT NULL,                -- 'file_progress', 'analysis_progress', 'test_progress', etc.
    current_value INTEGER NOT NULL,           -- Current progress value
    total_value INTEGER,                      -- Total value (if known)
    description TEXT,                         -- Human-readable description
    data_json TEXT,                           -- Additional structured data (JSON)
    created_at TEXT NOT NULL,
    FOREIGN KEY (checkpoint_id) REFERENCES workflow_step_checkpoints(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_progress_markers_checkpoint ON step_progress_markers(checkpoint_id);

-- =============================================================================
-- Execution Spans (Version 52)
-- =============================================================================
-- Stores summary-level tracing spans for AI analysis and performance debugging.
-- Only "summary" spans are persisted (workflow phases, AI sessions, etc.)
-- Full span data is in runner-spans.jsonl for real-time debugging.

CREATE TABLE IF NOT EXISTS execution_spans (
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    -- Task context (links to task_runs)
    execution_id TEXT,                      -- Same as task_run_id (may be empty for global spans)

    -- Span identity
    trace_id TEXT NOT NULL,                 -- Shared across related spans in a trace
    span_id TEXT NOT NULL,                  -- Unique span identifier
    parent_span_id TEXT,                    -- Parent span (NULL for root spans)
    name TEXT NOT NULL,                     -- Span name (e.g., 'workflow.execute', 'ai.session')

    -- Timing
    start_ts TEXT NOT NULL,                 -- ISO 8601 start timestamp
    end_ts TEXT,                            -- ISO 8601 end timestamp
    duration_ms INTEGER,                    -- Duration in milliseconds

    -- Data
    attributes TEXT,                        -- JSON object of span attributes
    success INTEGER DEFAULT 1,              -- 1 = success, 0 = had error
    error TEXT,                             -- Error message if failed

    created_at TEXT NOT NULL,

    FOREIGN KEY (execution_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

-- Indexes for efficient queries
CREATE INDEX IF NOT EXISTS idx_spans_execution ON execution_spans(execution_id);
CREATE INDEX IF NOT EXISTS idx_spans_trace ON execution_spans(trace_id);
CREATE INDEX IF NOT EXISTS idx_spans_name ON execution_spans(name);
CREATE INDEX IF NOT EXISTS idx_spans_start ON execution_spans(start_ts);
CREATE INDEX IF NOT EXISTS idx_spans_duration ON execution_spans(duration_ms);

-- Initialize singleton tables
INSERT OR IGNORE INTO gui_lock (id, holder_session_id, acquired_at) VALUES (1, NULL, NULL);
INSERT OR IGNORE INTO scheduler_settings (id) VALUES (1);
INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (52, datetime('now'));
