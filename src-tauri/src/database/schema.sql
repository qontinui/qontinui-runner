-- SQLite Schema for qontinui-runner
-- Version: 99
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
-- Version 23 adds task_knowledge_summaries (compression).
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
-- Version 55 adds embedding BLOB columns for hybrid RAG search and workflow_generation_feedback table.
-- Version 58 adds reflection workflow system (reflection_fixes table, is_reflection/reflection_source_task_run_id on task_runs, reflection_fix_id on findings/knowledge).
-- Version 59 adds content_hash to reflection_fixes for deduplication, archives stale fixes, adds verification best practice knowledge entry.

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
    workflow_id TEXT,    -- FK to unified_workflows (links run to workflow definition)

    -- Summary (post-completion analysis)
    summary TEXT,  -- AI-generated paragraph summary of the task run (canonical)
    ai_summary TEXT,  -- Deprecated: kept for backward compatibility with COALESCE queries
    goal_achieved BOOLEAN,  -- Whether the stated goal was achieved
    remaining_work TEXT,  -- What remains to be done if goal was not achieved
    summary_generated_at TEXT,  -- Timestamp when the summary was generated

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

    -- Structured result data (JSON blob for meta-workflow outputs, etc.)
    result_data TEXT,  -- JSON: e.g. {"generated_workflow_id": "..."}

    -- Web integration
    workspace_id TEXT,  -- Links task to a workspace/organization from qontinui-web
    triggered_by TEXT,  -- Identifies who/what triggered the task run

    -- Embedding vectors for hybrid RAG search (384-dim MiniLM as f32 BLOB, 1536 bytes each)
    prompt_embedding BLOB,   -- Embedding of the task prompt/description
    summary_embedding BLOB,  -- Embedding of the AI-generated summary

    -- Reflection (v58)
    is_reflection INTEGER DEFAULT 0,            -- Whether this is a reflection analysis run
    reflection_source_task_run_id TEXT,          -- Source task run being analyzed by this reflection

    -- Follow-up (v71)
    is_follow_up INTEGER DEFAULT 0,             -- Whether this is a follow-up run for unfixed issues
    follow_up_source_task_run_id TEXT,           -- Source task run whose unfixed issues this run addresses

    -- Runner port (v113)
    runner_port INTEGER,                         -- Port of the runner instance that owns this task

    -- Fixer (v114)
    is_fixer INTEGER DEFAULT 0,                 -- Whether this is a fixer run (aggregates reflection/follow-up fixes)
    fixer_source_task_run_id TEXT,               -- Source task run that this fixer addresses

    -- Meta-Optimizer (v119)
    is_meta_optimizer INTEGER DEFAULT 0,        -- Whether this is a meta-optimizer run

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
-- Runner port index for filtering by runner instance
CREATE INDEX IF NOT EXISTS idx_task_runs_runner_port ON task_runs(runner_port);

-- Per-phase token usage tracking for cost analysis.
-- Records input/output tokens and estimated cost for each AI call in a workflow.
CREATE TABLE IF NOT EXISTS phase_token_usage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_run_id TEXT NOT NULL,
    phase TEXT NOT NULL,           -- setup, verification, agentic, completion, investigation, summary, generation
    stage_index INTEGER,           -- NULL for single-stage workflows
    iteration INTEGER,             -- iteration number within the loop
    model_used TEXT,               -- e.g. "claude-sonnet-4-20250514"
    provider_used TEXT,            -- e.g. "claude_api"
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cost_cents INTEGER NOT NULL DEFAULT 0,  -- estimated cost in hundredths of cents (microdollars * 100)
    duration_ms INTEGER,           -- AI call wall-clock time
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_phase_token_usage_task_run ON phase_token_usage(task_run_id);

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

    -- Embedding vectors for hybrid RAG search (384-dim MiniLM as f32 BLOB)
    title_embedding BLOB,        -- Embedding of the finding title
    description_embedding BLOB,  -- Embedding of the finding description

    -- Reflection linkage (v58)
    reflection_fix_id TEXT,      -- FK to reflection_fixes if this finding was addressed by a reflection

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

    -- Embedding vector for hybrid RAG search (384-dim MiniLM as f32 BLOB)
    content_embedding BLOB,  -- Embedding of the knowledge content

    -- Reflection linkage (v58)
    reflection_fix_id TEXT,  -- FK to reflection_fixes if this knowledge was created by a reflection

    -- Project scoping (v97)
    project_path TEXT,  -- Project/workspace path for project-scoped knowledge

    -- Relevance tracking (v99)
    last_validated_at TEXT,              -- When this knowledge was last confirmed still-relevant
    validation_count INTEGER DEFAULT 0,  -- How many times this knowledge has been validated

    created_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_task_knowledge_task_run_id ON task_knowledge(task_run_id);
CREATE INDEX IF NOT EXISTS idx_task_knowledge_category ON task_knowledge(category);
CREATE INDEX IF NOT EXISTS idx_task_knowledge_is_resolved ON task_knowledge(is_resolved);
CREATE INDEX IF NOT EXISTS idx_task_knowledge_project ON task_knowledge(project_path);

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
    completion_steps TEXT DEFAULT '[]',     -- JSON array of CompletionStep

    -- Agentic configuration
    max_iterations INTEGER DEFAULT 10,
    provider TEXT,  -- 'claude_cli', 'gemini_api', etc.
    model TEXT,     -- Model identifier
    skip_ai_summary BOOLEAN NOT NULL DEFAULT 0,  -- Skip AI summary generation
    timeout_seconds INTEGER DEFAULT NULL,  -- Optional timeout for AI sessions
    prompt_template TEXT DEFAULT NULL,  -- Custom prompt template

    -- Context configuration
    context_ids TEXT DEFAULT '[]',  -- JSON array of context IDs to include
    disabled_context_ids TEXT DEFAULT '[]',  -- JSON array of disabled context IDs
    auto_include_contexts INTEGER DEFAULT 1,  -- Auto-include relevant contexts

    -- Log configuration
    log_watch_enabled INTEGER DEFAULT 1,  -- 1 = enabled (default), 0 = disabled
    log_source_selection TEXT DEFAULT '"default"',  -- Log source selection config

    -- Health check configuration
    health_check_enabled INTEGER DEFAULT 1,  -- 1 = enabled (default), 0 = disabled
    health_check_urls TEXT DEFAULT '[]',  -- JSON array of { name, url, expected_status, timeout_seconds, is_critical }

    -- Pre-flight check configuration
    preflight_check_enabled INTEGER DEFAULT 1,  -- 1 = enabled (default), 0 = disabled

    -- Completion sweep configuration
    enable_sweep INTEGER DEFAULT 0,  -- 0 = disabled (default), 1 = enabled
    max_sweep_iterations INTEGER DEFAULT 5,  -- Maximum sweep iterations

    -- Multi-stage workflow configuration
    stages TEXT DEFAULT '[]',  -- JSON array of WorkflowStage objects
    stop_on_failure INTEGER DEFAULT 0,  -- 0 = continue on failure (default), 1 = stop
    constraint_overrides TEXT DEFAULT '{}',  -- JSON map of constraint_id → enabled (true/false)
    approval_gate INTEGER DEFAULT 0,  -- 0 = disabled (default), 1 = pause for human approval
    reflection_mode INTEGER DEFAULT 1,  -- 0 = disabled, 1 = enabled (default)
    completion_prompts_first INTEGER NOT NULL DEFAULT 0,  -- 0 = automation first (default), 1 = prompts first
    model_overrides TEXT DEFAULT '{}',  -- JSON map of phase → {provider, model} overrides

    -- Generation tracking (for workflows created by meta-workflows)
    generated_by_task_run_id TEXT,  -- Links back to the meta-workflow task_run that created this

    -- Embedding vector for hybrid RAG search (384-dim MiniLM as f32 BLOB)
    description_embedding BLOB,  -- Embedding of the workflow description

    -- Example library status for RAG-based generation
    -- 'pending' = not yet in library (auto-added on first successful AI-generated run)
    -- 'active' = in the example library, available for RAG retrieval
    -- 'excluded' = user opted out, never auto-added
    example_status TEXT DEFAULT 'pending',

    -- Sync status
    sync_pending INTEGER DEFAULT 0,  -- Whether workflow needs to be synced to backend

    -- Favorites
    is_favorite INTEGER DEFAULT 0,

    -- Quality improvements (generation metadata)
    dependency_graph TEXT DEFAULT NULL,
    cost_annotations TEXT DEFAULT NULL,
    quality_report TEXT DEFAULT NULL,
    acceptance_criteria TEXT DEFAULT NULL,
    ai_reviewed INTEGER DEFAULT 1,

    -- Slash command import tracking
    source_file_path TEXT DEFAULT NULL,    -- Relative path to source .md file (e.g. "qontinui-claude-config/.claude/commands/fix.md")
    source_content_hash TEXT DEFAULT NULL, -- SHA-256 hex of file content for change detection

    -- Timestamps
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,

    FOREIGN KEY (generated_by_task_run_id) REFERENCES task_runs(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_unified_workflows_category ON unified_workflows(category);
CREATE INDEX IF NOT EXISTS idx_unified_workflows_updated_at ON unified_workflows(updated_at);
CREATE INDEX IF NOT EXISTS idx_unified_workflows_name ON unified_workflows(name);
CREATE INDEX IF NOT EXISTS idx_unified_workflows_example_status ON unified_workflows(example_status);
CREATE INDEX IF NOT EXISTS idx_unified_workflows_sync_pending ON unified_workflows(sync_pending);
CREATE INDEX IF NOT EXISTS idx_unified_workflows_is_favorite ON unified_workflows(is_favorite);
CREATE INDEX IF NOT EXISTS idx_unified_workflows_source_file_path ON unified_workflows(source_file_path);

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
    workflow_architecture TEXT,  -- 'traditional', 'agentic_verification', 'multi_agent_pipeline'
    context_embedding BLOB,  -- Embedding of task context (384-dim MiniLM as f32 BLOB)
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
    description_embedding BLOB,  -- Embedding of pattern description (384-dim MiniLM as f32 BLOB)
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

-- Task runs: workflow_name filtering, workflow_id lookup, and updated_at ordering
CREATE INDEX IF NOT EXISTS idx_task_runs_workflow_name ON task_runs(workflow_name);
CREATE INDEX IF NOT EXISTS idx_task_runs_workflow_id ON task_runs(workflow_id);
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
    resolved_by_fix_id TEXT,               -- FK to reflection_fixes — links error resolution to specific fix (v99)
    resolution_notes TEXT,

    -- Embedding vector for hybrid RAG search (384-dim MiniLM as f32 BLOB)
    message_embedding BLOB,  -- Embedding of the error message

    -- Cross-service trace correlation
    trace_id TEXT,

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
CREATE INDEX IF NOT EXISTS idx_error_events_trace_id ON error_events(trace_id);

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
-- Workflow Generation Feedback (Version 55)
-- =============================================================================
-- Captures structured feedback when users edit, delete, or rate AI-generated workflows.
-- Replaces info! log-only feedback with queryable DB records for self-improvement.

CREATE TABLE IF NOT EXISTS workflow_generation_feedback (
    id TEXT PRIMARY KEY,
    workflow_id TEXT NOT NULL,              -- The workflow that received feedback
    task_run_id TEXT,                       -- The meta-workflow task run that generated it

    -- Feedback type: 'edit', 'delete', 'rating', 'fork'
    feedback_type TEXT NOT NULL,

    -- Edit details (when feedback_type = 'edit')
    edited_field TEXT,                      -- Which field was changed (e.g., 'setup_steps', 'description')
    old_value TEXT,                         -- Previous value (JSON for complex fields)
    new_value TEXT,                         -- New value (JSON for complex fields)

    -- Delete details (when feedback_type = 'delete')
    delete_reason TEXT,                     -- Optional reason for deletion

    -- Rating details (when feedback_type = 'rating')
    rating INTEGER,                         -- 1-5 star rating
    rating_comment TEXT,                    -- Optional comment with the rating

    -- Context
    workflow_category TEXT,                 -- Category of the workflow at time of feedback
    workflow_description TEXT,              -- Description of the workflow at time of feedback

    created_at TEXT NOT NULL DEFAULT (datetime('now')),

    FOREIGN KEY (workflow_id) REFERENCES unified_workflows(id) ON DELETE CASCADE,
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_wgf_workflow_id ON workflow_generation_feedback(workflow_id);
CREATE INDEX IF NOT EXISTS idx_wgf_task_run_id ON workflow_generation_feedback(task_run_id);
CREATE INDEX IF NOT EXISTS idx_wgf_feedback_type ON workflow_generation_feedback(feedback_type);
CREATE INDEX IF NOT EXISTS idx_wgf_created_at ON workflow_generation_feedback(created_at);

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
    stage_index INTEGER DEFAULT 0,         -- Stage index for multi-stage workflows (0 = single-stage)
    FOREIGN KEY (execution_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    UNIQUE(execution_id, phase, iteration, step_index, stage_index)
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

-- Workflow Verification Phase Results (Version 36/37)
-- Stores results from execute_verification_steps in unified workflow execution
CREATE TABLE IF NOT EXISTS workflow_verification_phase_results (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    iteration INTEGER NOT NULL,

    -- Summary fields
    all_passed BOOLEAN NOT NULL,
    total_steps INTEGER NOT NULL,
    passed_steps INTEGER NOT NULL,
    failed_steps INTEGER NOT NULL,
    skipped_steps INTEGER NOT NULL,
    total_duration_ms INTEGER NOT NULL,
    critical_failure BOOLEAN NOT NULL DEFAULT 0,

    -- Full result as JSON (for detailed access)
    result_json TEXT NOT NULL,

    created_at TEXT NOT NULL,

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_wf_ver_phase_task_run_id ON workflow_verification_phase_results(task_run_id);
CREATE INDEX IF NOT EXISTS idx_wf_ver_phase_iteration ON workflow_verification_phase_results(iteration);
CREATE INDEX IF NOT EXISTS idx_wf_ver_phase_all_passed ON workflow_verification_phase_results(all_passed);
CREATE UNIQUE INDEX IF NOT EXISTS idx_wf_ver_phase_unique ON workflow_verification_phase_results(task_run_id, iteration);

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

-- Reflection Fixes (v58)
-- Tracks fixes applied by reflection workflows and their effectiveness
CREATE TABLE IF NOT EXISTS reflection_fixes (
    id TEXT PRIMARY KEY,
    source_task_run_id TEXT NOT NULL,         -- The run being analyzed
    reflection_task_run_id TEXT NOT NULL,      -- The reflection run that created this fix
    source_finding_id TEXT,                    -- FK to task_run_findings (if fix addresses a specific finding)
    source_knowledge_id TEXT,                  -- FK to task_knowledge (if fix addresses a specific knowledge entry)
    fix_type TEXT NOT NULL,                    -- knowledge_base_update, workflow_step_rewrite, selector_fix, tool_config_update, context_addition, instruction_clarification
    fix_description TEXT NOT NULL,
    file_changed TEXT,
    old_value TEXT,
    new_value TEXT,
    confidence TEXT NOT NULL DEFAULT 'medium', -- high, medium, low
    content_hash TEXT,                         -- Hash for deduplication (v59)
    status TEXT NOT NULL DEFAULT 'applied',    -- applied, reverted, superseded
    effectiveness TEXT,                        -- NULL -> effective, ineffective, caused_regression, inconclusive
    effectiveness_evidence TEXT,
    applied_at TEXT NOT NULL,
    evaluated_at TEXT,
    created_at TEXT NOT NULL,
    source_agent TEXT,                        -- Which generation agent caused this (specification, builder, verification, hardener)
    reasoning TEXT,                           -- Root cause diagnosis and reasoning behind the fix (v104)
    alternatives_considered TEXT,             -- Other approaches considered and why they were rejected (v104)
    reflection_scope TEXT DEFAULT 'workflow',  -- 'workflow' (existing) or 'project' (project reflection) (v97)
    project_path TEXT,                        -- Project/workspace path for project-scoped fixes (v97)
    target_component TEXT,                    -- File path or module the fix targets (v99)
    reuse_count INTEGER DEFAULT 0,            -- How many times this fix has been successfully reused (v99)
    applicability_context TEXT,               -- When this universal pattern applies (v105)
    fix_description_embedding BLOB,           -- 384-dim MiniLM embedding for semantic retrieval (v105)
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
-- Generation Rules (Version 60)
-- =============================================================================
-- Externalized workflow generation rules. Rules that were previously hardcoded
-- as Rust string literals across schema_context.rs, hardener.rs, and generator.rs
-- are now stored here, enabling runtime modification by the reflection system.

CREATE TABLE IF NOT EXISTS generation_rules (
    id TEXT PRIMARY KEY,
    agent TEXT NOT NULL,          -- 'schema_context', 'hardener', 'verification'
    section TEXT NOT NULL,        -- 'important_rules', 'verification_quality', 'conversion_rules', 'critical_rules', 'check_rules'
    rule_number INTEGER NOT NULL, -- Ordering within section
    title TEXT NOT NULL,          -- Short title (e.g., "Gate step required with ALL non-prompt steps")
    content TEXT NOT NULL,        -- Full markdown rule text
    condition TEXT,               -- NULL = always, 'has_sdk_connect', 'targets_web_app'
    status TEXT NOT NULL DEFAULT 'active',  -- 'active', 'disabled', 'superseded'
    provenance TEXT NOT NULL DEFAULT 'seed', -- 'seed' (original hardcoded), 'reflection' (created by reflection auto-apply), 'auto_insight' (from prompt analysis)
    source_fix_id TEXT,           -- FK to reflection_fixes.id if provenance = 'reflection'
    confidence REAL DEFAULT 1.0,          -- Confidence score for auto-generated rules
    auto_generated_at TEXT,               -- When auto-generated (NULL for manual/seed rules)
    evidence_count INTEGER DEFAULT 0,     -- How many examples support this rule
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (source_fix_id) REFERENCES reflection_fixes(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_generation_rules_agent ON generation_rules(agent);
CREATE INDEX IF NOT EXISTS idx_generation_rules_status ON generation_rules(status);
CREATE INDEX IF NOT EXISTS idx_generation_rules_agent_section ON generation_rules(agent, section, rule_number);

-- =============================================================================
-- Step Type Knowledge (v63)
-- Per-step-type best practices and pitfalls for workflow generation.
-- =============================================================================

CREATE TABLE IF NOT EXISTS step_type_knowledge (
    id TEXT PRIMARY KEY,
    step_type TEXT NOT NULL,
    layer TEXT NOT NULL DEFAULT 'universal',  -- 'universal' | 'system_specific'
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,      -- higher = more important
    status TEXT NOT NULL DEFAULT 'active',     -- 'active' | 'disabled'
    provenance TEXT NOT NULL DEFAULT 'seed',   -- 'seed' | 'reflection' | 'manual'
    source_fix_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (source_fix_id) REFERENCES reflection_fixes(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_stk_step_type ON step_type_knowledge(step_type);
CREATE INDEX IF NOT EXISTS idx_stk_layer ON step_type_knowledge(layer);
CREATE INDEX IF NOT EXISTS idx_stk_composite ON step_type_knowledge(step_type, layer, status);

-- =============================================================================
-- Process Sessions (v68)
-- Persistent history of managed process sessions (start/stop/output).
-- =============================================================================

CREATE TABLE IF NOT EXISTS process_sessions (
    id TEXT PRIMARY KEY,
    process_config_id TEXT NOT NULL,
    process_name TEXT NOT NULL,
    started_at TEXT NOT NULL,
    stopped_at TEXT,
    exit_code INTEGER,
    state TEXT NOT NULL DEFAULT 'running',
    error_count INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_process_sessions_config_id ON process_sessions(process_config_id);
CREATE INDEX IF NOT EXISTS idx_process_sessions_started_at ON process_sessions(started_at);

CREATE TABLE IF NOT EXISTS process_session_output (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    stream TEXT NOT NULL,
    line TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES process_sessions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_process_session_output_session ON process_session_output(session_id);

-- ============================================================================
-- Generator Evaluation
-- ============================================================================

CREATE TABLE IF NOT EXISTS generation_pipeline_artifacts (
    id TEXT PRIMARY KEY,
    workflow_id TEXT,
    task_run_id TEXT,
    description TEXT NOT NULL,
    category TEXT,
    created_at TEXT NOT NULL,

    -- Investigation
    investigation_duration_ms INTEGER,
    investigation_enriched_description TEXT,

    -- Timing (milliseconds)
    discovery_duration_ms INTEGER,
    builder_duration_ms INTEGER,
    autofix_duration_ms INTEGER,
    verification_duration_ms INTEGER,
    hardener_duration_ms INTEGER,
    total_duration_ms INTEGER,

    -- Intermediate snapshots (JSON)
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

    -- Specification
    specification_duration_ms INTEGER,
    specification_criteria TEXT,

    -- Prompt capture (for training data / analysis)
    specification_prompt TEXT,
    builder_prompt TEXT,
    verification_prompts TEXT,
    hardener_prompt TEXT,

    -- Revision phase
    revision_duration_ms INTEGER DEFAULT NULL,
    quality_report TEXT DEFAULT NULL,
    revision_cycles INTEGER DEFAULT NULL,

    -- Outcome
    success INTEGER NOT NULL DEFAULT 1,
    error_message TEXT,
    model_used TEXT
);

CREATE INDEX IF NOT EXISTS idx_pipeline_artifacts_workflow ON generation_pipeline_artifacts(workflow_id);
CREATE INDEX IF NOT EXISTS idx_pipeline_artifacts_created ON generation_pipeline_artifacts(created_at);

CREATE TABLE IF NOT EXISTS generator_benchmarks (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    category TEXT,
    tags TEXT,
    expected_structure TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS generator_benchmark_results (
    id TEXT PRIMARY KEY,
    benchmark_id TEXT NOT NULL REFERENCES generator_benchmarks(id),
    artifact_id TEXT REFERENCES generation_pipeline_artifacts(id),
    run_at TEXT NOT NULL,
    model_used TEXT,

    -- Scores (0.0 - 1.0)
    structure_score REAL,
    content_score REAL,
    step_type_score REAL,
    overall_score REAL,

    -- Details
    score_breakdown TEXT,
    generated_json TEXT,
    duration_ms INTEGER,
    passed INTEGER NOT NULL DEFAULT 0,
    notes TEXT
);

CREATE INDEX IF NOT EXISTS idx_benchmark_results_benchmark ON generator_benchmark_results(benchmark_id);
CREATE INDEX IF NOT EXISTS idx_benchmark_results_run_at ON generator_benchmark_results(run_at);

-- Initialize singleton tables
INSERT OR IGNORE INTO gui_lock (id, holder_session_id, acquired_at) VALUES (1, NULL, NULL);
INSERT OR IGNORE INTO scheduler_settings (id) VALUES (1);
-- ============================================================================
-- Workflow Triggers (event-driven automation)
-- ============================================================================

CREATE TABLE IF NOT EXISTS workflow_triggers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    trigger_type TEXT NOT NULL,         -- webhook, file_watch, workflow_chain, git_event, health_check
    trigger_config TEXT NOT NULL,       -- Type-specific JSON (serde tagged enum)
    workflow_id TEXT NOT NULL,          -- FK to unified_workflows
    workflow_overrides TEXT,            -- Optional JSON overrides (max_iterations, model, etc.)
    conditions TEXT DEFAULT '[]',       -- JSON array of conditions
    debounce_ms INTEGER DEFAULT 1000,
    cooldown_seconds INTEGER DEFAULT 60,
    max_concurrent INTEGER DEFAULT 1,
    retry_count INTEGER DEFAULT 0,
    retry_delay_seconds INTEGER DEFAULT 30,
    enabled BOOLEAN DEFAULT 1,
    last_triggered_at TEXT,
    last_execution_id TEXT,
    trigger_count INTEGER DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (workflow_id) REFERENCES unified_workflows(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_workflow_triggers_type ON workflow_triggers(trigger_type);
CREATE INDEX IF NOT EXISTS idx_workflow_triggers_enabled ON workflow_triggers(enabled);

CREATE TABLE IF NOT EXISTS trigger_history (
    id TEXT PRIMARY KEY,
    trigger_id TEXT NOT NULL,
    event_type TEXT NOT NULL,           -- webhook_received, file_changed, workflow_completed, etc.
    event_data TEXT DEFAULT '{}',       -- JSON payload
    action TEXT NOT NULL,               -- executed, debounced, throttled, condition_failed, error
    task_run_id TEXT,                   -- FK to task_runs (if workflow was spawned)
    error_message TEXT,
    triggered_at TEXT NOT NULL,
    FOREIGN KEY (trigger_id) REFERENCES workflow_triggers(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_trigger_history_trigger_id ON trigger_history(trigger_id);
CREATE INDEX IF NOT EXISTS idx_trigger_history_triggered_at ON trigger_history(triggered_at);

-- Canvas Panels (A2UI agent-to-UI structured panels)
-- Version 76: Canvas panels for rich visual content from AI agent
CREATE TABLE IF NOT EXISTS canvas_panels (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    component TEXT NOT NULL,
    title TEXT NOT NULL,
    data_json TEXT NOT NULL,
    priority INTEGER DEFAULT 50,
    size TEXT DEFAULT 'normal',
    group_name TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_canvas_panels_task_run_id ON canvas_panels(task_run_id);

-- Approval Gates (human-in-the-loop audit trail)
-- Version 78: Approval gate records for workflow pause/resume decisions
CREATE TABLE IF NOT EXISTS approval_gates (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    iteration INTEGER NOT NULL,
    prompt TEXT NOT NULL,
    context_json TEXT DEFAULT '{}',     -- ApprovalContext as JSON (summary, files_modified, diffs)
    action TEXT,                         -- approve, reject, abort (NULL while pending)
    comment TEXT,                        -- reviewer comment
    status TEXT NOT NULL DEFAULT 'pending', -- pending, approved, rejected, aborted
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    resolved_at TEXT,
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_approval_gates_task_run_id ON approval_gates(task_run_id);
CREATE INDEX IF NOT EXISTS idx_approval_gates_status ON approval_gates(status);

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (78, datetime('now'));

-- User-created skills (parameterized step templates)
-- Built-in skills are embedded in the binary; only user skills are stored here.
CREATE TABLE IF NOT EXISTS user_skills (
    id TEXT PRIMARY KEY,                    -- "user:<slug>"
    name TEXT NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    description TEXT DEFAULT '',
    category TEXT DEFAULT 'custom',         -- code-quality, testing, monitoring, ai-task, deployment, composition, custom
    tags TEXT DEFAULT '[]',                 -- JSON array of tag strings
    icon TEXT DEFAULT 'puzzle',
    color TEXT DEFAULT 'gray',
    allowed_phases TEXT NOT NULL DEFAULT '["setup"]',  -- JSON array of phase strings
    parameters TEXT DEFAULT '[]',           -- JSON array of SkillParameter objects
    template TEXT NOT NULL,                 -- JSON SkillTemplate (single_step or multi_step)
    source TEXT NOT NULL DEFAULT 'user',    -- "user" | "community"
    version TEXT DEFAULT '1.0.0',          -- semantic version of the skill
    author TEXT DEFAULT NULL,              -- JSON SkillAuthor object
    checksum TEXT DEFAULT NULL,            -- SHA-256 of skill content
    depends_on TEXT DEFAULT '[]',          -- JSON array of skill IDs this skill depends on
    usage_count INTEGER DEFAULT 0,         -- number of times this skill has been instantiated
    approval_status TEXT DEFAULT NULL,     -- "pending" | "approved" | "rejected" (org context)
    forked_from TEXT DEFAULT NULL,         -- ID of the skill this was forked from
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_user_skills_slug ON user_skills(slug);
CREATE INDEX IF NOT EXISTS idx_user_skills_category ON user_skills(category);
CREATE INDEX IF NOT EXISTS idx_user_skills_updated_at ON user_skills(updated_at);
CREATE INDEX IF NOT EXISTS idx_user_skills_source ON user_skills(source);

-- UI Bridge integration tracking (projects with source integration)
CREATE TABLE IF NOT EXISTS ui_bridge_integrations (
    id TEXT PRIMARY KEY,
    project_path TEXT NOT NULL,
    label TEXT,
    framework TEXT,
    integration_type TEXT NOT NULL,  -- 'source'
    sdk_version TEXT,
    status TEXT NOT NULL DEFAULT 'active',  -- 'active' | 'disconnected' | 'outdated'
    target_url TEXT,
    last_health_check INTEGER,
    element_count INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_ui_bridge_integrations_status ON ui_bridge_integrations(status);
CREATE INDEX IF NOT EXISTS idx_ui_bridge_integrations_type ON ui_bridge_integrations(integration_type);

-- =============================================================================
-- Known Issues Registry (Version 91)
-- =============================================================================
-- Persistent known issue tracking that survives across workflow runs.
-- Issues are scoped to specs, URLs, components, or global.
-- Used by the generation pipeline to inject regression verification steps.

-- Issue Pattern Templates: Reusable detection strategies for common bug categories
CREATE TABLE IF NOT EXISTS issue_pattern_templates (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    category TEXT NOT NULL,
    detection_type TEXT NOT NULL,
    step_template TEXT,
    ai_prompt_template TEXT,
    parameters TEXT NOT NULL DEFAULT '[]',
    built_in BOOLEAN NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_ipt_category ON issue_pattern_templates(category);
CREATE INDEX IF NOT EXISTS idx_ipt_status ON issue_pattern_templates(status);

CREATE TABLE IF NOT EXISTS known_issues (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    category TEXT NOT NULL DEFAULT 'other',

    -- Scope: where does this issue apply?
    scope_type TEXT NOT NULL DEFAULT 'global',
    scope_value TEXT,
    scope_tags TEXT DEFAULT '[]',

    -- Detection strategy
    detection_method TEXT NOT NULL DEFAULT 'ai_judgment',
    detection_config TEXT DEFAULT '{}',
    pattern_template_id TEXT,

    -- Reproduction context
    reproduction_context TEXT,
    trigger_conditions TEXT DEFAULT '[]',

    -- Severity and lifecycle
    severity TEXT NOT NULL DEFAULT 'medium',
    status TEXT NOT NULL DEFAULT 'active',
    confidence REAL NOT NULL DEFAULT 1.0,

    -- Provenance
    provenance TEXT NOT NULL DEFAULT 'manual',
    source_finding_ids TEXT DEFAULT '[]',
    source_task_run_id TEXT,

    -- Verification hints
    verification_hint TEXT,
    verification_step_template TEXT,

    -- Tracking
    times_detected INTEGER DEFAULT 1,
    times_checked INTEGER DEFAULT 0,
    last_detected_at TEXT,
    last_checked_at TEXT,
    resolved_at TEXT,

    -- Embedding for semantic search
    description_embedding BLOB,

    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),

    FOREIGN KEY (pattern_template_id) REFERENCES issue_pattern_templates(id) ON DELETE SET NULL,
    FOREIGN KEY (source_task_run_id) REFERENCES task_runs(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_known_issues_category ON known_issues(category);
CREATE INDEX IF NOT EXISTS idx_known_issues_scope_type ON known_issues(scope_type);
CREATE INDEX IF NOT EXISTS idx_known_issues_status ON known_issues(status);
CREATE INDEX IF NOT EXISTS idx_known_issues_severity ON known_issues(severity);
CREATE INDEX IF NOT EXISTS idx_known_issues_scope_value ON known_issues(scope_value);
CREATE INDEX IF NOT EXISTS idx_known_issues_scope_compound ON known_issues(scope_type, scope_value, status);

-- State Machine Config Builder tables (for creating/editing state machines in the runner)
CREATE TABLE IF NOT EXISTS state_machine_configs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL DEFAULT 'default',
    description TEXT,
    render_count INTEGER NOT NULL DEFAULT 0,
    element_count INTEGER NOT NULL DEFAULT 0,
    include_html_ids BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS state_machine_states (
    id TEXT PRIMARY KEY,
    config_id TEXT NOT NULL REFERENCES state_machine_configs(id) ON DELETE CASCADE,
    state_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    element_ids TEXT NOT NULL DEFAULT '[]',
    render_ids TEXT NOT NULL DEFAULT '[]',
    confidence REAL NOT NULL DEFAULT 0.9,
    acceptance_criteria TEXT NOT NULL DEFAULT '[]',
    extra_metadata TEXT NOT NULL DEFAULT '{}',
    domain_knowledge TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_sm_states_config_id ON state_machine_states(config_id);

CREATE TABLE IF NOT EXISTS state_machine_transitions (
    id TEXT PRIMARY KEY,
    config_id TEXT NOT NULL REFERENCES state_machine_configs(id) ON DELETE CASCADE,
    transition_id TEXT NOT NULL,
    name TEXT NOT NULL,
    from_states TEXT NOT NULL DEFAULT '[]',
    activate_states TEXT NOT NULL DEFAULT '[]',
    exit_states TEXT NOT NULL DEFAULT '[]',
    actions TEXT NOT NULL DEFAULT '[]',
    path_cost REAL NOT NULL DEFAULT 1.0,
    stays_visible BOOLEAN NOT NULL DEFAULT FALSE,
    extra_metadata TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_sm_transitions_config_id ON state_machine_transitions(config_id);

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (93, datetime('now'));

-- =============================================================================
-- Workflow AI Sessions (Phase 94: Workflow Restart Survival)
-- =============================================================================
-- Tracks Claude CLI sessions spawned during workflow execution.
-- Stores the CLI session ID so workflows can be resumed after runner restarts
-- using `claude --resume <session-id>`.

CREATE TABLE IF NOT EXISTS workflow_ai_sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_run_id TEXT NOT NULL,
    iteration INTEGER NOT NULL,
    phase TEXT NOT NULL,                    -- 'setup', 'agentic', 'completion'
    stage_index INTEGER,
    claude_cli_session_id TEXT,            -- UUID passed via --session-id to Claude CLI
    session_started_at TEXT NOT NULL,
    session_completed_at TEXT,
    output_length INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'running', -- 'running', 'completed', 'failed', 'interrupted'
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_wf_ai_sessions_task_run ON workflow_ai_sessions(task_run_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_wf_ai_sessions_unique
    ON workflow_ai_sessions(task_run_id, iteration, phase, COALESCE(stage_index, -1));

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (94, datetime('now'));

-- =============================================================================
-- Workflow Constraint Results (Version 98)
-- =============================================================================
-- Stores constraint engine evaluation results per-iteration for post-run review.
-- Similar to workflow_verification_phase_results but for constraint checks
-- (secrets, scope violations, forbidden patterns, etc.)

CREATE TABLE IF NOT EXISTS workflow_constraint_results (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_run_id TEXT NOT NULL,
    iteration INTEGER NOT NULL,
    constraint_id TEXT NOT NULL,
    constraint_name TEXT NOT NULL,
    passed INTEGER NOT NULL,              -- 0/1 boolean
    severity TEXT NOT NULL,               -- 'block', 'warn', 'log'
    violations_json TEXT,                 -- JSON array of ConstraintViolation objects
    created_at TEXT NOT NULL DEFAULT (datetime('now')),

    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_wf_constraint_task_run ON workflow_constraint_results(task_run_id);
CREATE INDEX IF NOT EXISTS idx_wf_constraint_iteration ON workflow_constraint_results(iteration);
CREATE INDEX IF NOT EXISTS idx_wf_constraint_passed ON workflow_constraint_results(passed);

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (98, datetime('now'));

-- =============================================================================
-- Cognitive System Model (Version 99)
-- =============================================================================
-- Adds knowledge properties (accumulation monotonicity, convergence gradient,
-- relevance decay) and prediction capabilities to the reflection system.

-- Fix Applications: tracks each time a fix is reused for similar errors
CREATE TABLE IF NOT EXISTS fix_applications (
    id TEXT PRIMARY KEY,
    fix_id TEXT NOT NULL,                    -- FK to reflection_fixes
    task_run_id TEXT NOT NULL,               -- The run where the fix was applied
    error_signature_hash TEXT,               -- The error that triggered this application
    outcome TEXT DEFAULT 'pending',          -- 'resolved', 'ineffective', 'pending'
    applied_at TEXT NOT NULL,
    evaluated_at TEXT,
    FOREIGN KEY (fix_id) REFERENCES reflection_fixes(id) ON DELETE CASCADE,
    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_fix_applications_fix ON fix_applications(fix_id);
CREATE INDEX IF NOT EXISTS idx_fix_applications_task ON fix_applications(task_run_id);
CREATE INDEX IF NOT EXISTS idx_fix_applications_sig ON fix_applications(error_signature_hash);

-- Convergence Snapshots: time-series convergence metrics
CREATE TABLE IF NOT EXISTS convergence_snapshots (
    id TEXT PRIMARY KEY,
    workflow_name TEXT NOT NULL,
    project_path TEXT,
    scope TEXT NOT NULL DEFAULT 'workflow',  -- 'workflow' or 'project'
    convergence_score REAL NOT NULL,         -- 0.0 to 1.0
    consecutive_clean_runs INTEGER NOT NULL,
    novelty_score REAL NOT NULL,             -- 0.0 to 1.0 (how much new stuff was learned)
    effective_fix_rate REAL NOT NULL,        -- 0.0 to 1.0
    change_velocity REAL NOT NULL,           -- fixes per run over sliding window
    total_fixes INTEGER NOT NULL,
    effective_fixes INTEGER NOT NULL,
    snapshot_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_convergence_workflow ON convergence_snapshots(workflow_name);
CREATE INDEX IF NOT EXISTS idx_convergence_project ON convergence_snapshots(project_path);
CREATE INDEX IF NOT EXISTS idx_convergence_scope ON convergence_snapshots(scope);

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (99, datetime('now'));

-- =============================================================================
-- Version 100: Causal Chain Tracking
-- =============================================================================
-- Adds directed cause→effect graph for tracking causal relationships between
-- events (findings, errors, fixes, verifications).

CREATE TABLE IF NOT EXISTS causal_events (
    id TEXT PRIMARY KEY,
    -- Cause side
    cause_event_type TEXT NOT NULL,       -- 'finding_detected', 'error_occurred', 'code_change', etc.
    cause_event_id TEXT NOT NULL,         -- FK to the source table (polymorphic)
    -- Effect side
    effect_event_type TEXT NOT NULL,      -- same enum as cause_event_type
    effect_event_id TEXT NOT NULL,        -- FK to the target table (polymorphic)
    -- Relationship metadata
    relationship TEXT NOT NULL,           -- 'caused', 'triggered', 'resolved', 'prevented'
    confidence TEXT NOT NULL DEFAULT 'high', -- 'high', 'medium', 'low'
    source TEXT NOT NULL DEFAULT 'automated', -- 'automated' or 'ai_identified'
    -- Context
    task_run_id TEXT,                     -- Which run this relationship was identified in
    workflow_name TEXT,
    description TEXT,                     -- Human-readable explanation of the link
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_causal_cause ON causal_events(cause_event_type, cause_event_id);
CREATE INDEX IF NOT EXISTS idx_causal_effect ON causal_events(effect_event_type, effect_event_id);
CREATE INDEX IF NOT EXISTS idx_causal_workflow ON causal_events(workflow_name);
CREATE INDEX IF NOT EXISTS idx_causal_task_run ON causal_events(task_run_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_causal_dedup ON causal_events(cause_event_type, cause_event_id, effect_event_type, effect_event_id);

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (100, datetime('now'));

-- =============================================================================
-- Version 101: Architecture Model
-- =============================================================================
-- Aggregated component-level data from reflection fixes, causal events, and
-- knowledge into a queryable graph of components and their relationships.

CREATE TABLE IF NOT EXISTS architecture_components (
    id TEXT PRIMARY KEY,
    workflow_name TEXT NOT NULL,
    component_path TEXT NOT NULL,
    component_type TEXT NOT NULL DEFAULT 'file',  -- 'file', 'module', 'service'
    fix_count INTEGER NOT NULL DEFAULT 0,
    error_count INTEGER NOT NULL DEFAULT 0,
    causal_involvement_count INTEGER NOT NULL DEFAULT 0,
    effective_fix_count INTEGER NOT NULL DEFAULT 0,
    ineffective_fix_count INTEGER NOT NULL DEFAULT 0,
    health_score REAL NOT NULL DEFAULT 1.0,
    change_velocity REAL NOT NULL DEFAULT 0.0,
    last_activity_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(workflow_name, component_path)
);
CREATE INDEX IF NOT EXISTS idx_arch_comp_workflow ON architecture_components(workflow_name);
CREATE INDEX IF NOT EXISTS idx_arch_comp_health ON architecture_components(health_score);

CREATE TABLE IF NOT EXISTS component_relationships (
    id TEXT PRIMARY KEY,
    workflow_name TEXT NOT NULL,
    source_component TEXT NOT NULL,
    target_component TEXT NOT NULL,
    relationship_type TEXT NOT NULL,  -- 'impacts', 'co_changes_with'
    strength INTEGER NOT NULL DEFAULT 1,
    last_seen_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(workflow_name, source_component, target_component, relationship_type)
);
CREATE INDEX IF NOT EXISTS idx_comp_rel_workflow ON component_relationships(workflow_name);
CREATE INDEX IF NOT EXISTS idx_comp_rel_source ON component_relationships(source_component);

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (101, datetime('now'));

-- =============================================================================
-- Version 102: Constraint Overrides
-- =============================================================================
-- Adds constraint_overrides column to unified_workflows table.
-- Column is already in the canonical CREATE TABLE above; this migration
-- adds it to existing databases via ALTER TABLE.

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (102, datetime('now'));

-- =============================================================================
-- Version 103: Component Health Snapshots (temporal trends)
-- =============================================================================
CREATE TABLE IF NOT EXISTS component_health_snapshots (
    id TEXT PRIMARY KEY,
    workflow_name TEXT NOT NULL,
    component_path TEXT NOT NULL,
    health_score REAL NOT NULL,
    fix_count INTEGER NOT NULL DEFAULT 0,
    effective_fix_count INTEGER NOT NULL DEFAULT 0,
    change_velocity REAL NOT NULL DEFAULT 0.0,
    snapshot_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_comp_health_snap_wf ON component_health_snapshots(workflow_name);
CREATE INDEX IF NOT EXISTS idx_comp_health_snap_comp ON component_health_snapshots(workflow_name, component_path);
CREATE INDEX IF NOT EXISTS idx_comp_health_snap_at ON component_health_snapshots(snapshot_at);

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (103, datetime('now'));

-- =============================================================================
-- Version 104: Decision Context Capture
-- =============================================================================
-- Adds reasoning and alternatives_considered to reflection_fixes for structured
-- decision context capture. Separates "why" from "what" in fix descriptions.

-- Columns reasoning and alternatives_considered are already in the CREATE TABLE above.
-- ALTER TABLE statements removed to avoid "duplicate column" errors on fresh databases.

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (104, datetime('now'));

-- =============================================================================
-- Version 105: Cross-Project Patterns (Hybrid RAG)
-- =============================================================================
-- Adds applicability_context and fix_description_embedding to reflection_fixes
-- for universal cross-project pattern retrieval via hybrid semantic search.

-- Columns applicability_context and fix_description_embedding are already in the CREATE TABLE above.
-- ALTER TABLE statements removed to avoid "duplicate column" errors on fresh databases.

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (105, datetime('now'));

-- =============================================================================
-- Version 106: Generation Rule Application Tracking
-- =============================================================================
-- Tracks which generation rules were used in each workflow generation run,
-- enabling effectiveness analysis of rules over time.

CREATE TABLE IF NOT EXISTS rule_applications (
    id TEXT PRIMARY KEY,
    rule_id TEXT NOT NULL,
    workflow_id TEXT,
    task_run_id TEXT,
    agent TEXT NOT NULL,
    section TEXT NOT NULL,
    applied_at TEXT NOT NULL,
    FOREIGN KEY (rule_id) REFERENCES generation_rules(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_rule_apps_rule ON rule_applications(rule_id);
CREATE INDEX IF NOT EXISTS idx_rule_apps_workflow ON rule_applications(workflow_id);

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (106, datetime('now'));

-- =============================================================================
-- Cached App Specs (from version 69 migration)
-- =============================================================================
-- Stores specs discovered from UI Bridge SDK-integrated apps via /control/specs.
-- Used by the Architecture page to display project architecture diagrams.

CREATE TABLE IF NOT EXISTS cached_app_specs (
    id TEXT PRIMARY KEY,
    app_url TEXT NOT NULL,
    app_name TEXT NOT NULL,
    spec_id TEXT NOT NULL,
    spec_json TEXT NOT NULL,
    discovered_at TEXT NOT NULL,
    page_url TEXT
);

CREATE INDEX IF NOT EXISTS idx_cached_specs_app ON cached_app_specs(app_url);

-- =============================================================================
-- Capture Screenshots for State View (migration 111)
-- =============================================================================

CREATE TABLE IF NOT EXISTS sm_capture_screenshots (
    id TEXT PRIMARY KEY,
    config_id TEXT NOT NULL REFERENCES state_machine_configs(id) ON DELETE CASCADE,
    capture_index INTEGER NOT NULL,
    screenshot_webp BLOB NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    element_bounds_json TEXT NOT NULL DEFAULT '{}',
    fingerprint_hashes_json TEXT NOT NULL DEFAULT '[]',
    captured_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_sm_screenshots_config ON sm_capture_screenshots(config_id);

-- =============================================================================
-- Orchestration Loop Configs (migration 112)
-- =============================================================================

CREATE TABLE IF NOT EXISTS orchestration_loop_configs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    is_favorite BOOLEAN DEFAULT 0,
    config_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ol_configs_favorite ON orchestration_loop_configs(is_favorite);
CREATE INDEX IF NOT EXISTS idx_ol_configs_updated ON orchestration_loop_configs(updated_at);

-- =============================================================================
-- Autoresearch Tables (migration 118)
-- =============================================================================

CREATE TABLE IF NOT EXISTS autoresearch_campaigns (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    config_json TEXT NOT NULL,
    current_control_json TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'running',
    experiment_count INTEGER NOT NULL DEFAULT 0,
    accepted_count INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS autoresearch_experiments (
    id TEXT PRIMARY KEY,
    campaign_id TEXT NOT NULL REFERENCES autoresearch_campaigns(id) ON DELETE CASCADE,
    experiment_number INTEGER NOT NULL,
    config_json TEXT NOT NULL,
    trials_json TEXT NOT NULL,
    aggregate_json TEXT NOT NULL,
    accepted INTEGER NOT NULL DEFAULT 0,
    reason TEXT,
    p_value REAL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_autoresearch_exp_campaign ON autoresearch_experiments(campaign_id);
CREATE INDEX IF NOT EXISTS idx_autoresearch_exp_number ON autoresearch_experiments(campaign_id, experiment_number);

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (118, datetime('now'));

-- =============================================================================
-- Meta-Optimizer Tables (migration 119)
-- =============================================================================

-- Persists PipelineAgentTrace data (currently in-memory only during pipeline execution).
-- Each row records one pipeline agent invocation with its inputs, outputs, and metrics.
CREATE TABLE IF NOT EXISTS pipeline_agent_traces (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    agent_type TEXT NOT NULL,              -- 'spec_analyst', 'locator', 'implementer', 'verifier'
    agent_id TEXT NOT NULL,                -- Unique agent instance ID (e.g. 'implementer_0')
    run_id TEXT NOT NULL,                  -- Pipeline run / execution ID
    input_snapshot TEXT NOT NULL DEFAULT '{}',   -- JSON: serialized input data
    output_snapshot TEXT NOT NULL DEFAULT '{}',  -- JSON: serialized output data
    config_json TEXT NOT NULL DEFAULT '{}',      -- JSON: serialized PipelineAgentConfig
    duration_ms INTEGER NOT NULL DEFAULT 0,
    tokens_in INTEGER NOT NULL DEFAULT 0,
    tokens_out INTEGER NOT NULL DEFAULT 0,
    cost_usd REAL NOT NULL DEFAULT 0.0,
    downstream_success INTEGER,            -- NULL until backfilled; 0/1
    output_quality_score REAL,             -- NULL until scored
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_pipeline_agent_traces_task_run ON pipeline_agent_traces(task_run_id);
CREATE INDEX IF NOT EXISTS idx_pipeline_agent_traces_agent_type ON pipeline_agent_traces(agent_type);
CREATE INDEX IF NOT EXISTS idx_pipeline_agent_traces_run_id ON pipeline_agent_traces(run_id);

-- Stores prompt variants for pipeline agents. The optimizer creates new variants;
-- humans activate them from the UI. Only one variant per agent_type can be active.
CREATE TABLE IF NOT EXISTS prompt_registry (
    id TEXT PRIMARY KEY,
    agent_type TEXT NOT NULL,              -- 'spec_analyst', 'locator', 'implementer', 'verifier'
    variant_name TEXT NOT NULL,
    prompt_content TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    is_active INTEGER NOT NULL DEFAULT 0,  -- Only one active per agent_type
    source_recommendation_id TEXT,         -- FK to meta_optimizer_recommendations
    performance_metrics TEXT DEFAULT '{}', -- JSON: {success_rate, avg_cost, sample_size, ...}
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(agent_type, variant_name, version)
);

CREATE INDEX IF NOT EXISTS idx_prompt_registry_agent_type ON prompt_registry(agent_type);
CREATE INDEX IF NOT EXISTS idx_prompt_registry_active ON prompt_registry(agent_type, is_active);

-- All optimizer outputs (recommendations). Never auto-applied — human reviews from UI.
CREATE TABLE IF NOT EXISTS meta_optimizer_recommendations (
    id TEXT PRIMARY KEY,
    optimizer_type TEXT NOT NULL,           -- 'pipeline_prompt', 'architecture', 'generation_template'
    recommendation_type TEXT NOT NULL,      -- 'prompt_rewrite', 'config_change', 'rule_update', 'rule_create'
    target_agent TEXT,                      -- Which agent/component this targets
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    current_value TEXT DEFAULT '{}',        -- JSON: what is currently in place
    recommended_value TEXT DEFAULT '{}',    -- JSON: what the optimizer recommends
    evidence TEXT DEFAULT '{}',            -- JSON: data supporting the recommendation
    confidence REAL NOT NULL DEFAULT 0.0,  -- 0.0 to 1.0
    status TEXT NOT NULL DEFAULT 'pending', -- 'pending', 'applied', 'rejected', 'superseded', 'rolled_back'
    applied_at TEXT,
    outcome_after_apply TEXT,              -- JSON: measured impact after application
    optimizer_run_id TEXT,                 -- FK to meta_optimizer_runs
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_meta_optimizer_recs_type ON meta_optimizer_recommendations(optimizer_type);
CREATE INDEX IF NOT EXISTS idx_meta_optimizer_recs_status ON meta_optimizer_recommendations(status);
CREATE INDEX IF NOT EXISTS idx_meta_optimizer_recs_run ON meta_optimizer_recommendations(optimizer_run_id);

-- Tracks meta-optimizer execution runs.
CREATE TABLE IF NOT EXISTS meta_optimizer_runs (
    id TEXT PRIMARY KEY,
    optimizer_type TEXT NOT NULL,           -- 'pipeline_prompt', 'architecture', 'generation_template'
    trigger_type TEXT NOT NULL DEFAULT 'threshold', -- 'threshold' or 'manual'
    runs_analyzed INTEGER NOT NULL DEFAULT 0,
    recommendations_produced INTEGER NOT NULL DEFAULT 0,
    task_run_id TEXT,                       -- FK to task_runs (the optimizer's own task run)
    status TEXT NOT NULL DEFAULT 'running', -- 'running', 'complete', 'failed'
    created_at TEXT NOT NULL,
    completed_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_meta_optimizer_runs_type ON meta_optimizer_runs(optimizer_type);

-- Progress tracking snapshots for measuring meta-optimizer impact
CREATE TABLE IF NOT EXISTS meta_optimizer_snapshots (
    id TEXT PRIMARY KEY,
    snapshot_type TEXT NOT NULL,         -- 'baseline', 'periodic', 'post_apply'
    period_start TEXT NOT NULL,
    period_end TEXT NOT NULL,
    metrics_json TEXT NOT NULL,          -- JSON: {success_rate, avg_duration_secs, avg_iterations, avg_cost_cents, total_runs, ...}
    breakdown_json TEXT DEFAULT '{}',    -- JSON: per-architecture or per-agent breakdown
    recommendation_id TEXT,             -- FK for post_apply snapshots
    runs_included INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_meta_optimizer_snapshots_type ON meta_optimizer_snapshots(snapshot_type);
CREATE INDEX IF NOT EXISTS idx_meta_optimizer_snapshots_rec ON meta_optimizer_snapshots(recommendation_id);

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (119, datetime('now'));
INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (120, datetime('now'));

-- =============================================================================
-- Canary Rollouts Table (migration 121)
-- =============================================================================

CREATE TABLE IF NOT EXISTS canary_rollouts (
    id TEXT PRIMARY KEY,
    recommendation_id TEXT NOT NULL,
    percentage INTEGER NOT NULL DEFAULT 10,
    status TEXT NOT NULL DEFAULT 'active',    -- 'active', 'promoted', 'rolled_back'
    start_date TEXT NOT NULL,
    end_date TEXT,
    baseline_run_count INTEGER DEFAULT 0,
    canary_run_count INTEGER DEFAULT 0,
    baseline_metrics_json TEXT DEFAULT '{}',
    canary_metrics_json TEXT DEFAULT '{}',
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_canary_status ON canary_rollouts(status);
CREATE INDEX IF NOT EXISTS idx_canary_rec ON canary_rollouts(recommendation_id);

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (121, datetime('now'));

-- =============================================================================
-- Comparison Runs Table (migration 122)
-- =============================================================================

CREATE TABLE IF NOT EXISTS comparison_runs (
    id TEXT PRIMARY KEY,
    workflow_id TEXT NOT NULL,
    variation_type TEXT NOT NULL,          -- 'architecture', 'same', 'custom'
    status TEXT NOT NULL DEFAULT 'running', -- 'running', 'completed', 'failed'
    entries_json TEXT NOT NULL DEFAULT '[]', -- JSON array of {label, overrides, task_run_id, status}
    report TEXT,                            -- AI comparison report (filled after all complete)
    created_at TEXT NOT NULL,
    completed_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_comparison_runs_workflow ON comparison_runs(workflow_id);
CREATE INDEX IF NOT EXISTS idx_comparison_runs_status ON comparison_runs(status);

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (122, datetime('now'));

-- =============================================================================
-- Spec Experimentation Tables (migration 123)
-- =============================================================================

CREATE TABLE IF NOT EXISTS spec_compliance_results (
    id TEXT PRIMARY KEY,
    task_run_id TEXT NOT NULL,
    spec_id TEXT,
    iteration INTEGER NOT NULL,
    overall_score REAL NOT NULL,
    raw_pass_rate REAL NOT NULL,
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
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_spec_compliance_task ON spec_compliance_results(task_run_id);
CREATE INDEX IF NOT EXISTS idx_spec_compliance_score ON spec_compliance_results(overall_score);
CREATE INDEX IF NOT EXISTS idx_spec_compliance_spec ON spec_compliance_results(spec_id);

CREATE TABLE IF NOT EXISTS spec_accuracy_results (
    id TEXT PRIMARY KEY,
    spec_id TEXT NOT NULL,
    analysis_type TEXT NOT NULL,
    score REAL NOT NULL,
    detail_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_spec_accuracy_spec ON spec_accuracy_results(spec_id);
CREATE INDEX IF NOT EXISTS idx_spec_accuracy_type ON spec_accuracy_results(analysis_type);

INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (123, datetime('now'));
