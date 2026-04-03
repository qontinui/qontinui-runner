//! PostgreSQL database layer for qontinui-runner.
//!
//! Runs alongside SQLite during migration. New tables and queries are added here
//! via Clorinde-generated code. Callers prefer PG when available, fall back to SQLite.

pub mod activity_timeline;
pub mod adaptive_learning;
pub mod agentic_metrics;
pub mod ai_sessions;
pub mod approval_gates;
pub mod cached_specs;
pub mod canary;
pub mod canvas;
pub mod checkpoints;
pub mod checks;
pub mod comparison;
pub mod decision_trail;
pub mod deferred_questions;
pub mod entailment_cache;
pub mod error_monitor;
pub mod export;
pub mod findings;
pub mod flows;
pub mod generation;
pub mod generation_artifacts;
pub mod generation_feedback;
pub mod graph_ops;
pub mod hooks;
pub mod knowledge;
pub mod known_issues;
pub mod learning;
pub mod log_sources;
pub mod meta_optimizer;
pub mod misc_crud;
pub mod observations;
pub mod online_learning;
pub mod orchestration_loop;
pub mod pipeline_traces;
pub mod process_sessions;
pub mod prompt_evolution;
pub mod prompt_registry;
pub mod q_routing;
pub mod queued_workflows;
pub mod recordings;
pub mod reflection;
pub mod restate;
pub mod scheduler;
pub mod security_audit;
pub mod settings;
pub mod skills;
pub mod spec_experimentation;
pub mod state_machine;
pub mod step_type_knowledge;
pub mod task_run_events;
pub mod task_runs;
pub mod tiered_info;
pub mod token_analytics;
pub mod token_usage;
pub mod triggers;
pub mod ui_bridge;
pub mod watchers;
pub mod workflow_state;
pub mod workflows;
pub mod worktrees;

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

        let mgr =
            deadpool_postgres::Manager::from_config(pg_config, tokio_postgres::NoTls, mgr_config);

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
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
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
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
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
                started_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                completed_at    TIMESTAMPTZ
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
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
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
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
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
                auto_generated_at TIMESTAMPTZ,
                evidence_count  INTEGER DEFAULT 0,
                severity        TEXT NOT NULL DEFAULT 'normal',
                failure_count   INTEGER NOT NULL DEFAULT 0,
                examples_json   TEXT,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
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
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
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
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
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
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                completed_at    TIMESTAMPTZ
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
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
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
                applied_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                evaluated_at    TIMESTAMPTZ,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
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
                applied_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                evaluated_at    TIMESTAMPTZ
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
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_wgf_workflow_id ON workflow_generation_feedback(workflow_id)",
            "CREATE INDEX IF NOT EXISTS idx_wgf_feedback_type ON workflow_generation_feedback(feedback_type)",
            "CREATE INDEX IF NOT EXISTS idx_wgf_created_at ON workflow_generation_feedback(created_at)",
            // Entailment Cache (PG persistence for evaluation entailment cache)
            "CREATE TABLE IF NOT EXISTS entailment_cache (
                criterion_hash  BIGINT NOT NULL,
                step_hash       BIGINT NOT NULL,
                score           FLOAT8 NOT NULL,
                explanation     TEXT,
                tier            TEXT,
                cached_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                PRIMARY KEY (criterion_hash, step_hash)
            )",
            "CREATE INDEX IF NOT EXISTS idx_entailment_cache_cached_at ON entailment_cache(cached_at)",
            // PRM Training Exports
            "CREATE TABLE IF NOT EXISTS prm_training_exports (
                id              TEXT PRIMARY KEY,
                export_format   TEXT NOT NULL DEFAULT 'jsonl',
                total_examples  INTEGER NOT NULL DEFAULT 0,
                passed_count    INTEGER NOT NULL DEFAULT 0,
                failed_count    INTEGER NOT NULL DEFAULT 0,
                fixed_count     INTEGER NOT NULL DEFAULT 0,
                runs_processed  INTEGER NOT NULL DEFAULT 0,
                file_path       TEXT,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_prm_exports_created ON prm_training_exports(created_at)",
            // Playbook Entries (Adaptive Learning)
            "CREATE TABLE IF NOT EXISTS playbook_entries (
                id              TEXT PRIMARY KEY,
                lesson          TEXT NOT NULL,
                category        TEXT NOT NULL,
                domain          TEXT,
                severity        TEXT NOT NULL DEFAULT 'minor',
                source_run_id   TEXT NOT NULL,
                source_step_id  TEXT,
                positive        INTEGER NOT NULL DEFAULT 1,
                times_applied   INTEGER NOT NULL DEFAULT 0,
                times_helped    INTEGER NOT NULL DEFAULT 0,
                embedding       BYTEA,
                status          TEXT NOT NULL DEFAULT 'staged',
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_playbook_entries_domain ON playbook_entries(domain)",
            "CREATE INDEX IF NOT EXISTS idx_playbook_entries_status ON playbook_entries(status)",
            "CREATE INDEX IF NOT EXISTS idx_playbook_entries_severity ON playbook_entries(severity)",
            // Curated Examples (Adaptive Learning)
            "CREATE TABLE IF NOT EXISTS curated_examples (
                id                      TEXT PRIMARY KEY,
                domain                  TEXT NOT NULL,
                criterion_description   TEXT NOT NULL,
                steps_json              TEXT NOT NULL,
                quality_score           DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                execution_verified      INTEGER NOT NULL DEFAULT 0,
                times_used              INTEGER NOT NULL DEFAULT 0,
                created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_curated_examples_domain ON curated_examples(domain)",
            "CREATE INDEX IF NOT EXISTS idx_curated_examples_quality ON curated_examples(quality_score)",
            // Template Performance
            "CREATE TABLE IF NOT EXISTS template_performance (
                template_id         TEXT PRIMARY KEY,
                template_name       TEXT NOT NULL,
                source              TEXT NOT NULL DEFAULT 'manual',
                success_count       INTEGER NOT NULL DEFAULT 0,
                failure_count       INTEGER NOT NULL DEFAULT 0,
                total_quality_score DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                last_used_at        TIMESTAMPTZ,
                created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            // Template Lifecycle Events
            "CREATE TABLE IF NOT EXISTS template_lifecycle_events (
                id                          TEXT PRIMARY KEY,
                template_id                 TEXT NOT NULL,
                action                      TEXT NOT NULL,
                old_source                  TEXT NOT NULL,
                new_source                  TEXT NOT NULL,
                confidence_at_transition    DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                created_at                  TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_lifecycle_events_template ON template_lifecycle_events(template_id)",
            // GEPA Optimization Runs
            "CREATE TABLE IF NOT EXISTS gepa_optimization_runs (
                id                  TEXT PRIMARY KEY,
                domain              TEXT NOT NULL,
                old_instructions    TEXT NOT NULL,
                new_instructions    TEXT,
                old_score           DOUBLE PRECISION,
                new_score           DOUBLE PRECISION,
                improvement         DOUBLE PRECISION,
                status              TEXT NOT NULL DEFAULT 'pending',
                created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_gepa_runs_domain ON gepa_optimization_runs(domain)",
            "CREATE INDEX IF NOT EXISTS idx_gepa_runs_created ON gepa_optimization_runs(created_at)",
            // Step Templates (exploration-based generation)
            "CREATE TABLE IF NOT EXISTS step_templates (
                id                      TEXT PRIMARY KEY,
                domain                  TEXT NOT NULL,
                pattern_description     TEXT NOT NULL,
                template_steps_json     TEXT NOT NULL,
                parameters_json         TEXT NOT NULL DEFAULT '[]',
                success_count           INTEGER NOT NULL DEFAULT 0,
                failure_count           INTEGER NOT NULL DEFAULT 0,
                confidence              DOUBLE PRECISION NOT NULL DEFAULT 0.5,
                source                  TEXT NOT NULL DEFAULT 'seeded',
                created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_step_templates_domain ON step_templates(domain)",
            // Exploration Stats
            "CREATE TABLE IF NOT EXISTS exploration_stats (
                id                  TEXT PRIMARY KEY,
                workflow_id         TEXT,
                task_run_id         TEXT,
                total_candidates    INTEGER NOT NULL DEFAULT 0,
                search_depth        INTEGER NOT NULL DEFAULT 0,
                search_duration_ms  INTEGER NOT NULL DEFAULT 0,
                best_score          DOUBLE PRECISION,
                strategy_used       TEXT,
                score_progression   TEXT,
                created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_exploration_stats_workflow ON exploration_stats(workflow_id)",
            // Workflow Triggers
            "CREATE TABLE IF NOT EXISTS workflow_triggers (
                id                  TEXT PRIMARY KEY,
                name                TEXT NOT NULL,
                description         TEXT,
                trigger_type        TEXT NOT NULL,
                trigger_config      TEXT NOT NULL,
                workflow_id         TEXT NOT NULL,
                workflow_overrides  TEXT,
                conditions          TEXT NOT NULL DEFAULT '[]',
                debounce_ms         BIGINT NOT NULL DEFAULT 1000,
                cooldown_seconds    BIGINT NOT NULL DEFAULT 60,
                max_concurrent      INTEGER NOT NULL DEFAULT 1,
                retry_count         INTEGER NOT NULL DEFAULT 0,
                retry_delay_seconds BIGINT NOT NULL DEFAULT 30,
                enabled             BOOLEAN NOT NULL DEFAULT TRUE,
                last_triggered_at   TIMESTAMPTZ,
                last_execution_id   TEXT,
                trigger_count       BIGINT NOT NULL DEFAULT 0,
                created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_wt_enabled ON workflow_triggers(enabled) WHERE enabled",
            "CREATE INDEX IF NOT EXISTS idx_wt_workflow ON workflow_triggers(workflow_id)",
            // Trigger History
            "CREATE TABLE IF NOT EXISTS trigger_history (
                id              TEXT PRIMARY KEY,
                trigger_id      TEXT NOT NULL,
                event_type      TEXT NOT NULL,
                event_data      TEXT,
                action          TEXT NOT NULL,
                task_run_id     TEXT,
                error_message   TEXT,
                triggered_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_th_trigger ON trigger_history(trigger_id)",
            "CREATE INDEX IF NOT EXISTS idx_th_triggered_at ON trigger_history(triggered_at)",
            // SM Element Thumbnails
            "CREATE TABLE IF NOT EXISTS sm_element_thumbnails (
                config_id           TEXT NOT NULL,
                fingerprint_hash    TEXT NOT NULL,
                thumbnail_base64    TEXT NOT NULL,
                PRIMARY KEY (config_id, fingerprint_hash)
            )",
            // SM Capture Screenshots
            "CREATE TABLE IF NOT EXISTS sm_capture_screenshots (
                id                      TEXT PRIMARY KEY,
                config_id               TEXT NOT NULL,
                capture_index           INTEGER NOT NULL,
                screenshot_webp         BYTEA NOT NULL,
                width                   INTEGER NOT NULL,
                height                  INTEGER NOT NULL,
                element_bounds_json     TEXT NOT NULL DEFAULT '{}',
                fingerprint_hashes_json TEXT NOT NULL DEFAULT '[]',
                captured_at             TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_scs_config ON sm_capture_screenshots(config_id)",
            // Iteration Logs
            "CREATE TABLE IF NOT EXISTS iteration_logs (
                id              TEXT PRIMARY KEY,
                task_run_id     TEXT NOT NULL,
                iteration       INTEGER NOT NULL DEFAULT 0,
                provider_used   TEXT,
                model_used      TEXT,
                duration_ms     INTEGER,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_iteration_logs_task_run ON iteration_logs(task_run_id)",
            "CREATE INDEX IF NOT EXISTS idx_iteration_logs_provider ON iteration_logs(provider_used) WHERE provider_used IS NOT NULL",
            // Issue Pattern Templates
            "CREATE TABLE IF NOT EXISTS issue_pattern_templates (
                id                  TEXT PRIMARY KEY,
                name                TEXT NOT NULL,
                description         TEXT NOT NULL,
                category            TEXT NOT NULL,
                detection_type      TEXT NOT NULL,
                step_template       TEXT,
                ai_prompt_template  TEXT,
                parameters          TEXT NOT NULL DEFAULT '[]',
                built_in            BOOLEAN NOT NULL DEFAULT FALSE,
                status              TEXT NOT NULL DEFAULT 'active',
                created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            // ── Online Learning: Drift Detection ──
            "CREATE TABLE IF NOT EXISTS performance_drift_signals (
                id              TEXT PRIMARY KEY,
                detector_type   TEXT NOT NULL,
                metric_name     TEXT NOT NULL,
                context_key     TEXT NOT NULL DEFAULT '',
                drift_level     TEXT NOT NULL,
                pre_drift_mean  DOUBLE PRECISION,
                post_drift_mean DOUBLE PRECISION,
                window_size     BIGINT,
                acknowledged    BOOLEAN NOT NULL DEFAULT false,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_drift_context
                ON performance_drift_signals(context_key, metric_name)",
            "CREATE INDEX IF NOT EXISTS idx_drift_unack
                ON performance_drift_signals(acknowledged) WHERE acknowledged = false",
            "CREATE TABLE IF NOT EXISTS drift_detector_state (
                detector_id     TEXT PRIMARY KEY,
                detector_type   TEXT NOT NULL,
                state_json      TEXT NOT NULL,
                last_updated    TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            // ── Online Learning: Model Routing ──
            "CREATE TABLE IF NOT EXISTS model_routing_table (
                context_key     TEXT NOT NULL,
                model_id        TEXT NOT NULL,
                q_value         DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                visit_count     INTEGER NOT NULL DEFAULT 0,
                sum_of_squares  DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                last_updated    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                PRIMARY KEY (context_key, model_id)
            )",
            "CREATE TABLE IF NOT EXISTS model_routing_overrides (
                context_key     TEXT PRIMARY KEY,
                forced_model    TEXT NOT NULL,
                reason          TEXT,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE TABLE IF NOT EXISTS model_routing_decisions (
                id              TEXT PRIMARY KEY,
                task_run_id     TEXT NOT NULL,
                context_key     TEXT NOT NULL,
                model_selected  TEXT NOT NULL,
                source          TEXT NOT NULL,
                exploration     BOOLEAN NOT NULL DEFAULT false,
                reward          DOUBLE PRECISION,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_mrd_task_run
                ON model_routing_decisions(task_run_id)",
            // ── Online Learning: Experience Summaries ──
            "CREATE TABLE IF NOT EXISTS experience_summaries (
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
            )",
            "CREATE INDEX IF NOT EXISTS idx_exp_domain
                ON experience_summaries(domain)",
            // ── Online Learning: Step Credit Assignments ──
            "CREATE TABLE IF NOT EXISTS step_credit_assignments (
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
            )",
            "CREATE INDEX IF NOT EXISTS idx_sca_task_run
                ON step_credit_assignments(task_run_id)",
            "CREATE INDEX IF NOT EXISTS idx_sca_step_type
                ON step_credit_assignments(step_type)",
            // ── Online Learning: Strategy Bank ──
            "CREATE TABLE IF NOT EXISTS strategy_bank (
                id                  TEXT PRIMARY KEY,
                name                TEXT NOT NULL,
                description         TEXT NOT NULL,
                applicability_json  TEXT NOT NULL,
                components_json     TEXT NOT NULL,
                stats_json          TEXT NOT NULL DEFAULT '{}',
                provenance_json     TEXT NOT NULL,
                status              TEXT NOT NULL DEFAULT 'candidate',
                parent_strategy_id  TEXT,
                embedding           BYTEA,
                created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_strategy_status
                ON strategy_bank(status)",
            // ── Online Learning: model_used column on learning_outcomes ──
            "ALTER TABLE learning_outcomes ADD COLUMN IF NOT EXISTS model_used TEXT",
            // Execution state snapshots for replay / observability
            "CREATE TABLE IF NOT EXISTS execution_state_snapshots (
                id              BIGSERIAL PRIMARY KEY,
                execution_id    TEXT NOT NULL,
                span_id         TEXT NOT NULL DEFAULT '',
                snapshot_ts     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                state_type      TEXT NOT NULL,
                summary         TEXT,
                context_json    TEXT,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_ess_execution
                ON execution_state_snapshots(execution_id)",
            "CREATE INDEX IF NOT EXISTS idx_ess_ts
                ON execution_state_snapshots(snapshot_ts)",

            // Security audit events
            "CREATE TABLE IF NOT EXISTS security_audit_events (
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
            )",
            "CREATE INDEX IF NOT EXISTS idx_sec_audit_task_run
                ON security_audit_events(task_run_id) WHERE task_run_id IS NOT NULL",
            "CREATE INDEX IF NOT EXISTS idx_sec_audit_type
                ON security_audit_events(event_type)",
            "CREATE INDEX IF NOT EXISTS idx_sec_audit_decision
                ON security_audit_events(decision)",
            "CREATE INDEX IF NOT EXISTS idx_sec_audit_created
                ON security_audit_events(created_at)",
            // Phase model routing (Q-learning state for model tier selection)
            "CREATE TABLE IF NOT EXISTS phase_model_routing (
                state_key    TEXT NOT NULL,
                phase        TEXT NOT NULL,
                model_tier   TEXT NOT NULL,
                q_value      DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                visit_count  INTEGER NOT NULL DEFAULT 0,
                last_updated TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                PRIMARY KEY (state_key, phase, model_tier)
            )",
            "CREATE INDEX IF NOT EXISTS idx_phase_model_routing_state
                ON phase_model_routing(state_key)",
            // Schema compliance tracking columns on pipeline_agent_traces (v172)
            "ALTER TABLE pipeline_agent_traces ADD COLUMN IF NOT EXISTS schema_valid_first_attempt BOOLEAN",
            "ALTER TABLE pipeline_agent_traces ADD COLUMN IF NOT EXISTS validation_retries INTEGER",
            "ALTER TABLE pipeline_agent_traces ADD COLUMN IF NOT EXISTS coercions_applied TEXT",
            "ALTER TABLE pipeline_agent_traces ADD COLUMN IF NOT EXISTS validation_error_summary TEXT",
            // Scheduler tables
            "CREATE TABLE IF NOT EXISTS scheduled_tasks (
                id                  TEXT PRIMARY KEY,
                name                TEXT NOT NULL,
                description         TEXT,
                enabled             BOOLEAN NOT NULL DEFAULT TRUE,
                schedule_type       TEXT NOT NULL,
                schedule_value      TEXT NOT NULL,
                task_config         TEXT NOT NULL DEFAULT '{}',
                skip_if_completed   BOOLEAN NOT NULL DEFAULT FALSE,
                auto_fix_on_failure BOOLEAN NOT NULL DEFAULT FALSE,
                success_criteria    TEXT,
                created_at          TEXT NOT NULL DEFAULT '',
                modified_at         TEXT NOT NULL DEFAULT '',
                next_run            TEXT,
                last_run_id         TEXT,
                condition_status    TEXT
            )",
            "CREATE TABLE IF NOT EXISTS scheduler_history (
                execution_id        TEXT PRIMARY KEY,
                task_id             TEXT NOT NULL REFERENCES scheduled_tasks(id) ON DELETE CASCADE,
                session_id          TEXT,
                started_at          TEXT NOT NULL,
                ended_at            TEXT,
                status              TEXT NOT NULL DEFAULT 'pending',
                success             BOOLEAN,
                error_message       TEXT,
                triggered_auto_fix  BOOLEAN NOT NULL DEFAULT FALSE,
                auto_fix_session_id TEXT
            )",
            "CREATE INDEX IF NOT EXISTS idx_scheduler_history_task ON scheduler_history(task_id)",
            // Restate durable execution tables
            "CREATE TABLE IF NOT EXISTS restate_workflow_executions (
                execution_id          TEXT PRIMARY KEY REFERENCES task_runs(id) ON DELETE CASCADE,
                restate_workflow_id   TEXT NOT NULL,
                restate_invocation_id TEXT,
                status                TEXT NOT NULL DEFAULT 'pending',
                launched_via_restate  BOOLEAN NOT NULL DEFAULT TRUE,
                created_at            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at            TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_rwe_status ON restate_workflow_executions(status)",
            "CREATE INDEX IF NOT EXISTS idx_rwe_restate_wf ON restate_workflow_executions(restate_workflow_id)",
            "CREATE TABLE IF NOT EXISTS restate_awakeables (
                awakeable_id   TEXT PRIMARY KEY,
                execution_id   TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
                awakeable_type TEXT NOT NULL,
                type_data      TEXT,
                status         TEXT NOT NULL DEFAULT 'pending',
                created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                resolved_at    TIMESTAMPTZ
            )",
            "CREATE INDEX IF NOT EXISTS idx_ra_execution ON restate_awakeables(execution_id)",
            "CREATE INDEX IF NOT EXISTS idx_ra_status ON restate_awakeables(status)",
        ];

        for sql in &ddl {
            if let Err(e) = conn.execute(*sql, &[]).await {
                warn!("DDL execution failed (non-fatal): {}", e);
            }
        }

        info!("All managed PG tables ensured (activity_timeline, watchers, agentic_metric_scores, prompt_registry, canary_rollouts, canary_run_records, prompt_template_canaries, error_events, decisions, concept_summaries)");
    }

    // ========================================================================
    // Execution State Snapshots
    // ========================================================================

    /// Record a state snapshot for replay / observability.
    pub async fn record_state_snapshot(
        &self,
        execution_id: &str,
        span_id: &str,
        snapshot_ts: &str,
        state_type: &str,
        summary: Option<&str>,
        context_json: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            r#"INSERT INTO execution_state_snapshots
               (execution_id, span_id, snapshot_ts, state_type, summary, context_json)
               VALUES ($1, $2, $3::timestamptz, $4, $5, $6)"#,
            &[
                &execution_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &span_id,
                &snapshot_ts,
                &state_type,
                &summary as &(dyn tokio_postgres::types::ToSql + Sync),
                &context_json as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG record_state_snapshot: {}", e))?;
        Ok(())
    }

    /// Query execution state snapshots for replay.
    pub async fn get_state_snapshots(
        &self,
        execution_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let client = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = client
            .query(
                "SELECT id, execution_id, span_id, snapshot_ts, state_type, summary, context_json, created_at
                 FROM execution_state_snapshots
                 WHERE execution_id = $1
                 ORDER BY snapshot_ts ASC",
                &[&execution_id as &(dyn tokio_postgres::types::ToSql + Sync)],
            )
            .await
            .map_err(|e| format!("Failed to query snapshots: {}", e))?;

        Ok(rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "id": row.get::<usize, i64>(0),
                    "execution_id": row.get::<usize, String>(1),
                    "span_id": row.get::<usize, String>(2),
                    "snapshot_ts": row.get::<usize, String>(3),
                    "state_type": row.get::<usize, String>(4),
                    "summary": row.get::<usize, Option<String>>(5),
                    "context_json": row.get::<usize, Option<String>>(6),
                    "created_at": row.get::<usize, String>(7),
                })
            })
            .collect())
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
    /// SQLite has been removed — this is now a no-op.
    pub async fn migrate_token_data_from_sqlite(&self) -> Result<u64, String> {
        info!("SQLite removed — token data migration is a no-op");
        Ok(0)
    }

    /// Blocking helper for tests: create a PgDb using DATABASE_URL.
    /// Panics if PG is not available.
    #[cfg(test)]
    pub fn new_blocking_for_test() -> std::sync::Arc<Self> {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://localhost:5432/qontinui_test".to_string());
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime for test");
        std::sync::Arc::new(
            rt.block_on(Self::new(&url))
                .expect("PgDb connection for test"),
        )
    }
}
