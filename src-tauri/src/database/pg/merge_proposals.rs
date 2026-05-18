//! Self-heal helper for `coord.merge_proposals` + `coord.merge_proposal_repos`.
//!
//! Plan reference:
//! `D:/qontinui-root/plans/2026-05-14-branch-per-agent-coordination-plan.md`
//! §4.5 (Merge proposal API). The schema is authored authoritatively in
//! alembic revision `coord_phase_3_01_merge_proposals`; this module mirrors
//! it via the self-heal pattern documented in memory
//! [[proj_pg_dual_schema_runner_public]] — the runner can ensure the
//! tables exist without waiting for qontinui-web's alembic upgrade to run.
//!
//! Unlike `agent_worktrees.rs`, the runner does NOT execute write paths
//! against these tables — `POST /merge/propose` and its siblings live
//! entirely in qontinui-coord. The ensure_* helper is provided so a
//! runner-driven local-coord harness (or a fresh dev database whose
//! alembic chain hasn't reached Phase 3 yet) can still allocate the
//! tables before coord first writes to them. Schema must stay
//! byte-equivalent to the alembic migration — downstream phases (Phase
//! 4 scheduler) depend on the column shape.

use super::PgDb;

impl PgDb {
    /// Self-heal `coord.merge_proposals` + `coord.merge_proposal_repos`.
    /// Idempotent — `CREATE TABLE IF NOT EXISTS` + `ADD CONSTRAINT IF
    /// NOT EXISTS` pattern, matching the convention in
    /// `ensure_agent_worktrees_table`.
    ///
    /// Allowed `status` values are kept in sync with the alembic CHECK
    /// constraint and the Rust enum in `qontinui-coord/src/merge.rs`.
    pub async fn ensure_merge_proposals_tables(&self) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.batch_execute(
            r#"
            CREATE SCHEMA IF NOT EXISTS coord;

            CREATE TABLE IF NOT EXISTS coord.merge_proposals (
                proposal_id        UUID PRIMARY KEY,
                agent_id           UUID NOT NULL,
                description        TEXT,
                requires_clean_ci  BOOLEAN NOT NULL DEFAULT true,
                status             TEXT NOT NULL DEFAULT 'queued',
                error              TEXT,
                cancelled_at       TIMESTAMPTZ,
                merged_at          TIMESTAMPTZ,
                created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
            );

            DO $$
            BEGIN
                IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                    WHERE conname = 'merge_proposals_status_chk'
                ) THEN
                    ALTER TABLE coord.merge_proposals
                        ADD CONSTRAINT merge_proposals_status_chk
                        CHECK (status IN (
                            'queued', 'dry-rebasing', 'awaiting-ci',
                            'landing', 'blocked-by-overlap', 'conflict',
                            'merged', 'cancelled'
                        ));
                END IF;
            END$$;

            CREATE INDEX IF NOT EXISTS idx_merge_proposals_status
                ON coord.merge_proposals (status, proposal_id DESC);
            CREATE INDEX IF NOT EXISTS idx_merge_proposals_agent_id
                ON coord.merge_proposals (agent_id);

            CREATE TABLE IF NOT EXISTS coord.merge_proposal_repos (
                proposal_id     UUID NOT NULL REFERENCES coord.merge_proposals(proposal_id)
                                ON DELETE CASCADE,
                repo            TEXT NOT NULL,
                branch          TEXT NOT NULL,
                head_sha        TEXT NOT NULL,
                rebase_result   JSONB,
                conflict_diff   TEXT,
                ci_run_url      TEXT,
                overlap_paths   TEXT[],
                created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
                PRIMARY KEY (proposal_id, repo)
            );

            CREATE INDEX IF NOT EXISTS idx_merge_proposal_repos_head_sha
                ON coord.merge_proposal_repos (repo, head_sha);

            -- Plan `coord-agent-session-id-tracking.md` Phase 2 (Side B):
            -- agent_session_id lineage column on the parent proposal row.
            -- Mirrors the alembic migration shape. NULL until Phase 6
            -- tightens to required after Side C2 lands.
            ALTER TABLE coord.merge_proposals
                ADD COLUMN IF NOT EXISTS agent_session_id UUID;

            CREATE INDEX IF NOT EXISTS idx_merge_proposals_agent_session
                ON coord.merge_proposals (agent_session_id)
                WHERE agent_session_id IS NOT NULL;
            "#,
        )
        .await
        .map_err(|e| format!("Failed to ensure coord.merge_proposal* tables: {}", e))
    }
}
