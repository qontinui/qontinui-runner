-- SQLite Schema for qontinui-runner
-- Version: 8
--
-- This schema provides persistent storage for task runs, settings,
-- prompts, and scheduler state.
--
-- Key concept: Every task runs until completion (marked by [TASK_COMPLETE]).
-- Sessions are internal implementation details - users only see task runs.

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

-- Task Runs (simplified task execution model)
-- Every task runs until [TASK_COMPLETE] marker is found in output.
-- Sessions are internal - output is accumulated in output_log with session markers.
CREATE TABLE IF NOT EXISTS task_runs (
    id TEXT PRIMARY KEY,
    task_name TEXT NOT NULL,
    prompt TEXT NOT NULL,  -- The task description/instructions
    status TEXT NOT NULL DEFAULT 'running',  -- 'running', 'complete', 'failed', 'stopped'
    sessions_count INTEGER NOT NULL DEFAULT 0,  -- How many Claude sessions spawned
    max_sessions INTEGER,  -- NULL = unlimited, otherwise max before giving up
    output_log TEXT DEFAULT '',  -- Accumulated output with [SESSION_START:N] markers
    error_message TEXT,
    auto_continue BOOLEAN NOT NULL DEFAULT 1,  -- Per-run auto-continue setting (1=true, 0=false)
    execution_steps_json TEXT,  -- JSON array of ExecutionStepConfig for re-execution on resume
    log_sources_json TEXT,  -- JSON array of LogSourceConfig for log capture during execution
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    completed_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_task_runs_status ON task_runs(status);
CREATE INDEX IF NOT EXISTS idx_task_runs_created_at ON task_runs(created_at);

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

-- Run Details (Tier 1 - Detailed run data)
-- Stores comprehensive information about each automation run for debugging and analysis
CREATE TABLE IF NOT EXISTS run_details (
    id TEXT PRIMARY KEY,
    config_id TEXT NOT NULL,
    workflow_name TEXT,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    duration_ms INTEGER,
    status TEXT NOT NULL,  -- 'running', 'completed', 'failed', 'timeout', 'cancelled'
    success BOOLEAN,
    error_type TEXT,
    error_message TEXT,
    actions_summary TEXT,  -- JSON {"total": N, "success": N, "failed": N, "skipped": N}
    states_visited TEXT,   -- JSON array of state names
    transitions_executed TEXT,  -- JSON array of {from, to, action, success, duration_ms}
    template_matches TEXT, -- JSON array of {template, count, avg_confidence, failures}
    anomalies TEXT,        -- JSON array for Tier 2 triggering
    FOREIGN KEY (config_id) REFERENCES configs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_run_details_config_id ON run_details(config_id);
CREATE INDEX IF NOT EXISTS idx_run_details_started_at ON run_details(started_at);
CREATE INDEX IF NOT EXISTS idx_run_details_status ON run_details(status);

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

-- Initialize singleton tables
INSERT OR IGNORE INTO gui_lock (id, holder_session_id, acquired_at) VALUES (1, NULL, NULL);
INSERT OR IGNORE INTO scheduler_settings (id) VALUES (1);
INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (8, datetime('now'));
