//! PostgreSQL database layer for qontinui-runner.
//!
//! Runs alongside SQLite during migration. New tables and queries are added here
//! via Clorinde-generated code. Callers prefer PG when available, fall back to SQLite.

pub mod activity_timeline;
pub mod agentic_metrics;
pub mod cached_specs;
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
pub mod decision_trail;
pub mod generation;
pub mod generation_artifacts;
pub mod generation_feedback;
pub mod meta_optimizer;
pub mod pipeline_traces;
pub mod reflection;
pub mod log_sources;
pub mod known_issues;
pub mod state_machine;
pub mod process_sessions;
pub mod canvas;
pub mod orchestration_loop;
pub mod scheduler;
pub mod recordings;
pub mod spec_experimentation;
pub mod step_type_knowledge;
pub mod hooks;
pub mod comparison;
pub mod tiered_info;
pub mod adaptive_learning;

use std::sync::{Arc, OnceLock};
use tracing::{info, warn};

/// Global PgDb instance, set once during app initialization.
/// Allows sync-context code (thread spawns, closures) to access PG
/// without threading Arc<PgDb> through every call chain.
static GLOBAL_PG_DB: OnceLock<Arc<PgDb>> = OnceLock::new();

/// PostgreSQL connection pool backed by deadpool-postgres.
pub struct PgDb {
    pool: deadpool_postgres::Pool,
}

impl PgDb {
    /// Set the global PgDb instance. Call once during app initialization.
    /// Warns and ignores if called more than once.
    pub fn set_global(pg_db: Arc<PgDb>) {
        GLOBAL_PG_DB
            .set(pg_db)
            .unwrap_or_else(|_| warn!("PgDb::set_global called more than once (ignored)"));
    }

    /// Get the global PgDb instance. Panics if not initialized.
    /// Use from sync contexts where Arc<PgDb> is not threaded through.
    pub fn global() -> Arc<PgDb> {
        GLOBAL_PG_DB
            .get()
            .expect("PgDb::global() called before PgDb::set_global()")
            .clone()
    }

    /// Try to get the global PgDb instance. Returns None if not initialized.
    pub fn try_global() -> Option<Arc<PgDb>> {
        GLOBAL_PG_DB.get().cloned()
    }

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
            // Decision Trail
            "CREATE TABLE IF NOT EXISTS decisions (
                id              TEXT PRIMARY KEY,
                timestamp       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                scale           TEXT NOT NULL,
                category        TEXT NOT NULL,
                status          TEXT NOT NULL DEFAULT 'active',
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
            )",
            "CREATE INDEX IF NOT EXISTS idx_dec_timestamp ON decisions(timestamp)",
            "CREATE INDEX IF NOT EXISTS idx_dec_category ON decisions(category)",
            "CREATE INDEX IF NOT EXISTS idx_dec_scale ON decisions(scale)",
            "CREATE INDEX IF NOT EXISTS idx_dec_status ON decisions(status) WHERE status = 'active'",
            "CREATE INDEX IF NOT EXISTS idx_dec_fts ON decisions USING GIN (to_tsvector('english', title || ' ' || summary || ' ' || rationale)) WHERE NOT is_deleted",
            // Concept Summaries
            "CREATE TABLE IF NOT EXISTS concept_summaries (
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
            )",
            "CREATE INDEX IF NOT EXISTS idx_cs_fts ON concept_summaries USING GIN (to_tsvector('english', name || ' ' || tagline || ' ' || description)) WHERE NOT is_deleted",
            // Temporal validity columns on observations (idempotent ALTERs)
            "ALTER TABLE observations ADD COLUMN IF NOT EXISTS valid_from TIMESTAMPTZ NOT NULL DEFAULT NOW()",
            "ALTER TABLE observations ADD COLUMN IF NOT EXISTS valid_until TIMESTAMPTZ",
            "ALTER TABLE observations ADD COLUMN IF NOT EXISTS superseded_by BIGINT REFERENCES observations(id)",
            "CREATE INDEX IF NOT EXISTS idx_obs_valid_from ON observations(valid_from) WHERE NOT is_deleted",
            "CREATE INDEX IF NOT EXISTS idx_obs_valid_until ON observations(valid_until) WHERE NOT is_deleted",
            "CREATE INDEX IF NOT EXISTS idx_obs_superseded ON observations(superseded_by) WHERE superseded_by IS NOT NULL",
            // Backfill valid_from from created_at for existing rows that were
            // created before the column was added (valid_from ≈ updated_at means
            // it was set by DEFAULT NOW() during ALTER TABLE, not explicitly).
            "UPDATE observations SET valid_from = created_at WHERE valid_from = updated_at AND created_at < updated_at - INTERVAL '1 second'",
            // Observation History table
            "CREATE TABLE IF NOT EXISTS observation_history (
                id              BIGSERIAL PRIMARY KEY,
                observation_id  BIGINT NOT NULL REFERENCES observations(id) ON DELETE CASCADE,
                title           TEXT NOT NULL,
                content         TEXT NOT NULL,
                content_hash    TEXT NOT NULL,
                valid_from      TIMESTAMPTZ NOT NULL,
                valid_until     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                revision_number INTEGER NOT NULL,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_obs_history_obs_id ON observation_history(observation_id)",
            "CREATE INDEX IF NOT EXISTS idx_obs_history_valid ON observation_history(valid_from, valid_until)",
            // Memory consolidation columns on observations (idempotent ALTERs)
            "ALTER TABLE observations ADD COLUMN IF NOT EXISTS importance DOUBLE PRECISION NOT NULL DEFAULT 0.5",
            "ALTER TABLE observations ADD COLUMN IF NOT EXISTS last_accessed_at TIMESTAMPTZ",
            "ALTER TABLE observations ADD COLUMN IF NOT EXISTS access_count INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE observations ADD COLUMN IF NOT EXISTS decay_rate DOUBLE PRECISION NOT NULL DEFAULT 0.1",
            "ALTER TABLE observations ADD COLUMN IF NOT EXISTS consolidated_from BIGINT[]",
            "ALTER TABLE observations ADD COLUMN IF NOT EXISTS is_mental_model BOOLEAN NOT NULL DEFAULT false",
            "CREATE INDEX IF NOT EXISTS idx_obs_importance ON observations(importance) WHERE NOT is_deleted",
            "CREATE INDEX IF NOT EXISTS idx_obs_mental_model ON observations(is_mental_model) WHERE NOT is_deleted AND is_mental_model",
            "CREATE INDEX IF NOT EXISTS idx_obs_last_accessed ON observations(last_accessed_at) WHERE NOT is_deleted",
            // Memory consolidation log
            "CREATE TABLE IF NOT EXISTS memory_consolidation_log (
                id                      BIGSERIAL PRIMARY KEY,
                started_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                completed_at            TIMESTAMPTZ,
                observations_scanned    INTEGER NOT NULL DEFAULT 0,
                groups_found            INTEGER NOT NULL DEFAULT 0,
                models_created          INTEGER NOT NULL DEFAULT 0,
                observations_merged     INTEGER NOT NULL DEFAULT 0,
                observations_decayed    INTEGER NOT NULL DEFAULT 0,
                observations_archived   INTEGER NOT NULL DEFAULT 0,
                error                   TEXT
            )",
            // Generation Rules
            "CREATE TABLE IF NOT EXISTS generation_rules (
                id              TEXT PRIMARY KEY,
                agent           TEXT NOT NULL,
                section         TEXT NOT NULL,
                rule_number     INTEGER NOT NULL,
                title           TEXT NOT NULL,
                content         TEXT NOT NULL,
                condition       TEXT,
                status          TEXT NOT NULL DEFAULT 'active',
                provenance      TEXT NOT NULL DEFAULT 'seed',
                source_fix_id   TEXT,
                confidence      DOUBLE PRECISION DEFAULT 1.0,
                auto_generated_at TEXT,
                evidence_count  INTEGER DEFAULT 0,
                severity        TEXT NOT NULL DEFAULT 'normal',
                failure_count   INTEGER NOT NULL DEFAULT 0,
                examples_json   TEXT,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL
            )",
            "CREATE INDEX IF NOT EXISTS idx_generation_rules_agent ON generation_rules(agent)",
            "CREATE INDEX IF NOT EXISTS idx_generation_rules_status ON generation_rules(status)",
            "CREATE INDEX IF NOT EXISTS idx_generation_rules_agent_section ON generation_rules(agent, section, rule_number)",
            // Generation Pipeline Artifacts
            "CREATE TABLE IF NOT EXISTS generation_pipeline_artifacts (
                id              TEXT PRIMARY KEY,
                workflow_id     TEXT,
                task_run_id     TEXT,
                description     TEXT NOT NULL,
                category        TEXT,
                created_at      TEXT NOT NULL,
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
                autofix_diff    TEXT,
                verification_iterations TEXT,
                fixer_snapshots TEXT,
                hardening_summary TEXT,
                hardened_json   TEXT,
                final_json      TEXT,
                validation_errors TEXT,
                specification_duration_ms INTEGER,
                specification_criteria TEXT,
                specification_prompt TEXT,
                builder_prompt  TEXT,
                verification_prompts TEXT,
                hardener_prompt TEXT,
                revision_duration_ms INTEGER,
                quality_report  TEXT,
                revision_cycles INTEGER,
                confidence_score DOUBLE PRECISION,
                success         BOOLEAN NOT NULL DEFAULT true,
                error_message   TEXT,
                model_used      TEXT
            )",
            "CREATE INDEX IF NOT EXISTS idx_pipeline_artifacts_workflow ON generation_pipeline_artifacts(workflow_id)",
            "CREATE INDEX IF NOT EXISTS idx_pipeline_artifacts_created ON generation_pipeline_artifacts(created_at)",
            // Pipeline Agent Traces
            "CREATE TABLE IF NOT EXISTS pipeline_agent_traces (
                id              TEXT PRIMARY KEY,
                task_run_id     TEXT NOT NULL,
                agent_type      TEXT NOT NULL,
                agent_id        TEXT NOT NULL,
                run_id          TEXT NOT NULL,
                input_snapshot  TEXT NOT NULL DEFAULT '{}',
                output_snapshot TEXT NOT NULL DEFAULT '{}',
                config_json     TEXT NOT NULL DEFAULT '{}',
                duration_ms     INTEGER NOT NULL DEFAULT 0,
                tokens_in       INTEGER NOT NULL DEFAULT 0,
                tokens_out      INTEGER NOT NULL DEFAULT 0,
                cost_usd        DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                downstream_success BOOLEAN,
                output_quality_score DOUBLE PRECISION,
                parent_span_id  TEXT,
                span_type       TEXT DEFAULT 'agent',
                guardrail_results_json TEXT,
                handoff_context_json TEXT,
                created_at      TEXT NOT NULL
            )",
            "CREATE INDEX IF NOT EXISTS idx_pipeline_agent_traces_task_run ON pipeline_agent_traces(task_run_id)",
            "CREATE INDEX IF NOT EXISTS idx_pipeline_agent_traces_agent_type ON pipeline_agent_traces(agent_type)",
            "CREATE INDEX IF NOT EXISTS idx_pipeline_agent_traces_run_id ON pipeline_agent_traces(run_id)",
            // Meta-Optimizer Runs
            "CREATE TABLE IF NOT EXISTS meta_optimizer_runs (
                id              TEXT PRIMARY KEY,
                optimizer_type  TEXT NOT NULL,
                trigger_type    TEXT NOT NULL DEFAULT 'threshold',
                runs_analyzed   INTEGER NOT NULL DEFAULT 0,
                recommendations_produced INTEGER NOT NULL DEFAULT 0,
                task_run_id     TEXT,
                status          TEXT NOT NULL DEFAULT 'running',
                created_at      TEXT NOT NULL,
                completed_at    TEXT
            )",
            "CREATE INDEX IF NOT EXISTS idx_meta_optimizer_runs_type ON meta_optimizer_runs(optimizer_type)",
            // Meta-Optimizer Snapshots
            "CREATE TABLE IF NOT EXISTS meta_optimizer_snapshots (
                id              TEXT PRIMARY KEY,
                snapshot_type   TEXT NOT NULL,
                period_start    TEXT NOT NULL,
                period_end      TEXT NOT NULL,
                metrics_json    TEXT NOT NULL,
                breakdown_json  TEXT DEFAULT '{}',
                recommendation_id TEXT,
                runs_included   INTEGER NOT NULL DEFAULT 0,
                created_at      TEXT NOT NULL
            )",
            "CREATE INDEX IF NOT EXISTS idx_meta_optimizer_snapshots_type ON meta_optimizer_snapshots(snapshot_type)",
            "CREATE INDEX IF NOT EXISTS idx_meta_optimizer_snapshots_rec ON meta_optimizer_snapshots(recommendation_id)",
            // Reflection Fixes
            "CREATE TABLE IF NOT EXISTS reflection_fixes (
                id              TEXT PRIMARY KEY,
                source_task_run_id TEXT NOT NULL,
                reflection_task_run_id TEXT NOT NULL,
                source_finding_id TEXT,
                source_knowledge_id TEXT,
                fix_type        TEXT NOT NULL,
                fix_description TEXT NOT NULL,
                file_changed    TEXT,
                old_value       TEXT,
                new_value       TEXT,
                confidence      TEXT NOT NULL DEFAULT 'medium',
                content_hash    TEXT,
                status          TEXT NOT NULL DEFAULT 'applied',
                effectiveness   TEXT,
                effectiveness_evidence TEXT,
                applied_at      TEXT NOT NULL,
                evaluated_at    TEXT,
                created_at      TEXT NOT NULL,
                source_agent    TEXT,
                reasoning       TEXT,
                alternatives_considered TEXT,
                reflection_scope TEXT DEFAULT 'workflow',
                project_path    TEXT,
                target_component TEXT,
                reuse_count     INTEGER DEFAULT 0,
                applicability_context TEXT,
                fix_description_embedding BYTEA
            )",
            "CREATE INDEX IF NOT EXISTS idx_reflection_fixes_source ON reflection_fixes(source_task_run_id)",
            "CREATE INDEX IF NOT EXISTS idx_reflection_fixes_reflection ON reflection_fixes(reflection_task_run_id)",
            "CREATE INDEX IF NOT EXISTS idx_reflection_fixes_content_hash ON reflection_fixes(content_hash)",
            "CREATE INDEX IF NOT EXISTS idx_reflection_fixes_status ON reflection_fixes(status)",
            "CREATE INDEX IF NOT EXISTS idx_reflection_fixes_source_agent ON reflection_fixes(source_agent)",
            // Fix Applications
            "CREATE TABLE IF NOT EXISTS fix_applications (
                id              TEXT PRIMARY KEY,
                fix_id          TEXT NOT NULL,
                task_run_id     TEXT NOT NULL,
                error_signature_hash TEXT,
                outcome         TEXT DEFAULT 'pending',
                applied_at      TEXT NOT NULL,
                evaluated_at    TEXT
            )",
            "CREATE INDEX IF NOT EXISTS idx_fix_applications_fix ON fix_applications(fix_id)",
            "CREATE INDEX IF NOT EXISTS idx_fix_applications_task ON fix_applications(task_run_id)",
            // Workflow Generation Feedback
            "CREATE TABLE IF NOT EXISTS workflow_generation_feedback (
                id              TEXT PRIMARY KEY,
                workflow_id     TEXT NOT NULL,
                task_run_id     TEXT,
                feedback_type   TEXT NOT NULL,
                edited_field    TEXT,
                old_value       TEXT,
                new_value       TEXT,
                delete_reason   TEXT,
                rating          INTEGER,
                rating_comment  TEXT,
                workflow_category TEXT,
                workflow_description TEXT,
                created_at      TEXT NOT NULL
            )",
            "CREATE INDEX IF NOT EXISTS idx_wgf_workflow_id ON workflow_generation_feedback(workflow_id)",
            "CREATE INDEX IF NOT EXISTS idx_wgf_feedback_type ON workflow_generation_feedback(feedback_type)",
            "CREATE INDEX IF NOT EXISTS idx_wgf_created_at ON workflow_generation_feedback(created_at)",
        ];

        for sql in &ddl {
            if let Err(e) = conn.execute(*sql, &[]).await {
                warn!("DDL execution failed (non-fatal): {}", e);
            }
        }

        info!("All managed PG tables ensured (activity_timeline, watchers, agentic_metric_scores, prompt_registry, canary_rollouts, canary_run_records, prompt_template_canaries, error_events, decisions, concept_summaries)");
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

    /// Blocking helper for tests: create a PgDb using DATABASE_URL.
    /// Panics if PG is not available.
    #[cfg(test)]
    pub fn new_blocking_for_test() -> std::sync::Arc<Self> {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://localhost:5432/qontinui_test".to_string());
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime for test");
        std::sync::Arc::new(rt.block_on(Self::new(&url)).expect("PgDb connection for test"))
    }
}
