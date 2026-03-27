//! PostgreSQL database layer for qontinui-runner.
//!
//! Runs alongside SQLite during migration. New tables and queries are added here
//! via Clorinde-generated code. Callers prefer PG when available, fall back to SQLite.

pub mod activity_timeline;
pub mod agentic_metrics;
pub mod graph_ops;
pub mod approval_gates;
pub mod error_monitor;
pub mod canary;
pub mod checks;
pub mod findings;
pub mod knowledge;
pub mod learning;
pub mod misc_crud;
pub mod observations;
pub mod prompt_evolution;
pub mod prompt_registry;
pub mod q_routing;
pub mod queued_workflows;
pub mod settings;
pub mod skills;
pub mod task_run_events;
pub mod task_runs;
pub mod token_analytics;
pub mod token_usage;
pub mod ui_bridge;
pub mod watchers;
pub mod workflow_state;
pub mod workflows;
pub mod ai_sessions;
pub mod checkpoints;
pub mod flows;
pub mod worktrees;
pub mod export;
pub mod data_migration;

use tracing::{info, warn};

/// PostgreSQL connection pool backed by deadpool-postgres.
pub struct PgDb {
    pool: deadpool_postgres::Pool,
}

impl PgDb {
    /// Connect to PostgreSQL using the given connection string.
    ///
    /// The connection string should NOT include search_path — it's set per-connection
    /// via the pool's post_create hook to avoid parsing issues.
    ///
    /// Returns `Err` if the connection string is invalid or initial connectivity fails.
    pub async fn new(database_url: &str) -> Result<Self, String> {
        let pg_config: tokio_postgres::Config = database_url
            .parse()
            .map_err(|e| format!("Invalid PostgreSQL connection string: {}", e))?;

        let mgr_config = deadpool_postgres::ManagerConfig {
            recycling_method: deadpool_postgres::RecyclingMethod::Fast,
        };

        let mgr = deadpool_postgres::Manager::from_config(pg_config, tokio_postgres::NoTls, mgr_config);

        let pool = deadpool_postgres::Pool::builder(mgr)
            .max_size(16)
            .post_create(deadpool_postgres::Hook::async_fn(|conn, _| {
                Box::pin(async move {
                    conn.simple_query("SET search_path TO runner, public")
                        .await
                        .map_err(|e| {
                            deadpool_postgres::HookError::Message(
                                format!("Failed to set search_path: {}", e).into(),
                            )
                        })?;
                    Ok(())
                })
            }))
            .build()
            .map_err(|e| format!("Failed to create PG pool: {}", e))?;

        // Verify connectivity and schema
        let conn = pool
            .get()
            .await
            .map_err(|e| format!("PostgreSQL connection failed: {}", e))?;

        // Verify the runner schema exists (search_path is already set by post_create)
        conn.query_one(
                "SELECT 1 FROM information_schema.schemata WHERE schema_name = 'runner'",
                &[],
            )
            .await
            .map_err(|e| format!("Runner schema not found in PostgreSQL: {}", e))?;

        info!("PostgreSQL connected (deadpool, max_size=16, schema=runner)");

        let db = Self { pool };
        db.ensure_tables().await;
        Ok(db)
    }

    /// Ensure all required tables exist. Runs CREATE TABLE IF NOT EXISTS
    /// for tables managed by this module (activity_timeline, watchers, etc.).
    /// This is idempotent — safe to call on every startup.
    async fn ensure_tables(&self) {
        let conn = match self.pool.get().await {
            Ok(c) => c,
            Err(e) => {
                warn!("Cannot ensure tables — PG pool error: {}", e);
                return;
            }
        };

        let ddl = [
            // Activity Timeline
            "CREATE TABLE IF NOT EXISTS activity_timeline (
                id              BIGSERIAL PRIMARY KEY,
                text_content    TEXT NOT NULL,
                content_hash    TEXT NOT NULL,
                source_type     TEXT NOT NULL,
                capture_mode    TEXT NOT NULL,
                app_name        TEXT,
                window_title    TEXT,
                url             TEXT,
                task_run_id     TEXT REFERENCES task_runs(id) ON DELETE SET NULL,
                screenshot_path TEXT,
                element_count   INTEGER,
                confidence      DOUBLE PRECISION,
                metadata_json   TEXT,
                duplicate_count INTEGER NOT NULL DEFAULT 0,
                is_deleted      BOOLEAN NOT NULL DEFAULT false,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_at_content_hash ON activity_timeline(content_hash)",
            "CREATE INDEX IF NOT EXISTS idx_at_created_at ON activity_timeline(created_at)",
            "CREATE INDEX IF NOT EXISTS idx_at_task_run ON activity_timeline(task_run_id) WHERE task_run_id IS NOT NULL",
            "CREATE INDEX IF NOT EXISTS idx_at_app_name ON activity_timeline(app_name) WHERE app_name IS NOT NULL",
            "CREATE INDEX IF NOT EXISTS idx_at_source_type ON activity_timeline(source_type) WHERE NOT is_deleted",
            "CREATE INDEX IF NOT EXISTS idx_at_fts ON activity_timeline USING GIN (to_tsvector('english', text_content)) WHERE NOT is_deleted",
            // Watchers
            "CREATE TABLE IF NOT EXISTS watchers (
                id                  TEXT PRIMARY KEY,
                name                TEXT NOT NULL,
                schedule_json       TEXT NOT NULL,
                timeline_query      TEXT NOT NULL,
                app_name_filter     TEXT,
                source_type_filter  TEXT,
                lookback_window     TEXT NOT NULL DEFAULT '15 minutes',
                reasoning_prompt    TEXT NOT NULL,
                action_json         TEXT NOT NULL,
                enabled             BOOLEAN NOT NULL DEFAULT true,
                last_run_at         TIMESTAMPTZ,
                last_result_json    TEXT,
                created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_watchers_enabled ON watchers(enabled) WHERE enabled",
            // Agentic Metric Scores
            "CREATE TABLE IF NOT EXISTS agentic_metric_scores (
                id              TEXT PRIMARY KEY,
                task_run_id     TEXT NOT NULL,
                metric_type     TEXT NOT NULL,
                score           DOUBLE PRECISION NOT NULL,
                confidence      DOUBLE PRECISION NOT NULL,
                rationale       TEXT,
                is_llm_judged   BOOLEAN NOT NULL DEFAULT false,
                model_used      TEXT,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_ams_task_run ON agentic_metric_scores(task_run_id)",
            "CREATE INDEX IF NOT EXISTS idx_ams_metric_type ON agentic_metric_scores(metric_type)",
            "CREATE INDEX IF NOT EXISTS idx_ams_created_at ON agentic_metric_scores(created_at)",
            // Prompt Registry
            "CREATE TABLE IF NOT EXISTS prompt_registry (
                id                          TEXT PRIMARY KEY,
                agent_type                  TEXT NOT NULL,
                variant_name                TEXT NOT NULL,
                prompt_content              TEXT NOT NULL,
                version                     INTEGER NOT NULL,
                is_active                   BOOLEAN NOT NULL DEFAULT false,
                source_recommendation_id    TEXT,
                performance_metrics         TEXT,
                created_at                  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at                  TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_pr_agent_type ON prompt_registry(agent_type)",
            "CREATE INDEX IF NOT EXISTS idx_pr_active ON prompt_registry(agent_type, is_active) WHERE is_active",
            // Canary Rollouts
            "CREATE TABLE IF NOT EXISTS canary_rollouts (
                id                      TEXT PRIMARY KEY,
                recommendation_id       TEXT NOT NULL,
                percentage              BIGINT NOT NULL,
                status                  TEXT NOT NULL DEFAULT 'active',
                start_date              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                end_date                TIMESTAMPTZ,
                baseline_run_count      BIGINT NOT NULL DEFAULT 0,
                canary_run_count        BIGINT NOT NULL DEFAULT 0,
                baseline_metrics_json   TEXT NOT NULL DEFAULT '{}',
                canary_metrics_json     TEXT NOT NULL DEFAULT '{}',
                created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_cr_status ON canary_rollouts(status) WHERE status = 'active'",
            "CREATE INDEX IF NOT EXISTS idx_cr_recommendation ON canary_rollouts(recommendation_id)",
            // Canary Run Records
            "CREATE TABLE IF NOT EXISTS canary_run_records (
                id              TEXT PRIMARY KEY,
                canary_id       TEXT NOT NULL REFERENCES canary_rollouts(id) ON DELETE CASCADE,
                is_canary       BOOLEAN NOT NULL,
                task_run_id     TEXT,
                success         BOOLEAN,
                cost_usd        DOUBLE PRECISION,
                duration_ms     DOUBLE PRECISION,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_crr_canary ON canary_run_records(canary_id)",
            // Prompt Template Canaries
            "CREATE TABLE IF NOT EXISTS prompt_template_canaries (
                id                      TEXT PRIMARY KEY,
                template_id             TEXT NOT NULL,
                baseline_version        INTEGER NOT NULL,
                candidate_version       INTEGER NOT NULL,
                traffic_percentage      DOUBLE PRECISION NOT NULL,
                status                  TEXT NOT NULL DEFAULT 'active',
                baseline_metrics_json   TEXT NOT NULL DEFAULT '{}',
                candidate_metrics_json  TEXT NOT NULL DEFAULT '{}',
                created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                ended_at                TIMESTAMPTZ
            )",
            "CREATE INDEX IF NOT EXISTS idx_ptc_template ON prompt_template_canaries(template_id)",
            "CREATE INDEX IF NOT EXISTS idx_ptc_active ON prompt_template_canaries(template_id, status) WHERE status = 'active'",
            // Task Knowledge (knowledge acquisition flywheel)
            "CREATE TABLE IF NOT EXISTS task_knowledge (
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
                content_embedding       BYTEA,
                created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_tk_task_run ON task_knowledge(task_run_id)",
            "CREATE INDEX IF NOT EXISTS idx_tk_category ON task_knowledge(category)",
            "CREATE INDEX IF NOT EXISTS idx_tk_resolved ON task_knowledge(is_resolved) WHERE NOT is_resolved",
            "CREATE INDEX IF NOT EXISTS idx_tk_created ON task_knowledge(created_at DESC)",
            // Prompt Evolution (meta-prompt optimizer history)
            "CREATE TABLE IF NOT EXISTS prompt_evolution (
                id                      TEXT PRIMARY KEY,
                agent_type              TEXT NOT NULL,
                parent_variant_id       TEXT,
                variant_id              TEXT NOT NULL,
                recommendation_id       TEXT,
                critique                TEXT,
                changes_summary         TEXT,
                canary_verdict          TEXT,
                score_before            DOUBLE PRECISION,
                score_after             DOUBLE PRECISION,
                baseline_prompt_hash    TEXT,
                consecutive_rejections  INTEGER DEFAULT 0,
                created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_pe_agent ON prompt_evolution(agent_type)",
            "CREATE INDEX IF NOT EXISTS idx_pe_verdict ON prompt_evolution(agent_type, canary_verdict)",
            "CREATE INDEX IF NOT EXISTS idx_pe_variant ON prompt_evolution(variant_id)",
            // Error Events (application log error detection)
            "CREATE TABLE IF NOT EXISTS error_events (
                id              BIGSERIAL PRIMARY KEY,
                log_source_id   BIGINT,
                log_source_name TEXT NOT NULL,
                task_run_id     TEXT REFERENCES task_runs(id) ON DELETE SET NULL,
                workflow_step_id TEXT,
                log_timestamp   TIMESTAMPTZ,
                captured_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                severity        TEXT NOT NULL DEFAULT 'error',
                error_type      TEXT,
                error_code      TEXT,
                message         TEXT NOT NULL,
                stack_trace     TEXT,
                context_lines   TEXT,
                raw_entry       TEXT,
                file_path       TEXT,
                line_number     INTEGER,
                column_number   INTEGER,
                function_name   TEXT,
                signature_hash  TEXT NOT NULL,
                occurrence_count INTEGER DEFAULT 1,
                first_seen_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                last_seen_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                status          TEXT DEFAULT 'new',
                finding_id      TEXT,
                resolved_by_task_run_id TEXT,
                resolved_by_fix_id TEXT,
                resolution_notes TEXT,
                message_embedding BYTEA,
                trace_id        TEXT,
                acknowledged_at TIMESTAMPTZ,
                resolved_at     TIMESTAMPTZ
            )",
            "CREATE INDEX IF NOT EXISTS idx_error_events_log_source ON error_events(log_source_id)",
            "CREATE INDEX IF NOT EXISTS idx_error_events_task_run ON error_events(task_run_id)",
            "CREATE INDEX IF NOT EXISTS idx_error_events_signature ON error_events(signature_hash)",
            "CREATE INDEX IF NOT EXISTS idx_error_events_status ON error_events(status)",
            "CREATE INDEX IF NOT EXISTS idx_error_events_severity ON error_events(severity)",
            "CREATE INDEX IF NOT EXISTS idx_error_events_captured ON error_events(captured_at DESC)",
            "CREATE INDEX IF NOT EXISTS idx_error_events_last_seen ON error_events(last_seen_at DESC)",
            "CREATE INDEX IF NOT EXISTS idx_error_events_source_name ON error_events(log_source_name)",
            "CREATE INDEX IF NOT EXISTS idx_error_events_trace_id ON error_events(trace_id)",
            // Workflow Verification Phase Results
            "CREATE TABLE IF NOT EXISTS workflow_verification_phase_results (
                id              TEXT PRIMARY KEY,
                task_run_id     TEXT NOT NULL,
                iteration       INTEGER NOT NULL,
                all_passed      BOOLEAN NOT NULL,
                total_steps     INTEGER NOT NULL,
                passed_steps    INTEGER NOT NULL,
                failed_steps    INTEGER NOT NULL,
                skipped_steps   INTEGER NOT NULL,
                total_duration_ms BIGINT NOT NULL,
                critical_failure BOOLEAN NOT NULL DEFAULT false,
                result_json     TEXT NOT NULL,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_wf_ver_phase_unique ON workflow_verification_phase_results(task_run_id, iteration)",
            // Workflow AI Sessions
            "CREATE TABLE IF NOT EXISTS workflow_ai_sessions (
                id                      BIGSERIAL PRIMARY KEY,
                task_run_id             TEXT NOT NULL,
                iteration               INTEGER NOT NULL,
                phase                   TEXT NOT NULL,
                stage_index             INTEGER,
                claude_cli_session_id   TEXT,
                session_started_at      TIMESTAMPTZ NOT NULL,
                session_completed_at    TIMESTAMPTZ,
                output_length           INTEGER NOT NULL DEFAULT 0,
                status                  TEXT NOT NULL DEFAULT 'running'
            )",
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_wf_ai_sessions_unique ON workflow_ai_sessions(task_run_id, iteration, phase, COALESCE(stage_index, -1))",
            "CREATE INDEX IF NOT EXISTS idx_wf_ai_sessions_task_run ON workflow_ai_sessions(task_run_id)",
            // Worktrees
            "CREATE TABLE IF NOT EXISTS worktrees (
                id              TEXT PRIMARY KEY,
                worktree_path   TEXT NOT NULL,
                branch_name     TEXT NOT NULL,
                source_branch   TEXT NOT NULL,
                source_commit   TEXT NOT NULL,
                repo_path       TEXT NOT NULL,
                task_run_id     TEXT,
                workflow_name   TEXT,
                status          TEXT NOT NULL DEFAULT 'active',
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_worktrees_status ON worktrees(status)",
            "CREATE INDEX IF NOT EXISTS idx_worktrees_task_run ON worktrees(task_run_id)",
            // Workflow Constraint Results
            "CREATE TABLE IF NOT EXISTS workflow_constraint_results (
                id              BIGSERIAL PRIMARY KEY,
                task_run_id     TEXT NOT NULL,
                iteration       INTEGER NOT NULL,
                constraint_id   TEXT NOT NULL,
                constraint_name TEXT NOT NULL,
                passed          BOOLEAN NOT NULL,
                severity        TEXT NOT NULL,
                violations_json TEXT,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_wf_constraint_task_run ON workflow_constraint_results(task_run_id)",
            // Active Workflows (checkpoint storage)
            "CREATE TABLE IF NOT EXISTS active_workflows (
                id              BIGSERIAL PRIMARY KEY,
                workflow_name   TEXT NOT NULL UNIQUE,
                checkpoint_data TEXT NOT NULL,
                run_id          TEXT NOT NULL,
                phase_field     TEXT NOT NULL DEFAULT 'current_phase',
                completion_value INTEGER NOT NULL DEFAULT 1,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL,
                completed       BOOLEAN NOT NULL DEFAULT false
            )",
            // Orchestrator Flows
            "CREATE TABLE IF NOT EXISTS orchestrator_flows (
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
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL
            )",
            "CREATE INDEX IF NOT EXISTS idx_orch_flows_name ON orchestrator_flows(name)",
            // Flow Executions
            "CREATE TABLE IF NOT EXISTS flow_executions (
                instance_id     TEXT PRIMARY KEY,
                flow_id         TEXT NOT NULL,
                current_step    TEXT,
                status          TEXT NOT NULL DEFAULT 'pending',
                context         TEXT,
                history         TEXT,
                error           TEXT,
                started_at      TEXT NOT NULL,
                completed_at    TEXT
            )",
            "CREATE INDEX IF NOT EXISTS idx_flow_exec_flow ON flow_executions(flow_id)",
            "CREATE INDEX IF NOT EXISTS idx_flow_exec_status ON flow_executions(status)",
            // Flow Versions
            "CREATE TABLE IF NOT EXISTS flow_versions (
                id              TEXT PRIMARY KEY,
                flow_id         TEXT NOT NULL,
                version         INTEGER NOT NULL,
                definition      TEXT NOT NULL,
                message         TEXT,
                created_by      TEXT,
                created_at      TEXT NOT NULL,
                UNIQUE(flow_id, version)
            )",
            "CREATE INDEX IF NOT EXISTS idx_flow_versions_flow_id ON flow_versions(flow_id)",
            "CREATE INDEX IF NOT EXISTS idx_flow_versions_flow_version ON flow_versions(flow_id, version)",
            // Orchestrator Checkpoints
            "CREATE TABLE IF NOT EXISTS orchestrator_checkpoints (
                id              TEXT PRIMARY KEY,
                task_id         TEXT NOT NULL,
                iteration       INTEGER NOT NULL DEFAULT 0,
                trigger         TEXT NOT NULL,
                state           TEXT NOT NULL DEFAULT '{}',
                name            TEXT,
                created_at      TEXT NOT NULL
            )",
            "CREATE INDEX IF NOT EXISTS idx_orch_checkpoints_task ON orchestrator_checkpoints(task_id)",
            "CREATE INDEX IF NOT EXISTS idx_orch_checkpoints_task_iter ON orchestrator_checkpoints(task_id, iteration)",
        ];

        for sql in &ddl {
            if let Err(e) = conn.execute(*sql, &[]).await {
                warn!("DDL execution failed (non-fatal): {}", e);
            }
        }

        info!("All managed PG tables ensured (activity_timeline, watchers, agentic_metric_scores, prompt_registry, canary_rollouts, canary_run_records, prompt_template_canaries, error_events)");
    }

    /// Get a reference to the connection pool.
    pub fn pool(&self) -> &deadpool_postgres::Pool {
        &self.pool
    }

    /// Try to connect to PostgreSQL. Returns None with a warning if unavailable.
    /// Used during startup for graceful degradation to SQLite.
    pub async fn try_new(database_url: &str) -> Option<Self> {
        match Self::new(database_url).await {
            Ok(db) => Some(db),
            Err(e) => {
                warn!("PostgreSQL unavailable, using SQLite only: {}", e);
                None
            }
        }
    }

    /// One-time migration of token usage data from SQLite to PostgreSQL.
    /// Skips if PG already has data. Called once on startup.
    pub async fn migrate_token_data_from_sqlite(
        &self,
        sqlite_db: &crate::database::CheckpointDb,
    ) -> Result<u64, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        // Skip if PG already has data
        let count: i64 = conn
            .query_one("SELECT COUNT(*) FROM phase_token_usage", &[])
            .await
            .map_err(|e| format!("PG count query failed: {}", e))?
            .get(0);

        if count > 0 {
            info!("PG phase_token_usage already has {} rows, skipping migration", count);
            return Ok(0);
        }

        // Read all token usage from SQLite
        let sqlite_rows = sqlite_db.get_all_phase_token_usage_for_migration()
            .unwrap_or_else(|e| {
                warn!("Failed to read SQLite token data for migration: {}", e);
                Vec::new()
            });

        if sqlite_rows.is_empty() {
            info!("No SQLite token data to migrate");
            return Ok(0);
        }

        // Ensure task_run stubs exist in PG
        let mut task_run_ids = std::collections::HashSet::new();
        for row in &sqlite_rows {
            task_run_ids.insert(row.task_run_id.as_str());
        }
        for task_run_id in &task_run_ids {
            conn.execute(
                "INSERT INTO task_runs (id) VALUES ($1) ON CONFLICT DO NOTHING",
                &[task_run_id],
            )
            .await
            .map_err(|e| format!("PG insert task_run stub: {}", e))?;
        }

        // Batch insert token usage
        let mut migrated = 0u64;
        for row in &sqlite_rows {
            let stage_i = row.stage_index.map(|v| v as i32);
            let iter_i = row.iteration.map(|v| v as i32);
            let input_i = row.input_tokens as i64;
            let output_i = row.output_tokens as i64;
            let cost_i = row.cost_cents as i64;
            let dur_i = row.duration_ms.map(|v| v as i64);

            conn.execute(
                "INSERT INTO phase_token_usage (task_run_id, phase, stage_index, iteration, model_used, provider_used, input_tokens, output_tokens, cost_cents, duration_ms, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11::timestamptz)",
                &[
                    &row.task_run_id, &row.phase, &stage_i, &iter_i,
                    &row.model_used, &row.provider_used,
                    &input_i, &output_i, &cost_i, &dur_i,
                    &row.created_at,
                ],
            )
            .await
            .map_err(|e| format!("PG insert migration row: {}", e))?;
            migrated += 1;
        }

        info!("Migrated {} token usage rows from SQLite to PostgreSQL", migrated);
        Ok(migrated)
    }
}
