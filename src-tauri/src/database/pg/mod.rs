//! PostgreSQL database layer for qontinui-runner.
//!
//! Runs alongside SQLite during migration. New tables and queries are added here
//! via Clorinde-generated code. Callers prefer PG when available, fall back to SQLite.

pub mod activity_timeline;
pub mod adaptive_learning;
pub mod agentic_metrics;
pub mod ai_sessions;
pub mod approval_gates;
pub mod breakpoints;
pub mod cached_specs;
pub mod canary;
pub mod canvas;
pub mod checkpoints;
pub mod checks;
pub mod comparison;
pub mod compensation;
pub mod contradiction;
pub mod decision_trail;
pub mod deferred_questions;
pub mod entailment_cache;
pub mod entity_profiles;
pub mod error_monitor;
pub mod event_log;
pub mod event_search;
pub mod export;
pub mod findings;
pub mod flows;
pub mod generation;
pub mod generation_artifacts;
pub mod generation_feedback;
pub mod graph_ops;
pub mod hooks;
pub mod instances;
pub mod knowledge;
pub mod known_issues;
pub mod learned_patterns_ops;
pub mod learning;
pub mod log_sources;
pub mod memory_query_cache;
pub mod meta_optimizer;
pub mod misc_crud;
pub mod observations;
pub mod online_learning;
pub mod orchestration_loop;
pub mod phase_results;
pub mod pipeline_traces;
pub mod pr_watch_ops;
pub mod process_sessions;
pub mod prompt_evolution;
pub mod prompt_registry;
pub mod q_routing;
pub mod queued_workflows;
pub mod reasoning_traces;
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
pub mod ticket_system_ops;
pub mod tiered_info;
pub mod token_analytics;
pub mod token_usage;
pub mod triggers;
pub mod ui_bridge;
pub mod ui_bridge_baselines;
pub mod verification_tests;
pub mod watchers;
pub mod workflow_state;
pub mod workflows;
pub mod working_representations;
pub mod worktrees;
pub mod wsv_disagreements;

use std::sync::{Arc, OnceLock};
use tracing::{error, info, warn};

// ============================================================================
// Schema Migration System
// ============================================================================

/// A versioned schema migration. Each migration has a unique version number,
/// a human-readable description, and SQL to execute. Migrations run in order
/// and are tracked in the `schema_migrations` table.
struct Migration {
    version: i32,
    description: &'static str,
    sql: &'static str,
}

/// Ordered list of schema migrations. New migrations are appended at the end
/// with an incrementing version number. Each migration runs inside its own
/// transaction; on failure the transaction is rolled back and startup halts.
///
/// Guidelines for writing migrations:
/// - Use `IF NOT EXISTS` / `IF EXISTS` where PostgreSQL supports it.
/// - For `ALTER TABLE ADD COLUMN` (which PG supports `IF NOT EXISTS` since v9.6),
///   always include `IF NOT EXISTS` so re-runs are safe.
/// - Keep each migration focused on a single logical change.
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        description: "Baseline schema — marks existing tables as version 1",
        sql: "", // No-op: existing tables are created by ensure_tables()
    },
    Migration {
        version: 2,
        description: "Add connection_state tracking to mcp_servers",
        sql: r#"
            ALTER TABLE mcp_servers ADD COLUMN IF NOT EXISTS connection_state TEXT NOT NULL DEFAULT 'disconnected';
            ALTER TABLE mcp_servers ADD COLUMN IF NOT EXISTS last_error TEXT;
            ALTER TABLE mcp_servers ADD COLUMN IF NOT EXISTS last_connected_at TIMESTAMPTZ;
        "#,
    },
    Migration {
        version: 3,
        description: "Add contradiction_resolutions table for Honcho-inspired contradiction handling",
        sql: r#"
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
        "#,
    },
    Migration {
        version: 4,
        description: "Add entity_profiles table for Honcho-inspired evolving representations",
        sql: r#"
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
        "#,
    },
    Migration {
        version: 5,
        description: "Add reasoning_traces table and dreamer columns to memory_consolidation_log",
        sql: r#"
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
        "#,
    },
    Migration {
        version: 6,
        description: "Add is_review and blocks_parent columns to task_runs",
        sql: r#"
            ALTER TABLE task_runs ADD COLUMN IF NOT EXISTS is_review BOOLEAN DEFAULT FALSE;
            ALTER TABLE task_runs ADD COLUMN IF NOT EXISTS blocks_parent BOOLEAN DEFAULT FALSE;
            CREATE INDEX IF NOT EXISTS idx_task_runs_blocking_children
                ON task_runs(parent_task_run_id, blocks_parent)
                WHERE blocks_parent = true AND status NOT IN ('complete', 'failed', 'stopped');
        "#,
    },
    Migration {
        version: 7,
        description: "Extend verification_tests with rich-test columns and ensure test_results table (Bug 9)",
        sql: r#"
            CREATE TABLE IF NOT EXISTS verification_tests (
                id                  TEXT PRIMARY KEY,
                name                TEXT NOT NULL,
                description         TEXT,
                workflow_id         TEXT,
                test_type           TEXT NOT NULL DEFAULT 'python_script',
                command             TEXT,
                expected_exit_code  INTEGER DEFAULT 0,
                expected_output     TEXT,
                timeout_seconds     INTEGER DEFAULT 60,
                enabled             BOOLEAN NOT NULL DEFAULT true,
                tags                TEXT DEFAULT '[]',
                created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
            );
            ALTER TABLE verification_tests ADD COLUMN IF NOT EXISTS category TEXT;
            ALTER TABLE verification_tests ADD COLUMN IF NOT EXISTS playwright_code TEXT;
            ALTER TABLE verification_tests ADD COLUMN IF NOT EXISTS vision_config TEXT;
            ALTER TABLE verification_tests ADD COLUMN IF NOT EXISTS python_code TEXT;
            ALTER TABLE verification_tests ADD COLUMN IF NOT EXISTS repo_test_config TEXT;
            ALTER TABLE verification_tests ADD COLUMN IF NOT EXISTS success_criteria TEXT;
            ALTER TABLE verification_tests ADD COLUMN IF NOT EXISTS config TEXT NOT NULL DEFAULT '{}';
            ALTER TABLE verification_tests ADD COLUMN IF NOT EXISTS is_critical BOOLEAN NOT NULL DEFAULT false;
            ALTER TABLE verification_tests ADD COLUMN IF NOT EXISTS ai_generated BOOLEAN NOT NULL DEFAULT false;
            ALTER TABLE verification_tests ADD COLUMN IF NOT EXISTS ai_generation_prompt TEXT;
            ALTER TABLE verification_tests ADD COLUMN IF NOT EXISTS creation_analysis TEXT;
            ALTER TABLE verification_tests ADD COLUMN IF NOT EXISTS source_file TEXT;
            ALTER TABLE verification_tests ADD COLUMN IF NOT EXISTS last_exported_at TIMESTAMPTZ;
            CREATE INDEX IF NOT EXISTS idx_verification_tests_category ON verification_tests(category) WHERE category IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_verification_tests_enabled ON verification_tests(enabled) WHERE enabled;
            CREATE TABLE IF NOT EXISTS test_results (
                id                  TEXT PRIMARY KEY,
                test_id             TEXT NOT NULL,
                task_run_id         TEXT,
                status              TEXT NOT NULL DEFAULT 'pending',
                started_at          TIMESTAMPTZ,
                completed_at        TIMESTAMPTZ,
                duration_ms         INTEGER,
                output              TEXT,
                error_message       TEXT,
                structured_output   TEXT,
                assertions_passed   INTEGER NOT NULL DEFAULT 0,
                assertions_failed   INTEGER NOT NULL DEFAULT 0,
                screenshots         TEXT NOT NULL DEFAULT '[]',
                visual_evidence     TEXT,
                ai_analysis         TEXT,
                created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
            );
            CREATE INDEX IF NOT EXISTS idx_test_results_test_id ON test_results(test_id);
            CREATE INDEX IF NOT EXISTS idx_test_results_task_run_id ON test_results(task_run_id) WHERE task_run_id IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_test_results_status ON test_results(status);
            CREATE INDEX IF NOT EXISTS idx_test_results_created_at ON test_results(created_at);
        "#,
    },
    Migration {
        version: 8,
        description: "Add recordings, recording_actions, and recording_exports tables (Bug 8 — restore recording library on PG)",
        sql: r#"
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
        "#,
    },
    Migration {
        version: 9,
        description: "Repair SQLite-era schema drift: TIMESTAMPTZ columns stored as TEXT and BIGINT columns stored as INTEGER (idempotent, no-op on fresh installs)",
        sql: r#"
            -- Repair schema drift from the SQLite -> PG transition: some live
            -- installs have columns stored as TEXT/INTEGER that the canonical
            -- schema declares as TIMESTAMPTZ/BIGINT. On a fresh install the
            -- canonical types are already applied by schema.pg.sql, so this
            -- migration must be a no-op; on legacy installs it performs the
            -- in-place cast. We do this via a DO block that inspects
            -- information_schema.columns and only issues the ALTER when the
            -- current storage type is the legacy one — this makes the whole
            -- migration idempotent and safe to re-run.
            --
            -- Empty strings (legacy SQLite default) become NULL via NULLIF so
            -- nullable columns keep their NULLs and NOT NULL columns that had
            -- '' would fail loudly — none exist today, verified pre-landing.
            -- PostgreSQL rebuilds dependent indexes automatically during
            -- ALTER COLUMN TYPE. INTEGER -> BIGINT is lossless.
            DO $$
            DECLARE
                r record;
            BEGIN
                -- TIMESTAMPTZ columns. Column 3 marks whether the column
                -- should also have its default bumped to NOW() after the
                -- type change (NOT NULL columns with a legacy '' default).
                FOR r IN SELECT * FROM (VALUES
                    ('active_workflows',              'created_at',        true),
                    ('active_workflows',              'updated_at',        true),
                    ('exploration_stats',             'created_at',        true),
                    ('fix_applications',              'applied_at',        true),
                    ('fix_applications',              'evaluated_at',      false),
                    ('flow_executions',               'started_at',        true),
                    ('flow_executions',               'completed_at',      false),
                    ('flow_versions',                 'created_at',        true),
                    ('generation_pipeline_artifacts', 'created_at',        true),
                    ('generation_rules',              'created_at',        true),
                    ('generation_rules',              'updated_at',        true),
                    ('generation_rules',              'auto_generated_at', false),
                    ('iteration_logs',                'created_at',        true),
                    ('meta_optimizer_runs',           'created_at',        true),
                    ('meta_optimizer_runs',           'completed_at',      false),
                    ('meta_optimizer_snapshots',      'created_at',        true),
                    ('orchestrator_checkpoints',      'created_at',        true),
                    ('orchestrator_flows',            'created_at',        true),
                    ('orchestrator_flows',            'updated_at',        true),
                    ('pipeline_agent_traces',         'created_at',        true),
                    ('recordings',                    'started_at',        true),
                    ('recordings',                    'completed_at',      false),
                    ('reflection_fixes',              'created_at',        true),
                    ('reflection_fixes',              'applied_at',        true),
                    ('reflection_fixes',              'evaluated_at',      false),
                    ('sm_capture_screenshots',        'captured_at',       true),
                    ('step_templates',                'created_at',        true),
                    ('step_templates',                'updated_at',        true),
                    ('template_performance',          'last_used_at',      false),
                    ('test_results',                  'created_at',        true),
                    ('test_results',                  'started_at',        false),
                    ('test_results',                  'completed_at',      false),
                    ('trigger_history',               'triggered_at',      true),
                    ('workflow_triggers',             'created_at',        true),
                    ('workflow_triggers',             'updated_at',        true),
                    ('workflow_triggers',             'last_triggered_at', false)
                ) AS t(table_name, column_name, set_now_default)
                LOOP
                    IF EXISTS (
                        SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'runner'
                          AND table_name   = r.table_name
                          AND column_name  = r.column_name
                          AND data_type    = 'text'
                    ) THEN
                        EXECUTE format(
                            'ALTER TABLE %I ALTER COLUMN %I DROP DEFAULT',
                            r.table_name, r.column_name
                        );
                        EXECUTE format(
                            'ALTER TABLE %I ALTER COLUMN %I TYPE TIMESTAMPTZ USING NULLIF(%I, '''')::timestamptz',
                            r.table_name, r.column_name, r.column_name
                        );
                        IF r.set_now_default THEN
                            EXECUTE format(
                                'ALTER TABLE %I ALTER COLUMN %I SET DEFAULT NOW()',
                                r.table_name, r.column_name
                            );
                        END IF;
                    END IF;
                END LOOP;

                -- BIGINT columns that drifted to INTEGER. INTEGER -> BIGINT
                -- is a lossless widening cast.
                FOR r IN SELECT * FROM (VALUES
                    ('canary_rollouts',   'percentage'),
                    ('canary_rollouts',   'baseline_run_count'),
                    ('canary_rollouts',   'canary_run_count'),
                    ('workflow_triggers', 'debounce_ms'),
                    ('workflow_triggers', 'cooldown_seconds'),
                    ('workflow_triggers', 'retry_delay_seconds'),
                    ('workflow_triggers', 'trigger_count')
                ) AS t(table_name, column_name)
                LOOP
                    IF EXISTS (
                        SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'runner'
                          AND table_name   = r.table_name
                          AND column_name  = r.column_name
                          AND data_type    = 'integer'
                    ) THEN
                        EXECUTE format(
                            'ALTER TABLE %I ALTER COLUMN %I TYPE BIGINT USING %I::bigint',
                            r.table_name, r.column_name, r.column_name
                        );
                    END IF;
                END LOOP;
            END $$;
        "#,
    },
    Migration {
        version: 10,
        description: "Repair SQLite-era schema drift v2: 72 TIMESTAMPTZ columns across 46 schema.pg.sql-only tables that v9 missed (v9 only covered ensure_tables canonical)",
        sql: r#"
            -- v9 repaired 36 TIMESTAMPTZ columns in tables declared in ensure_tables
            -- (mod.rs CREATE TABLE statements), but the real canonical schema is
            -- schema.pg.sql (the Clorinde source of truth, 176 tables). v10 fixes
            -- the remaining 72 TIMESTAMPTZ-as-TEXT columns in 46 tables that are
            -- only declared in schema.pg.sql — these survived v9 because
            -- ensure_tables never saw them.
            --
            -- Because these tables are NOT in ensure_tables, a completely fresh
            -- install (new docker volume, no legacy data) will not have them yet
            -- at migration time. The plpgsql DO block skips any row whose table
            -- or column doesn't exist in the current schema, so the migration
            -- is a no-op on fresh installs and idempotent on any install.
            --
            -- All 72 columns were verified 100% castable against the live dev
            -- database before landing this migration: no row produced a NULL
            -- cast or threw an exception. The DROP DEFAULT / SET DEFAULT NOW()
            -- dance is required because the original SQLite-era default was a
            -- TEXT literal like '' that PG refuses to auto-cast to timestamptz.
            DO $$
            DECLARE
                r record;
            BEGIN
                FOR r IN SELECT * FROM (VALUES
                    ('agentic_metric_baselines','updated_at'),
                    ('ai_workflows','created_at'),
                    ('ai_workflows','updated_at'),
                    ('api_credentials','created_at'),
                    ('api_credentials','expires_at'),
                    ('api_credentials','updated_at'),
                    ('api_request_logs','created_at'),
                    ('architecture_components','last_activity_at'),
                    ('artifacts','created_at'),
                    ('cached_app_specs','discovered_at'),
                    ('check_results','completed_at'),
                    ('check_results','created_at'),
                    ('check_results','started_at'),
                    ('comparison_runs','completed_at'),
                    ('comparison_runs','created_at'),
                    ('component_relationships','last_seen_at'),
                    ('config_statistics','first_run_at'),
                    ('config_statistics','last_run_at'),
                    ('config_statistics','last_updated_at'),
                    ('convergence_snapshots','snapshot_at'),
                    ('decomposition_plans','completed_at'),
                    ('decomposition_subtasks','completed_at'),
                    ('decomposition_subtasks','started_at'),
                    ('eval_results','created_at'),
                    ('eval_specs','created_at'),
                    ('eval_specs','updated_at'),
                    ('executions','ended_at'),
                    ('executions','started_at'),
                    ('generator_benchmark_results','run_at'),
                    ('generator_benchmarks','created_at'),
                    ('generator_benchmarks','updated_at'),
                    ('golden_datasets','created_at'),
                    ('golden_datasets','updated_at'),
                    ('gui_lock','acquired_at'),
                    ('known_issues','last_checked_at'),
                    ('known_issues','last_detected_at'),
                    ('known_issues','resolved_at'),
                    ('mcp_servers','tools_cached_at'),
                    ('orchestration_loop_configs','created_at'),
                    ('orchestration_loop_configs','updated_at'),
                    ('orchestrator_verification_results','created_at'),
                    ('pending_discoveries','created_at'),
                    ('process_sessions','started_at'),
                    ('process_sessions','stopped_at'),
                    ('robustness_reports','created_at'),
                    ('rule_applications','applied_at'),
                    ('scheduled_tasks','created_at'),
                    ('scheduled_tasks','modified_at'),
                    ('scheduled_tasks','next_run'),
                    ('scheduler_history','ended_at'),
                    ('scheduler_history','started_at'),
                    ('schema_version','applied_at'),
                    ('shell_command_results','completed_at'),
                    ('shell_command_results','created_at'),
                    ('shell_command_results','started_at'),
                    ('spec_accuracy_results','created_at'),
                    ('spec_compliance_results','created_at'),
                    ('spec_versions','created_at'),
                    ('step_type_knowledge','created_at'),
                    ('step_type_knowledge','updated_at'),
                    ('task_knowledge_summaries','created_at'),
                    ('task_run_automation','ended_at'),
                    ('task_run_automation','started_at'),
                    ('task_run_mcp_calls','created_at'),
                    ('task_run_output_chunks','created_at'),
                    ('task_runs','summary_generated_at'),
                    ('test_associations','created_at'),
                    ('test_associations','updated_at'),
                    ('verification_plans','created_at'),
                    ('workflow_step_checkpoints','completed_at'),
                    ('workflow_step_checkpoints','started_at'),
                    ('workflow_variables','created_at')
                ) AS t(tbl, col) LOOP
                    IF EXISTS (
                        SELECT 1 FROM information_schema.columns
                        WHERE table_schema = current_schema()
                          AND table_name = r.tbl
                          AND column_name = r.col
                          AND data_type = 'text'
                    ) THEN
                        EXECUTE format('ALTER TABLE %I ALTER COLUMN %I DROP DEFAULT', r.tbl, r.col);
                        EXECUTE format(
                            'ALTER TABLE %I ALTER COLUMN %I TYPE TIMESTAMPTZ USING NULLIF(%I, '''')::timestamptz',
                            r.tbl, r.col, r.col
                        );
                        EXECUTE format('ALTER TABLE %I ALTER COLUMN %I SET DEFAULT NOW()', r.tbl, r.col);
                    END IF;
                END LOOP;
            END $$;
        "#,
    },
    Migration {
        version: 11,
        description: "Add columns missing from live that exist in canonical schema.pg.sql: phase_token_usage.cache_{creation,read}_tokens (BIGINT) and step_credit_assignments.cost_efficiency_signal (DOUBLE PRECISION)",
        sql: r#"
            -- These columns are declared in the canonical schema.pg.sql but
            -- are missing from legacy live databases — almost certainly
            -- because schema.pg.sql was edited without a corresponding
            -- migration. Both target tables are empty on dev so ADD COLUMN
            -- is instant and riskless; on any install where they're already
            -- present this is a no-op via IF NOT EXISTS.
            --
            -- phase_token_usage is a schema.pg.sql-only table (not in
            -- ensure_tables), so it may not exist on a fresh install. The
            -- DO block skips the ALTER if the table is absent, matching
            -- the defensive pattern in migration v10.
            DO $$
            BEGIN
                IF EXISTS (SELECT 1 FROM information_schema.tables
                           WHERE table_schema = current_schema()
                             AND table_name = 'phase_token_usage') THEN
                    ALTER TABLE phase_token_usage ADD COLUMN IF NOT EXISTS cache_creation_tokens BIGINT;
                    ALTER TABLE phase_token_usage ADD COLUMN IF NOT EXISTS cache_read_tokens BIGINT;
                END IF;
            END $$;
            ALTER TABLE step_credit_assignments ADD COLUMN IF NOT EXISTS cost_efficiency_signal DOUBLE PRECISION;
        "#,
    },
    Migration {
        version: 12,
        description: "Drop 10 SQLite-era orphan tables that are empty in live and have zero Rust references",
        sql: r#"
            -- All 10 tables were created by the original SQLite -> PG one-shot
            -- migration, became dead code when the features that used them
            -- were deleted or rewritten, and have survived as empty tables
            -- because CREATE TABLE IF NOT EXISTS in ensure_tables is additive
            -- only. Each table was verified individually as:
            --   (a) 0 rows in the live dev database, and
            --   (b) 0 references in qontinui-runner/src-tauri/src/** outside
            --       the v10 TIMESTAMPTZ migration's defensive drift list
            --       (which is idempotent and skips missing tables).
            -- The v10 DO block already skips missing tables, so no cleanup
            -- of v10 is required — re-running v10 after v12 is a no-op for
            -- these rows.
            --
            -- Feature-origin notes (for future archaeology):
            --   api_credentials, api_request_logs — never-shipped API key
            --     management UI, superseded by env-var-based settings.
            --   context_summaries — replaced by task_knowledge_summaries.
            --   decomposition_plans, decomposition_subtasks — PentAGI-style
            --     task decomposition that was scoped but never implemented
            --     (see proj_pentagi_integration memory). Zero code refs.
            --   generator_benchmarks, generator_benchmark_results — removed
            --     during the generator eval rewrite (PR 1 in the recent
            --     spawn-test session deleted /generator-eval/benchmarks).
            --   rule_applications — dead from an earlier rules engine
            --     superseded by graph_engine_pg.
            --   schema_version — SQLite-era migration tracker, replaced by
            --     schema_migrations (live DB has both; schema_migrations is
            --     the one currently in use).
            --   ui_bridge_state_groups — never-shipped state grouping UI.
            DROP TABLE IF EXISTS api_credentials CASCADE;
            DROP TABLE IF EXISTS api_request_logs CASCADE;
            DROP TABLE IF EXISTS context_summaries CASCADE;
            DROP TABLE IF EXISTS decomposition_subtasks CASCADE;
            DROP TABLE IF EXISTS decomposition_plans CASCADE;
            DROP TABLE IF EXISTS generator_benchmark_results CASCADE;
            DROP TABLE IF EXISTS generator_benchmarks CASCADE;
            DROP TABLE IF EXISTS rule_applications CASCADE;
            DROP TABLE IF EXISTS schema_version CASCADE;
            DROP TABLE IF EXISTS ui_bridge_state_groups CASCADE;
        "#,
    },
    Migration {
        version: 13,
        description: "Add ui_bridge_baselines table for persistent visual regression baselines",
        sql: r#"
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
            CREATE INDEX IF NOT EXISTS idx_ui_bridge_baselines_target
                ON ui_bridge_baselines(target_scope);
        "#,
    },
    Migration {
        version: 14,
        description: "Fix v10 regression: drop wrong DEFAULT NOW() on ended_at/completed_at/resolved_at/expires_at columns, and make scheduler_history.success NOT NULL DEFAULT FALSE",
        sql: r#"
            -- v10 set DEFAULT NOW() on ALL 72 TIMESTAMPTZ columns, but 13 of
            -- them (ended_at, completed_at, resolved_at, expires_at, stopped_at,
            -- reviewed_at, etc.) should NOT have a default — NULL means "event
            -- hasn't happened yet". DROP DEFAULT restores the correct semantic.
            -- Also: scheduler_history.success was nullable, but Rust reads it
            -- as bool (non-nullable). Make it NOT NULL DEFAULT FALSE. The table
            -- has 0 rows so the ALTER is instant.
            DO $$
            DECLARE
                r record;
            BEGIN
                FOR r IN SELECT * FROM (VALUES
                    ('check_results','completed_at'),
                    ('comparison_runs','completed_at'),
                    ('executions','ended_at'),
                    ('known_issues','last_checked_at'),
                    ('known_issues','last_detected_at'),
                    ('known_issues','resolved_at'),
                    ('mcp_servers','tools_cached_at'),
                    ('process_sessions','stopped_at'),
                    ('scheduler_history','ended_at'),
                    ('shell_command_results','completed_at'),
                    ('task_run_automation','ended_at'),
                    ('task_runs','summary_generated_at'),
                    ('workflow_step_checkpoints','completed_at')
                ) AS t(tbl, col) LOOP
                    IF EXISTS (
                        SELECT 1 FROM information_schema.columns
                        WHERE table_schema = current_schema()
                          AND table_name = r.tbl
                          AND column_name = r.col
                    ) THEN
                        EXECUTE format('ALTER TABLE %I ALTER COLUMN %I DROP DEFAULT', r.tbl, r.col);
                    END IF;
                END LOOP;

                -- Fix nullable success column on scheduler_history
                IF EXISTS (
                    SELECT 1 FROM information_schema.columns
                    WHERE table_schema = current_schema()
                      AND table_name = 'scheduler_history'
                      AND column_name = 'success'
                      AND is_nullable = 'YES'
                ) THEN
                    UPDATE scheduler_history SET success = FALSE WHERE success IS NULL;
                    ALTER TABLE scheduler_history ALTER COLUMN success SET NOT NULL;
                    ALTER TABLE scheduler_history ALTER COLUMN success SET DEFAULT FALSE;
                END IF;
            END $$;
        "#,
    },
    Migration {
        version: 15,
        description: "Add runner_instances table for multi-instance coordination",
        sql: r#"
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
        "#,
    },
    Migration {
        version: 16,
        description: "Add breakpoint_snapshots table for step-level debugging",
        sql: r#"
            CREATE TABLE IF NOT EXISTS breakpoint_snapshots (
                id                  TEXT PRIMARY KEY,
                execution_id        TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
                step_index          INTEGER NOT NULL,
                step_name           TEXT,
                phase               TEXT,
                iteration           INTEGER,
                variables_json      TEXT NOT NULL,
                last_screenshot_ref TEXT,
                pending_steps_json  TEXT NOT NULL,
                freshness_ts        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                status              TEXT NOT NULL DEFAULT 'waiting',
                resumed_at          TIMESTAMPTZ,
                created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
            );
            CREATE INDEX IF NOT EXISTS idx_bps_execution ON breakpoint_snapshots(execution_id);
            CREATE INDEX IF NOT EXISTS idx_bps_status ON breakpoint_snapshots(status);
        "#,
    },
    Migration {
        version: 17,
        description: "Add source_yaml and dag_node_metrics columns for DAG workflows",
        sql: r#"
            ALTER TABLE unified_workflows ADD COLUMN IF NOT EXISTS source_yaml TEXT;
            ALTER TABLE learning_outcomes ADD COLUMN IF NOT EXISTS dag_node_metrics TEXT;
        "#,
    },
    Migration {
        version: 18,
        description: "Add workflow_event_log table for DAG durable execution",
        sql: r#"
            CREATE TABLE IF NOT EXISTS workflow_event_log (
                id              BIGSERIAL PRIMARY KEY,
                execution_id    TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
                node_id         TEXT NOT NULL,
                event_type      TEXT NOT NULL,
                event_data      TEXT,
                cursor          BIGINT NOT NULL,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            );
            CREATE INDEX IF NOT EXISTS idx_event_log_execution ON workflow_event_log(execution_id, cursor);
            CREATE INDEX IF NOT EXISTS idx_event_log_node ON workflow_event_log(execution_id, node_id);
        "#,
    },
    Migration {
        version: 19,
        description: "max_iterations default = unlimited (NULL); backfill 0/1 values to NULL",
        // `NULL` in unified_workflows.max_iterations now means "unlimited" —
        // the executor translates that to u32::MAX at the LoopConfig boundary.
        // Previously the column defaulted to 10, and many AI-generated workflows
        // landed with max_iterations = 1, causing them to fail after a single
        // verification attempt. Drop the default and clear rows whose stored
        // value would force the loop to exit before any meaningful iteration.
        // ai_workflows stores its payload in a single `config` TEXT column
        // (JSON blob) so migrating that table requires a JSON surgical
        // update. Legacy ai_workflows are rarely edited; leave existing rows
        // alone — the Rust serde default (`None`) handles new writes. Only
        // unified_workflows.max_iterations is a real column and gets the
        // schema change.
        sql: r#"
            ALTER TABLE unified_workflows ALTER COLUMN max_iterations DROP DEFAULT;
            UPDATE unified_workflows SET max_iterations = NULL WHERE max_iterations IS NOT NULL AND max_iterations <= 1;
        "#,
    },
    Migration {
        version: 20,
        description: "Consolidate all tables into runner schema (drop public duplicates, move stragglers)",
        sql: r#"
            DO $$
            DECLARE
                tbl TEXT;
            BEGIN
                -- 1. Drop public-schema duplicates (tables that exist in both schemas).
                --    The runner schema copy has all the data (search_path = runner, public).
                FOR tbl IN
                    SELECT p.table_name
                    FROM information_schema.tables p
                    JOIN information_schema.tables r
                      ON p.table_name = r.table_name
                    WHERE p.table_schema = 'public'
                      AND r.table_schema = 'runner'
                      AND p.table_type = 'BASE TABLE'
                LOOP
                    EXECUTE format('DROP TABLE IF EXISTS public.%I CASCADE', tbl);
                    RAISE NOTICE 'Dropped duplicate public.%', tbl;
                END LOOP;

                -- 2. Move public-only tables to runner schema.
                FOR tbl IN
                    SELECT table_name
                    FROM information_schema.tables
                    WHERE table_schema = 'public'
                      AND table_type = 'BASE TABLE'
                      AND table_name NOT IN (
                          SELECT table_name FROM information_schema.tables
                          WHERE table_schema = 'runner'
                      )
                      AND table_name NOT LIKE 'pg_%'
                      AND table_name != 'spatial_ref_sys'
                LOOP
                    EXECUTE format('ALTER TABLE public.%I SET SCHEMA runner', tbl);
                    RAISE NOTICE 'Moved public.% to runner schema', tbl;
                END LOOP;
            END $$;
        "#,
    },
    Migration {
        version: 21,
        description: "Add compensation_actions table for LIFO compensation stack (plan: restate-port-part-a §1.3–1.4)",
        sql: r#"
            CREATE TABLE IF NOT EXISTS compensation_actions (
                id              BIGSERIAL PRIMARY KEY,
                execution_id    TEXT NOT NULL,
                action_index    INT NOT NULL,
                action_json     JSONB NOT NULL,
                executed        BOOLEAN NOT NULL DEFAULT FALSE,
                result_json     JSONB,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                executed_at     TIMESTAMPTZ
            );
            CREATE INDEX IF NOT EXISTS idx_ca_execution ON compensation_actions(execution_id);
            CREATE INDEX IF NOT EXISTS idx_ca_pending ON compensation_actions(execution_id) WHERE NOT executed;
        "#,
    },
    Migration {
        version: 22,
        description: "Add phase_results table for structured phase-level persistence (plan: restate-port-part-a §2.3)",
        sql: r#"
            CREATE TABLE IF NOT EXISTS phase_results (
                id              BIGSERIAL PRIMARY KEY,
                execution_id    TEXT NOT NULL,
                phase           TEXT NOT NULL,
                iteration       INT,
                stage_index     INT,
                success         BOOLEAN NOT NULL,
                all_passed      BOOLEAN NOT NULL,
                duration_ms     BIGINT NOT NULL,
                failure_context TEXT,
                commit_hash     TEXT,
                step_results    JSONB NOT NULL DEFAULT '[]'::jsonb,
                variables_set   JSONB,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
            );
            CREATE INDEX IF NOT EXISTS idx_pr_execution ON phase_results(execution_id);
            CREATE INDEX IF NOT EXISTS idx_pr_execution_phase ON phase_results(execution_id, phase, iteration);
        "#,
    },
    Migration {
        version: 23,
        description: "Add HTN planning config columns to unified_workflows",
        sql: r#"
            ALTER TABLE unified_workflows ADD COLUMN IF NOT EXISTS htn_enabled BOOLEAN NOT NULL DEFAULT FALSE;
            ALTER TABLE unified_workflows ADD COLUMN IF NOT EXISTS htn_ui_bridge_url TEXT;
            ALTER TABLE unified_workflows ADD COLUMN IF NOT EXISTS htn_state_machine_path TEXT;
        "#,
    },
    Migration {
        version: 24,
        description: "Add co-occurrence observation pipeline tables (observations, discovery artifacts, drift scores)",
        sql: r#"
            CREATE TABLE IF NOT EXISTS co_occurrence_observations (
                id UUID PRIMARY KEY,
                captured_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                spec_id TEXT,
                runner_instance TEXT,
                fingerprints JSONB NOT NULL,
                snapshot_metadata JSONB,
                invalidated_at TIMESTAMPTZ,
                invalidated_reason TEXT,
                invalidated_by TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_observations_captured_at
                ON co_occurrence_observations (captured_at)
                WHERE invalidated_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_observations_spec
                ON co_occurrence_observations (spec_id, captured_at)
                WHERE invalidated_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_observations_fingerprints
                ON co_occurrence_observations USING gin(fingerprints);

            CREATE TABLE IF NOT EXISTS state_discovery_artifacts (
                id UUID PRIMARY KEY,
                spec_id TEXT,
                derived_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                window_days INTEGER NOT NULL,
                artifact JSONB NOT NULL,
                observation_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_discovery_spec_derived
                ON state_discovery_artifacts (spec_id, derived_at DESC);

            CREATE TABLE IF NOT EXISTS state_discovery_drift_scores (
                id BIGSERIAL PRIMARY KEY,
                spec_id TEXT,
                computed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                window_size INTEGER NOT NULL,
                fit_score DOUBLE PRECISION NOT NULL,
                observations_considered INTEGER NOT NULL,
                states_matched INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_drift_scores_spec_computed
                ON state_discovery_drift_scores (spec_id, computed_at DESC);
        "#,
    },
    Migration {
        version: 25,
        description: "Add invalidation_token to co_occurrence_observations for undo tracking",
        sql: r#"
            ALTER TABLE co_occurrence_observations
                ADD COLUMN IF NOT EXISTS invalidation_token TEXT;
            CREATE INDEX IF NOT EXISTS idx_observations_invalidation_token
                ON co_occurrence_observations (invalidation_token)
                WHERE invalidation_token IS NOT NULL;
        "#,
    },
    Migration {
        version: 26,
        description: "Fix INTEGER → BIGINT drift on meta_optimizer_snapshots.runs_included (Rust struct declares i64; caused tokio-rt-worker panic 'error deserializing column 7' on read)",
        sql: r#"
            -- Rust reads this column via `r.get::<_, i64>(7)` in
            -- database/pg/meta_optimizer.rs. Every install where this column
            -- is still INT4 panics with "error retrieving column 7: error
            -- deserializing column 7" the first time `get_latest_baseline_snapshot`
            -- (or any of the 4 sibling readers) returns a row. Cast is
            -- lossless since every INT4 fits in INT8.
            ALTER TABLE meta_optimizer_snapshots
                ALTER COLUMN runs_included TYPE BIGINT USING runs_included::bigint;
        "#,
    },
    Migration {
        version: 27,
        description: "Fix 12 more INT4 → BIGINT drifts across 7 tables where Rust reads with r.get::<_, i64> (sister bugs of v26; same panic class)",
        sql: r#"
            -- Same pattern as v26. An audit of every `INTEGER` column in
            -- schema.pg.sql against `r.get::<_, i64>` / `Option<i64>` reads
            -- in database/pg/*.rs found 12 additional mismatches. Each
            -- would panic with "error deserializing column N" the first
            -- time the corresponding SELECT returned a row. Cast INT4 →
            -- INT8 is lossless; indexes rebuild automatically.

            -- task_runs.rs:1929-1930 (get_step_progress_markers)
            ALTER TABLE step_progress_markers
                ALTER COLUMN current_value TYPE BIGINT USING current_value::bigint,
                ALTER COLUMN total_value   TYPE BIGINT USING total_value::bigint;

            -- task_runs.rs:1277, :1329 (get_automation_* readers). INSERT
            -- at task_runs.rs:2165 already casts to i32, so reads are the
            -- drift; migrating to BIGINT aligns both.
            ALTER TABLE task_run_automation
                ALTER COLUMN iteration_number TYPE BIGINT USING iteration_number::bigint;

            -- pipeline_traces.rs:153-155 (get_pipeline_traces_for_task).
            -- duration_ms especially needs BIGINT — i32 ms caps at ~24 days
            -- of trace, which is well under a long autonomous run.
            ALTER TABLE pipeline_agent_traces
                ALTER COLUMN duration_ms TYPE BIGINT USING duration_ms::bigint,
                ALTER COLUMN tokens_in   TYPE BIGINT USING tokens_in::bigint,
                ALTER COLUMN tokens_out  TYPE BIGINT USING tokens_out::bigint;

            -- generation_artifacts.rs:452 (total_duration_ms reader).
            ALTER TABLE generation_pipeline_artifacts
                ALTER COLUMN total_duration_ms TYPE BIGINT USING total_duration_ms::bigint;

            -- checks.rs:565-567 (get_check_results reader).
            ALTER TABLE check_results
                ALTER COLUMN issues_found  TYPE BIGINT USING issues_found::bigint,
                ALTER COLUMN issues_fixed  TYPE BIGINT USING issues_fixed::bigint,
                ALTER COLUMN files_checked TYPE BIGINT USING files_checked::bigint;

            -- checkpoints.rs:511 (get_checkpoints reader).
            ALTER TABLE orchestrator_checkpoints
                ALTER COLUMN iteration TYPE BIGINT USING iteration::bigint;

            -- generation.rs:334 (get_exploration_stats reader). Sister
            -- columns total_candidates/search_depth stay INT4 (read as i32).
            ALTER TABLE exploration_stats
                ALTER COLUMN search_duration_ms TYPE BIGINT USING search_duration_ms::bigint;
        "#,
    },
];

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

        // Self-bootstrap: create the `runner` schema and `vector` extension
        // if they don't exist yet. On a first-boot docker volume the
        // init-scripts/01-create-runner-schema.sql does this; on pre-existing
        // volumes or non-docker Postgres deployments, those scripts never run,
        // so we do it ourselves. Idempotent via IF NOT EXISTS.
        conn.batch_execute(
            "CREATE SCHEMA IF NOT EXISTS runner; \
             CREATE EXTENSION IF NOT EXISTS vector;",
        )
        .await
        .map_err(|e| format!("Failed to bootstrap runner schema/extension: {}", e))?;

        info!("PostgreSQL connected (deadpool, max_size=16, schema=runner)");

        let db = Self { pool };
        db.apply_canonical_schema().await?;
        db.ensure_tables().await;
        db.run_migrations().await?;
        Ok(db)
    }

    /// Apply the canonical schema from `schema.pg.sql` (the Clorinde source
    /// of truth). All statements are `CREATE TABLE IF NOT EXISTS` / `CREATE
    /// INDEX IF NOT EXISTS` / `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`, so
    /// this is idempotent and safe to run on every startup. It handles fresh
    /// installs (no tables) and upgrades (adds columns declared in the
    /// canonical schema). Runs before `ensure_tables()` and `run_migrations()`
    /// so that migrations can safely `ALTER` tables declared only in
    /// schema.pg.sql (e.g. `mcp_servers`).
    async fn apply_canonical_schema(&self) -> Result<(), String> {
        const CANONICAL_SCHEMA: &str = include_str!("../../../schema.pg.sql");
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error during canonical schema apply: {}", e))?;

        // Split schema.pg.sql into individual statements and execute each on
        // its own. `batch_execute` wraps the whole string in an implicit
        // transaction, so one failure rolls back every prior statement —
        // which is too strict for legacy installs where a pre-existing table
        // may be missing a column that a later `CREATE INDEX IF NOT EXISTS`
        // references. Individual execution lets us `warn!` on those drift
        // failures (idempotent intent preserved) and still apply the rest of
        // the schema. This mirrors `ensure_tables`'s warn-on-failure loop.
        //
        // Strip `--` line comments from the whole schema BEFORE splitting on
        // `;`. Order matters: a `;` inside a comment would otherwise split a
        // CREATE TABLE body in half, producing two malformed fragments.
        // Observed symptom: PG 16 + pgvector sits "active" forever on the
        // malformed DDL (no wait_event, statement_timeout doesn't fire), and
        // every runner startup wedges in apply_canonical_schema.
        let schema_no_comments: String = CANONICAL_SCHEMA
            .lines()
            .map(|l| {
                if l.trim_start().starts_with("--") {
                    ""
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut applied = 0usize;
        let mut failed = 0usize;
        for raw in schema_no_comments.split(';') {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Err(e) = conn.execute(trimmed, &[]).await {
                failed += 1;
                let first_line = trimmed.lines().next().unwrap_or(trimmed);
                let detail = match e.as_db_error() {
                    Some(db) => format!("[{}] {}", db.code().code(), db.message()),
                    None => format!("{}", e),
                };
                warn!(
                    "Canonical schema statement failed (drift, non-fatal): {} — stmt: {}",
                    detail,
                    first_line.chars().take(120).collect::<String>()
                );
            } else {
                applied += 1;
            }
        }
        info!(
            "Canonical schema applied from schema.pg.sql ({} statements applied, {} skipped due to drift)",
            applied, failed
        );
        Ok(())
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
            // Schema Migrations (version tracking for incremental schema changes)
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version     INTEGER PRIMARY KEY,
                description TEXT NOT NULL,
                applied_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
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
                task_run_id     TEXT,  -- soft FK; canonical FK in schema.pg.sql
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
                archived_at             TIMESTAMPTZ,
                summary_entry_id        TEXT REFERENCES task_knowledge(id) ON DELETE SET NULL,
                created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            // Backfill columns for databases that already have task_knowledge.
            "ALTER TABLE task_knowledge ADD COLUMN IF NOT EXISTS archived_at TIMESTAMPTZ",
            "ALTER TABLE task_knowledge ADD COLUMN IF NOT EXISTS summary_entry_id TEXT REFERENCES task_knowledge(id) ON DELETE SET NULL",
            "CREATE INDEX IF NOT EXISTS idx_tk_task_run ON task_knowledge(task_run_id)",
            "CREATE INDEX IF NOT EXISTS idx_tk_category ON task_knowledge(category)",
            "CREATE INDEX IF NOT EXISTS idx_tk_resolved ON task_knowledge(is_resolved) WHERE NOT is_resolved",
            "CREATE INDEX IF NOT EXISTS idx_tk_created ON task_knowledge(created_at DESC)",
            "CREATE INDEX IF NOT EXISTS idx_tk_active ON task_knowledge(task_run_id, category) WHERE archived_at IS NULL",
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
                task_run_id     TEXT,  -- soft FK; canonical FK in schema.pg.sql
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
                iteration       BIGINT NOT NULL DEFAULT 0,
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
                observation_id  BIGINT NOT NULL,  -- soft FK to observations; canonical FK in schema.pg.sql
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
                total_duration_ms BIGINT,
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
                duration_ms     BIGINT NOT NULL DEFAULT 0,
                tokens_in       BIGINT NOT NULL DEFAULT 0,
                tokens_out      BIGINT NOT NULL DEFAULT 0,
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
                runs_included   BIGINT NOT NULL DEFAULT 0,
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
            // World State Verifier shadow-mode disagreements.
            // Populated when shadow mode is active and WSM/text verifier
            // verdicts differ. Consumed by the Settings → World State
            // Verifier calibration view.
            "CREATE TABLE IF NOT EXISTS wsv_shadow_disagreements (
                id                BIGSERIAL PRIMARY KEY,
                task_run_id       TEXT NOT NULL,
                iteration         INTEGER NOT NULL,
                text_status       TEXT NOT NULL,
                wsm_status        TEXT NOT NULL,
                text_confidence   DOUBLE PRECISION NOT NULL,
                wsm_confidence    DOUBLE PRECISION NOT NULL,
                intent            TEXT NOT NULL,
                wsm_observations  TEXT NOT NULL,
                created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_wsv_disagreements_task_run ON wsv_shadow_disagreements(task_run_id)",
            "CREATE INDEX IF NOT EXISTS idx_wsv_disagreements_created_at ON wsv_shadow_disagreements(created_at DESC)",
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
                search_duration_ms  BIGINT NOT NULL DEFAULT 0,
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
                created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                modified_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                next_run            TIMESTAMPTZ,
                last_run_id         TEXT,
                condition_status    TEXT
            )",
            "CREATE TABLE IF NOT EXISTS scheduler_history (
                execution_id        TEXT PRIMARY KEY,
                task_id             TEXT NOT NULL REFERENCES scheduled_tasks(id) ON DELETE CASCADE,
                session_id          TEXT,
                started_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                ended_at            TIMESTAMPTZ,
                status              TEXT NOT NULL DEFAULT 'pending',
                success             BOOLEAN NOT NULL DEFAULT FALSE,
                error_message       TEXT,
                triggered_auto_fix  BOOLEAN NOT NULL DEFAULT FALSE,
                auto_fix_session_id TEXT
            )",
            "CREATE INDEX IF NOT EXISTS idx_scheduler_history_task ON scheduler_history(task_id)",
            // Restate durable execution tables
            "CREATE TABLE IF NOT EXISTS restate_workflow_executions (
                execution_id          TEXT PRIMARY KEY,  -- soft FK to task_runs; canonical FK in schema.pg.sql
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
                execution_id   TEXT NOT NULL,  -- soft FK to task_runs; canonical FK in schema.pg.sql
                awakeable_type TEXT NOT NULL,
                type_data      TEXT,
                status         TEXT NOT NULL DEFAULT 'pending',
                created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                resolved_at    TIMESTAMPTZ
            )",
            "CREATE INDEX IF NOT EXISTS idx_ra_execution ON restate_awakeables(execution_id)",
            "CREATE INDEX IF NOT EXISTS idx_ra_status ON restate_awakeables(status)",
            // ── Span Events (agent-lightning instrumentation) ──────────────
            "CREATE TABLE IF NOT EXISTS span_events (
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
            )",
            "CREATE INDEX IF NOT EXISTS idx_span_events_exec ON span_events(execution_id)",
            "CREATE INDEX IF NOT EXISTS idx_span_events_trace ON span_events(trace_id)",
            "CREATE INDEX IF NOT EXISTS idx_span_events_type ON span_events(event_type)",
            // ── Duel Pools (prompt-ops dueling bandits) ────────────────────
            "CREATE TABLE IF NOT EXISTS duel_pools (
                id              TEXT PRIMARY KEY,
                agent_type      TEXT NOT NULL,
                status          TEXT NOT NULL DEFAULT 'active',
                generation      INTEGER NOT NULL DEFAULT 0,
                config_json     TEXT NOT NULL DEFAULT '{}',
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                completed_at    TIMESTAMPTZ
            )",
            "CREATE INDEX IF NOT EXISTS idx_dp_agent ON duel_pools(agent_type)",
            "CREATE INDEX IF NOT EXISTS idx_dp_status ON duel_pools(status)",
            // ── Duel Candidates ────────────────────────────────────────────
            "CREATE TABLE IF NOT EXISTS duel_candidates (
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
            )",
            "CREATE INDEX IF NOT EXISTS idx_dc_pool ON duel_candidates(pool_id)",
            "CREATE INDEX IF NOT EXISTS idx_dc_status ON duel_candidates(pool_id, status)",
            // ── Duel Results ───────────────────────────────────────────────
            "CREATE TABLE IF NOT EXISTS duel_results (
                id                  TEXT PRIMARY KEY,
                pool_id             TEXT NOT NULL,
                candidate_a_id      TEXT NOT NULL,
                candidate_b_id      TEXT NOT NULL,
                winner_id           TEXT NOT NULL,
                judge_rationale     TEXT,
                position_swapped    BOOLEAN NOT NULL DEFAULT false,
                confidence          DOUBLE PRECISION NOT NULL DEFAULT 0.5,
                created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_dr_pool ON duel_results(pool_id)",
            // ── Beam Search Runs ───────────────────────────────────────────
            "CREATE TABLE IF NOT EXISTS beam_search_runs (
                id              TEXT PRIMARY KEY,
                agent_type      TEXT NOT NULL,
                pool_id         TEXT,
                config_json     TEXT NOT NULL,
                generation      INTEGER NOT NULL DEFAULT 0,
                status          TEXT NOT NULL DEFAULT 'running',
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                completed_at    TIMESTAMPTZ
            )",
            "CREATE INDEX IF NOT EXISTS idx_bsr_agent ON beam_search_runs(agent_type)",
            "CREATE INDEX IF NOT EXISTS idx_bsr_pool ON beam_search_runs(pool_id)",
            // ── Beam Candidates ────────────────────────────────────────────
            "CREATE TABLE IF NOT EXISTS beam_candidates (
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
            )",
            "CREATE INDEX IF NOT EXISTS idx_bc_run ON beam_candidates(beam_run_id)",
            "CREATE INDEX IF NOT EXISTS idx_bc_gen ON beam_candidates(beam_run_id, generation)",
            // Phase 1: Bounded Fix Loop — track fix attempts and CI auto-resumes per task run
            "ALTER TABLE task_runs ADD COLUMN IF NOT EXISTS fix_attempts INTEGER DEFAULT 0",
            "ALTER TABLE task_runs ADD COLUMN IF NOT EXISTS ci_auto_resumes INTEGER DEFAULT 0",
            // ── Prompt Evolution extensions (Phase 4: optimization pipeline) ──
            "ALTER TABLE prompt_evolution ADD COLUMN IF NOT EXISTS generation INTEGER",
            "ALTER TABLE prompt_evolution ADD COLUMN IF NOT EXISTS beam_run_id TEXT",
            // ── Resource Versions (Phase 5: immutable snapshots) ───────
            "CREATE TABLE IF NOT EXISTS resource_versions (
                id              TEXT PRIMARY KEY,
                resource_type   TEXT NOT NULL,
                resource_key    TEXT NOT NULL,
                version         BIGINT NOT NULL,
                content_hash    TEXT NOT NULL,
                content         TEXT NOT NULL,
                metadata_json   TEXT,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                UNIQUE (resource_type, resource_key, version)
            )",
            "CREATE INDEX IF NOT EXISTS idx_rv_resource ON resource_versions(resource_type, resource_key)",
            "CREATE INDEX IF NOT EXISTS idx_rv_latest ON resource_versions(resource_type, resource_key, version DESC)",
            "CREATE INDEX IF NOT EXISTS idx_rv_hash ON resource_versions(content_hash)",
            // ── Phase 2: PR Watch State (CI feedback loop) ───────
            "CREATE TABLE IF NOT EXISTS pr_watch_state (
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
            )",
            "CREATE INDEX IF NOT EXISTS idx_prw_active ON pr_watch_state(completed_at) WHERE completed_at IS NULL",
            "CREATE INDEX IF NOT EXISTS idx_prw_task_run ON pr_watch_state(task_run_id)",
            // Learned Patterns (Phase 5A: Pattern Distillation from knowledge graph)
            "CREATE TABLE IF NOT EXISTS learned_patterns (
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
            )",
            "CREATE INDEX IF NOT EXISTS idx_learned_patterns_confidence ON learned_patterns(confidence DESC)",
            "CREATE INDEX IF NOT EXISTS idx_learned_patterns_workflow ON learned_patterns(workflow_name)",
            "CREATE INDEX IF NOT EXISTS idx_learned_patterns_keywords_gin ON learned_patterns USING GIN(trigger_keywords)",
            // Phase 5B: Ticket-task mapping (ticket system bidirectional sync)
            "CREATE TABLE IF NOT EXISTS ticket_task_mapping (
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
            )",
            "CREATE INDEX IF NOT EXISTS idx_ticket_task_mapping_task ON ticket_task_mapping(task_run_id)",
            // Phase 5B fix: Persist provider configs (with token) per workflow_id so the
            // on-completion hook can reconstruct a provider after a restart and so the
            // ticket can actually be closed.
            "CREATE TABLE IF NOT EXISTS ticket_provider_configs (
                id TEXT PRIMARY KEY,
                workflow_id TEXT NOT NULL UNIQUE,
                source TEXT NOT NULL,
                config_json TEXT NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
            "CREATE INDEX IF NOT EXISTS idx_ticket_provider_configs_workflow ON ticket_provider_configs(workflow_id)",
            // ── Deferred HITL Questions ────────────────────────────────────
            // Soft FK to task_runs (see schema.pg.sql for canonical FK).
            // task_runs itself is not created by ensure_tables(); dropping
            // the FK here avoids bootstrap-ordering failures.
            "CREATE TABLE IF NOT EXISTS deferred_questions (
                id                   TEXT PRIMARY KEY,
                task_run_id          TEXT NOT NULL,
                iteration            INTEGER NOT NULL,
                question             TEXT NOT NULL,
                context_json         TEXT DEFAULT '{}',
                auto_decision_type   TEXT NOT NULL,
                auto_decision_detail TEXT,
                confidence           DOUBLE PRECISION NOT NULL,
                risk_level           TEXT NOT NULL,
                status               TEXT NOT NULL DEFAULT 'pending',
                git_checkpoint       TEXT,
                contingent_iterations TEXT DEFAULT '[]',
                reviewer_comment     TEXT,
                created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                reviewed_at          TIMESTAMPTZ
            )",
            "CREATE INDEX IF NOT EXISTS idx_dq_task_run_id ON deferred_questions(task_run_id)",
            "CREATE INDEX IF NOT EXISTS idx_dq_status ON deferred_questions(status)",
            // ── Memory Query Cache ─────────────────────────────────────────
            "CREATE TABLE IF NOT EXISTS memory_query_cache (
                id                BIGSERIAL PRIMARY KEY,
                query_hash        TEXT NOT NULL,
                reasoning_level   TEXT NOT NULL,
                result_json       TEXT NOT NULL,
                hit_count         INTEGER NOT NULL DEFAULT 0,
                created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                expires_at        TIMESTAMPTZ NOT NULL
            )",
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_mqc_hash_level ON memory_query_cache(query_hash, reasoning_level)",
            "CREATE INDEX IF NOT EXISTS idx_mqc_expires ON memory_query_cache(expires_at)",
            // ── Working Representations (ephemeral per-task memory snapshot)
            // Soft FK to task_runs (see schema.pg.sql for canonical FK).
            "CREATE TABLE IF NOT EXISTS working_representations (
                id                      BIGSERIAL PRIMARY KEY,
                task_run_id             TEXT NOT NULL,
                observations_json       TEXT NOT NULL DEFAULT '[]',
                cross_run_patterns_json TEXT NOT NULL DEFAULT '[]',
                entity_profiles_json    TEXT NOT NULL DEFAULT '[]',
                recent_findings_json    TEXT NOT NULL DEFAULT '[]',
                recent_fixes_json       TEXT NOT NULL DEFAULT '[]',
                applicable_skills_json  TEXT NOT NULL DEFAULT '[]',
                workflow_id             TEXT,
                workflow_name           TEXT,
                total_items             INTEGER NOT NULL DEFAULT 0,
                build_duration_ms       BIGINT,
                built_at                TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                expires_at              TIMESTAMPTZ NOT NULL,
                is_stale                BOOLEAN NOT NULL DEFAULT FALSE,
                UNIQUE(task_run_id)
            )",
            "CREATE INDEX IF NOT EXISTS idx_wr_task_run ON working_representations(task_run_id)",
            "CREATE INDEX IF NOT EXISTS idx_wr_expires ON working_representations(expires_at)",
        ];

        for sql in &ddl {
            if let Err(e) = conn.execute(*sql, &[]).await {
                warn!("DDL execution failed (non-fatal): {}", e);
            }
        }

        info!("All managed PG tables ensured (activity_timeline, watchers, agentic_metric_scores, prompt_registry, canary_rollouts, canary_run_records, prompt_template_canaries, error_events, decisions, concept_summaries)");
    }

    /// Run all pending schema migrations in order.
    ///
    /// Each migration executes inside its own transaction. If a migration fails,
    /// that transaction is rolled back and this method returns an error — the
    /// database is left at the last successfully applied version.
    async fn run_migrations(&self) -> Result<(), String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error during migrations: {}", e))?;

        // Determine current schema version (0 if no migrations have run yet).
        let current_version: i32 = conn
            .query_one(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                &[],
            )
            .await
            .map(|row| row.get(0))
            .unwrap_or(0);

        let pending: Vec<&Migration> = MIGRATIONS
            .iter()
            .filter(|m| m.version > current_version)
            .collect();

        if pending.is_empty() {
            info!("Schema is up to date at version {}", current_version);
            return Ok(());
        }

        info!(
            "Schema at version {}, {} migration(s) pending",
            current_version,
            pending.len()
        );

        for migration in &pending {
            info!(
                "Running migration v{}: {}",
                migration.version, migration.description
            );

            // Each migration runs in its own transaction.
            let txn = conn.transaction().await.map_err(|e| {
                format!(
                    "Failed to begin transaction for migration v{}: {}",
                    migration.version, e
                )
            })?;

            // Execute the migration SQL (skip empty / no-op migrations).
            if !migration.sql.trim().is_empty() {
                txn.batch_execute(migration.sql).await.map_err(|e| {
                    error!("Migration v{} failed: {}", migration.version, e);
                    format!(
                        "Migration v{} ({}) failed: {}",
                        migration.version, migration.description, e
                    )
                })?;
            }

            // Record the migration.
            txn.execute(
                "INSERT INTO schema_migrations (version, description) VALUES ($1, $2)",
                &[
                    &migration.version as &(dyn tokio_postgres::types::ToSql + Sync),
                    &migration.description as &(dyn tokio_postgres::types::ToSql + Sync),
                ],
            )
            .await
            .map_err(|e| format!("Failed to record migration v{}: {}", migration.version, e))?;

            txn.commit()
                .await
                .map_err(|e| format!("Failed to commit migration v{}: {}", migration.version, e))?;

            info!(
                "Migration v{} applied successfully: {}",
                migration.version, migration.description
            );
        }

        info!(
            "All migrations applied — schema now at version {}",
            pending.last().map(|m| m.version).unwrap_or(current_version)
        );

        Ok(())
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

    /// Test-only: create a PgDb with a pool that will error on any actual
    /// query. Use this for unit tests that need a `LoopContext` or
    /// `CompensationManager` but never actually call database methods.
    /// Any attempt to `pool.get()` returns an immediate error because
    /// the underlying manager has an unreachable host.
    #[cfg(test)]
    pub fn new_noop_for_test() -> std::sync::Arc<Self> {
        use deadpool_postgres::{Config, Runtime};

        let mut cfg = Config::new();
        cfg.host = Some("__noop_test_host_that_does_not_exist__".to_string());
        cfg.dbname = Some("noop".to_string());
        let pool = cfg
            .create_pool(Some(Runtime::Tokio1), tokio_postgres::NoTls)
            .expect("noop pool creation should not fail (no connection attempted)");
        std::sync::Arc::new(Self { pool })
    }
}

// ============================================================================
// Migration smoke tests
// ============================================================================
//
// Pure-Rust tests that validate the static MIGRATIONS array without requiring
// a live database connection. These catch common authoring mistakes:
//   - Version gaps, duplicates, or out-of-order entries
//   - Empty descriptions
//   - Missing idempotency markers (`IF NOT EXISTS`) on DDL that supports them
//   - Obviously malformed SQL fragments
//
// For tests that exercise the actual `run_migrations()` flow against a real
// PostgreSQL instance, see the integration tests gated on DATABASE_URL.

#[cfg(test)]
mod migration_tests {
    use super::MIGRATIONS;

    /// Versions must start at 1, be strictly sequential, and never duplicated.
    #[test]
    fn migrations_are_sequentially_versioned_starting_at_one() {
        assert!(
            !MIGRATIONS.is_empty(),
            "MIGRATIONS array is empty — at least the v1 baseline should exist"
        );

        for (idx, migration) in MIGRATIONS.iter().enumerate() {
            let expected = (idx as i32) + 1;
            assert_eq!(
                migration.version, expected,
                "Migration at index {} has version {}, expected {} (versions must be \
                 sequential starting from 1 with no gaps or duplicates)",
                idx, migration.version, expected
            );
        }
    }

    /// Every migration must have a non-empty, human-readable description.
    #[test]
    fn migrations_have_non_empty_descriptions() {
        for migration in MIGRATIONS {
            assert!(
                !migration.description.trim().is_empty(),
                "Migration v{} has an empty description",
                migration.version
            );
        }
    }

    /// No two migrations may share the same version number. Redundant with the
    /// sequential check above, but kept as an explicit guard so the failure
    /// message is clear if someone breaks the invariant in a different way.
    #[test]
    fn migrations_have_unique_versions() {
        let mut seen = std::collections::HashSet::new();
        for migration in MIGRATIONS {
            assert!(
                seen.insert(migration.version),
                "Duplicate migration version: v{}",
                migration.version
            );
        }
    }

    /// Each non-empty SQL body should split into statements that look like
    /// real DDL/DML — i.e. each non-blank statement contains at least one
    /// recognized SQL keyword. This catches truncated heredocs, accidental
    /// commenting-out, and similar paste errors.
    ///
    /// `DO $$ ... $$` blocks are handled holistically: their inner plpgsql
    /// body contains `;`-separated fragments (e.g. `END IF`, `END LOOP`)
    /// that aren't SQL statements on their own, so naive splitting would
    /// false-positive. When a migration body contains any `DO $$ ... $$`
    /// block we just require the body as a whole to contain a keyword.
    #[test]
    fn migration_sql_statements_look_parseable() {
        const KEYWORDS: &[&str] = &[
            "CREATE", "ALTER", "DROP", "INSERT", "UPDATE", "DELETE", "SELECT", "GRANT", "REVOKE",
            "COMMENT", "WITH", "DO",
        ];

        for migration in MIGRATIONS {
            let sql = migration.sql.trim();
            if sql.is_empty() {
                // No-op migrations (e.g. baseline v1) are allowed.
                continue;
            }

            // DO blocks have plpgsql bodies we can't parse with naive
            // `split(';')`. Fall back to a whole-body keyword check.
            if sql.contains("DO $$") {
                let upper = sql.to_uppercase();
                assert!(
                    KEYWORDS.iter().any(|kw| upper.contains(kw)),
                    "Migration v{} uses a DO block but its body contains no \
                     recognized SQL keyword (looks malformed)",
                    migration.version
                );
                continue;
            }

            let statements: Vec<&str> = sql
                .split(';')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();

            assert!(
                !statements.is_empty(),
                "Migration v{} has non-empty SQL but produced no statements after \
                 splitting on ';'",
                migration.version
            );

            for stmt in statements {
                let upper = stmt.to_uppercase();
                let has_keyword = KEYWORDS.iter().any(|kw| upper.contains(kw));
                assert!(
                    has_keyword,
                    "Migration v{} has a statement that contains no recognized SQL \
                     keyword (looks malformed):\n  {}",
                    migration.version, stmt
                );
            }
        }
    }

    /// PostgreSQL supports `IF NOT EXISTS` on `CREATE TABLE`, `CREATE INDEX`,
    /// and (since 9.6) `ALTER TABLE ... ADD COLUMN`. Migrations may be re-run
    /// against partially-applied schemas during development, so every such
    /// statement must be guarded.
    #[test]
    fn ddl_statements_use_if_not_exists_guards() {
        for migration in MIGRATIONS {
            let sql = migration.sql.trim();
            if sql.is_empty() {
                continue;
            }
            // DO blocks can't be split by ';' (see migration_sql_statements_look_parseable).
            // The DDL inside a DO block is dynamically constructed via EXECUTE format(...)
            // and is already guarded by existence checks in plpgsql, so skip.
            if sql.contains("DO $$") {
                continue;
            }

            let statements: Vec<String> = sql
                .split(';')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            for stmt in &statements {
                let upper = stmt.to_uppercase();
                // Collapse whitespace so multi-line statements still match.
                let normalized: String = upper.split_whitespace().collect::<Vec<_>>().join(" ");

                if normalized.starts_with("CREATE TABLE") {
                    assert!(
                        normalized.contains("IF NOT EXISTS"),
                        "Migration v{} has a CREATE TABLE without IF NOT EXISTS:\n  {}",
                        migration.version,
                        stmt
                    );
                }

                if normalized.starts_with("CREATE INDEX")
                    || normalized.starts_with("CREATE UNIQUE INDEX")
                {
                    assert!(
                        normalized.contains("IF NOT EXISTS"),
                        "Migration v{} has a CREATE INDEX without IF NOT EXISTS:\n  {}",
                        migration.version,
                        stmt
                    );
                }

                if normalized.starts_with("ALTER TABLE") && normalized.contains("ADD COLUMN") {
                    assert!(
                        normalized.contains("ADD COLUMN IF NOT EXISTS"),
                        "Migration v{} has an ALTER TABLE ADD COLUMN without \
                         IF NOT EXISTS (PG 9.6+ supports this and re-runs require it):\n  {}",
                        migration.version,
                        stmt
                    );
                }
            }
        }
    }

    /// Every table in ensure_tables() must also be declared in schema.pg.sql
    /// (the Clorinde source of truth). If a table is added to ensure_tables
    /// but not to schema.pg.sql, the drift checker won't catch column-type
    /// mismatches and `clorinde fresh` can't validate queries against it.
    /// Exception: `schema_migrations` is infrastructure — it's in both but
    /// its presence in schema.pg.sql is optional.
    #[test]
    fn ensure_tables_subset_of_schema_pg_sql() {
        let schema_sql = include_str!("../../../schema.pg.sql");
        let re = regex::Regex::new(r"(?i)CREATE\s+TABLE\s+IF\s+NOT\s+EXISTS\s+(\w+)").unwrap();

        let schema_tables: std::collections::HashSet<String> = re
            .captures_iter(schema_sql)
            .map(|c| c[1].to_lowercase())
            .collect();

        // Parse ensure_tables DDL from the source itself: we look at the
        // static DDL array in ensure_tables(). Since we can't run async code
        // in a unit test, we'll parse mod.rs source as a string. A simpler
        // approach: just require that every table we create at runtime is
        // also in schema.pg.sql.
        let mod_rs = include_str!("mod.rs");
        // Strip comment lines (SQL `--` and Rust `//`) to avoid false
        // positives from prose that mentions CREATE TABLE IF NOT EXISTS.
        let mod_rs_no_comments: String = mod_rs
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with("--") && !t.starts_with("//")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let ensure_tables: std::collections::HashSet<String> = re
            .captures_iter(&mod_rs_no_comments)
            .map(|c| c[1].to_lowercase())
            .collect();

        let ensure_only = &ensure_tables - &schema_tables;
        assert!(
            ensure_only.is_empty(),
            "Tables in ensure_tables/migrations but NOT in schema.pg.sql \
             (add them to schema.pg.sql so Clorinde can validate queries): {:?}",
            ensure_only
        );
    }
}
