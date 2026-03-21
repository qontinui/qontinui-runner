//! Database migration logic for the runner database.
//!
//! Contains all schema migrations from version 0 to current.

use chrono::Utc;
use rusqlite::{params, Connection};
use tracing::info;

use super::CheckpointDb;

impl CheckpointDb {
    /// Run database migrations on a specific connection.
    /// This is a static method to allow calling during pool initialization.
    pub(crate) fn run_migrations_on_conn(conn: &Connection) -> Result<(), String> {
        // Check current schema version
        let current_version: i32 = conn
            .query_row(
                "SELECT version FROM schema_version ORDER BY version DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if current_version == 0 {
            // Fresh database - create schema
            // schema.sql contains the complete, up-to-date schema, so no migrations needed
            info!("Creating database schema from schema.sql (fresh database)");
            conn.execute_batch(include_str!("schema.sql"))
                .map_err(|e| format!("Failed to create schema: {}", e))?;
            // Return early - schema.sql is the complete schema at the latest version
            // No need to run migrations which would try to add columns/tables that already exist
            return Ok(());
        }

        // Migration to version 2: Add task_runs table
        if current_version == 1 {
            info!("Migrating database to version 2 (adding task_runs table)");
            conn.execute_batch(
                r#"
                -- Task Runs (simplified task execution model)
                CREATE TABLE IF NOT EXISTS task_runs (
                    id TEXT PRIMARY KEY,
                    task_name TEXT NOT NULL,
                    prompt TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'running',
                    sessions_count INTEGER NOT NULL DEFAULT 0,
                    max_sessions INTEGER,
                    output_log TEXT DEFAULT '',
                    error_message TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    completed_at TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_task_runs_status ON task_runs(status);
                CREATE INDEX IF NOT EXISTS idx_task_runs_created_at ON task_runs(created_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (2, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 2: {}", e))?;
        }

        // Migration to version 3: Add auto_continue column to task_runs
        if current_version == 1 || current_version == 2 {
            info!("Migrating database to version 3 (adding auto_continue to task_runs)");
            conn.execute_batch(
                r#"
                -- Add auto_continue column to task_runs (default true)
                ALTER TABLE task_runs ADD COLUMN auto_continue BOOLEAN NOT NULL DEFAULT 1;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (3, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 3: {}", e))?;
        }

        // Migration to version 4: Add task_run_output_chunks table for O(1) appending
        if (1..4).contains(&current_version) {
            info!("Migrating database to version 4 (adding task_run_output_chunks table)");

            // Create the chunks table
            conn.execute_batch(
                r#"
                -- Task Run Output Chunks (for efficient O(1) appending)
                CREATE TABLE IF NOT EXISTS task_run_output_chunks (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_run_id TEXT NOT NULL,
                    chunk_sequence INTEGER NOT NULL,
                    content TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_chunks_task_run ON task_run_output_chunks(task_run_id, chunk_sequence);
                "#,
            )
            .map_err(|e| format!("Failed to create task_run_output_chunks table: {}", e))?;

            // Migrate existing output_log data to chunks (one-time migration)
            let now = Utc::now().to_rfc3339();

            // Get all task_runs with non-empty output_log
            let mut stmt = conn
                .prepare("SELECT id, output_log FROM task_runs WHERE output_log != '' AND output_log IS NOT NULL")
                .map_err(|e| format!("Failed to prepare migration query: {}", e))?;

            let tasks: Vec<(String, String)> = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|e| format!("Failed to query task_runs for migration: {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            drop(stmt); // Release the statement before executing more queries

            for (task_id, output) in &tasks {
                conn.execute(
                    "INSERT INTO task_run_output_chunks (task_run_id, chunk_sequence, content, created_at) VALUES (?1, 1, ?2, ?3)",
                    params![task_id, output, now],
                )
                .map_err(|e| format!("Failed to migrate output for task {}: {}", task_id, e))?;
            }

            if !tasks.is_empty() {
                info!("Migrated {} task runs' output_log to chunks", tasks.len());
            }

            conn.execute_batch(
                r#"
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (4, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to update schema version to 4: {}", e))?;
        }

        // Migration to version 5: Add configs table for ConfigStorage
        if (1..5).contains(&current_version) {
            info!("Migrating database to version 5 (adding configs table)");
            conn.execute_batch(
                r#"
                -- Configs storage (for auto-storing imported/loaded configs)
                CREATE TABLE IF NOT EXISTS configs (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    config_json TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    source_path TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_configs_name ON configs(name);
                CREATE INDEX IF NOT EXISTS idx_configs_updated_at ON configs(updated_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (5, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 5: {}", e))?;
        }

        // Migration to version 6: Add task_run_findings table
        // (This migration was added between 5 and 7, keeping the version gap for backward compat)
        if (1..6).contains(&current_version) {
            info!("Migrating database to version 6 (adding task_run_findings table)");
            conn.execute_batch(
                r#"
                -- Task Run Findings (AI-detected issues tied to task runs)
                CREATE TABLE IF NOT EXISTS task_run_findings (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
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
                    needs_input BOOLEAN DEFAULT 0,
                    question TEXT,
                    input_options TEXT,
                    user_response TEXT,
                    detected_at TEXT NOT NULL,
                    resolved_at TEXT,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_findings_task_run ON task_run_findings(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_findings_status ON task_run_findings(status);
                CREATE INDEX IF NOT EXISTS idx_findings_signature ON task_run_findings(signature_hash);
                CREATE INDEX IF NOT EXISTS idx_findings_category ON task_run_findings(category);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (6, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 6: {}", e))?;
        }

        // Migration to version 7: Add Tiered Information Model tables
        if (1..7).contains(&current_version) {
            info!("Migrating database to version 7 (adding tiered information tables)");
            conn.execute_batch(
                r#"
                -- Run Details (Tier 1 - Detailed run data)
                CREATE TABLE IF NOT EXISTS run_details (
                    id TEXT PRIMARY KEY,
                    config_id TEXT NOT NULL,
                    workflow_name TEXT,
                    started_at TEXT NOT NULL,
                    ended_at TEXT,
                    duration_ms INTEGER,
                    status TEXT NOT NULL,
                    success BOOLEAN,
                    error_type TEXT,
                    error_message TEXT,
                    actions_summary TEXT,
                    states_visited TEXT,
                    transitions_executed TEXT,
                    template_matches TEXT,
                    anomalies TEXT,
                    FOREIGN KEY (config_id) REFERENCES configs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_run_details_config_id ON run_details(config_id);
                CREATE INDEX IF NOT EXISTS idx_run_details_started_at ON run_details(started_at);
                CREATE INDEX IF NOT EXISTS idx_run_details_status ON run_details(status);

                -- Config Statistics (Tier 4 - Aggregated statistics)
                CREATE TABLE IF NOT EXISTS config_statistics (
                    id TEXT PRIMARY KEY,
                    config_id TEXT NOT NULL UNIQUE,
                    config_hash TEXT,
                    total_runs INTEGER DEFAULT 0,
                    successful_runs INTEGER DEFAULT 0,
                    failed_runs INTEGER DEFAULT 0,
                    timeout_runs INTEGER DEFAULT 0,
                    avg_duration_ms INTEGER,
                    recent_success_rate REAL,
                    recent_avg_duration_ms INTEGER,
                    transition_stats TEXT,
                    template_stats TEXT,
                    state_stats TEXT,
                    error_patterns TEXT,
                    flaky_transitions TEXT,
                    flaky_templates TEXT,
                    first_run_at TEXT,
                    last_run_at TEXT,
                    last_updated_at TEXT,
                    FOREIGN KEY (config_id) REFERENCES configs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_config_statistics_config_id ON config_statistics(config_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (7, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 7: {}", e))?;
        }

        // Migration to version 8: Add pending_discoveries table for Discovery Push
        if (1..8).contains(&current_version) {
            info!("Migrating database to version 8 (adding pending_discoveries table)");
            conn.execute_batch(
                r#"
                -- Pending Discoveries (Discovery Push queue)
                -- Stores discoveries awaiting sync to qontinui-web
                CREATE TABLE IF NOT EXISTS pending_discoveries (
                    id TEXT PRIMARY KEY,
                    payload TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    last_attempt TEXT,
                    attempt_count INTEGER DEFAULT 0,
                    error TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_pending_discoveries_created_at ON pending_discoveries(created_at);
                CREATE INDEX IF NOT EXISTS idx_pending_discoveries_attempt_count ON pending_discoveries(attempt_count);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (8, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 8: {}", e))?;
        }

        // Migration to version 9: Add execution_steps_json and log_sources_json to task_runs
        if (1..9).contains(&current_version) {
            info!("Migrating database to version 9 (adding execution_steps_json and log_sources_json to task_runs)");
            conn.execute_batch(
                r#"
                -- Add columns for storing execution steps and log sources for re-execution on resume
                ALTER TABLE task_runs ADD COLUMN execution_steps_json TEXT;
                ALTER TABLE task_runs ADD COLUMN log_sources_json TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (9, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 9: {}", e))?;
        }

        // Migration to version 10: Add AI summary fields to task_runs
        if (1..10).contains(&current_version) {
            info!("Migrating database to version 10 (adding AI summary fields to task_runs)");
            conn.execute_batch(
                r#"
                -- Add columns for AI-generated summary and goal achievement tracking
                ALTER TABLE task_runs ADD COLUMN ai_summary TEXT;
                ALTER TABLE task_runs ADD COLUMN goal_achieved BOOLEAN;
                ALTER TABLE task_runs ADD COLUMN remaining_work TEXT;
                ALTER TABLE task_runs ADD COLUMN summary_generated_at TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (10, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 10: {}", e))?;
        }

        // Migration to version 11: Unified TaskRun architecture
        // - Add task_type, config_id, workflow_name to task_runs
        // - Rename ai_summary -> summary (keeping ai_summary as alias via code)
        // - Create task_run_automation table
        if (1..11).contains(&current_version) {
            info!("Migrating database to version 11 (unified TaskRun architecture)");

            // Step 1: Add new columns to task_runs
            // Note: SQLite doesn't support renaming columns in older versions, so we
            // add a new 'summary' column and will handle ai_summary -> summary mapping in code
            conn.execute_batch(
                r#"
                -- Add task_type column (default 'task' for existing runs)
                ALTER TABLE task_runs ADD COLUMN task_type TEXT NOT NULL DEFAULT 'task';

                -- Add config linkage columns
                ALTER TABLE task_runs ADD COLUMN config_id TEXT;
                ALTER TABLE task_runs ADD COLUMN workflow_name TEXT;

                -- Add summary column (new name for ai_summary - existing ai_summary still works)
                ALTER TABLE task_runs ADD COLUMN summary TEXT;

                -- Create indexes for new columns
                CREATE INDEX IF NOT EXISTS idx_task_runs_task_type ON task_runs(task_type);
                CREATE INDEX IF NOT EXISTS idx_task_runs_config_id ON task_runs(config_id);
                "#,
            )
            .map_err(|e| format!("Failed to add unified columns to task_runs: {}", e))?;

            // Step 2: Copy ai_summary to summary for existing rows
            conn.execute(
                "UPDATE task_runs SET summary = ai_summary WHERE ai_summary IS NOT NULL AND summary IS NULL",
                [],
            )
            .map_err(|e| format!("Failed to copy ai_summary to summary: {}", e))?;

            // Step 3: Create task_run_automation table
            conn.execute_batch(
                r#"
                -- Task Run Automation (child table for automation metrics)
                CREATE TABLE IF NOT EXISTS task_run_automation (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    workflow_name TEXT,
                    started_at TEXT NOT NULL,
                    ended_at TEXT,
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

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (11, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to create task_run_automation table: {}", e))?;

            info!("Successfully migrated to version 11 (unified TaskRun architecture)");
        }

        // Migration to version 12: Migrate existing run_details to task_run_automation
        // Creates parent task_runs rows for each run_details, then task_run_automation children
        if (1..12).contains(&current_version) {
            info!(
                "Migrating database to version 12 (migrating run_details to task_run_automation)"
            );

            // Step 1: Create task_runs rows for each run_details that doesn't have one
            // We use the run_details.id prefixed with 'tr-' as the task_run_id
            conn.execute_batch(
                r#"
                -- Create task_runs for each run_details (pure automation tasks)
                INSERT OR IGNORE INTO task_runs (
                    id,
                    task_name,
                    prompt,
                    task_type,
                    status,
                    sessions_count,
                    auto_continue,
                    output_log,
                    config_id,
                    workflow_name,
                    created_at,
                    updated_at,
                    completed_at
                )
                SELECT
                    'tr-' || rd.id,
                    COALESCE(rd.workflow_name, 'Automation Run'),
                    NULL,  -- No prompt for pure automation
                    'automation',
                    CASE
                        WHEN rd.status = 'completed' AND rd.success = 1 THEN 'complete'
                        WHEN rd.status = 'completed' AND rd.success = 0 THEN 'failed'
                        WHEN rd.status = 'failed' THEN 'failed'
                        WHEN rd.status = 'timeout' THEN 'failed'
                        WHEN rd.status = 'cancelled' THEN 'stopped'
                        ELSE 'complete'
                    END,
                    0,
                    0,
                    '',
                    rd.config_id,
                    rd.workflow_name,
                    rd.started_at,
                    COALESCE(rd.ended_at, rd.started_at),
                    rd.ended_at
                FROM run_details rd
                WHERE NOT EXISTS (
                    SELECT 1 FROM task_runs tr WHERE tr.id = 'tr-' || rd.id
                );
                "#,
            )
            .map_err(|e| format!("Failed to create task_runs from run_details: {}", e))?;

            // Step 2: Create task_run_automation rows from run_details
            conn.execute_batch(
                r#"
                -- Create task_run_automation for each run_details
                INSERT OR IGNORE INTO task_run_automation (
                    id,
                    task_run_id,
                    workflow_name,
                    started_at,
                    ended_at,
                    duration_ms,
                    automation_status,
                    success,
                    error_type,
                    error_message,
                    actions_summary,
                    states_visited,
                    transitions_executed,
                    template_matches,
                    anomalies,
                    iteration_number
                )
                SELECT
                    'tra-' || rd.id,
                    'tr-' || rd.id,
                    rd.workflow_name,
                    rd.started_at,
                    rd.ended_at,
                    rd.duration_ms,
                    CASE
                        WHEN rd.status = 'completed' AND rd.success = 1 THEN 'success'
                        WHEN rd.status = 'completed' AND rd.success = 0 THEN 'failed'
                        WHEN rd.status = 'failed' THEN 'failed'
                        WHEN rd.status = 'timeout' THEN 'timeout'
                        WHEN rd.status = 'cancelled' THEN 'cancelled'
                        ELSE 'success'
                    END,
                    rd.success,
                    rd.error_type,
                    rd.error_message,
                    rd.actions_summary,
                    rd.states_visited,
                    rd.transitions_executed,
                    rd.template_matches,
                    rd.anomalies,
                    1
                FROM run_details rd
                WHERE NOT EXISTS (
                    SELECT 1 FROM task_run_automation tra WHERE tra.id = 'tra-' || rd.id
                );

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (12, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to create task_run_automation from run_details: {}", e))?;

            info!(
                "Successfully migrated to version 12 (run_details migrated to task_run_automation)"
            );
        }

        // Migration to version 13: Drop deprecated run_details table
        // All data was migrated to task_run_automation in version 12
        if (1..13).contains(&current_version) {
            info!("Migrating database to version 13 (removing deprecated run_details table)");

            conn.execute_batch(
                r#"
                -- Drop the deprecated run_details table
                DROP TABLE IF EXISTS run_details;

                -- Update schema version
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (13, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to drop run_details table: {}", e))?;

            info!("Successfully migrated to version 13 (run_details table removed)");
        }

        // Migration to version 14: Add verification test infrastructure
        // Creates tables for test definitions, results, and associations
        if (1..14).contains(&current_version) {
            info!("Migrating database to version 14 (adding verification test infrastructure)");

            conn.execute_batch(
                r#"
                -- Verification Tests (test definitions stored in runner)
                CREATE TABLE IF NOT EXISTS verification_tests (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,
                    test_type TEXT NOT NULL,
                    category TEXT,
                    playwright_code TEXT,
                    vision_config TEXT,
                    python_code TEXT,
                    repo_test_config TEXT,
                    success_criteria TEXT,
                    config TEXT DEFAULT '{}',
                    timeout_seconds INTEGER NOT NULL DEFAULT 60,
                    is_critical BOOLEAN NOT NULL DEFAULT 1,
                    enabled BOOLEAN NOT NULL DEFAULT 1,
                    ai_generated BOOLEAN NOT NULL DEFAULT 0,
                    ai_generation_prompt TEXT,
                    tags TEXT DEFAULT '[]',
                    source_file TEXT,
                    last_exported_at TEXT,
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
                    task_run_id TEXT,
                    status TEXT NOT NULL DEFAULT 'pending',
                    started_at TEXT,
                    completed_at TEXT,
                    duration_ms INTEGER,
                    output TEXT,
                    error_message TEXT,
                    structured_output TEXT,
                    assertions_passed INTEGER DEFAULT 0,
                    assertions_failed INTEGER DEFAULT 0,
                    screenshots TEXT DEFAULT '[]',
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
                    config_id TEXT,
                    workflow_name TEXT,
                    trigger_point TEXT NOT NULL,
                    action_id TEXT,
                    execution_order INTEGER DEFAULT 0,
                    enabled BOOLEAN NOT NULL DEFAULT 1,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (test_id) REFERENCES verification_tests(id) ON DELETE CASCADE,
                    FOREIGN KEY (config_id) REFERENCES configs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_test_associations_test_id ON test_associations(test_id);
                CREATE INDEX IF NOT EXISTS idx_test_associations_config_id ON test_associations(config_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (14, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 14: {}", e))?;

            info!("Successfully migrated to version 14 (verification test infrastructure added)");
        }

        // Migration to version 15: Add creation_analysis to verification_tests and visual_evidence to test_results
        if (1..15).contains(&current_version) {
            info!("Migrating database to version 15 (adding creation_analysis and visual_evidence columns)");

            conn.execute_batch(
                r#"
                -- Add creation_analysis to verification_tests for AI debugging context
                ALTER TABLE verification_tests ADD COLUMN creation_analysis TEXT;

                -- Add visual_evidence to test_results for annotated screenshots
                ALTER TABLE test_results ADD COLUMN visual_evidence TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (15, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 15: {}", e))?;

            info!("Successfully migrated to version 15 (creation_analysis and visual_evidence columns added)");
        }

        // Migration to version 16: Add API request infrastructure tables
        if (1..16).contains(&current_version) {
            info!("Migrating database to version 16 (adding API request infrastructure tables)");

            conn.execute_batch(
                r#"
                -- API Credentials (metadata only, secrets in secure storage)
                CREATE TABLE IF NOT EXISTS api_credentials (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    credential_type TEXT NOT NULL,
                    storage_type TEXT NOT NULL DEFAULT 'secure',
                    token_endpoint TEXT,
                    client_id TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    expires_at TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_api_credentials_name ON api_credentials(name);
                CREATE INDEX IF NOT EXISTS idx_api_credentials_type ON api_credentials(credential_type);

                -- API Request Logs
                CREATE TABLE IF NOT EXISTS api_request_logs (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT,
                    step_id TEXT NOT NULL,
                    step_name TEXT,
                    method TEXT NOT NULL,
                    url TEXT NOT NULL,
                    resolved_url TEXT NOT NULL,
                    status_code INTEGER NOT NULL,
                    response_time_ms INTEGER NOT NULL,
                    response_body_type TEXT NOT NULL,
                    response_file_path TEXT,
                    response_size_bytes INTEGER,
                    success BOOLEAN NOT NULL,
                    assertion_failures INTEGER DEFAULT 0,
                    extractions_json TEXT,
                    assertions_json TEXT,
                    error TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_api_request_logs_task_run_id ON api_request_logs(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_api_request_logs_created_at ON api_request_logs(created_at);
                CREATE INDEX IF NOT EXISTS idx_api_request_logs_step_id ON api_request_logs(step_id);

                -- Workflow Variables
                CREATE TABLE IF NOT EXISTS workflow_variables (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    variable_name TEXT NOT NULL,
                    variable_value TEXT NOT NULL,
                    source TEXT NOT NULL,
                    source_step_id TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    UNIQUE(task_run_id, variable_name)
                );

                CREATE INDEX IF NOT EXISTS idx_workflow_variables_task_run_id ON workflow_variables(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_workflow_variables_name ON workflow_variables(variable_name);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (16, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 16: {}", e))?;

            info!("Successfully migrated to version 16 (API request infrastructure tables added)");
        }

        // Migration to version 18: Add saved_api_requests table for API Request Library
        if (1..18).contains(&current_version) {
            info!("Migrating database to version 18 (adding saved_api_requests table)");

            conn.execute_batch(
                r#"
                -- Saved API Request Templates (Library)
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
                    follow_redirects BOOLEAN DEFAULT 1,
                    variable_extractions TEXT DEFAULT '[]',
                    assertions TEXT DEFAULT '[]',
                    credential_id TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (credential_id) REFERENCES api_credentials(id) ON DELETE SET NULL
                );

                CREATE INDEX IF NOT EXISTS idx_saved_api_requests_category ON saved_api_requests(category);
                CREATE INDEX IF NOT EXISTS idx_saved_api_requests_updated_at ON saved_api_requests(updated_at);
                CREATE INDEX IF NOT EXISTS idx_saved_api_requests_name ON saved_api_requests(name);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (18, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 18: {}", e))?;

            info!("Successfully migrated to version 18 (saved_api_requests table added)");
        }

        // Migration to version 19: Add unified_workflows table for Workflow Builder
        if (1..19).contains(&current_version) {
            info!("Migrating database to version 19 (adding unified_workflows table)");

            conn.execute_batch(
                r#"
                -- Unified Workflows (Phase-based workflow builder)
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

                    -- Agentic configuration
                    max_iterations INTEGER DEFAULT 10,
                    provider TEXT,
                    model TEXT,

                    -- Timestamps
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_unified_workflows_category ON unified_workflows(category);
                CREATE INDEX IF NOT EXISTS idx_unified_workflows_updated_at ON unified_workflows(updated_at);
                CREATE INDEX IF NOT EXISTS idx_unified_workflows_name ON unified_workflows(name);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (19, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 19: {}", e))?;

            info!("Successfully migrated to version 19 (unified_workflows table added)");
        }

        // Migration to version 20: Ensure API infrastructure tables exist (fix for missing v16 migration)
        if (1..20).contains(&current_version) {
            info!("Migrating database to version 20 (ensuring API infrastructure tables exist)");

            conn.execute_batch(
                r#"
                -- Create api_credentials if it doesn't exist (was missing in some v18 migrations)
                CREATE TABLE IF NOT EXISTS api_credentials (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    credential_type TEXT NOT NULL,
                    storage_type TEXT NOT NULL DEFAULT 'secure',
                    token_endpoint TEXT,
                    client_id TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    expires_at TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_api_credentials_name ON api_credentials(name);
                CREATE INDEX IF NOT EXISTS idx_api_credentials_type ON api_credentials(credential_type);

                -- Create other API tables if they don't exist
                CREATE TABLE IF NOT EXISTS api_request_logs (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT,
                    step_id TEXT NOT NULL,
                    step_name TEXT,
                    method TEXT NOT NULL,
                    url TEXT NOT NULL,
                    resolved_url TEXT NOT NULL,
                    status_code INTEGER NOT NULL,
                    response_time_ms INTEGER NOT NULL,
                    response_body_type TEXT NOT NULL,
                    response_file_path TEXT,
                    response_size_bytes INTEGER,
                    success BOOLEAN NOT NULL,
                    assertion_failures INTEGER DEFAULT 0,
                    extractions_json TEXT,
                    assertions_json TEXT,
                    error TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_api_request_logs_task_run_id ON api_request_logs(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_api_request_logs_created_at ON api_request_logs(created_at);
                CREATE INDEX IF NOT EXISTS idx_api_request_logs_step_id ON api_request_logs(step_id);

                CREATE TABLE IF NOT EXISTS workflow_variables (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    variable_name TEXT NOT NULL,
                    variable_value TEXT NOT NULL,
                    source TEXT NOT NULL,
                    source_step_id TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    UNIQUE(task_run_id, variable_name)
                );

                CREATE INDEX IF NOT EXISTS idx_workflow_variables_task_run_id ON workflow_variables(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_workflow_variables_name ON workflow_variables(variable_name);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (20, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 20: {}", e))?;

            info!("Successfully migrated to version 20 (API infrastructure tables ensured)");
        }

        // Migration 21: Add completion_steps and skip_ai_summary to unified_workflows
        if current_version < 21 {
            conn.execute_batch(
                r#"
                -- Add completion_steps column for completion phase steps
                ALTER TABLE unified_workflows ADD COLUMN completion_steps TEXT DEFAULT '[]';

                -- Add skip_ai_summary column for controlling AI summary generation
                ALTER TABLE unified_workflows ADD COLUMN skip_ai_summary BOOLEAN NOT NULL DEFAULT 0;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (21, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 21: {}", e))?;

            info!("Successfully migrated to version 21 (completion_steps and skip_ai_summary columns added to unified_workflows)");
        }

        // Migration 22: Add hybrid logging tables (task_run_events, task_run_screenshots, task_run_playwright_results)
        if current_version < 22 {
            conn.execute_batch(
                r#"
                -- Task Run Events (unifies JSONL logs for historical queries)
                CREATE TABLE IF NOT EXISTS task_run_events (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_run_id TEXT NOT NULL,
                    event_type TEXT NOT NULL,
                    event_subtype TEXT,
                    message TEXT NOT NULL,
                    data TEXT,
                    workflow_name TEXT,
                    state_name TEXT,
                    action_id TEXT,
                    timestamp TEXT NOT NULL,
                    duration_ms INTEGER,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_task_run_events_task_run_id ON task_run_events(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_task_run_events_event_type ON task_run_events(event_type);
                CREATE INDEX IF NOT EXISTS idx_task_run_events_timestamp ON task_run_events(timestamp);

                -- Task Run Screenshots
                CREATE TABLE IF NOT EXISTS task_run_screenshots (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    event_id INTEGER,
                    file_path TEXT NOT NULL,
                    screenshot_type TEXT NOT NULL,
                    template_name TEXT,
                    confidence REAL,
                    match_location TEXT,
                    width INTEGER,
                    height INTEGER,
                    file_size_bytes INTEGER,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    FOREIGN KEY (event_id) REFERENCES task_run_events(id) ON DELETE SET NULL
                );

                CREATE INDEX IF NOT EXISTS idx_task_run_screenshots_task_run_id ON task_run_screenshots(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_task_run_screenshots_type ON task_run_screenshots(screenshot_type);

                -- Task Run Playwright Results
                CREATE TABLE IF NOT EXISTS task_run_playwright_results (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    test_name TEXT NOT NULL,
                    spec_file TEXT,
                    status TEXT NOT NULL,
                    duration_ms INTEGER,
                    stdout TEXT,
                    stderr TEXT,
                    console_output TEXT,
                    page_snapshot TEXT,
                    error_message TEXT,
                    failure_screenshot_path TEXT,
                    assertions_passed INTEGER DEFAULT 0,
                    assertions_failed INTEGER DEFAULT 0,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_task_run_playwright_task_run_id ON task_run_playwright_results(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_task_run_playwright_status ON task_run_playwright_results(status);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (22, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 22: {}", e))?;

            info!("Successfully migrated to version 22 (hybrid logging tables: task_run_events, task_run_screenshots, task_run_playwright_results)");
        }

        // Migration 23: Add task_knowledge_summaries
        if current_version < 23 {
            conn.execute_batch(
                r#"
                -- Task Knowledge Summaries (Memory Compression)
                CREATE TABLE IF NOT EXISTS task_knowledge_summaries (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    category TEXT NOT NULL,
                    summary TEXT NOT NULL,
                    covered_iterations TEXT NOT NULL,
                    item_count INTEGER NOT NULL,
                    original_tokens INTEGER,
                    compressed_tokens INTEGER,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_task_knowledge_summaries_task_run_id ON task_knowledge_summaries(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_task_knowledge_summaries_category ON task_knowledge_summaries(category);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (23, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 23: {}", e))?;

            info!("Successfully migrated to version 23 (task_knowledge_summaries)");
        }

        // Migration 24: Add task_run_api_requests table
        if current_version < 24 {
            conn.execute_batch(
                r#"
                -- Task Run API Requests (from runner-api-requests.jsonl)
                CREATE TABLE IF NOT EXISTS task_run_api_requests (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
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
                    response_time_ms INTEGER NOT NULL,
                    response_body_type TEXT NOT NULL,
                    response_body TEXT,
                    response_size_bytes INTEGER,
                    extractions TEXT,
                    assertions TEXT,
                    success BOOLEAN NOT NULL,
                    error_message TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_task_run_api_requests_task_run_id ON task_run_api_requests(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_task_run_api_requests_step_id ON task_run_api_requests(step_id);
                CREATE INDEX IF NOT EXISTS idx_task_run_api_requests_created_at ON task_run_api_requests(created_at);
                CREATE INDEX IF NOT EXISTS idx_task_run_api_requests_success ON task_run_api_requests(success);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (24, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 24: {}", e))?;

            info!("Successfully migrated to version 24 (task_run_api_requests table)");
        }

        // Migration 25: Add runtime_context_json to task_runs and task_hooks table
        if current_version < 25 {
            conn.execute_batch(
                r#"
                -- Task Hooks (Lifecycle hooks for execution events)
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

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (25, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 25: {}", e))?;

            // Add runtime_context_json column (ALTER TABLE must be separate)
            let _ = conn.execute(
                "ALTER TABLE task_runs ADD COLUMN runtime_context_json TEXT",
                [],
            );

            info!("Successfully migrated to version 25 (task_hooks table, runtime_context_json)");
        }

        // Migration to version 26: Add orchestrator tables (learning, checkpoints, flows)
        if (1..26).contains(&current_version) {
            info!("Migrating database to version 26 (orchestrator tables: learning, checkpoints, flows)");
            conn.execute_batch(
                r#"
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

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (26, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 26: {}", e))?;

            info!("Successfully migrated to version 26 (orchestrator tables)");
        }

        // Migration to version 27: Add code quality checks infrastructure
        if (1..27).contains(&current_version) {
            info!("Migrating database to version 27 (adding code quality checks infrastructure)");

            conn.execute_batch(
                r#"
                -- Code Quality Checks (check definitions stored in runner)
                CREATE TABLE IF NOT EXISTS checks (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,
                    check_type TEXT NOT NULL,
                    tool TEXT NOT NULL,
                    command TEXT,
                    working_directory TEXT,
                    config_path TEXT,
                    auto_fix BOOLEAN NOT NULL DEFAULT 0,
                    fail_on_warning BOOLEAN NOT NULL DEFAULT 0,
                    timeout_seconds INTEGER NOT NULL DEFAULT 60,
                    is_critical BOOLEAN NOT NULL DEFAULT 0,
                    enabled BOOLEAN NOT NULL DEFAULT 1,
                    ai_generated BOOLEAN NOT NULL DEFAULT 0,
                    ai_generation_prompt TEXT,
                    tags TEXT DEFAULT '[]',
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
                    status TEXT NOT NULL DEFAULT 'pending',
                    started_at TEXT,
                    completed_at TEXT,
                    duration_ms INTEGER,
                    output TEXT,
                    error_message TEXT,
                    issues_found INTEGER DEFAULT 0,
                    issues_fixed INTEGER DEFAULT 0,
                    files_checked INTEGER DEFAULT 0,
                    structured_output TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (check_id) REFERENCES checks(id) ON DELETE CASCADE,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_check_results_check_id ON check_results(check_id);
                CREATE INDEX IF NOT EXISTS idx_check_results_task_run_id ON check_results(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_check_results_status ON check_results(status);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (27, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 27: {}", e))?;

            info!("Successfully migrated to version 27 (code quality checks infrastructure)");
        }

        // Migration to version 28: Performance optimization - additional indexes
        // Also creates task_knowledge table if missing (was in schema.sql but not in migrations)
        if (1..28).contains(&current_version) {
            info!("Migrating database to version 28 (performance optimization indexes)");

            // First, ensure task_knowledge table exists (was missing from migrations)
            conn.execute_batch(
                r#"
                -- Task Knowledge table (was in schema.sql but missing from migrations)
                CREATE TABLE IF NOT EXISTS task_knowledge (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    category TEXT NOT NULL,
                    agent_type TEXT NOT NULL,
                    iteration INTEGER NOT NULL DEFAULT 1,
                    content TEXT NOT NULL,
                    evidence TEXT,
                    confidence TEXT DEFAULT 'medium',
                    related_files TEXT DEFAULT '[]',
                    related_criterion_id TEXT,
                    is_resolved BOOLEAN NOT NULL DEFAULT 0,
                    resolution_notes TEXT,
                    resolved_at TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_task_knowledge_task_run_id ON task_knowledge(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_task_knowledge_category ON task_knowledge(category);
                CREATE INDEX IF NOT EXISTS idx_task_knowledge_is_resolved ON task_knowledge(is_resolved);
                "#,
            )
            .map_err(|e| format!("Failed to create task_knowledge table: {}", e))?;

            // Create verification_plans table (was in schema.sql but missing from migrations)
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS verification_plans (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    version INTEGER NOT NULL DEFAULT 1,
                    plan_json TEXT NOT NULL,
                    goal_summary TEXT NOT NULL,
                    criteria_count INTEGER NOT NULL DEFAULT 0,
                    has_ai_criteria BOOLEAN NOT NULL DEFAULT 0,
                    replan_reason TEXT,
                    previous_version_id TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    FOREIGN KEY (previous_version_id) REFERENCES verification_plans(id) ON DELETE SET NULL
                );
                CREATE INDEX IF NOT EXISTS idx_verification_plans_task_run_id ON verification_plans(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_verification_plans_version ON verification_plans(version);
                "#,
            )
            .map_err(|e| format!("Failed to create verification_plans table: {}", e))?;

            // Create orchestrator_verification_results table (was in schema.sql but missing from migrations)
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS orchestrator_verification_results (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    plan_id TEXT NOT NULL,
                    iteration INTEGER NOT NULL,
                    criterion_id TEXT NOT NULL,
                    criterion_type TEXT NOT NULL,
                    passed BOOLEAN NOT NULL,
                    is_critical BOOLEAN NOT NULL DEFAULT 1,
                    confidence TEXT,
                    observations TEXT DEFAULT '[]',
                    issues TEXT DEFAULT '[]',
                    suggestions TEXT DEFAULT '[]',
                    raw_output TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    FOREIGN KEY (plan_id) REFERENCES verification_plans(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_orch_ver_results_task_run_id ON orchestrator_verification_results(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_orch_ver_results_plan_id ON orchestrator_verification_results(plan_id);
                CREATE INDEX IF NOT EXISTS idx_orch_ver_results_iteration ON orchestrator_verification_results(iteration);
                CREATE INDEX IF NOT EXISTS idx_orch_ver_results_passed ON orchestrator_verification_results(passed);
                "#,
            )
            .map_err(|e| format!("Failed to create orchestrator_verification_results table: {}", e))?;

            conn.execute_batch(
                r#"
                -- Additional indexes for learning_outcomes (frequently filtered columns)
                CREATE INDEX IF NOT EXISTS idx_learning_outcomes_strategy ON learning_outcomes(strategy);

                -- Additional indexes for learning_patterns (ORDER BY support)
                CREATE INDEX IF NOT EXISTS idx_learning_patterns_updated_at ON learning_patterns(updated_at);

                -- Additional indexes for orchestrator_checkpoints (filtering)
                CREATE INDEX IF NOT EXISTS idx_orchestrator_checkpoints_trigger ON orchestrator_checkpoints(trigger);
                CREATE INDEX IF NOT EXISTS idx_orchestrator_checkpoints_created_at ON orchestrator_checkpoints(created_at);

                -- Additional indexes for orchestrator_flows (ORDER BY support)
                CREATE INDEX IF NOT EXISTS idx_orchestrator_flows_created_at ON orchestrator_flows(created_at);
                CREATE INDEX IF NOT EXISTS idx_orchestrator_flows_updated_at ON orchestrator_flows(updated_at);

                -- Additional indexes for flow_executions (ORDER BY support)
                CREATE INDEX IF NOT EXISTS idx_flow_executions_started_at ON flow_executions(started_at);

                -- Additional composite indexes for task_knowledge (iteration queries)
                CREATE INDEX IF NOT EXISTS idx_task_knowledge_task_run_iteration ON task_knowledge(task_run_id, iteration);
                CREATE INDEX IF NOT EXISTS idx_task_knowledge_iteration ON task_knowledge(iteration);

                -- Additional indexes for orchestrator_verification_results (criterion queries)
                CREATE INDEX IF NOT EXISTS idx_orch_ver_results_criterion_id ON orchestrator_verification_results(criterion_id);

                -- Additional indexes for task_run_events (event filtering)
                CREATE INDEX IF NOT EXISTS idx_task_run_events_subtype ON task_run_events(event_subtype);
                CREATE INDEX IF NOT EXISTS idx_task_run_events_workflow ON task_run_events(workflow_name);

                -- Additional indexes for checks (ordering)
                CREATE INDEX IF NOT EXISTS idx_checks_created_at ON checks(created_at);
                CREATE INDEX IF NOT EXISTS idx_checks_updated_at ON checks(updated_at);

                -- Additional indexes for check_results (created_at for ordering)
                CREATE INDEX IF NOT EXISTS idx_check_results_created_at ON check_results(created_at);

                -- Additional indexes for sessions (completed status filtering)
                CREATE INDEX IF NOT EXISTS idx_sessions_completed ON sessions(completed);
                CREATE INDEX IF NOT EXISTS idx_sessions_session_type ON sessions(session_type);

                -- Additional indexes for scheduler_history (status filtering)
                CREATE INDEX IF NOT EXISTS idx_scheduler_history_status ON scheduler_history(status);

                -- Additional indexes for task_runs (workflow_name filtering)
                CREATE INDEX IF NOT EXISTS idx_task_runs_workflow_name ON task_runs(workflow_name);
                CREATE INDEX IF NOT EXISTS idx_task_runs_updated_at ON task_runs(updated_at);

                -- Additional indexes for verification_plans (created_at ordering)
                CREATE INDEX IF NOT EXISTS idx_verification_plans_created_at ON verification_plans(created_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (28, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 28: {}", e))?;

            info!("Successfully migrated to version 28 (performance optimization indexes)");
        }

        // Migration to version 29: Add shell_commands and shell_command_results tables
        if (1..29).contains(&current_version) {
            info!("Migrating database to version 29 (shell command library)");

            conn.execute_batch(
                r#"
                -- Shell commands table
                CREATE TABLE IF NOT EXISTS shell_commands (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,
                    command TEXT NOT NULL,
                    working_directory TEXT,
                    timeout_seconds INTEGER DEFAULT 30,
                    fail_on_error INTEGER DEFAULT 1,
                    category TEXT,
                    tags TEXT DEFAULT '[]',
                    enabled INTEGER DEFAULT 1,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_shell_commands_category ON shell_commands(category);
                CREATE INDEX IF NOT EXISTS idx_shell_commands_enabled ON shell_commands(enabled);
                CREATE INDEX IF NOT EXISTS idx_shell_commands_name ON shell_commands(name);
                CREATE INDEX IF NOT EXISTS idx_shell_commands_created_at ON shell_commands(created_at);
                CREATE INDEX IF NOT EXISTS idx_shell_commands_updated_at ON shell_commands(updated_at);

                -- Shell command execution results
                CREATE TABLE IF NOT EXISTS shell_command_results (
                    id TEXT PRIMARY KEY,
                    shell_command_id TEXT NOT NULL,
                    task_run_id TEXT,
                    status TEXT NOT NULL DEFAULT 'pending',
                    exit_code INTEGER,
                    stdout TEXT,
                    stderr TEXT,
                    duration_ms INTEGER,
                    started_at TEXT,
                    completed_at TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (shell_command_id) REFERENCES shell_commands(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_shell_command_results_shell_command_id ON shell_command_results(shell_command_id);
                CREATE INDEX IF NOT EXISTS idx_shell_command_results_task_run_id ON shell_command_results(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_shell_command_results_status ON shell_command_results(status);
                CREATE INDEX IF NOT EXISTS idx_shell_command_results_created_at ON shell_command_results(created_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (29, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 29: {}", e))?;

            info!("Successfully migrated to version 29 (shell command library)");
        }

        // Migration to version 30: Add mobile development feedback tables
        if (1..30).contains(&current_version) {
            info!("Migrating database to version 30 (mobile development feedback)");

            conn.execute_batch(
                r#"
                -- Mobile state captures
                CREATE TABLE IF NOT EXISTS task_run_mobile_state (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_run_id TEXT NOT NULL,
                    timestamp TEXT NOT NULL,
                    device_id TEXT,
                    device_type TEXT,
                    device_model TEXT,
                    app_package TEXT,
                    app_activity TEXT,
                    app_state TEXT,
                    metro_connected INTEGER DEFAULT 0,
                    bundle_status TEXT,
                    last_reload_type TEXT,
                    last_reload_time TEXT,
                    screenshot_path TEXT,
                    logcat_path TEXT,
                    has_errors INTEGER DEFAULT 0,
                    error_summary TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_mobile_state_task_run_id ON task_run_mobile_state(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_mobile_state_timestamp ON task_run_mobile_state(timestamp);
                CREATE INDEX IF NOT EXISTS idx_mobile_state_device_id ON task_run_mobile_state(device_id);
                CREATE INDEX IF NOT EXISTS idx_mobile_state_has_errors ON task_run_mobile_state(has_errors);

                -- Mobile log entries
                CREATE TABLE IF NOT EXISTS task_run_mobile_logs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_run_id TEXT NOT NULL,
                    mobile_state_id INTEGER,
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
                    timestamp TEXT NOT NULL,
                    device_timestamp TEXT,
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

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (30, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 30: {}", e))?;

            info!("Successfully migrated to version 30 (mobile development feedback)");
        }

        // Version 31: Add log_source_selection to unified_workflows
        if current_version < 31 {
            info!("Migrating database to version 31 (adding log_source_selection to unified_workflows)");

            conn.execute_batch(
                r#"
                -- Add log_source_selection column for per-workflow log source selection
                -- Values: "default", "ai", "all", or JSON object like {"profile_id": "..."}
                ALTER TABLE unified_workflows ADD COLUMN log_source_selection TEXT DEFAULT '"default"';

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (31, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 31: {}", e))?;

            info!("Successfully migrated to version 31 (log_source_selection)");
        }

        // Version 32: Add context management fields to unified_workflows
        if current_version < 32 {
            info!(
                "Migrating database to version 32 (adding context management to unified_workflows)"
            );

            conn.execute_batch(
                r#"
                -- Add context_ids column (JSON array of manually added context IDs)
                ALTER TABLE unified_workflows ADD COLUMN context_ids TEXT DEFAULT '[]';

                -- Add disabled_context_ids column (JSON array of disabled context IDs)
                ALTER TABLE unified_workflows ADD COLUMN disabled_context_ids TEXT DEFAULT '[]';

                -- Add auto_include_contexts column (whether to auto-include contexts, default true)
                ALTER TABLE unified_workflows ADD COLUMN auto_include_contexts INTEGER DEFAULT 1;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (32, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 32: {}", e))?;

            info!("Successfully migrated to version 32 (context management for unified_workflows)");
        }

        // Version 33: Add prompt_template to unified_workflows
        if current_version < 33 {
            info!("Migrating database to version 33 (adding prompt_template to unified_workflows)");

            conn.execute_batch(
                r#"
                -- Add prompt_template column (custom developer prompt template per workflow)
                ALTER TABLE unified_workflows ADD COLUMN prompt_template TEXT DEFAULT NULL;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (33, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 33: {}", e))?;

            info!("Successfully migrated to version 33 (prompt_template for unified_workflows)");
        }

        // Version 34: Add transition_history_json to task_runs for stage-based recap
        if current_version < 34 {
            info!("Migrating database to version 34 (adding transition_history_json to task_runs)");

            conn.execute_batch(
                r#"
                -- Add transition_history_json column for orchestrator state transition history
                ALTER TABLE task_runs ADD COLUMN transition_history_json TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (34, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 34: {}", e))?;

            info!("Successfully migrated to version 34 (transition_history_json for task_runs)");
        }

        // Version 35: Add check_groups tables for organizing checks
        if current_version < 35 {
            info!("Migrating database to version 35 (adding check_groups infrastructure)");

            conn.execute_batch(
                r#"
                -- Check Groups (organize checks into reusable groups)
                CREATE TABLE IF NOT EXISTS check_groups (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,
                    color TEXT,
                    enabled BOOLEAN NOT NULL DEFAULT 1,
                    run_in_parallel BOOLEAN NOT NULL DEFAULT 0,
                    stop_on_failure BOOLEAN NOT NULL DEFAULT 1,
                    tags TEXT DEFAULT '[]',
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

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (35, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 35: {}", e))?;

            info!("Successfully migrated to version 35 (check_groups infrastructure)");
        }

        // Migration to version 36: Add workflow_verification_phase_results table
        // Stores step-executor-based verification results from unified workflow execution
        if (1..36).contains(&current_version) {
            info!(
                "Migrating database to version 36 (adding workflow_verification_phase_results table)"
            );
            conn.execute_batch(
                r#"
                -- Workflow Verification Phase Results
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

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (36, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 36: {}", e))?;

            info!(
                "Successfully migrated to version 36 (workflow_verification_phase_results table)"
            );
        }

        // Migration to version 37: Add unique constraint on (task_run_id, iteration) for verification results
        if current_version < 37 {
            info!(
                "Migrating database to version 37 (unique constraint for verification phase results)"
            );

            // First, remove any duplicate rows keeping only the latest per (task_run_id, iteration)
            conn.execute_batch(
                r#"
                -- Delete duplicates, keeping only the row with the latest created_at for each (task_run_id, iteration)
                DELETE FROM workflow_verification_phase_results
                WHERE id NOT IN (
                    SELECT id FROM (
                        SELECT id, ROW_NUMBER() OVER (
                            PARTITION BY task_run_id, iteration
                            ORDER BY created_at DESC
                        ) as rn
                        FROM workflow_verification_phase_results
                    ) WHERE rn = 1
                );

                -- Now add the unique constraint
                CREATE UNIQUE INDEX IF NOT EXISTS idx_wf_ver_phase_unique
                ON workflow_verification_phase_results(task_run_id, iteration);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (37, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 37: {}", e))?;

            info!("Successfully migrated to version 37 (unique constraint for verification phase results)");
        }

        // Migration to version 38: Add workflow_type column to task_runs
        // This enables external code to check if a task is a "unified" workflow
        // before modifying its status. Unified workflows should only have status
        // modified by the LoopController, not by TaskMonitor or legacy session code.
        if current_version < 38 {
            info!("Migrating database to version 38 (adding workflow_type to task_runs)");

            conn.execute_batch(
                r#"
                -- Add workflow_type column: 'unified', 'legacy_session', 'automation_only', or NULL (legacy)
                -- Unified workflows should only have status modified by LoopController
                ALTER TABLE task_runs ADD COLUMN workflow_type TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (38, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 38: {}", e))?;

            info!("Successfully migrated to version 38 (workflow_type column added)");
        }

        // Migration to version 39: Add task hierarchy fields to task_runs
        // Enables nested subtasks with parent/root/depth tracking for complex workflows
        if current_version < 39 {
            info!("Migrating database to version 39 (adding task hierarchy fields to task_runs)");

            conn.execute_batch(
                r#"
                -- Add parent_task_run_id column: links subtasks to their parent task
                ALTER TABLE task_runs ADD COLUMN parent_task_run_id TEXT REFERENCES task_runs(id) ON DELETE SET NULL;

                -- Add root_task_run_id column: links to the root of the task hierarchy (same as id for root tasks)
                ALTER TABLE task_runs ADD COLUMN root_task_run_id TEXT;

                -- Add depth column: nesting depth (0 = root/top-level)
                ALTER TABLE task_runs ADD COLUMN depth INTEGER DEFAULT 0;

                -- Create indexes for querying child/subtasks
                CREATE INDEX IF NOT EXISTS idx_task_runs_parent_task_run_id ON task_runs(parent_task_run_id);
                CREATE INDEX IF NOT EXISTS idx_task_runs_root_task_run_id ON task_runs(root_task_run_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (39, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 39: {}", e))?;

            info!("Successfully migrated to version 39 (task hierarchy fields added)");
        }

        // Migration to version 40: Add workspace_id and triggered_by to task_runs
        // Enables integration with qontinui-web backend for multi-tenant workspaces
        if current_version < 40 {
            info!(
                "Migrating database to version 40 (adding workspace_id and triggered_by to task_runs)"
            );

            conn.execute_batch(
                r#"
                -- Add workspace_id column: links task to a workspace/organization from qontinui-web
                ALTER TABLE task_runs ADD COLUMN workspace_id TEXT;

                -- Add triggered_by column: identifies who/what triggered the task run
                ALTER TABLE task_runs ADD COLUMN triggered_by TEXT;

                -- Create index for workspace queries
                CREATE INDEX IF NOT EXISTS idx_task_runs_workspace_id ON task_runs(workspace_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (40, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 40: {}", e))?;

            info!("Successfully migrated to version 40 (workspace_id and triggered_by added)");
        }

        // Migration to version 41: Add log_watch_enabled to unified_workflows
        // Enables automatic log error detection during verification phase
        if current_version < 41 {
            info!(
                "Migrating database to version 41 (adding log_watch_enabled to unified_workflows)"
            );

            conn.execute_batch(
                r#"
                -- Add log_watch_enabled column: enables automatic log error detection
                -- Default to 1 (enabled) for all existing and new workflows
                ALTER TABLE unified_workflows ADD COLUMN log_watch_enabled INTEGER DEFAULT 1;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (41, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 41: {}", e))?;

            info!("Successfully migrated to version 41 (log_watch_enabled added)");
        }

        // Migration to version 42: Add health_check_enabled to unified_workflows
        // Enables automatic server health checks (backend/frontend) before verification
        if current_version < 42 {
            info!(
                "Migrating database to version 42 (adding health_check_enabled to unified_workflows)"
            );

            conn.execute_batch(
                r#"
                -- Add health_check_enabled column: enables automatic server health checks
                -- Default to 1 (enabled) for all existing and new workflows
                ALTER TABLE unified_workflows ADD COLUMN health_check_enabled INTEGER DEFAULT 1;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (42, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 42: {}", e))?;

            info!("Successfully migrated to version 42 (health_check_enabled added)");
        }

        // Migration to version 43: Add health_check_urls to unified_workflows
        // User-configurable health check URLs (replaces hardcoded backend/frontend checks)
        if current_version < 43 {
            info!(
                "Migrating database to version 43 (adding health_check_urls to unified_workflows)"
            );

            conn.execute_batch(
                r#"
                -- Add health_check_urls column: JSON array of health check configurations
                -- Each entry: { name, url, expected_status, timeout_seconds, is_critical }
                -- Default to empty array (no health checks configured)
                ALTER TABLE unified_workflows ADD COLUMN health_check_urls TEXT DEFAULT '[]';

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (43, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 43: {}", e))?;

            info!("Successfully migrated to version 43 (health_check_urls added)");
        }

        // Migration to version 44: Add error monitoring system
        // Captures errors from user-configured application logs for debug agent integration
        if current_version < 44 {
            info!("Migrating database to version 44 (adding error monitoring system)");

            conn.execute_batch(
                r#"
                -- Log Sources: User-configured application log files to monitor
                CREATE TABLE IF NOT EXISTS log_sources (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
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
                    enabled INTEGER DEFAULT 1,
                    poll_interval_ms INTEGER DEFAULT 5000,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                );

                CREATE INDEX IF NOT EXISTS idx_log_sources_name ON log_sources(name);
                CREATE INDEX IF NOT EXISTS idx_log_sources_enabled ON log_sources(enabled);

                -- Error Events: Captured errors from application logs
                CREATE TABLE IF NOT EXISTS error_events (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    log_source_id INTEGER REFERENCES log_sources(id) ON DELETE SET NULL,
                    log_source_name TEXT NOT NULL,
                    task_run_id TEXT REFERENCES task_runs(id) ON DELETE SET NULL,
                    workflow_step_id TEXT,
                    log_timestamp TEXT,
                    captured_at TEXT NOT NULL DEFAULT (datetime('now')),
                    severity TEXT NOT NULL DEFAULT 'error',
                    error_type TEXT,
                    error_code TEXT,
                    message TEXT NOT NULL,
                    stack_trace TEXT,
                    context_lines TEXT,
                    raw_entry TEXT,
                    file_path TEXT,
                    line_number INTEGER,
                    column_number INTEGER,
                    function_name TEXT,
                    signature_hash TEXT NOT NULL,
                    occurrence_count INTEGER DEFAULT 1,
                    first_seen_at TEXT NOT NULL DEFAULT (datetime('now')),
                    last_seen_at TEXT NOT NULL DEFAULT (datetime('now')),
                    status TEXT DEFAULT 'new',
                    finding_id INTEGER REFERENCES task_run_findings(id) ON DELETE SET NULL,
                    resolved_by_task_run_id TEXT,
                    resolution_notes TEXT,
                    acknowledged_at TEXT,
                    resolved_at TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_error_events_log_source ON error_events(log_source_id);
                CREATE INDEX IF NOT EXISTS idx_error_events_task_run ON error_events(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_error_events_signature ON error_events(signature_hash);
                CREATE INDEX IF NOT EXISTS idx_error_events_status ON error_events(status);
                CREATE INDEX IF NOT EXISTS idx_error_events_severity ON error_events(severity);
                CREATE INDEX IF NOT EXISTS idx_error_events_captured ON error_events(captured_at DESC);
                CREATE INDEX IF NOT EXISTS idx_error_events_last_seen ON error_events(last_seen_at DESC);
                CREATE INDEX IF NOT EXISTS idx_error_events_source_name ON error_events(log_source_name);

                -- Full-text search for error messages
                CREATE VIRTUAL TABLE IF NOT EXISTS error_events_fts USING fts5(
                    message,
                    stack_trace,
                    error_type,
                    content='error_events',
                    content_rowid='id'
                );

                -- FTS sync triggers
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

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (44, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 44: {}", e))?;

            info!("Successfully migrated to version 44 (error monitoring system added)");
        }

        // Version 45: Add bridge_id to task_runs for multi-bridge support
        if current_version < 45 {
            let _ = conn.execute("ALTER TABLE task_runs ADD COLUMN bridge_id TEXT", []);

            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_task_runs_bridge_id ON task_runs(bridge_id)",
                [],
            )
            .map_err(|e| format!("Failed to create bridge_id index: {}", e))?;

            conn.execute(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (45, datetime('now'))",
                [],
            )
            .map_err(|e| format!("Failed to migrate to version 45: {}", e))?;

            info!("Successfully migrated to version 45 (bridge_id added to task_runs)");
        }

        // Migration to version 46: Add flow_versions table for version history
        if current_version < 46 {
            info!("Migrating database to version 46 (adding flow_versions table)");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS flow_versions (
                    id TEXT PRIMARY KEY,
                    flow_id TEXT NOT NULL,
                    version INTEGER NOT NULL,
                    definition TEXT NOT NULL,
                    message TEXT,
                    created_by TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (flow_id) REFERENCES orchestrator_flows(id) ON DELETE CASCADE,
                    UNIQUE(flow_id, version)
                );
                CREATE INDEX IF NOT EXISTS idx_flow_versions_flow_id ON flow_versions(flow_id);
                CREATE INDEX IF NOT EXISTS idx_flow_versions_flow_version ON flow_versions(flow_id, version);
                "#,
            )
            .map_err(|e| format!("Failed to create flow_versions table: {}", e))?;

            conn.execute(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (46, datetime('now'))",
                [],
            )
            .map_err(|e| format!("Failed to migrate to version 46: {}", e))?;

            info!("Successfully migrated to version 46 (flow_versions table added)");
        }

        // Migration to version 47: Add timeout_seconds to unified_workflows
        if (1..47).contains(&current_version) {
            info!("Migrating database to version 47 (add timeout_seconds to unified_workflows)");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN timeout_seconds INTEGER DEFAULT NULL;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (47, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 47: {}", e))?;

            info!(
                "Successfully migrated to version 47 (timeout_seconds added to unified_workflows)"
            );
        }

        // Migration to version 48: Add workflow state management tables
        if (1..48).contains(&current_version) {
            info!("Migrating database to version 48 (workflow state management tables)");

            conn.execute_batch(
                r#"
                -- Workflow Execution State: Explicit state tracking for workflows
                CREATE TABLE IF NOT EXISTS workflow_execution_state (
                    execution_id TEXT PRIMARY KEY,
                    workflow_type TEXT NOT NULL,
                    state_name TEXT NOT NULL,
                    state_data TEXT,
                    phase TEXT,
                    iteration INTEGER,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (execution_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_workflow_exec_state_type ON workflow_execution_state(workflow_type);
                CREATE INDEX IF NOT EXISTS idx_workflow_exec_state_name ON workflow_execution_state(state_name);

                -- Workflow Step Checkpoints: Step-level checkpointing for resume
                CREATE TABLE IF NOT EXISTS workflow_step_checkpoints (
                    id TEXT PRIMARY KEY,
                    execution_id TEXT NOT NULL,
                    workflow_type TEXT NOT NULL,
                    phase TEXT NOT NULL,
                    iteration INTEGER,
                    step_index INTEGER NOT NULL,
                    step_type TEXT NOT NULL,
                    step_name TEXT,
                    status TEXT NOT NULL,
                    result_json TEXT,
                    started_at TEXT,
                    completed_at TEXT,
                    duration_ms INTEGER,
                    error TEXT,
                    FOREIGN KEY (execution_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    UNIQUE(execution_id, phase, iteration, step_index)
                );

                CREATE INDEX IF NOT EXISTS idx_step_checkpoints_execution ON workflow_step_checkpoints(execution_id);
                CREATE INDEX IF NOT EXISTS idx_step_checkpoints_lookup ON workflow_step_checkpoints(execution_id, phase, iteration);
                CREATE INDEX IF NOT EXISTS idx_step_checkpoints_status ON workflow_step_checkpoints(status);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (48, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 48: {}", e))?;

            info!("Successfully migrated to version 48 (workflow state management tables)");
        }

        // Migration to version 49: Add step_config_json column to workflow_step_checkpoints
        // This provides a single source of truth for step configuration instead of
        // duplicating data between checkpoints and task_runs.execution_steps_json
        if (1..49).contains(&current_version) {
            info!("Migrating database to version 49 (add step_config_json and cursor index)");

            conn.execute_batch(
                r#"
                ALTER TABLE workflow_step_checkpoints ADD COLUMN step_config_json TEXT;

                -- Add index for cursor-based pagination
                CREATE INDEX IF NOT EXISTS idx_step_checkpoints_cursor ON workflow_step_checkpoints(execution_id, step_index);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (49, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 49: {}", e))?;

            info!("Successfully migrated to version 49 (step_config_json column added)");
        }

        // Migration to version 50: Add step_progress_markers table for intra-step progress tracking
        // This enables tracking progress within long AI operations (e.g., "analyzed 50/100 files")
        if (1..50).contains(&current_version) {
            info!("Migrating database to version 50 (add step_progress_markers table)");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS step_progress_markers (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    checkpoint_id TEXT NOT NULL,
                    marker_type TEXT NOT NULL,
                    current_value INTEGER NOT NULL,
                    total_value INTEGER,
                    description TEXT,
                    data_json TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (checkpoint_id) REFERENCES workflow_step_checkpoints(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_progress_markers_checkpoint ON step_progress_markers(checkpoint_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (50, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 50: {}", e))?;

            info!("Successfully migrated to version 50 (step_progress_markers table added)");
        }

        // Migration to version 51: Change timeout_seconds from NOT NULL to nullable
        // This disables default timeouts - None means no timeout (user must explicitly set one)
        // Fixes: "The AI step reports that multiple checks timed out after 60 seconds"
        //
        // SQLite doesn't support ALTER COLUMN to remove NOT NULL, so we use the
        // rename-recreate pattern: create new table, copy data, drop old, rename new.
        if (1..51).contains(&current_version) {
            info!("Migrating database to version 51 (disable default timeouts)");

            // Step 1: Recreate 'checks' table without NOT NULL on timeout_seconds
            conn.execute_batch(
                r#"
                -- Create new checks table with nullable timeout_seconds
                CREATE TABLE IF NOT EXISTS checks_new (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,
                    check_type TEXT NOT NULL,
                    tool TEXT NOT NULL,
                    command TEXT,
                    working_directory TEXT,
                    config_path TEXT,
                    auto_fix BOOLEAN NOT NULL DEFAULT 0,
                    fail_on_warning BOOLEAN NOT NULL DEFAULT 0,
                    timeout_seconds INTEGER,  -- Now nullable (NULL = no timeout)
                    is_critical BOOLEAN NOT NULL DEFAULT 0,
                    enabled BOOLEAN NOT NULL DEFAULT 1,
                    ai_generated BOOLEAN NOT NULL DEFAULT 0,
                    ai_generation_prompt TEXT,
                    tags TEXT DEFAULT '[]',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                -- Copy data, converting 60 to NULL
                INSERT INTO checks_new SELECT
                    id, name, description, check_type, tool, command,
                    working_directory, config_path, auto_fix, fail_on_warning,
                    CASE WHEN timeout_seconds = 60 THEN NULL ELSE timeout_seconds END,
                    is_critical, enabled, ai_generated, ai_generation_prompt,
                    tags, created_at, updated_at
                FROM checks;

                -- Drop old table and rename new
                DROP TABLE checks;
                ALTER TABLE checks_new RENAME TO checks;

                -- Recreate indexes
                CREATE INDEX IF NOT EXISTS idx_checks_check_type ON checks(check_type);
                CREATE INDEX IF NOT EXISTS idx_checks_tool ON checks(tool);
                CREATE INDEX IF NOT EXISTS idx_checks_enabled ON checks(enabled);
                CREATE INDEX IF NOT EXISTS idx_checks_created_at ON checks(created_at);
                CREATE INDEX IF NOT EXISTS idx_checks_updated_at ON checks(updated_at);
                "#,
            )
            .map_err(|e| format!("Failed to migrate checks table to version 51: {}", e))?;

            // Step 2: Recreate 'verification_tests' table without NOT NULL on timeout_seconds
            conn.execute_batch(
                r#"
                -- Create new verification_tests table with nullable timeout_seconds
                CREATE TABLE IF NOT EXISTS verification_tests_new (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,
                    test_type TEXT NOT NULL,
                    category TEXT,
                    playwright_code TEXT,
                    vision_config TEXT,
                    python_code TEXT,
                    repo_test_config TEXT,
                    success_criteria TEXT,
                    config TEXT DEFAULT '{}',
                    timeout_seconds INTEGER,  -- Now nullable (NULL = no timeout)
                    is_critical BOOLEAN NOT NULL DEFAULT 1,
                    enabled BOOLEAN NOT NULL DEFAULT 1,
                    ai_generated BOOLEAN NOT NULL DEFAULT 0,
                    ai_generation_prompt TEXT,
                    tags TEXT DEFAULT '[]',
                    source_file TEXT,
                    last_exported_at TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                -- Copy data, converting 60 to NULL
                INSERT INTO verification_tests_new SELECT
                    id, name, description, test_type, category,
                    playwright_code, vision_config, python_code, repo_test_config,
                    success_criteria, config,
                    CASE WHEN timeout_seconds = 60 THEN NULL ELSE timeout_seconds END,
                    is_critical, enabled, ai_generated, ai_generation_prompt,
                    tags, source_file, last_exported_at, created_at, updated_at
                FROM verification_tests;

                -- Drop old table and rename new
                DROP TABLE verification_tests;
                ALTER TABLE verification_tests_new RENAME TO verification_tests;

                -- Recreate indexes
                CREATE INDEX IF NOT EXISTS idx_verification_tests_test_type ON verification_tests(test_type);
                CREATE INDEX IF NOT EXISTS idx_verification_tests_category ON verification_tests(category);
                CREATE INDEX IF NOT EXISTS idx_verification_tests_enabled ON verification_tests(enabled);
                CREATE INDEX IF NOT EXISTS idx_verification_tests_created_at ON verification_tests(created_at);
                CREATE INDEX IF NOT EXISTS idx_verification_tests_updated_at ON verification_tests(updated_at);
                "#,
            )
            .map_err(|e| format!("Failed to migrate verification_tests table to version 51: {}", e))?;

            // Step 3: Update shell_commands (already allows NULL, just update values)
            conn.execute_batch(
                r#"
                UPDATE shell_commands SET timeout_seconds = NULL WHERE timeout_seconds = 30;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (51, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate shell_commands to version 51: {}", e))?;

            info!("Successfully migrated to version 51 (default timeouts disabled)");
        }

        // Migration to version 52: Add execution_spans table for tracing
        if current_version < 52 {
            info!("Migrating to version 52 (execution spans table)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS execution_spans (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    execution_id TEXT,
                    trace_id TEXT NOT NULL,
                    span_id TEXT NOT NULL,
                    parent_span_id TEXT,
                    name TEXT NOT NULL,
                    start_ts TEXT NOT NULL,
                    end_ts TEXT,
                    duration_ms INTEGER,
                    attributes TEXT,
                    success INTEGER DEFAULT 1,
                    error TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (execution_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_spans_execution ON execution_spans(execution_id);
                CREATE INDEX IF NOT EXISTS idx_spans_trace ON execution_spans(trace_id);
                CREATE INDEX IF NOT EXISTS idx_spans_name ON execution_spans(name);
                CREATE INDEX IF NOT EXISTS idx_spans_start ON execution_spans(start_ts);
                CREATE INDEX IF NOT EXISTS idx_spans_duration ON execution_spans(duration_ms);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (52, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to create execution_spans table (version 52): {}", e))?;

            info!("Successfully migrated to version 52 (execution spans table)");
        }

        // Migration to version 53: Add preflight_check_enabled to unified_workflows
        if current_version < 53 {
            info!("Migrating database to version 53 (adding preflight_check_enabled to unified_workflows)");

            // Check if column already exists (idempotent migration)
            let column_exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('unified_workflows') WHERE name = 'preflight_check_enabled'",
                    [],
                    |row| row.get::<_, i32>(0),
                )
                .map(|count| count > 0)
                .unwrap_or(false);

            if !column_exists {
                conn.execute(
                    "ALTER TABLE unified_workflows ADD COLUMN preflight_check_enabled INTEGER DEFAULT 1",
                    [],
                )
                .map_err(|e| format!("Failed to add preflight_check_enabled column: {}", e))?;
                info!("Added preflight_check_enabled column to unified_workflows");
            } else {
                info!("Column preflight_check_enabled already exists, skipping ALTER TABLE");
            }

            conn.execute(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (53, datetime('now'))",
                [],
            )
            .map_err(|e| format!("Failed to update schema version to 53: {}", e))?;

            info!("Successfully migrated to version 53 (preflight_check_enabled)");
        }

        // Migration to version 54: Add result_data to task_runs, generated_by_task_run_id to unified_workflows
        if current_version < 54 {
            info!("Migrating database to version 54 (result_data, generated_by_task_run_id)");
            conn.execute_batch(
                r#"
                -- Structured result data for task runs (JSON blob for meta-workflow outputs)
                ALTER TABLE task_runs ADD COLUMN result_data TEXT;

                -- Links generated workflows back to the meta-workflow task run that created them
                ALTER TABLE unified_workflows ADD COLUMN generated_by_task_run_id TEXT REFERENCES task_runs(id) ON DELETE SET NULL;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (54, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 54: {}", e))?;

            info!(
                "Successfully migrated to version 54 (result_data, generated_by_task_run_id added)"
            );
        }

        // Migration to version 55: Add embedding BLOB columns for hybrid RAG search + feedback table
        if current_version < 55 {
            info!("Migrating database to version 55 (embedding columns + workflow_generation_feedback)");
            conn.execute_batch(
                r#"
                -- Embedding BLOB columns for hybrid RAG search (384-dim MiniLM, 1536 bytes each)
                ALTER TABLE task_runs ADD COLUMN prompt_embedding BLOB;
                ALTER TABLE task_runs ADD COLUMN summary_embedding BLOB;

                ALTER TABLE task_run_findings ADD COLUMN title_embedding BLOB;
                ALTER TABLE task_run_findings ADD COLUMN description_embedding BLOB;

                ALTER TABLE task_knowledge ADD COLUMN content_embedding BLOB;

                ALTER TABLE unified_workflows ADD COLUMN description_embedding BLOB;

                ALTER TABLE learning_outcomes ADD COLUMN context_embedding BLOB;

                ALTER TABLE learning_patterns ADD COLUMN description_embedding BLOB;

                ALTER TABLE error_events ADD COLUMN message_embedding BLOB;

                -- Workflow generation feedback table
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
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    FOREIGN KEY (workflow_id) REFERENCES unified_workflows(id) ON DELETE CASCADE,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE SET NULL
                );
                CREATE INDEX IF NOT EXISTS idx_wgf_workflow_id ON workflow_generation_feedback(workflow_id);
                CREATE INDEX IF NOT EXISTS idx_wgf_task_run_id ON workflow_generation_feedback(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_wgf_feedback_type ON workflow_generation_feedback(feedback_type);
                CREATE INDEX IF NOT EXISTS idx_wgf_created_at ON workflow_generation_feedback(created_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (55, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 55: {}", e))?;

            info!("Successfully migrated to version 55 (embedding columns + workflow_generation_feedback)");
        }

        // Version 56: Add sync_pending column to unified_workflows for offline cache sync
        if current_version < 56 {
            info!("Migrating to version 56 (unified_workflows sync_pending)...");
            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN sync_pending INTEGER DEFAULT 0;
                CREATE INDEX IF NOT EXISTS idx_unified_workflows_sync_pending ON unified_workflows(sync_pending);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (56, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 56: {}", e))?;

            info!("Successfully migrated to version 56 (unified_workflows sync_pending)");
        }

        // Migration to version 57: Add example_status column to unified_workflows
        // Tracks whether a workflow is in the example library for RAG-based generation
        if current_version < 57 {
            info!("Migrating database to version 57 (add example_status to unified_workflows)");
            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN example_status TEXT DEFAULT 'pending';
                CREATE INDEX IF NOT EXISTS idx_unified_workflows_example_status ON unified_workflows(example_status);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (57, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 57: {}", e))?;

            info!("Successfully migrated to version 57 (example_status column added)");
        }

        // Migration to version 58: Reflection workflow system
        // Adds reflection_fixes table, is_reflection/reflection_source_task_run_id on task_runs,
        // and reflection_fix_id on task_run_findings and task_knowledge.
        if current_version < 58 {
            info!("Migrating database to version 58 (reflection workflow system)");
            conn.execute_batch(
                r#"
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
                    status TEXT NOT NULL DEFAULT 'applied',
                    effectiveness TEXT,
                    effectiveness_evidence TEXT,
                    applied_at TEXT NOT NULL,
                    evaluated_at TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (source_task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    FOREIGN KEY (reflection_task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    FOREIGN KEY (source_finding_id) REFERENCES task_run_findings(id) ON DELETE SET NULL,
                    FOREIGN KEY (source_knowledge_id) REFERENCES task_knowledge(id) ON DELETE SET NULL
                );

                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_source ON reflection_fixes(source_task_run_id);
                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_reflection ON reflection_fixes(reflection_task_run_id);
                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_status ON reflection_fixes(status);
                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_effectiveness ON reflection_fixes(effectiveness);
                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_applied_at ON reflection_fixes(applied_at);

                ALTER TABLE task_runs ADD COLUMN is_reflection INTEGER DEFAULT 0;
                ALTER TABLE task_runs ADD COLUMN reflection_source_task_run_id TEXT;

                ALTER TABLE task_run_findings ADD COLUMN reflection_fix_id TEXT;
                ALTER TABLE task_knowledge ADD COLUMN reflection_fix_id TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (58, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 58: {}", e))?;

            info!("Successfully migrated to version 58 (reflection workflow system)");
        }

        // Version 59: Reflection deduplication, auto-apply support, and stale fix archival
        if current_version < 59 {
            info!("Migrating database to version 59 (reflection dedup & auto-apply)");

            // Check if content_hash column already exists (idempotent migration)
            let column_exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('reflection_fixes') WHERE name = 'content_hash'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count > 0)
                .unwrap_or(false);

            if !column_exists {
                conn.execute(
                    "ALTER TABLE reflection_fixes ADD COLUMN content_hash TEXT",
                    [],
                )
                .map_err(|e| format!("Failed to add content_hash column: {}", e))?;
            }

            conn.execute_batch(
                r#"
                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_content_hash ON reflection_fixes(content_hash);

                -- Archive stale template variable resolution fixes (resolved by code fix)
                UPDATE reflection_fixes SET status = 'superseded'
                WHERE fix_description LIKE '%template variable%' AND status = 'applied';

                -- Archive stale UI Bridge API migration fixes (already absorbed)
                UPDATE reflection_fixes SET status = 'superseded'
                WHERE (fix_description LIKE '%@qontinui/ui-bridge%' OR fix_description LIKE '%CapturedError%' OR fix_description LIKE '%onBrowserEvent%')
                AND status = 'applied';

                -- Archive duplicate finding dedup instruction fixes (issue resolved)
                UPDATE reflection_fixes SET status = 'superseded'
                WHERE fix_description LIKE '%duplicate%FINDING%' AND status = 'applied';

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (59, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 59: {}", e))?;

            info!("Successfully migrated to version 59 (reflection dedup & auto-apply)");
        }

        // Version 60: Generation rules table — externalized workflow generation rules
        if current_version < 60 {
            info!("Migrating database to version 60 (generation_rules table)");

            // Create the table
            conn.execute_batch(
                r#"
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
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (source_fix_id) REFERENCES reflection_fixes(id) ON DELETE SET NULL
                );

                CREATE INDEX IF NOT EXISTS idx_generation_rules_agent ON generation_rules(agent);
                CREATE INDEX IF NOT EXISTS idx_generation_rules_status ON generation_rules(status);
                CREATE INDEX IF NOT EXISTS idx_generation_rules_agent_section ON generation_rules(agent, section, rule_number);
                "#,
            )
            .map_err(|e| format!("Failed to create generation_rules table (version 60): {}", e))?;

            // Seed schema_context important_rules (rules 1-5)
            let now = Utc::now().to_rfc3339();
            let seed_rules: Vec<(&str, &str, &str, i32, &str, &str, Option<&str>)> = vec![
                // schema_context / important_rules
                ("seed-schema_context-important_rules-1", "schema_context", "important_rules", 1,
                 "Generate valid UUIDs",
                 "Generate valid UUIDs for all `id` fields (format: xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx)",
                 None),
                ("seed-schema_context-important_rules-2", "schema_context", "important_rules", 2,
                 "Phase field must match array",
                 "`phase` field MUST match the array the step is in (setup_steps -> \"setup\", etc.)",
                 None),
                ("seed-schema_context-important_rules-3", "schema_context", "important_rules", 3,
                 "Agentic steps are prompt only",
                 "`agentic_steps` can ONLY contain `prompt` type steps",
                 None),
                ("seed-schema_context-important_rules-4", "schema_context", "important_rules", 4,
                 "ISO 8601 timestamps",
                 "Use ISO 8601 format for timestamps (e.g., \"2024-01-15T10:30:00Z\")",
                 None),
                ("seed-schema_context-important_rules-5", "schema_context", "important_rules", 5,
                 "JSON only output",
                 "Return ONLY the JSON object, no markdown formatting",
                 None),

                // schema_context / verification_quality (rules 6-16)
                ("seed-schema_context-verification_quality-6", "schema_context", "verification_quality", 6,
                 "Deterministic verification step required",
                 "verification_steps MUST include at least one deterministic, automated step — a `command` step (with check_type, test_type, or a shell command) or a `ui_bridge` step (with assert action). Do NOT use only `prompt` type steps for verification. Prompts provide AI judgment, not deterministic pass/fail results. A verification phase with ONLY prompt steps is INVALID.",
                 None),
                ("seed-schema_context-verification_quality-7", "schema_context", "verification_quality", 7,
                 "Code modification requires typecheck",
                 "When the workflow creates or modifies source code files (TypeScript, Python, Rust, etc.), verification MUST include a `command` step with `check_type` set to the appropriate type checker:\n   - TypeScript/TSX/JSX: `{\"type\": \"command\", \"check_type\": \"typecheck\", \"command\": \"npx tsc --noEmit\", \"working_directory\": \"...\"}`\n   - Python: `{\"type\": \"command\", \"check_type\": \"typecheck\", \"command\": \"mypy .\", \"working_directory\": \"...\"}`\n   - Rust: `{\"type\": \"command\", \"check_type\": \"typecheck\", \"command\": \"cargo check\", \"working_directory\": \"...\"}`",
                 None),
                ("seed-schema_context-verification_quality-8", "schema_context", "verification_quality", 8,
                 "Web app verification requires SDK or Playwright",
                 "When the workflow targets a web application (localhost:3001, localhost:1420), verification MUST include at least one of:\n   - A `command` step using curl to query UI Bridge SDK endpoints (preferred) to verify UI state\n   - A `command` step with `test_type: \"playwright\"` for browser-based verification\n   - A `ui_bridge` step with an `assert` action for direct element assertions",
                 None),
                ("seed-schema_context-verification_quality-9", "schema_context", "verification_quality", 9,
                 "Verification must be deterministic",
                 "Every workflow with 2+ verification steps should ensure ALL non-prompt verification steps are meaningful and required. If a step is worth including in verification, its failure should be visible to the verification loop. Do NOT include verification steps whose failures would be silently ignored.",
                 None),
                ("seed-schema_context-verification_quality-10", "schema_context", "verification_quality", 10,
                 "Prompts are supplementary",
                 "`prompt` type steps in verification are acceptable as supplementary checks (e.g., semantic code review, cross-referencing documentation) but must NEVER be the sole verification mechanism",
                 None),
                ("seed-schema_context-verification_quality-11", "schema_context", "verification_quality", 11,
                 "Test steps with inline commands use repository",
                 "When a `command` step with `test_type` runs a shell command (e.g., `npx playwright test ...`, `cargo test ...`), set `test_type: \"repository\"`. The `test_type: \"playwright\"` value is ONLY for steps that provide `code` with Playwright assertions to be executed via CDP. Using `\"playwright\"` for shell commands causes a \"No test_id specified\" error.",
                 None),
                ("seed-schema_context-verification_quality-12", "schema_context", "verification_quality", 12,
                 "Next.js App Router path conventions",
                 "For Next.js projects using the App Router (`src/app/`), components are organized under route groups like `src/app/(app)/`. When creating `command` steps with `check_type`, use the correct working directory paths — e.g., the frontend directory, not `src/components/`. Always verify path patterns match the actual project structure.",
                 None),
                ("seed-schema_context-verification_quality-13", "schema_context", "verification_quality", 13,
                 "Verification step caching",
                 "Running tasks cache their verification steps at startup. Updating a workflow definition via the API (e.g., `PUT /unified-workflows/{id}`) does NOT affect the currently executing task's verification steps. The AI agent should NOT attempt to fix verification step configuration by updating the workflow API mid-run — instead, it should focus on fixing the underlying code issues or report the config issue for the next run.",
                 None),
                ("seed-schema_context-verification_quality-14", "schema_context", "verification_quality", 14,
                 "Feature-specific verification required",
                 "Verification plans for feature-implementation workflows MUST include feature-specific checks (element presence, tab visibility, component rendering) in addition to compilation/lint gates. Compilation-only verification causes premature completion when the AI fixes pre-existing errors without implementing the actual feature.",
                 None),
                ("seed-schema_context-verification_quality-15", "schema_context", "verification_quality", 15,
                 "SDK verification must verify content",
                 "When a `command` step calls a UI Bridge SDK endpoint via curl (`/ui-bridge/sdk/...`), checking only the exit code is INSUFFICIENT. SDK endpoints return 200 even for empty results (e.g., `ai/search` returns `{\"results\": [], \"total\": 0}`). Every SDK verification command MUST pipe to `grep` with expected text/element content to verify meaningful results.",
                 None),
                ("seed-schema_context-verification_quality-16", "schema_context", "verification_quality", 16,
                 "Agentic-verification correspondence",
                 "Each `prompt` step in `agentic_steps` describes a specific piece of work (e.g., \"implement drag-and-drop\", \"add thumbnails\"). For EACH agentic step, `verification_steps` MUST contain at least one deterministic `command` or `ui_bridge` step that verifies the output of that work.",
                 None),

                // hardener / conversion_rules
                ("seed-hardener-conversion_rules-1", "hardener", "conversion_rules", 1,
                 "Convert prompt steps to deterministic",
                 "Convert `prompt` steps to deterministic equivalents. Only 3 step types are valid: `command`, `ui_bridge`, `prompt`.\n| Prompt check type | Convert to | Method |\n|---|---|---|\n| UI element presence/structure | `command` | curl to UI Bridge SDK endpoint, pipe to grep for content check |\n| Content/text on page | `command` | curl to UI Bridge SDK `/ai/search`, pipe to grep for expected text |\n| File existence | `command` | `check_type: \"custom_command\"` with `test -f <path>` |\n| File content | `command` | `check_type: \"custom_command\"` with `grep -q <pattern> <file>` |\n| Code quality (lint) | `command` | `check_type: \"lint\"` with appropriate command |\n| Code quality (typecheck) | `command` | `check_type: \"typecheck\"` with appropriate command |\n| API health/response | `command` | curl to endpoint, check exit code |\n| UI assertion | `ui_bridge` | Use assert action with target and expected value |\n| Subjective/qualitative | Keep as `prompt` | Cannot be made deterministic |",
                 None),
                ("seed-hardener-conversion_rules-2", "hardener", "conversion_rules", 2,
                 "Replace Playwright with SDK checks",
                 "When the UI Bridge SDK is connected, Playwright-based UI verification tests should be converted to `command` steps (using curl to SDK endpoints piped to grep) or `ui_bridge` steps. The SDK provides direct programmatic access to registered UI elements without requiring a Playwright browser instance. If a single Playwright test checks multiple things, split it into multiple `command` or `ui_bridge` steps — one per distinct verification concern. Tests that require keyboard shortcuts, file uploads, or screenshot comparisons MUST remain as `command` steps with `test_type: \"playwright\"`.",
                 Some("has_sdk_connect")),
                ("seed-hardener-conversion_rules-3", "hardener", "conversion_rules", 3,
                 "Strengthen weak SDK verification commands",
                 "If an existing `command` step calls a UI Bridge SDK endpoint via curl but only checks exit code (no grep), add a pipe to `grep` to verify meaningful content. A successful curl to the SDK just means the endpoint is reachable — it doesn't verify the UI state. SDK endpoints return 200 even for EMPTY results.",
                 Some("has_sdk_connect")),
                ("seed-hardener-conversion_rules-4", "hardener", "conversion_rules", 4,
                 "Inject page navigation before SDK checks",
                 "If the workflow's setup_steps include a page navigation step (curl POST to `/ui-bridge/sdk/page/navigate` or a `ui_bridge` step with `action: \"navigate\"`), the verification phase MUST also navigate to that same URL before any SDK element checks. Use a `command` step with curl or a `ui_bridge` navigate step.",
                 Some("has_sdk_connect")),
                ("seed-hardener-conversion_rules-5", "hardener", "conversion_rules", 5,
                 "Agentic-verification correspondence",
                 "Examine EACH prompt step in `agentic_steps` and identify the distinct goals/features it describes. Then check whether `verification_steps` has at least one deterministic `command` or `ui_bridge` step that would FAIL if that specific goal was NOT implemented. For each uncovered agentic goal, ADD a new `command` verification step (e.g., curl to SDK endpoint piped to grep for expected content).",
                 None),

                // hardener / critical_rules
                ("seed-hardener-critical_rules-1", "hardener", "critical_rules", 1,
                 "Only modify verification_steps",
                 "Do NOT change setup_steps, agentic_steps, or completion_steps",
                 None),
                ("seed-hardener-critical_rules-2", "hardener", "critical_rules", 2,
                 "Preserve step IDs",
                 "Every step must keep its original `id` field (unless splitting a step, in which case keep the original ID on one and generate new UUIDs for additions)",
                 None),
                ("seed-hardener-critical_rules-3", "hardener", "critical_rules", 3,
                 "Preserve step order",
                 "Steps must remain in the same relative position",
                 None),
                ("seed-hardener-critical_rules-4", "hardener", "critical_rules", 4,
                 "Adding steps is allowed",
                 "If a Playwright test step checks multiple things, you MAY replace it with multiple `command` or `ui_bridge` steps. You MAY also add NEW verification steps to cover uncovered agentic goals. Keep original `id`s on existing steps and generate new UUIDs for additions.",
                 None),
                ("seed-hardener-critical_rules-5", "hardener", "critical_rules", 5,
                 "Keep subjective prompts",
                 "If a prompt step is genuinely subjective (e.g., \"Is the UX intuitive?\"), keep it as `prompt`",
                 None),
                ("seed-hardener-critical_rules-6", "hardener", "critical_rules", 6,
                 "Complete required fields",
                 "Every converted step must have all required fields for its new type",
                 None),
                ("seed-hardener-critical_rules-7", "hardener", "critical_rules", 7,
                 "Only 3 step types",
                 "All steps must use `command`, `ui_bridge`, or `prompt`. Do NOT output `api_request`, `check`, `test`, `gate`, or `spec` types.",
                 None),
                ("seed-hardener-critical_rules-8", "hardener", "critical_rules", 8,
                 "Command with check_type fields",
                 "For check conversions, use `command` type with `check_type`, `command`, and `working_directory` fields.",
                 None),
                ("seed-hardener-critical_rules-9", "hardener", "critical_rules", 9,
                 "Do not convert existing command+check_type steps",
                 "Do NOT convert `command` steps that already have `check_type` set (lint, typecheck, etc.) — they are already deterministic.",
                 None),
                ("seed-hardener-critical_rules-10", "hardener", "critical_rules", 10,
                 "SDK verification uses command+curl",
                 "Use `command` steps with curl piped to grep for SDK-based verification, not `api_request`.",
                 None),

                // verification / check_rules
                ("seed-verification-check_rules-1", "verification", "check_rules", 1,
                 "command step validation (plain shell mode)",
                 "`command` is a real, syntactically valid shell command (not a placeholder like \"echo TODO\" or \"/path/to/script\"). `working_directory`, if present, looks like a real path. `timeout_seconds` is reasonable. `fail_on_error` is appropriate. Step type MUST be `command` (not `shell_command`).",
                 None),
                ("seed-verification-check_rules-2", "verification", "check_rules", 2,
                 "command step validation (check mode — check_type set)",
                 "`check_type` and `command` are consistent: \"lint\" → linter, \"typecheck\" → type checker, \"format\" → formatter check, \"analyze\" → static analysis, \"security\" → security scanner, \"custom_command\" → any command. `command` is non-empty and syntactically valid. Step type MUST be `command` (not `check`).",
                 None),
                ("seed-verification-check_rules-3", "verification", "check_rules", 3,
                 "command step validation (test mode — test_type set)",
                 "Has either `command` (for repository/custom_command) or `code` (for playwright/python). `test_type` is one of: playwright, qontinui_vision, python, repository, custom_command. The command/code looks substantive (not a placeholder). Step type MUST be `command` (not `test`).",
                 None),
                ("seed-verification-check_rules-4", "verification", "check_rules", 4,
                 "ui_bridge step validation",
                 "`action` is one of: navigate, execute, assert, snapshot. Required fields vary by action: navigate needs `url`, execute needs `instruction`, assert needs `target` and `assert_type`. `timeout_ms` is reasonable if set.",
                 None),
                ("seed-verification-check_rules-5", "verification", "check_rules", 5,
                 "prompt step quality",
                 "Content is substantive — at least 2 sentences with specific instructions. Agentic prompts reference verification results and describe what to fix. Not a generic placeholder like \"Fix the errors\" or \"Do the task\".",
                 None),
                ("seed-verification-check_rules-6", "verification", "check_rules", 6,
                 "Invalid step type detection",
                 "If any step uses a type other than `command`, `ui_bridge`, or `prompt`, flag it immediately. Common mistakes: using `check` (should be `command` with `check_type`), `test` (should be `command` with `test_type`), `api_request` (should be `command` with curl), `shell_command` (should be `command`), `gate` or `spec` (removed).",
                 None),
                ("seed-verification-check_rules-7", "verification", "check_rules", 7,
                 "Step type consistency",
                 "All step types must be one of: `command`, `ui_bridge`, `prompt`. No other types are valid. Verify that the `type` field of every step matches this constraint.",
                 None),
                ("seed-verification-check_rules-8", "verification", "check_rules", 8,
                 "UI Bridge SDK usage",
                 "If the workflow targets a web app but does NOT include a setup step to connect via UI Bridge SDK (POST to /ui-bridge/sdk/connect), flag it. If the workflow uses Playwright for simple element inspection when SDK endpoints could be used instead, flag it. If agentic prompt steps mention web UI interaction but don't reference SDK tools, flag it.",
                 None),
                ("seed-verification-check_rules-9", "verification", "check_rules", 9,
                 "Agentic-verification correspondence",
                 "For EACH prompt step in agentic_steps, there MUST be at least one corresponding deterministic verification step that can detect whether that agentic step's work succeeded. Tab/section existence checks do NOT count as adequate verification for the tab's CONTENT or FUNCTIONALITY.",
                 None),
                ("seed-verification-check_rules-10", "verification", "check_rules", 10,
                 "Cross-step and structural checks",
                 "If there are verification steps, there should be at least one agentic prompt step. Setup steps should logically prepare for what verification checks. Step names are descriptive (not \"Step 1\", \"Test\", \"Check\"). No duplicate step IDs. Steps match the user's original request.",
                 None),
            ];

            for (id, agent, section, rule_number, title, content, condition) in &seed_rules {
                conn.execute(
                    "INSERT OR IGNORE INTO generation_rules (id, agent, section, rule_number, title, content, condition, status, provenance, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', 'seed', ?8, ?9)",
                    params![id, agent, section, rule_number, title, content, condition, now, now],
                )
                .map_err(|e| format!("Failed to seed generation rule {}: {}", id, e))?;
            }

            conn.execute_batch(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (60, datetime('now'));",
            )
            .map_err(|e| format!("Failed to update schema version to 60: {}", e))?;

            info!(
                "Successfully migrated to version 60 (generation_rules table with {} seed rules)",
                seed_rules.len()
            );
        }

        // Migration to version 61: Add sweep fields to unified_workflows
        if current_version < 61 {
            info!("Migrating database to version 61 (adding sweep fields to unified_workflows)");
            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN enable_sweep INTEGER DEFAULT 0;
                ALTER TABLE unified_workflows ADD COLUMN max_sweep_iterations INTEGER DEFAULT 5;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (61, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 61: {}", e))?;
        }

        // Migration to version 62: Add assertion types generation rule
        if current_version < 62 {
            info!(
                "Migrating database to version 62 (adding valid assertion types generation rule)"
            );
            let now = chrono::Utc::now().to_rfc3339();
            conn.execute(
                "INSERT OR IGNORE INTO generation_rules (id, agent, section, rule_number, title, content, condition, status, provenance, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', 'seed', ?8, ?9)",
                params![
                    "seed-schema_context-verification_quality-17",
                    "schema_context",
                    "verification_quality",
                    17,
                    "Valid assertion types only",
                    "API request assertions MUST use one of these 5 types: `status_code`, `body_contains`, `json_path`, `header`, `response_time`. Do NOT use `body_not_contains` or any other unlisted type — they will cause runtime errors. Supported operators (optional `operator` field, default `equals`): `equals`, `contains`, `matches` (regex), `greater_than`, `less_than`.",
                    Option::<&str>::None,
                    now,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to seed assertion types rule: {}", e))?;

            conn.execute_batch(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (62, datetime('now'));",
            )
            .map_err(|e| format!("Failed to update schema version to 62: {}", e))?;

            info!("Successfully migrated to version 62 (valid assertion types rule)");
        }

        // Migration to version 63: Add step_type_knowledge table with seed data
        if current_version < 63 {
            info!("Migrating database to version 63 (adding step_type_knowledge table)");

            conn.execute_batch(
                r#"
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
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (source_fix_id) REFERENCES reflection_fixes(id) ON DELETE SET NULL
                );

                CREATE INDEX IF NOT EXISTS idx_stk_step_type ON step_type_knowledge(step_type);
                CREATE INDEX IF NOT EXISTS idx_stk_layer ON step_type_knowledge(layer);
                CREATE INDEX IF NOT EXISTS idx_stk_composite ON step_type_knowledge(step_type, layer, status);
                "#,
            )
            .map_err(|e| format!("Failed to create step_type_knowledge table: {}", e))?;

            // Seed universal knowledge entries
            let now = chrono::Utc::now().to_rfc3339();
            let seed_entries: &[(&str, &str, &str, i32)] = &[
                // command (6 entries - covers shell commands, API requests, checks)
                (
                    "command",
                    "Always set working_directory for shell commands",
                    "Shell commands must specify working_directory to ensure they run in the correct location. Without it, the command runs in whatever directory the runner process happens to be in, which is unpredictable.",
                    10,
                ),
                (
                    "command",
                    "Use fail_on_error appropriately",
                    "Set fail_on_error: true for critical setup steps (installing deps, building). Set fail_on_error: false only for commands where non-zero exit is expected (e.g., grep that may match nothing).",
                    8,
                ),
                (
                    "command",
                    "Keep commands simple and single-purpose",
                    "Each command step should do one thing. Avoid long chained commands with && or ||. Split complex operations into multiple steps so failures are attributable to a specific operation.",
                    6,
                ),
                (
                    "command",
                    "Always include content-specific assertions for API requests",
                    "A status_code: 200 assertion alone is INSUFFICIENT. Always add body_contains or json_path assertions to verify the response has the expected content. Many endpoints return 200 with empty or error bodies.",
                    10,
                ),
                (
                    "command",
                    "Set content_type for request bodies",
                    "When sending a request body, always set content_type (usually 'application/json'). Missing content_type causes the server to reject or misinterpret the body.",
                    8,
                ),
                (
                    "command",
                    "Use json_path for structured response validation",
                    "Prefer json_path assertions over body_contains for JSON APIs. Use json_path with operators like 'greater_than' or 'contains' for flexible yet precise validation. Example: json_path '$.total' greater_than '0'.",
                    6,
                ),
                // prompt (2 entries)
                (
                    "prompt",
                    "Include specific agent instructions",
                    "The prompt field must contain clear, actionable instructions for the AI agent. Vague prompts like 'fix the errors' lead to unfocused actions. Tell the agent exactly what to look for and what corrective actions to take.",
                    10,
                ),
                (
                    "prompt",
                    "Reference verification failures explicitly",
                    "In agentic prompts, tell the agent to check previous verification results and address specific failures. Use phrases like 'Check which verification steps failed and fix the underlying issues'.",
                    8,
                ),
                // check-related knowledge (stored under "command" since check is now command)
                (
                    "command",
                    "Match check_type to the technology",
                    "Use the correct check_type for the project: 'typescript' for TS/JS projects, 'python' for Python, 'rust' for Rust. Using the wrong check_type produces misleading results.",
                    10,
                ),
                (
                    "command",
                    "Set path to the project directory for checks",
                    "Always set the path field to the project root or source directory being checked. Without it, the checker may run in the wrong location or fail to find source files.",
                    8,
                ),
                // test (2 entries)
                (
                    "test",
                    "Use test_type repository for inline commands",
                    "When a test step runs a shell command (e.g., 'npm test', 'pytest'), set test_type: 'repository'. The 'repository' type executes the command in the project directory.",
                    10,
                ),
                (
                    "test",
                    "Target specific test files or patterns",
                    "Avoid running the entire test suite when only specific functionality needs verification. Use the command field to target specific test files or patterns (e.g., 'pytest tests/test_auth.py').",
                    6,
                ),
                // gate (2 entries)
                (
                    "gate",
                    "List ALL non-gate non-prompt verification step IDs",
                    "The required_steps array must contain the IDs of every non-gate, non-prompt step in verification_steps. Missing a step ID means that step's failure won't block the workflow from completing.",
                    10,
                ),
                (
                    "gate",
                    "Use exactly one gate step per workflow",
                    "Each workflow should have exactly one gate step in verification_steps. Multiple gates create confusing pass/fail semantics. The single gate aggregates all deterministic check results.",
                    8,
                ),
                // spec (2 entries)
                (
                    "spec",
                    "Describe behavioral requirements not implementation",
                    "Spec content should describe what the system should DO, not how it's implemented. Focus on observable behavior: 'The login form should display an error message when credentials are invalid'.",
                    10,
                ),
                (
                    "spec",
                    "Pair with deterministic verification",
                    "Spec steps use AI to evaluate behavior, which is non-deterministic. Always pair spec steps with deterministic steps (check, test, api_request) for reliable verification.",
                    8,
                ),
            ];

            for (step_type, title, content, priority) in seed_entries {
                let id = format!(
                    "seed-stk-{}-{}",
                    step_type,
                    title
                        .to_lowercase()
                        .replace(' ', "-")
                        .chars()
                        .take(30)
                        .collect::<String>()
                );
                conn.execute(
                    "INSERT OR IGNORE INTO step_type_knowledge (id, step_type, layer, title, content, priority, status, provenance, created_at, updated_at)
                     VALUES (?1, ?2, 'universal', ?3, ?4, ?5, 'active', 'seed', ?6, ?7)",
                    params![id, step_type, title, content, priority, now, now],
                )
                .map_err(|e| format!("Failed to seed step type knowledge: {}", e))?;
            }

            conn.execute_batch(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (63, datetime('now'));",
            )
            .map_err(|e| format!("Failed to update schema version to 63: {}", e))?;

            info!("Successfully migrated to version 63 (step_type_knowledge table with {} seed entries)", seed_entries.len());
        }

        // Version 64: Add react-doctor knowledge entries for command step type
        if current_version < 64 {
            info!("Migrating to version 64 (react-doctor step_type_knowledge entries)...");

            let now = chrono::Utc::now().to_rfc3339();

            let react_doctor_entries: Vec<(&str, &str, &str, i32)> = vec![
                (
                    "command",
                    "Use react-doctor for React project health analysis",
                    "For React/Next.js projects, use `npx -y react-doctor@latest <path> --verbose --yes` as a command step to get a health score (0-100) and diagnostics across state/effects, performance, architecture, bundle size, security, correctness, and accessibility. Always include --yes to skip interactive prompts.",
                    6,
                ),
                (
                    "command",
                    "Detect React projects before running react-doctor",
                    "Only run react-doctor on directories that have a package.json containing a 'react' dependency. Non-React TypeScript projects will produce meaningless output.",
                    4,
                ),
            ];

            for (step_type, title, content, priority) in &react_doctor_entries {
                let id = format!(
                    "seed-stk-{}-{}",
                    step_type,
                    title
                        .to_lowercase()
                        .replace(' ', "-")
                        .chars()
                        .take(30)
                        .collect::<String>()
                );
                conn.execute(
                    "INSERT OR IGNORE INTO step_type_knowledge (id, step_type, layer, title, content, priority, status, provenance, created_at, updated_at)
                     VALUES (?1, ?2, 'universal', ?3, ?4, ?5, 'active', 'seed', ?6, ?7)",
                    params![id, step_type, title, content, priority, now, now],
                )
                .map_err(|e| format!("Failed to seed react-doctor knowledge: {}", e))?;
            }

            conn.execute_batch(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (64, datetime('now'));",
            )
            .map_err(|e| format!("Failed to update schema version to 64: {}", e))?;

            info!("Successfully migrated to version 64 (react-doctor knowledge entries)");
        }

        // Version 65: Update seed generation rules for 3-type step system
        //
        // The original v60 seeds referenced old step types (api_request, check, test,
        // gate, spec). This migration updates existing seed rules to use only the 3
        // valid types: command, ui_bridge, prompt. Only updates provenance='seed' rules
        // to avoid touching reflection-learned rules.
        if current_version < 65 {
            info!("Migrating to version 65 (update seed rules for 3-type system)...");

            let rule_updates: Vec<(&str, &str, &str)> = vec![
                // schema_context / verification_quality
                ("seed-schema_context-verification_quality-6",
                 "Deterministic verification step required",
                 "verification_steps MUST include at least one deterministic, automated step — a `command` step (with check_type, test_type, or a shell command) or a `ui_bridge` step (with assert action). Do NOT use only `prompt` type steps for verification. Prompts provide AI judgment, not deterministic pass/fail results. A verification phase with ONLY prompt steps is INVALID."),
                ("seed-schema_context-verification_quality-7",
                 "Code modification requires typecheck",
                 "When the workflow creates or modifies source code files (TypeScript, Python, Rust, etc.), verification MUST include a `command` step with `check_type` set to the appropriate type checker:\n   - TypeScript/TSX/JSX: `{\"type\": \"command\", \"check_type\": \"typecheck\", \"command\": \"npx tsc --noEmit\", \"working_directory\": \"...\"}`\n   - Python: `{\"type\": \"command\", \"check_type\": \"typecheck\", \"command\": \"mypy .\", \"working_directory\": \"...\"}`\n   - Rust: `{\"type\": \"command\", \"check_type\": \"typecheck\", \"command\": \"cargo check\", \"working_directory\": \"...\"}`"),
                ("seed-schema_context-verification_quality-8",
                 "Web app verification requires SDK or Playwright",
                 "When the workflow targets a web application (localhost:3001, localhost:1420), verification MUST include at least one of:\n   - A `command` step using curl to query UI Bridge SDK endpoints (preferred) to verify UI state\n   - A `command` step with `test_type: \"playwright\"` for browser-based verification\n   - A `ui_bridge` step with an `assert` action for direct element assertions"),
                ("seed-schema_context-verification_quality-9",
                 "Verification must be deterministic",
                 "Every workflow with 2+ verification steps should ensure ALL non-prompt verification steps are meaningful and required. If a step is worth including in verification, its failure should be visible to the verification loop. Do NOT include verification steps whose failures would be silently ignored."),
                ("seed-schema_context-verification_quality-11",
                 "Test steps with inline commands use repository",
                 "When a `command` step with `test_type` runs a shell command (e.g., `npx playwright test ...`, `cargo test ...`), set `test_type: \"repository\"`. The `test_type: \"playwright\"` value is ONLY for steps that provide `code` with Playwright assertions to be executed via CDP. Using `\"playwright\"` for shell commands causes a \"No test_id specified\" error."),
                ("seed-schema_context-verification_quality-12",
                 "Next.js App Router path conventions",
                 "For Next.js projects using the App Router (`src/app/`), components are organized under route groups like `src/app/(app)/`. When creating `command` steps with `check_type`, use the correct working directory paths — e.g., the frontend directory, not `src/components/`. Always verify path patterns match the actual project structure."),
                ("seed-schema_context-verification_quality-15",
                 "SDK verification must verify content",
                 "When a `command` step calls a UI Bridge SDK endpoint via curl (`/ui-bridge/sdk/...`), checking only the exit code is INSUFFICIENT. SDK endpoints return 200 even for empty results (e.g., `ai/search` returns `{\"results\": [], \"total\": 0}`). Every SDK verification command MUST pipe to `grep` with expected text/element content to verify meaningful results."),
                ("seed-schema_context-verification_quality-16",
                 "Agentic-verification correspondence",
                 "Each `prompt` step in `agentic_steps` describes a specific piece of work (e.g., \"implement drag-and-drop\", \"add thumbnails\"). For EACH agentic step, `verification_steps` MUST contain at least one deterministic `command` or `ui_bridge` step that verifies the output of that work."),

                // hardener / conversion_rules
                ("seed-hardener-conversion_rules-1",
                 "Convert prompt steps to deterministic",
                 "Convert `prompt` steps to deterministic equivalents. Only 3 step types are valid: `command`, `ui_bridge`, `prompt`.\n| Prompt check type | Convert to | Method |\n|---|---|---|\n| UI element presence/structure | `command` | curl to UI Bridge SDK endpoint, pipe to grep for content check |\n| Content/text on page | `command` | curl to UI Bridge SDK `/ai/search`, pipe to grep for expected text |\n| File existence | `command` | `check_type: \"custom_command\"` with `test -f <path>` |\n| File content | `command` | `check_type: \"custom_command\"` with `grep -q <pattern> <file>` |\n| Code quality (lint) | `command` | `check_type: \"lint\"` with appropriate command |\n| Code quality (typecheck) | `command` | `check_type: \"typecheck\"` with appropriate command |\n| API health/response | `command` | curl to endpoint, check exit code |\n| UI assertion | `ui_bridge` | Use assert action with target and expected value |\n| Subjective/qualitative | Keep as `prompt` | Cannot be made deterministic |"),
                ("seed-hardener-conversion_rules-2",
                 "Replace Playwright with SDK checks",
                 "When the UI Bridge SDK is connected, Playwright-based UI verification tests should be converted to `command` steps (using curl to SDK endpoints piped to grep) or `ui_bridge` steps. The SDK provides direct programmatic access to registered UI elements without requiring a Playwright browser instance. If a single Playwright test checks multiple things, split it into multiple `command` or `ui_bridge` steps — one per distinct verification concern. Tests that require keyboard shortcuts, file uploads, or screenshot comparisons MUST remain as `command` steps with `test_type: \"playwright\"`."),
                ("seed-hardener-conversion_rules-3",
                 "Strengthen weak SDK verification commands",
                 "If an existing `command` step calls a UI Bridge SDK endpoint via curl but only checks exit code (no grep), add a pipe to `grep` to verify meaningful content. A successful curl to the SDK just means the endpoint is reachable — it doesn't verify the UI state. SDK endpoints return 200 even for EMPTY results."),
                ("seed-hardener-conversion_rules-4",
                 "Inject page navigation before SDK checks",
                 "If the workflow's setup_steps include a page navigation step (curl POST to `/ui-bridge/sdk/page/navigate` or a `ui_bridge` step with `action: \"navigate\"`), the verification phase MUST also navigate to that same URL before any SDK element checks. Use a `command` step with curl or a `ui_bridge` navigate step."),
                ("seed-hardener-conversion_rules-5",
                 "Agentic-verification correspondence",
                 "Examine EACH prompt step in `agentic_steps` and identify the distinct goals/features it describes. Then check whether `verification_steps` has at least one deterministic `command` or `ui_bridge` step that would FAIL if that specific goal was NOT implemented. For each uncovered agentic goal, ADD a new `command` verification step (e.g., curl to SDK endpoint piped to grep for expected content)."),

                // hardener / critical_rules
                ("seed-hardener-critical_rules-4",
                 "Adding steps is allowed",
                 "If a Playwright test step checks multiple things, you MAY replace it with multiple `command` or `ui_bridge` steps. You MAY also add NEW verification steps to cover uncovered agentic goals. Keep original `id`s on existing steps and generate new UUIDs for additions."),
                ("seed-hardener-critical_rules-7",
                 "Only 3 step types",
                 "All steps must use `command`, `ui_bridge`, or `prompt`. Do NOT output `api_request`, `check`, `test`, `gate`, or `spec` types."),
                ("seed-hardener-critical_rules-8",
                 "Command with check_type fields",
                 "For check conversions, use `command` type with `check_type`, `command`, and `working_directory` fields."),
                ("seed-hardener-critical_rules-9",
                 "Do not convert existing command+check_type steps",
                 "Do NOT convert `command` steps that already have `check_type` set (lint, typecheck, etc.) — they are already deterministic."),
                ("seed-hardener-critical_rules-10",
                 "SDK verification uses command+curl",
                 "Use `command` steps with curl piped to grep for SDK-based verification, not `api_request`."),

                // verification / check_rules
                ("seed-verification-check_rules-1",
                 "command step validation (plain shell mode)",
                 "`command` is a real, syntactically valid shell command (not a placeholder like \"echo TODO\" or \"/path/to/script\"). `working_directory`, if present, looks like a real path. `timeout_seconds` is reasonable. `fail_on_error` is appropriate. Step type MUST be `command` (not `shell_command`)."),
                ("seed-verification-check_rules-2",
                 "command step validation (check mode — check_type set)",
                 "`check_type` and `command` are consistent: \"lint\" → linter, \"typecheck\" → type checker, \"format\" → formatter check, \"analyze\" → static analysis, \"security\" → security scanner, \"custom_command\" → any command. `command` is non-empty and syntactically valid. Step type MUST be `command` (not `check`)."),
                ("seed-verification-check_rules-3",
                 "command step validation (test mode — test_type set)",
                 "Has either `command` (for repository/custom_command) or `code` (for playwright/python). `test_type` is one of: playwright, qontinui_vision, python, repository, custom_command. The command/code looks substantive (not a placeholder). Step type MUST be `command` (not `test`)."),
                ("seed-verification-check_rules-4",
                 "ui_bridge step validation",
                 "`action` is one of: navigate, execute, assert, snapshot. Required fields vary by action: navigate needs `url`, execute needs `instruction`, assert needs `target` and `assert_type`. `timeout_ms` is reasonable if set."),
                ("seed-verification-check_rules-6",
                 "Invalid step type detection",
                 "If any step uses a type other than `command`, `ui_bridge`, or `prompt`, flag it immediately. Common mistakes: using `check` (should be `command` with `check_type`), `test` (should be `command` with `test_type`), `api_request` (should be `command` with curl), `shell_command` (should be `command`), `gate` or `spec` (removed)."),
                ("seed-verification-check_rules-7",
                 "Step type consistency",
                 "All step types must be one of: `command`, `ui_bridge`, `prompt`. No other types are valid. Verify that the `type` field of every step matches this constraint."),
            ];

            let mut updated = 0;
            for (rule_id, new_title, new_content) in &rule_updates {
                let rows = conn.execute(
                    "UPDATE generation_rules SET title = ?1, content = ?2, updated_at = datetime('now') WHERE id = ?3 AND provenance = 'seed'",
                    params![new_title, new_content, rule_id],
                )
                .map_err(|e| format!("Failed to update seed rule {}: {}", rule_id, e))?;
                if rows > 0 {
                    updated += 1;
                }
            }

            // Insert new rules that didn't exist in the original v60 seeds
            let now = chrono::Utc::now().to_rfc3339();

            // Rule 17: Only 3 step types
            conn.execute(
                "INSERT OR IGNORE INTO generation_rules (id, agent, section, rule_number, title, content, condition, status, provenance, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', 'seed', ?8, ?9)",
                params![
                    "seed-schema_context-verification_quality-17",
                    "schema_context", "verification_quality", 17,
                    "Only 3 step types exist",
                    "The only valid step types are `command`, `ui_bridge`, and `prompt`. Do NOT use `shell_command`, `api_request`, `mcp_call`, `check`, `check_group`, `test`, `gate`, or `spec` — these are not valid types. Tests are run via `command` with `test_type` set. Checks are run via `command` with `check_type` set.",
                    Option::<&str>::None, now, now
                ],
            )
            .map_err(|e| format!("Failed to insert rule 17: {}", e))?;

            // Rule for explicit mode field on command steps
            conn.execute(
                "INSERT OR IGNORE INTO generation_rules (id, agent, section, rule_number, title, content, condition, status, provenance, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', 'seed', ?8, ?9)",
                params![
                    "seed-schema_context-important_rules-6",
                    "schema_context", "important_rules", 6,
                    "Every command step MUST include a mode field",
                    "Every `command` step MUST include a `mode` field set to one of: `shell`, `check`, `check_group`, `test`. The mode must match the fields present: `check` requires `check_type`, `check_group` requires `check_group_id`, `test` requires `test_type` or `test_id`, `shell` is the default for plain commands.",
                    Option::<&str>::None, now, now
                ],
            )
            .map_err(|e| format!("Failed to insert mode rule: {}", e))?;

            conn.execute_batch(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (65, datetime('now'));",
            )
            .map_err(|e| format!("Failed to update schema version to 65: {}", e))?;

            info!(
                "Successfully migrated to version 65 ({} seed rules updated for 3-type system)",
                updated
            );
        }

        // Version 66: Add stage_index to workflow_step_checkpoints and stages/stop_on_failure to unified_workflows
        if current_version < 66 {
            info!(
                "Migrating to version 66 (add stage_index to checkpoints, stages to workflows)..."
            );

            // Add stage_index column to workflow_step_checkpoints
            // For existing rows, default to 0 (single-stage backward compat)
            conn.execute_batch(
                r#"
                ALTER TABLE workflow_step_checkpoints ADD COLUMN stage_index INTEGER DEFAULT 0;

                -- Add stages and stop_on_failure to unified_workflows
                ALTER TABLE unified_workflows ADD COLUMN stages TEXT DEFAULT '[]';
                ALTER TABLE unified_workflows ADD COLUMN stop_on_failure INTEGER DEFAULT 0;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (66, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 66: {}", e))?;

            info!("Successfully migrated to version 66 (stage_index, stages, stop_on_failure)");
        }

        // Migration to version 67: Fix UNIQUE constraint to include stage_index
        // The original UNIQUE(execution_id, phase, iteration, step_index) from v48 causes
        // multi-stage workflows to silently overwrite checkpoints when two stages have
        // steps with the same (phase, iteration, step_index). Recreate table with
        // UNIQUE(execution_id, phase, iteration, step_index, stage_index).
        if current_version < 67 {
            info!("Migrating to version 67 (fix UNIQUE constraint to include stage_index)...");

            conn.execute_batch(
                r#"
                -- Recreate workflow_step_checkpoints with corrected UNIQUE constraint
                CREATE TABLE IF NOT EXISTS workflow_step_checkpoints_new (
                    id TEXT PRIMARY KEY,
                    execution_id TEXT NOT NULL,
                    workflow_type TEXT NOT NULL,
                    phase TEXT NOT NULL,
                    iteration INTEGER,
                    step_index INTEGER NOT NULL,
                    step_type TEXT NOT NULL,
                    step_name TEXT,
                    status TEXT NOT NULL,
                    result_json TEXT,
                    step_config_json TEXT,
                    started_at TEXT,
                    completed_at TEXT,
                    duration_ms INTEGER,
                    error TEXT,
                    stage_index INTEGER DEFAULT 0,
                    FOREIGN KEY (execution_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    UNIQUE(execution_id, phase, iteration, step_index, stage_index)
                );

                -- Copy all existing data
                INSERT OR IGNORE INTO workflow_step_checkpoints_new
                    (id, execution_id, workflow_type, phase, iteration, step_index,
                     step_type, step_name, status, result_json, step_config_json,
                     started_at, completed_at, duration_ms, error, stage_index)
                SELECT
                    id, execution_id, workflow_type, phase, iteration, step_index,
                    step_type, step_name, status, result_json, step_config_json,
                    started_at, completed_at, duration_ms, error,
                    COALESCE(stage_index, 0)
                FROM workflow_step_checkpoints;

                -- Drop old table and rename new one
                DROP TABLE workflow_step_checkpoints;
                ALTER TABLE workflow_step_checkpoints_new RENAME TO workflow_step_checkpoints;

                -- Recreate indexes
                CREATE INDEX IF NOT EXISTS idx_step_checkpoints_execution ON workflow_step_checkpoints(execution_id);
                CREATE INDEX IF NOT EXISTS idx_step_checkpoints_lookup ON workflow_step_checkpoints(execution_id, phase, iteration);
                CREATE INDEX IF NOT EXISTS idx_step_checkpoints_status ON workflow_step_checkpoints(status);
                CREATE INDEX IF NOT EXISTS idx_step_checkpoints_cursor ON workflow_step_checkpoints(execution_id, step_index);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (67, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 67: {}", e))?;

            info!(
                "Successfully migrated to version 67 (UNIQUE constraint now includes stage_index)"
            );
        }

        // Migration to version 68: Add process_sessions and process_session_output tables
        if current_version < 68 {
            info!("Migrating to version 68 (process session persistence)...");

            conn.execute_batch(
                r#"
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

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (68, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 68: {}", e))?;

            info!("Successfully migrated to version 68 (process session persistence)");
        }

        if current_version < 69 {
            info!("Migrating to version 69 (cached app specs)...");

            conn.execute_batch(
                r#"
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

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (69, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 69: {}", e))?;

            info!("Successfully migrated to version 69 (cached app specs)");
        }

        // Version 70: Add reflection_mode to unified_workflows
        if current_version < 70 {
            info!("Migrating to version 70 (add reflection_mode to unified_workflows)...");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN reflection_mode INTEGER DEFAULT 1;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (70, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 70: {}", e))?;

            info!("Successfully migrated to version 70 (reflection_mode)");
        }

        // Version 71: Add follow-up columns to task_runs
        if current_version < 71 {
            info!("Migrating to version 71 (add follow-up columns to task_runs)...");

            conn.execute_batch(
                r#"
                ALTER TABLE task_runs ADD COLUMN is_follow_up INTEGER DEFAULT 0;
                ALTER TABLE task_runs ADD COLUMN follow_up_source_task_run_id TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (71, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 71: {}", e))?;

            info!("Successfully migrated to version 71 (follow-up columns)");
        }

        // Version 72: Generator evaluation - pipeline artifacts
        if current_version < 72 {
            info!("Migrating to version 72 (generation_pipeline_artifacts)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS generation_pipeline_artifacts (
                    id TEXT PRIMARY KEY,
                    workflow_id TEXT,
                    task_run_id TEXT,
                    description TEXT NOT NULL,
                    category TEXT,
                    created_at TEXT NOT NULL,
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
                    success INTEGER NOT NULL DEFAULT 1,
                    error_message TEXT,
                    model_used TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_pipeline_artifacts_workflow ON generation_pipeline_artifacts(workflow_id);
                CREATE INDEX IF NOT EXISTS idx_pipeline_artifacts_created ON generation_pipeline_artifacts(created_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (72, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 72: {}", e))?;

            info!("Successfully migrated to version 72 (generation_pipeline_artifacts)");
        }

        // Version 73: Generator evaluation - benchmarks
        if current_version < 73 {
            info!("Migrating to version 73 (generator_benchmarks)...");

            conn.execute_batch(
                r#"
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
                    structure_score REAL,
                    content_score REAL,
                    step_type_score REAL,
                    overall_score REAL,
                    score_breakdown TEXT,
                    generated_json TEXT,
                    duration_ms INTEGER,
                    passed INTEGER NOT NULL DEFAULT 0,
                    notes TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_benchmark_results_benchmark ON generator_benchmark_results(benchmark_id);
                CREATE INDEX IF NOT EXISTS idx_benchmark_results_run_at ON generator_benchmark_results(run_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (73, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 73: {}", e))?;

            info!("Successfully migrated to version 73 (generator_benchmarks)");
        }

        // Version 74: Add investigation columns to generation_pipeline_artifacts
        if current_version < 74 {
            info!("Migrating to version 74 (add investigation columns to pipeline_artifacts)...");

            conn.execute_batch(
                r#"
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN investigation_duration_ms INTEGER;
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN investigation_enriched_description TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (74, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 74: {}", e))?;

            info!("Successfully migrated to version 74 (investigation columns)");
        }

        if current_version < 75 {
            info!("Migrating to version 75 (workflow triggers system)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS workflow_triggers (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,
                    trigger_type TEXT NOT NULL,
                    trigger_config TEXT NOT NULL,
                    workflow_id TEXT NOT NULL,
                    workflow_overrides TEXT,
                    conditions TEXT DEFAULT '[]',
                    debounce_ms INTEGER DEFAULT 1000,
                    cooldown_seconds INTEGER DEFAULT 60,
                    max_concurrent INTEGER DEFAULT 1,
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
                    event_type TEXT NOT NULL,
                    event_data TEXT DEFAULT '{}',
                    action TEXT NOT NULL,
                    task_run_id TEXT,
                    error_message TEXT,
                    triggered_at TEXT NOT NULL,
                    FOREIGN KEY (trigger_id) REFERENCES workflow_triggers(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_trigger_history_trigger_id ON trigger_history(trigger_id);
                CREATE INDEX IF NOT EXISTS idx_trigger_history_triggered_at ON trigger_history(triggered_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (75, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 75: {}", e))?;

            info!("Successfully migrated to version 75 (workflow triggers system)");
        }

        if current_version < 76 {
            info!("Migrating to version 76 (canvas panels)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS canvas_panels (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    component TEXT NOT NULL,
                    title TEXT NOT NULL,
                    data_json TEXT NOT NULL,
                    priority INTEGER DEFAULT 50,
                    size TEXT DEFAULT 'normal',
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_canvas_panels_task_run_id ON canvas_panels(task_run_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (76, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 76: {}", e))?;

            info!("Successfully migrated to version 76 (canvas panels)");
        }

        // Version 77: Add model_overrides to unified_workflows
        if current_version < 77 {
            info!("Migrating to version 77 (add model_overrides to unified_workflows)...");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN model_overrides TEXT DEFAULT '{}';

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (77, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 77: {}", e))?;

            info!("Successfully migrated to version 77 (model_overrides)");
        }

        // Version 78: Add approval_gates table for human-in-the-loop audit trail
        if current_version < 78 {
            info!("Migrating to version 78 (approval_gates table)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS approval_gates (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    iteration INTEGER NOT NULL,
                    prompt TEXT NOT NULL,
                    context_json TEXT DEFAULT '{}',
                    action TEXT,
                    comment TEXT,
                    status TEXT NOT NULL DEFAULT 'pending',
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    resolved_at TEXT,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_approval_gates_task_run_id ON approval_gates(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_approval_gates_status ON approval_gates(status);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (78, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 78: {}", e))?;

            info!("Successfully migrated to version 78 (approval_gates)");
        }

        // Version 79: Add user_skills table for user-created skill definitions
        if current_version < 79 {
            info!("Migrating to version 79 (user_skills table)...");

            conn.execute_batch(
                r#"
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
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_user_skills_slug ON user_skills(slug);
                CREATE INDEX IF NOT EXISTS idx_user_skills_category ON user_skills(category);
                CREATE INDEX IF NOT EXISTS idx_user_skills_updated_at ON user_skills(updated_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (79, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 79: {}", e))?;

            info!("Successfully migrated to version 79 (user_skills)");
        }

        // Version 80: Add approval_gate column to unified_workflows
        if current_version < 80 {
            info!("Migrating to version 80 (add approval_gate to unified_workflows)...");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN approval_gate BOOLEAN DEFAULT 0;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (80, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 80: {}", e))?;

            info!("Successfully migrated to version 80 (approval_gate)");
        }

        // Version 81: Add source column to user_skills for community skills
        if current_version < 81 {
            info!("Migrating to version 81 (add source column to user_skills)...");

            conn.execute_batch(
                r#"
                ALTER TABLE user_skills ADD COLUMN source TEXT NOT NULL DEFAULT 'user';

                CREATE INDEX IF NOT EXISTS idx_user_skills_source ON user_skills(source);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (81, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 81: {}", e))?;

            info!("Successfully migrated to version 81 (user_skills source column)");
        }

        if current_version < 82 {
            info!("Migrating to version 82 (canvas panels group_name column)...");

            conn.execute_batch(
                r#"
                ALTER TABLE canvas_panels ADD COLUMN group_name TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (82, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 82: {}", e))?;

            info!("Successfully migrated to version 82 (canvas panels group_name)");
        }

        if current_version < 83 {
            info!("Migrating to version 83 (trigger retry columns)...");

            conn.execute_batch(
                r#"
                ALTER TABLE workflow_triggers ADD COLUMN retry_count INTEGER DEFAULT 0;
                ALTER TABLE workflow_triggers ADD COLUMN retry_delay_seconds INTEGER DEFAULT 30;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (83, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 83: {}", e))?;

            info!("Successfully migrated to version 83 (trigger retry columns)");
        }

        // Version 84: Extend user_skills with versioning, author, checksums, dependencies, approval
        if current_version < 84 {
            info!(
                "Migrating to version 84 (extend user_skills for skill registry improvements)..."
            );

            conn.execute_batch(
                r#"
                ALTER TABLE user_skills ADD COLUMN version TEXT DEFAULT '1.0.0';
                ALTER TABLE user_skills ADD COLUMN author TEXT DEFAULT NULL;
                ALTER TABLE user_skills ADD COLUMN checksum TEXT DEFAULT NULL;
                ALTER TABLE user_skills ADD COLUMN depends_on TEXT DEFAULT '[]';
                ALTER TABLE user_skills ADD COLUMN usage_count INTEGER DEFAULT 0;
                ALTER TABLE user_skills ADD COLUMN approval_status TEXT DEFAULT NULL;
                ALTER TABLE user_skills ADD COLUMN forked_from TEXT DEFAULT NULL;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (84, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 84: {}", e))?;

            info!("Successfully migrated to version 84 (skill registry improvements)");
        }

        // Version 85: Phase token usage tracking for cost analysis
        if current_version < 85 {
            info!("Migrating to version 85 (phase token usage tracking)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS phase_token_usage (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_run_id TEXT NOT NULL,
                    phase TEXT NOT NULL,
                    stage_index INTEGER,
                    iteration INTEGER,
                    model_used TEXT,
                    provider_used TEXT,
                    input_tokens INTEGER NOT NULL DEFAULT 0,
                    output_tokens INTEGER NOT NULL DEFAULT 0,
                    cost_cents INTEGER NOT NULL DEFAULT 0,
                    duration_ms INTEGER,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_phase_token_usage_task_run ON phase_token_usage(task_run_id);

                ALTER TABLE task_runs ADD COLUMN total_input_tokens INTEGER DEFAULT 0;
                ALTER TABLE task_runs ADD COLUMN total_output_tokens INTEGER DEFAULT 0;
                ALTER TABLE task_runs ADD COLUMN total_cost_cents INTEGER DEFAULT 0;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (85, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 85: {}", e))?;

            info!("Successfully migrated to version 85 (phase token usage tracking)");
        }

        // Version 86: Add trace_id to error_events for cross-service trace correlation
        if current_version < 86 {
            info!("Migrating to version 86 (cross-service trace propagation)...");

            conn.execute_batch(
                r#"
                ALTER TABLE error_events ADD COLUMN trace_id TEXT;
                CREATE INDEX IF NOT EXISTS idx_error_events_trace_id ON error_events(trace_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (86, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 86: {}", e))?;

            info!("Successfully migrated to version 86 (cross-service trace propagation)");
        }

        // Version 87: Add UI Bridge integrations tracking table
        if current_version < 87 {
            info!("Migrating to version 87 (UI Bridge integrations tracking)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS ui_bridge_integrations (
                    id TEXT PRIMARY KEY,
                    project_path TEXT NOT NULL,
                    label TEXT,
                    framework TEXT,
                    integration_type TEXT NOT NULL,
                    sdk_version TEXT,
                    status TEXT NOT NULL DEFAULT 'active',
                    proxy_port INTEGER,
                    target_url TEXT,
                    last_health_check INTEGER,
                    element_count INTEGER,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_ui_bridge_integrations_status ON ui_bridge_integrations(status);
                CREATE INDEX IF NOT EXISTS idx_ui_bridge_integrations_type ON ui_bridge_integrations(integration_type);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (87, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 87: {}", e))?;

            info!("Successfully migrated to version 87 (UI Bridge integrations tracking)");
        }

        if current_version < 88 {
            info!(
                "Migrating to version 88 (specification + prompt columns on pipeline_artifacts)..."
            );

            conn.execute_batch(
                r#"
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN specification_duration_ms INTEGER;
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN specification_criteria TEXT;
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN specification_prompt TEXT;
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN builder_prompt TEXT;
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN verification_prompts TEXT;
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN hardener_prompt TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (88, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 88: {}", e))?;

            info!("Successfully migrated to version 88 (specification + prompt columns on pipeline_artifacts)");
        }

        if current_version < 89 {
            info!("Migrating to version 89 (source_agent on reflection_fixes, auto-rule fields on generation_rules)...");

            conn.execute_batch(
                r#"
                ALTER TABLE reflection_fixes ADD COLUMN source_agent TEXT;
                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_source_agent ON reflection_fixes(source_agent);

                ALTER TABLE generation_rules ADD COLUMN confidence REAL DEFAULT 1.0;
                ALTER TABLE generation_rules ADD COLUMN auto_generated_at TEXT;
                ALTER TABLE generation_rules ADD COLUMN evidence_count INTEGER DEFAULT 0;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (89, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 89: {}", e))?;

            info!("Successfully migrated to version 89 (source_agent + auto-rule fields)");
        }

        // Version 90: Add completion_prompts_first to unified_workflows
        if current_version < 90 {
            info!("Migrating to version 90 (add completion_prompts_first to unified_workflows)...");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN completion_prompts_first INTEGER NOT NULL DEFAULT 0;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (90, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 90: {}", e))?;

            info!("Successfully migrated to version 90 (completion_prompts_first)");
        }

        // Version 91: Known Issues Registry + Issue Pattern Templates
        if current_version < 91 {
            info!("Migrating to version 91 (known issues registry + pattern templates)...");

            conn.execute_batch(
                r#"
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
                    confidence REAL NOT NULL DEFAULT 1.0,
                    provenance TEXT NOT NULL DEFAULT 'manual',
                    source_finding_ids TEXT DEFAULT '[]',
                    source_task_run_id TEXT,
                    verification_hint TEXT,
                    verification_step_template TEXT,
                    times_detected INTEGER DEFAULT 1,
                    times_checked INTEGER DEFAULT 0,
                    last_detected_at TEXT,
                    last_checked_at TEXT,
                    resolved_at TEXT,
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

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (91, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 91: {}", e))?;

            info!(
                "Successfully migrated to version 91 (known issues registry + pattern templates)"
            );
        }

        if current_version < 92 {
            info!("Migrating database to version 92 (workflow favorites)");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN is_favorite INTEGER DEFAULT 0;

                CREATE INDEX IF NOT EXISTS idx_unified_workflows_is_favorite ON unified_workflows(is_favorite);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (92, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 92: {}", e))?;

            info!("Successfully migrated to version 92 (workflow favorites)");
        }

        // Migration to version 93: State Machine Config Builder tables
        if current_version < 93 {
            info!("Migrating database to version 93 (state machine config builder tables)");
            conn.execute_batch(
                r#"
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

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (93, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 93: {}", e))?;

            info!("Successfully migrated to version 93 (state machine config builder tables)");
        }

        // Migration 94: Workflow AI Sessions table for restart survival
        if current_version < 94 {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS workflow_ai_sessions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_run_id TEXT NOT NULL,
                    iteration INTEGER NOT NULL,
                    phase TEXT NOT NULL,
                    stage_index INTEGER,
                    claude_cli_session_id TEXT,
                    session_started_at TEXT NOT NULL,
                    session_completed_at TEXT,
                    output_length INTEGER NOT NULL DEFAULT 0,
                    status TEXT NOT NULL DEFAULT 'running',
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_wf_ai_sessions_task_run ON workflow_ai_sessions(task_run_id);
                CREATE UNIQUE INDEX IF NOT EXISTS idx_wf_ai_sessions_unique
                    ON workflow_ai_sessions(task_run_id, iteration, phase, COALESCE(stage_index, -1));

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (94, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 94: {}", e))?;

            info!(
                "Successfully migrated to version 94 (workflow AI sessions for restart survival)"
            );
        }

        // Version 95: Quality improvements — dependency graph, cost annotations, quality report
        if current_version < 95 {
            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN dependency_graph TEXT DEFAULT NULL;
                ALTER TABLE unified_workflows ADD COLUMN cost_annotations TEXT DEFAULT NULL;
                ALTER TABLE unified_workflows ADD COLUMN quality_report TEXT DEFAULT NULL;

                ALTER TABLE generation_pipeline_artifacts ADD COLUMN revision_duration_ms INTEGER DEFAULT NULL;
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN quality_report TEXT DEFAULT NULL;
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN revision_cycles INTEGER DEFAULT NULL;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (95, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 95: {}", e))?;

            info!("Successfully migrated to version 95 (quality improvements)");
        }

        // Version 96: Add workflow_id to task_runs for linking to unified_workflows
        if current_version < 96 {
            conn.execute_batch(
                r#"
                ALTER TABLE task_runs ADD COLUMN workflow_id TEXT DEFAULT NULL;
                CREATE INDEX IF NOT EXISTS idx_task_runs_workflow_id ON task_runs(workflow_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (96, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 96: {}", e))?;

            info!("Successfully migrated to version 96 (workflow_id on task_runs)");
        }

        if current_version < 97 {
            info!("Migrating to version 97 (project reflection: scope + project_path columns)...");

            conn.execute_batch(
                r#"
                ALTER TABLE reflection_fixes ADD COLUMN reflection_scope TEXT DEFAULT 'workflow';
                ALTER TABLE reflection_fixes ADD COLUMN project_path TEXT;
                ALTER TABLE task_knowledge ADD COLUMN project_path TEXT;

                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_project ON reflection_fixes(project_path);
                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_scope ON reflection_fixes(reflection_scope);
                CREATE INDEX IF NOT EXISTS idx_task_knowledge_project ON task_knowledge(project_path);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (97, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 97: {}", e))?;

            info!("Successfully migrated to version 97 (project reflection columns)");
        }

        // Migration to version 98: Add workflow_constraint_results table
        // Stores constraint engine evaluation results per-iteration for post-run review
        if current_version < 98 {
            info!("Migrating to version 98 (workflow_constraint_results table)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS workflow_constraint_results (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_run_id TEXT NOT NULL,
                    iteration INTEGER NOT NULL,
                    constraint_id TEXT NOT NULL,
                    constraint_name TEXT NOT NULL,
                    passed INTEGER NOT NULL,
                    severity TEXT NOT NULL,
                    violations_json TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),

                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_wf_constraint_task_run ON workflow_constraint_results(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_wf_constraint_iteration ON workflow_constraint_results(iteration);
                CREATE INDEX IF NOT EXISTS idx_wf_constraint_passed ON workflow_constraint_results(passed);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (98, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 98: {}", e))?;

            info!("Successfully migrated to version 98 (workflow_constraint_results table)");
        }

        // Migration to version 99: Cognitive System Model
        // Adds knowledge properties (accumulation monotonicity, convergence gradient,
        // relevance decay) and prediction capabilities to the reflection system.
        if current_version < 99 {
            info!("Migrating to version 99 (cognitive system model)...");

            conn.execute_batch(
                r#"
                -- New columns on reflection_fixes
                ALTER TABLE reflection_fixes ADD COLUMN target_component TEXT;
                ALTER TABLE reflection_fixes ADD COLUMN reuse_count INTEGER DEFAULT 0;

                -- New columns on task_knowledge
                ALTER TABLE task_knowledge ADD COLUMN last_validated_at TEXT;
                ALTER TABLE task_knowledge ADD COLUMN validation_count INTEGER DEFAULT 0;

                -- New column on error_events
                ALTER TABLE error_events ADD COLUMN resolved_by_fix_id TEXT;

                -- Fix applications: tracks each time a fix is reused
                CREATE TABLE IF NOT EXISTS fix_applications (
                    id TEXT PRIMARY KEY,
                    fix_id TEXT NOT NULL,
                    task_run_id TEXT NOT NULL,
                    error_signature_hash TEXT,
                    outcome TEXT DEFAULT 'pending',
                    applied_at TEXT NOT NULL,
                    evaluated_at TEXT,
                    FOREIGN KEY (fix_id) REFERENCES reflection_fixes(id) ON DELETE CASCADE,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_fix_applications_fix ON fix_applications(fix_id);
                CREATE INDEX IF NOT EXISTS idx_fix_applications_task ON fix_applications(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_fix_applications_sig ON fix_applications(error_signature_hash);

                -- Convergence snapshots: time-series convergence metrics
                CREATE TABLE IF NOT EXISTS convergence_snapshots (
                    id TEXT PRIMARY KEY,
                    workflow_name TEXT NOT NULL,
                    project_path TEXT,
                    scope TEXT NOT NULL DEFAULT 'workflow',
                    convergence_score REAL NOT NULL,
                    consecutive_clean_runs INTEGER NOT NULL,
                    novelty_score REAL NOT NULL,
                    effective_fix_rate REAL NOT NULL,
                    change_velocity REAL NOT NULL,
                    total_fixes INTEGER NOT NULL,
                    effective_fixes INTEGER NOT NULL,
                    snapshot_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_convergence_workflow ON convergence_snapshots(workflow_name);
                CREATE INDEX IF NOT EXISTS idx_convergence_project ON convergence_snapshots(project_path);
                CREATE INDEX IF NOT EXISTS idx_convergence_scope ON convergence_snapshots(scope);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (99, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 99: {}", e))?;

            info!("Successfully migrated to version 99 (cognitive system model)");
        }

        // Migration to version 100: Causal Chain Tracking
        // Adds directed cause→effect graph for tracking causal relationships between
        // events (findings, errors, fixes, verifications).
        if current_version < 100 {
            info!("Migrating to version 100 (causal chain tracking)...");

            conn.execute_batch(
                r#"
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
                    created_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
                CREATE INDEX IF NOT EXISTS idx_causal_cause ON causal_events(cause_event_type, cause_event_id);
                CREATE INDEX IF NOT EXISTS idx_causal_effect ON causal_events(effect_event_type, effect_event_id);
                CREATE INDEX IF NOT EXISTS idx_causal_workflow ON causal_events(workflow_name);
                CREATE INDEX IF NOT EXISTS idx_causal_task_run ON causal_events(task_run_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (100, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 100: {}", e))?;

            info!("Successfully migrated to version 100 (causal chain tracking)");
        }

        // Migration to version 101: Architecture Model
        // Aggregated component-level data from reflection fixes, causal events,
        // and knowledge into a queryable graph of components and relationships.
        if current_version < 101 {
            info!("Migrating to version 101 (architecture model)...");

            conn.execute_batch(
                r#"
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
                    relationship_type TEXT NOT NULL,
                    strength INTEGER NOT NULL DEFAULT 1,
                    last_seen_at TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    UNIQUE(workflow_name, source_component, target_component, relationship_type)
                );
                CREATE INDEX IF NOT EXISTS idx_comp_rel_workflow ON component_relationships(workflow_name);
                CREATE INDEX IF NOT EXISTS idx_comp_rel_source ON component_relationships(source_component);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (101, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 101: {}", e))?;

            info!("Successfully migrated to version 101 (architecture model)");
        }

        // Migration to version 102: Add constraint_overrides to unified_workflows
        if current_version < 102 {
            info!("Migrating to version 102 (add constraint_overrides to unified_workflows)...");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN constraint_overrides TEXT DEFAULT '{}';
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (102, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 102: {}", e))?;

            info!("Successfully migrated to version 102 (constraint_overrides)");
        }

        // Migration to version 103: Component health snapshots for temporal trends
        if current_version < 103 {
            info!("Migrating to version 103 (component health snapshots)...");

            conn.execute_batch(
                r#"
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
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (103, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 103: {}", e))?;

            info!("Successfully migrated to version 103 (component health snapshots)");
        }

        // Migration to version 104: Decision context capture
        if current_version < 104 {
            info!("Migrating to version 104 (decision context capture)...");

            conn.execute_batch(
                r#"
                ALTER TABLE reflection_fixes ADD COLUMN reasoning TEXT;
                ALTER TABLE reflection_fixes ADD COLUMN alternatives_considered TEXT;
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (104, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 104: {}", e))?;

            info!("Successfully migrated to version 104 (decision context capture)");
        }

        // Migration to version 105: Cross-project patterns (hybrid RAG)
        if current_version < 105 {
            info!("Migrating to version 105 (cross-project patterns)...");

            conn.execute_batch(
                r#"
                ALTER TABLE reflection_fixes ADD COLUMN applicability_context TEXT;
                ALTER TABLE reflection_fixes ADD COLUMN fix_description_embedding BLOB;
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (105, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 105: {}", e))?;

            info!("Successfully migrated to version 105 (cross-project patterns)");
        }

        if current_version < 106 {
            info!("Migrating to version 106 (generation rule application tracking)...");

            conn.execute_batch(
                r#"
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
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (106, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 106: {}", e))?;

            info!("Successfully migrated to version 106 (generation rule application tracking)");
        }

        if current_version < 107 {
            info!("Migrating to version 107 (causal events dedup index)...");

            conn.execute_batch(
                r#"
                CREATE UNIQUE INDEX IF NOT EXISTS idx_causal_dedup ON causal_events(cause_event_type, cause_event_id, effect_event_type, effect_event_id);
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (107, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 107: {}", e))?;

            info!("Successfully migrated to version 107 (causal events dedup index)");
        }

        if current_version < 108 {
            info!("Migrating to version 108 (remove proxy_port from ui_bridge_integrations)...");

            conn.execute_batch(
                r#"
                ALTER TABLE ui_bridge_integrations DROP COLUMN proxy_port;
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (108, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 108: {}", e))?;

            info!("Successfully migrated to version 108 (remove proxy_port)");
        }

        if current_version < 109 {
            info!("Migrating to version 109 (add confidence_score to pipeline artifacts)...");

            conn.execute_batch(
                r#"
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN confidence_score REAL DEFAULT NULL;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (109, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 109: {}", e))?;

            info!("Successfully migrated to version 109 (confidence_score)");
        }

        if current_version < 110 {
            info!("Migrating to version 110 (state machine element thumbnails)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS sm_element_thumbnails (
                    config_id TEXT NOT NULL REFERENCES state_machine_configs(id) ON DELETE CASCADE,
                    fingerprint_hash TEXT NOT NULL,
                    thumbnail_base64 TEXT NOT NULL,
                    PRIMARY KEY (config_id, fingerprint_hash)
                );

                CREATE INDEX IF NOT EXISTS idx_sm_thumbnails_config ON sm_element_thumbnails(config_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (110, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 110: {}", e))?;

            info!("Successfully migrated to version 110 (sm_element_thumbnails)");
        }

        if current_version < 111 {
            info!("Migrating to version 111 (capture screenshots for state view)...");

            conn.execute_batch(
                r#"
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

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (111, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 111: {}", e))?;

            info!("Successfully migrated to version 111 (sm_capture_screenshots)");
        }

        // --- Migration 112: orchestration_loop_configs ---
        if current_version < 112 {
            info!("Migrating to version 112 (orchestration_loop_configs)...");

            conn.execute_batch(
                r#"
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

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (112, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 112: {}", e))?;

            info!("Successfully migrated to version 112 (orchestration_loop_configs)");
        }

        // --- Migration 113: runner_port column on task_runs ---
        if current_version < 113 {
            info!("Migrating to version 113 (task_runs.runner_port)...");

            conn.execute_batch(
                r#"
                ALTER TABLE task_runs ADD COLUMN runner_port INTEGER;

                CREATE INDEX IF NOT EXISTS idx_task_runs_runner_port ON task_runs(runner_port);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (113, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 113: {}", e))?;

            info!("Successfully migrated to version 113 (task_runs.runner_port)");
        }

        // --- Migration 114: is_fixer and fixer_source_task_run_id columns on task_runs ---
        if current_version < 114 {
            info!("Migrating to version 114 (task_runs.is_fixer, fixer_source_task_run_id)...");

            conn.execute_batch(
                r#"
                ALTER TABLE task_runs ADD COLUMN is_fixer INTEGER DEFAULT 0;
                ALTER TABLE task_runs ADD COLUMN fixer_source_task_run_id TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (114, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 114: {}", e))?;

            info!("Successfully migrated to version 114 (task_runs.is_fixer)");
        }

        // Migration to version 115: Add acceptance_criteria to unified_workflows
        if current_version < 115 {
            info!("Migrating to version 115 (unified_workflows.acceptance_criteria)...");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN acceptance_criteria TEXT DEFAULT NULL;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (115, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 115: {}", e))?;

            info!("Successfully migrated to version 115 (acceptance_criteria)");
        }

        if current_version < 116 {
            info!("Migrating to version 116 (unified_workflows.ai_reviewed)...");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN ai_reviewed INTEGER DEFAULT 1;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (116, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 116: {}", e))?;

            info!("Successfully migrated to version 116 (ai_reviewed)");
        }

        if current_version < 117 {
            info!("Migrating to version 117 (worktrees table + workflow worktree/multi_agent columns)...");

            conn.execute_batch(
                r#"
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
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                );

                CREATE INDEX IF NOT EXISTS idx_worktrees_status ON worktrees(status);
                CREATE INDEX IF NOT EXISTS idx_worktrees_task_run_id ON worktrees(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_worktrees_repo_path ON worktrees(repo_path);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (117, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 117: {}", e))?;

            // Add multi_agent_mode and use_worktree to unified_workflows (idempotent)
            let cols_to_add = [
                ("multi_agent_mode", "INTEGER DEFAULT 1"),
                ("use_worktree", "INTEGER DEFAULT 0"),
            ];
            for (col_name, col_def) in &cols_to_add {
                let has_col: bool = conn
                    .prepare("PRAGMA table_info(unified_workflows)")
                    .and_then(|mut stmt| {
                        stmt.query_map([], |row| row.get::<_, String>(1))
                            .map(|rows| rows.filter_map(|r| r.ok()).any(|name| name == *col_name))
                    })
                    .unwrap_or(false);
                if !has_col {
                    conn.execute_batch(&format!(
                        "ALTER TABLE unified_workflows ADD COLUMN {} {};",
                        col_name, col_def
                    ))
                    .map_err(|e| format!("Failed to add {}: {}", col_name, e))?;
                }
            }

            info!("Successfully migrated to version 117 (worktrees + workflow columns)");
        }

        if current_version < 118 {
            info!("Migrating to version 118 (autoresearch tables)...");

            conn.execute_batch(
                r#"
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

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (118, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 118: {}", e))?;

            info!("Successfully migrated to version 118 (autoresearch tables)");
        }

        // --- Migration 119: Meta-Optimizer tables and is_meta_optimizer column on task_runs ---
        if current_version < 119 {
            info!(
                "Migrating to version 119 (meta-optimizer tables, task_runs.is_meta_optimizer)..."
            );

            conn.execute_batch(
                r#"
                ALTER TABLE task_runs ADD COLUMN is_meta_optimizer INTEGER DEFAULT 0;

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
                    cost_usd REAL NOT NULL DEFAULT 0.0,
                    downstream_success INTEGER,
                    output_quality_score REAL,
                    created_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_pipeline_agent_traces_task_run ON pipeline_agent_traces(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_pipeline_agent_traces_agent_type ON pipeline_agent_traces(agent_type);
                CREATE INDEX IF NOT EXISTS idx_pipeline_agent_traces_run_id ON pipeline_agent_traces(run_id);

                CREATE TABLE IF NOT EXISTS prompt_registry (
                    id TEXT PRIMARY KEY,
                    agent_type TEXT NOT NULL,
                    variant_name TEXT NOT NULL,
                    prompt_content TEXT NOT NULL,
                    version INTEGER NOT NULL DEFAULT 1,
                    is_active INTEGER NOT NULL DEFAULT 0,
                    source_recommendation_id TEXT,
                    performance_metrics TEXT DEFAULT '{}',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    UNIQUE(agent_type, variant_name, version)
                );

                CREATE INDEX IF NOT EXISTS idx_prompt_registry_agent_type ON prompt_registry(agent_type);
                CREATE INDEX IF NOT EXISTS idx_prompt_registry_active ON prompt_registry(agent_type, is_active);

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
                    confidence REAL NOT NULL DEFAULT 0.0,
                    status TEXT NOT NULL DEFAULT 'pending',
                    applied_at TEXT,
                    outcome_after_apply TEXT,
                    optimizer_run_id TEXT,
                    created_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_meta_optimizer_recs_type ON meta_optimizer_recommendations(optimizer_type);
                CREATE INDEX IF NOT EXISTS idx_meta_optimizer_recs_status ON meta_optimizer_recommendations(status);
                CREATE INDEX IF NOT EXISTS idx_meta_optimizer_recs_run ON meta_optimizer_recommendations(optimizer_run_id);

                CREATE TABLE IF NOT EXISTS meta_optimizer_runs (
                    id TEXT PRIMARY KEY,
                    optimizer_type TEXT NOT NULL,
                    trigger_type TEXT NOT NULL DEFAULT 'threshold',
                    runs_analyzed INTEGER NOT NULL DEFAULT 0,
                    recommendations_produced INTEGER NOT NULL DEFAULT 0,
                    task_run_id TEXT,
                    status TEXT NOT NULL DEFAULT 'running',
                    created_at TEXT NOT NULL,
                    completed_at TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_meta_optimizer_runs_type ON meta_optimizer_runs(optimizer_type);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (119, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 119: {}", e))?;

            info!("Successfully migrated to version 119 (meta-optimizer tables)");
        }

        // --- Migration 120: workflow_architecture column and meta_optimizer_snapshots table ---
        if current_version < 120 {
            info!("Migrating to version 120 (workflow_architecture, meta_optimizer_snapshots)...");

            conn.execute_batch(
                r#"
                ALTER TABLE learning_outcomes ADD COLUMN workflow_architecture TEXT;

                CREATE TABLE IF NOT EXISTS meta_optimizer_snapshots (
                    id TEXT PRIMARY KEY,
                    snapshot_type TEXT NOT NULL,
                    period_start TEXT NOT NULL,
                    period_end TEXT NOT NULL,
                    metrics_json TEXT NOT NULL,
                    breakdown_json TEXT DEFAULT '{}',
                    recommendation_id TEXT,
                    runs_included INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_meta_optimizer_snapshots_type ON meta_optimizer_snapshots(snapshot_type);
                CREATE INDEX IF NOT EXISTS idx_meta_optimizer_snapshots_rec ON meta_optimizer_snapshots(recommendation_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (120, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 120: {}", e))?;

            info!("Successfully migrated to version 120 (workflow_architecture, meta_optimizer_snapshots)");
        }

        // --- Migration 121: Canary rollouts table ---
        if current_version < 121 {
            info!("Migrating to version 121 (canary_rollouts table)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS canary_rollouts (
                    id TEXT PRIMARY KEY,
                    recommendation_id TEXT NOT NULL,
                    percentage INTEGER NOT NULL DEFAULT 10,
                    status TEXT NOT NULL DEFAULT 'active',
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

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (121, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 121: {}", e))?;

            info!("Successfully migrated to version 121 (canary_rollouts)");
        }

        // --- Migration 122: Slash command import tracking ---
        if current_version < 122 {
            info!("Migrating to version 122 (slash command import tracking)...");

            // Check which columns already exist (schema.sql may have added them for fresh DBs)
            let existing_columns: Vec<String> = conn
                .prepare("PRAGMA table_info(unified_workflows)")
                .and_then(|mut stmt| {
                    stmt.query_map([], |row| row.get::<_, String>(1))
                        .map(|rows| rows.filter_map(|r| r.ok()).collect())
                })
                .unwrap_or_default();

            if !existing_columns.contains(&"source_file_path".to_string()) {
                conn.execute_batch(
                    "ALTER TABLE unified_workflows ADD COLUMN source_file_path TEXT DEFAULT NULL;",
                )
                .map_err(|e| format!("Failed to migrate to version 122: {}", e))?;
            }

            if !existing_columns.contains(&"source_content_hash".to_string()) {
                conn.execute_batch(
                    "ALTER TABLE unified_workflows ADD COLUMN source_content_hash TEXT DEFAULT NULL;",
                )
                .map_err(|e| format!("Failed to migrate to version 122: {}", e))?;
            }

            conn.execute_batch(
                r#"
                CREATE INDEX IF NOT EXISTS idx_unified_workflows_source_file_path ON unified_workflows(source_file_path);
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (122, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 122: {}", e))?;

            info!("Successfully migrated to version 122 (source_file_path, source_content_hash)");
        }

        // --- Migration 123: Spec experimentation tables ---
        if current_version < 123 {
            info!("Migrating to version 123 (spec experimentation tables)...");

            conn.execute_batch(
                r#"
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

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (123, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 123: {}", e))?;

            info!("Successfully migrated to version 123 (spec_compliance_results, spec_accuracy_results)");
        }

        // --- Migration 124: Comparison runs table ---
        if current_version < 124 {
            info!("Migrating to version 124 (comparison_runs table)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS comparison_runs (
                    id TEXT PRIMARY KEY,
                    workflow_id TEXT NOT NULL,
                    variation_type TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'running',
                    entries_json TEXT NOT NULL DEFAULT '[]',
                    report TEXT,
                    created_at TEXT NOT NULL,
                    completed_at TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_comparison_runs_workflow ON comparison_runs(workflow_id);
                CREATE INDEX IF NOT EXISTS idx_comparison_runs_status ON comparison_runs(status);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (124, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 124: {}", e))?;

            info!("Successfully migrated to version 124 (comparison_runs)");
        }

        // --- Migration 125: Spec versioning table ---
        if current_version < 125 {
            info!("Migrating to version 125 (spec versioning)...");

            conn.execute_batch(
                r#"
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
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_spec_versions_spec ON spec_versions(spec_id);
                CREATE INDEX IF NOT EXISTS idx_spec_versions_hash ON spec_versions(content_hash);
                CREATE UNIQUE INDEX IF NOT EXISTS idx_spec_versions_num ON spec_versions(spec_id, version_number);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (125, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 125: {}", e))?;

            info!("Successfully migrated to version 125 (spec_versions)");
        }

        // --- Migration 126: Backfill workflow_architecture + complexity indicator columns ---
        if current_version < 126 {
            info!(
                "Migrating to version 126 (backfill workflow_architecture, complexity columns)..."
            );

            // Check which columns already exist (schema.sql may have added them for fresh DBs)
            let existing_columns: Vec<String> = conn
                .prepare("PRAGMA table_info(learning_outcomes)")
                .and_then(|mut stmt| {
                    stmt.query_map([], |row| row.get::<_, String>(1))
                        .map(|rows| rows.filter_map(|r| r.ok()).collect())
                })
                .unwrap_or_default();

            // Backfill NULL workflow_architecture to 'traditional'.
            // task_runs does not have config_json, so we cannot infer architecture from stored config.
            // All historical NULL records are from the traditional execution path.
            conn.execute_batch(
                r#"
                UPDATE learning_outcomes
                SET workflow_architecture = 'traditional'
                WHERE workflow_architecture IS NULL;
                "#,
            )
            .map_err(|e| format!("Failed to backfill workflow_architecture: {}", e))?;

            // Backfill error_type from error_message patterns for existing records
            conn.execute_batch(
                r#"
                UPDATE learning_outcomes
                SET error_type = CASE
                    WHEN error_message LIKE '%max iterations%' OR error_message LIKE '%Max iterations%'
                        THEN 'max_iterations_reached'
                    WHEN error_message LIKE '%Max sessions%' OR error_message LIKE '%max sessions%'
                        THEN 'max_sessions_reached'
                    WHEN error_message LIKE '%generation%' OR error_message LIKE '%schema%'
                        THEN 'generation_error'
                    WHEN error_message LIKE '%timeout%' OR error_message LIKE '%Timeout%'
                        THEN 'timeout'
                    ELSE 'runtime_error'
                END
                WHERE error_type IS NULL AND error_message IS NOT NULL AND error_message != '';
                "#,
            )
            .map_err(|e| format!("Failed to backfill error_type: {}", e))?;

            // Add complexity indicator columns
            if !existing_columns.contains(&"step_count".to_string()) {
                conn.execute_batch("ALTER TABLE learning_outcomes ADD COLUMN step_count INTEGER;")
                    .map_err(|e| format!("Failed to add step_count column: {}", e))?;
            }

            if !existing_columns.contains(&"verification_step_count".to_string()) {
                conn.execute_batch(
                    "ALTER TABLE learning_outcomes ADD COLUMN verification_step_count INTEGER;",
                )
                .map_err(|e| format!("Failed to add verification_step_count column: {}", e))?;
            }

            if !existing_columns.contains(&"agentic_step_count".to_string()) {
                conn.execute_batch(
                    "ALTER TABLE learning_outcomes ADD COLUMN agentic_step_count INTEGER;",
                )
                .map_err(|e| format!("Failed to add agentic_step_count column: {}", e))?;
            }

            if !existing_columns.contains(&"has_ui_bridge".to_string()) {
                conn.execute_batch(
                    "ALTER TABLE learning_outcomes ADD COLUMN has_ui_bridge INTEGER DEFAULT 0;",
                )
                .map_err(|e| format!("Failed to add has_ui_bridge column: {}", e))?;
            }

            conn.execute_batch(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (126, datetime('now'));",
            )
            .map_err(|e| format!("Failed to migrate to version 126: {}", e))?;

            info!("Successfully migrated to version 126 (backfill workflow_architecture, complexity columns)");
        }

        // --- Migration 127: Progressive rule loading columns ---
        if current_version < 127 {
            info!("Migrating to version 127 (generation_rules severity + failure_count)...");

            conn.execute_batch(
                r#"
                ALTER TABLE generation_rules ADD COLUMN severity TEXT NOT NULL DEFAULT 'normal';
                ALTER TABLE generation_rules ADD COLUMN failure_count INTEGER NOT NULL DEFAULT 0;
                CREATE INDEX IF NOT EXISTS idx_generation_rules_severity ON generation_rules(severity);

                -- Set severity='critical' for rules about: valid step types, JSON output, valid UUID, deterministic verification
                UPDATE generation_rules SET severity = 'critical'
                    WHERE status = 'active' AND (
                        title LIKE '%Only 3 step types%'
                        OR title LIKE '%step types exist%'
                        OR title LIKE '%JSON only%'
                        OR title LIKE '%valid UUID%'
                        OR title LIKE '%Generate valid UUID%'
                        OR title LIKE '%Deterministic verification%'
                        OR title LIKE '%deterministic%automated step%'
                        OR title LIKE '%command step MUST include a mode%'
                    );

                -- Set severity='important' for rules about: typecheck, SDK content, test_type usage
                UPDATE generation_rules SET severity = 'important'
                    WHERE status = 'active' AND severity = 'normal' AND (
                        title LIKE '%typecheck%'
                        OR title LIKE '%Code modification requires%'
                        OR title LIKE '%SDK%'
                        OR title LIKE '%Web app verification%'
                        OR title LIKE '%test_type%'
                        OR title LIKE '%Test steps with inline%'
                        OR title LIKE '%Retry format%'
                        OR title LIKE '%Data flow references%'
                    );

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (127, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 127: {}", e))?;

            info!(
                "Successfully migrated to version 127 (generation_rules severity + failure_count)"
            );
        }

        // --- Migration 128: Cross-iteration context compression column ---
        if current_version < 128 {
            info!("Migrating to version 128 (iteration_history column on task_runs)...");

            conn.execute_batch(
                r#"
                ALTER TABLE task_runs ADD COLUMN iteration_history TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (128, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 128: {}", e))?;

            info!("Successfully migrated to version 128 (iteration_history)");
        }

        // --- Migration 129: Add workflow_architecture column to unified_workflows ---
        if current_version < 129 {
            info!("Migrating to version 129 (workflow_architecture on unified_workflows)...");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN workflow_architecture TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (129, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 129: {}", e))?;

            info!(
                "Successfully migrated to version 129 (workflow_architecture on unified_workflows)"
            );
        }

        // --- Migration 130: Add total_tokens and total_cost_usd to learning_outcomes ---
        if current_version < 130 {
            info!(
                "Migrating to version 130 (total_tokens, total_cost_usd on learning_outcomes)..."
            );

            conn.execute_batch(
                r#"
                ALTER TABLE learning_outcomes ADD COLUMN total_tokens INTEGER;
                ALTER TABLE learning_outcomes ADD COLUMN total_cost_usd REAL;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (130, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 130: {}", e))?;

            info!("Successfully migrated to version 130 (total_tokens, total_cost_usd on learning_outcomes)");
        }

        // --- Migration 131: Add examples_json to generation_rules ---
        if current_version < 131 {
            info!("Migrating to version 131 (examples_json on generation_rules)...");

            conn.execute_batch(
                r#"
                ALTER TABLE generation_rules ADD COLUMN examples_json TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (131, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 131: {}", e))?;

            info!("Successfully migrated to version 131 (examples_json on generation_rules)");
        }

        // --- Migration 132: Add span hierarchy and guardrail columns to pipeline_agent_traces ---
        if current_version < 132 {
            info!("Migrating to version 132 (span hierarchy + guardrail columns on pipeline_agent_traces)...");

            conn.execute_batch(
                r#"
                ALTER TABLE pipeline_agent_traces ADD COLUMN parent_span_id TEXT;
                ALTER TABLE pipeline_agent_traces ADD COLUMN span_type TEXT DEFAULT 'agent';
                ALTER TABLE pipeline_agent_traces ADD COLUMN guardrail_results_json TEXT;
                ALTER TABLE pipeline_agent_traces ADD COLUMN handoff_context_json TEXT;

                CREATE INDEX IF NOT EXISTS idx_pipeline_traces_parent_span
                    ON pipeline_agent_traces(parent_span_id);
                CREATE INDEX IF NOT EXISTS idx_pipeline_traces_span_type
                    ON pipeline_agent_traces(span_type);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (132, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 132: {}", e))?;

            info!("Successfully migrated to version 132 (span hierarchy + guardrail columns on pipeline_agent_traces)");
        }

        // --- Migration 133: Agentic metric scores and baselines tables ---
        if current_version < 133 {
            info!("Migrating to version 133 (agentic metric scores + baselines tables, composite_agentic_score column)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS agentic_metric_scores (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    metric_type TEXT NOT NULL,
                    score REAL NOT NULL,
                    confidence REAL NOT NULL DEFAULT 1.0,
                    rationale TEXT,
                    is_llm_judged INTEGER NOT NULL DEFAULT 0,
                    model_used TEXT,
                    created_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_agentic_scores_task_run ON agentic_metric_scores(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_agentic_scores_metric ON agentic_metric_scores(metric_type);
                CREATE UNIQUE INDEX IF NOT EXISTS idx_agentic_scores_unique ON agentic_metric_scores(task_run_id, metric_type);

                CREATE TABLE IF NOT EXISTS agentic_metric_baselines (
                    id TEXT PRIMARY KEY,
                    workflow_id TEXT,
                    metric_type TEXT NOT NULL,
                    baseline_value TEXT NOT NULL,
                    sample_count INTEGER NOT NULL DEFAULT 0,
                    updated_at TEXT NOT NULL
                );

                CREATE UNIQUE INDEX IF NOT EXISTS idx_baselines_unique ON agentic_metric_baselines(workflow_id, metric_type);

                ALTER TABLE learning_outcomes ADD COLUMN composite_agentic_score REAL;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (133, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 133: {}", e))?;

            info!(
                "Successfully migrated to version 133 (agentic metric scores + baselines tables)"
            );
        }

        // --- Migration 134: Eval specs, eval results, recommendation eval columns ---
        if current_version < 134 {
            info!("Migrating to version 134 (eval specs + eval results tables, recommendation eval columns)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS eval_specs (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    target_agent TEXT,
                    spec_json TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS eval_results (
                    id TEXT PRIMARY KEY,
                    spec_id TEXT NOT NULL,
                    recommendation_id TEXT,
                    status TEXT NOT NULL DEFAULT 'running',
                    result_json TEXT NOT NULL DEFAULT '{}',
                    p_value REAL,
                    trials_run INTEGER DEFAULT 0,
                    created_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_eval_results_spec ON eval_results(spec_id);
                CREATE INDEX IF NOT EXISTS idx_eval_results_rec ON eval_results(recommendation_id);

                ALTER TABLE meta_optimizer_recommendations ADD COLUMN eval_result_id TEXT;
                ALTER TABLE meta_optimizer_recommendations ADD COLUMN eval_status TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (134, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 134: {}", e))?;

            info!("Successfully migrated to version 134 (eval specs + eval results)");
        }

        // Migration to version 135: Add artifacts table for UI Bridge IPC artifact persistence
        if current_version < 135 {
            info!("Migrating to version 135 (artifacts table for UI Bridge IPC)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS artifacts (
                    artifact_id TEXT PRIMARY KEY,
                    source_json TEXT NOT NULL,
                    result_json TEXT NOT NULL,
                    environment_json TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    passed INTEGER
                );
                CREATE INDEX IF NOT EXISTS idx_artifacts_created_at ON artifacts(created_at);
                CREATE INDEX IF NOT EXISTS idx_artifacts_passed ON artifacts(passed);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (135, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 135: {}", e))?;

            info!("Successfully migrated to version 135 (artifacts table)");
        }

        // --- Migration 136: Golden datasets + robustness reports tables ---
        if current_version < 136 {
            info!("Migrating to version 136 (golden datasets + robustness reports)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS golden_datasets (
                    id TEXT PRIMARY KEY,
                    agent_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    entries_json TEXT NOT NULL DEFAULT '[]',
                    entry_count INTEGER DEFAULT 0,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_golden_agent ON golden_datasets(agent_type);

                CREATE TABLE IF NOT EXISTS robustness_reports (
                    id TEXT PRIMARY KEY,
                    prompt_variant_id TEXT,
                    recommendation_id TEXT,
                    total_tests INTEGER NOT NULL,
                    passed INTEGER NOT NULL,
                    failed INTEGER NOT NULL,
                    report_json TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_robustness_variant ON robustness_reports(prompt_variant_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (136, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 136: {}", e))?;

            info!("Successfully migrated to version 136 (golden datasets + robustness reports)");
        }

        // --- Migration 137: Auto-tagging enrichment columns for learning outcomes ---
        if current_version < 137 {
            info!("Migrating to version 137 (learning outcome auto-tagging columns)...");

            // Check which columns already exist (schema.sql may have added them for fresh DBs)
            let existing_columns: Vec<String> = conn
                .prepare("PRAGMA table_info(learning_outcomes)")
                .and_then(|mut stmt| {
                    stmt.query_map([], |row| row.get::<_, String>(1))
                        .map(|rows| rows.filter_map(|r| r.ok()).collect())
                })
                .unwrap_or_default();

            if !existing_columns.contains(&"technology_tags".to_string()) {
                conn.execute_batch(
                    "ALTER TABLE learning_outcomes ADD COLUMN technology_tags TEXT;",
                )
                .map_err(|e| format!("Failed to add technology_tags column: {}", e))?;
            }

            if !existing_columns.contains(&"domain_tags".to_string()) {
                conn.execute_batch(
                    "ALTER TABLE learning_outcomes ADD COLUMN domain_tags TEXT;",
                )
                .map_err(|e| format!("Failed to add domain_tags column: {}", e))?;
            }

            if !existing_columns.contains(&"complexity_tier".to_string()) {
                conn.execute_batch(
                    "ALTER TABLE learning_outcomes ADD COLUMN complexity_tier TEXT;",
                )
                .map_err(|e| format!("Failed to add complexity_tier column: {}", e))?;
            }

            // Backfill complexity_tier for existing records based on step_count, iterations, duration
            conn.execute_batch(
                r#"
                UPDATE learning_outcomes
                SET complexity_tier = CASE
                    WHEN (
                        (CASE WHEN COALESCE(step_count, 0) > 15 THEN 3
                              WHEN COALESCE(step_count, 0) > 8 THEN 2
                              WHEN COALESCE(step_count, 0) > 3 THEN 1 ELSE 0 END)
                        + (CASE WHEN COALESCE(agentic_step_count, 0) > 5 THEN 2
                                WHEN COALESCE(agentic_step_count, 0) > 2 THEN 1 ELSE 0 END)
                        + (CASE WHEN COALESCE(iterations, 0) > 5 THEN 2
                                WHEN COALESCE(iterations, 0) > 2 THEN 1 ELSE 0 END)
                        + (CASE WHEN COALESCE(duration_secs, 0) > 600 THEN 2
                                WHEN COALESCE(duration_secs, 0) > 120 THEN 1 ELSE 0 END)
                    ) >= 5 THEN 'complex'
                    WHEN (
                        (CASE WHEN COALESCE(step_count, 0) > 15 THEN 3
                              WHEN COALESCE(step_count, 0) > 8 THEN 2
                              WHEN COALESCE(step_count, 0) > 3 THEN 1 ELSE 0 END)
                        + (CASE WHEN COALESCE(agentic_step_count, 0) > 5 THEN 2
                                WHEN COALESCE(agentic_step_count, 0) > 2 THEN 1 ELSE 0 END)
                        + (CASE WHEN COALESCE(iterations, 0) > 5 THEN 2
                                WHEN COALESCE(iterations, 0) > 2 THEN 1 ELSE 0 END)
                        + (CASE WHEN COALESCE(duration_secs, 0) > 600 THEN 2
                                WHEN COALESCE(duration_secs, 0) > 120 THEN 1 ELSE 0 END)
                    ) >= 2 THEN 'moderate'
                    ELSE 'simple'
                END
                WHERE complexity_tier IS NULL;
                "#,
            )
            .map_err(|e| format!("Failed to backfill complexity_tier: {}", e))?;

            // Backfill technology_tags from files_modified JSON where available
            // For records with non-empty files_modified, infer tags from file extensions
            // Note: SQLite JSON functions are limited, so we use simple LIKE patterns
            conn.execute_batch(
                r#"
                UPDATE learning_outcomes
                SET technology_tags = '[]'
                WHERE technology_tags IS NULL AND (files_modified IS NULL OR files_modified = '[]');
                "#,
            )
            .map_err(|e| format!("Failed to backfill empty technology_tags: {}", e))?;

            // Backfill domain_tags — set empty for records without files_modified
            conn.execute_batch(
                r#"
                UPDATE learning_outcomes
                SET domain_tags = '[]'
                WHERE domain_tags IS NULL AND (files_modified IS NULL OR files_modified = '[]');
                "#,
            )
            .map_err(|e| format!("Failed to backfill empty domain_tags: {}", e))?;

            conn.execute_batch(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (137, datetime('now'));",
            )
            .map_err(|e| format!("Failed to update schema version to 137: {}", e))?;

            info!("Successfully migrated to version 137 (learning outcome auto-tagging)");
        }

        // Migration to version 138: Add pipeline_checkpoint column to task_runs
        if current_version < 138 {
            info!("Migrating to version 138 (pipeline_checkpoint column on task_runs)...");
            conn.execute_batch(
                r#"
                -- Pipeline checkpoint JSON for resuming multi-agent pipeline from last completed phase
                ALTER TABLE task_runs ADD COLUMN pipeline_checkpoint TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (138, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 138: {}", e))?;

            info!("Successfully migrated to version 138 (pipeline_checkpoint column added)");
        }

        // Repair migration: is_favorite column may be missing on databases created from
        // schema.sql (which set version >= 94, skipping migration 92 that adds the column).
        // This is idempotent — ALTER TABLE ADD COLUMN fails if the column already exists,
        // so we check for it first via PRAGMA table_info.
        {
            let has_is_favorite: bool = conn
                .prepare("PRAGMA table_info(unified_workflows)")
                .and_then(|mut stmt| {
                    stmt.query_map([], |row| row.get::<_, String>(1))
                        .map(|rows| {
                            rows.filter_map(|r| r.ok())
                                .any(|name| name == "is_favorite")
                        })
                })
                .unwrap_or(false);

            if !has_is_favorite {
                info!("Repair: adding missing is_favorite column to unified_workflows");
                conn.execute_batch(
                    r#"
                    ALTER TABLE unified_workflows ADD COLUMN is_favorite INTEGER DEFAULT 0;
                    CREATE INDEX IF NOT EXISTS idx_unified_workflows_is_favorite ON unified_workflows(is_favorite);
                    "#,
                )
                .map_err(|e| format!("Failed to repair unified_workflows.is_favorite: {}", e))?;
                info!("Repair: successfully added is_favorite column");
            }
        }

        // Repair migration: cached_app_specs table may be missing on databases created from
        // schema.sql before the table was added (schema.sql skipped migration 69).
        {
            let has_table: bool = conn
                .prepare(
                    "SELECT name FROM sqlite_master WHERE type='table' AND name='cached_app_specs'",
                )
                .and_then(|mut stmt| {
                    stmt.query_map([], |row| row.get::<_, String>(0))
                        .map(|rows| rows.filter_map(|r| r.ok()).count() > 0)
                })
                .unwrap_or(false);

            if !has_table {
                info!("Repair: creating missing cached_app_specs table");
                conn.execute_batch(
                    r#"
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
                    "#,
                )
                .map_err(|e| format!("Failed to repair cached_app_specs: {}", e))?;
                info!("Repair: successfully created cached_app_specs table");
            }
        }

        Ok(())
    }
}
