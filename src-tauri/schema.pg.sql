-- Runner PostgreSQL schema (Clorinde source of truth).
-- Each table here is validated against all queries in queries/*.sql.
-- This file is loaded by `clorinde fresh` into a temporary database for validation.

-- Stub for FK constraint (full task_runs migration comes later)
CREATE TABLE IF NOT EXISTS task_runs (
    id TEXT PRIMARY KEY,
    total_input_tokens BIGINT DEFAULT 0,
    total_output_tokens BIGINT DEFAULT 0,
    total_cost_cents BIGINT DEFAULT 0
);

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
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_ptu_task_run ON phase_token_usage(task_run_id);
CREATE INDEX IF NOT EXISTS idx_ptu_created_at ON phase_token_usage(created_at);
