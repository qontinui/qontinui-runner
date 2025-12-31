-- SQLite Schema for qontinui-runner
-- Version: 4
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

-- Initialize singleton tables
INSERT OR IGNORE INTO gui_lock (id, holder_session_id, acquired_at) VALUES (1, NULL, NULL);
INSERT OR IGNORE INTO scheduler_settings (id) VALUES (1);
INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (4, datetime('now'));
