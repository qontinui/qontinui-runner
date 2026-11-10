//! PostgreSQL CRUD for `atlas_managed.spec_proposals` — Stream E (Flywheel) queue.
//!
//! The table is authored declaratively in `atlas/schema.hcl`; this module
//! issues the queries directly via `tokio_postgres`, mirroring the
//! `regression.rs` / `merge_proposals.rs` style. There is no self-heal helper
//! here — Atlas owns the table shape and the runner expects it to be present
//! when `feature = "spec-authoring"` is enabled.
//!
//! Lifecycle (`status` column) is driven by Stream E:
//!   queued → inFlight → pendingValidation → pendingPromotion → promoted
//!                                                                    failed
//! See `spec_api/proposals.rs`: the execute handler claims a `queued` row
//! (`queued → inFlight`, one compare-and-set that writes a claim token into
//! `metadata.execute`) and moves it on to `pendingPromotion | failed`, or back
//! to `queued` when the device drain or the agent registry does not admit the
//! run, or when the execute is abandoned before its outcome is written. Each
//! of those writes lands only while the execute's claim is still held. The
//! scan re-queues a row whose claim has outlived the claimant's own bound (an
//! execute that died) and stops the task run that execute recorded. The
//! sweep-pending handler moves `pendingPromotion → promoted` after 2
//! consecutive greens.
//!
//! Every row belongs to ONE app (`app_id`, the registry id the scan ran for),
//! and every read here is scoped to it: an app's sweep, list and execute never
//! see another app's rows, and the dedup index includes it, so two apps may
//! queue the same page label.

use super::PgDb;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Serializes the PG tests that TRUNCATE `atlas_managed.spec_proposals` and
/// then assert on its contents (`spec_api::proposals::tests::pg`,
/// `flywheel_e2e_tests`): run concurrently, one test's TRUNCATE wipes the row
/// another just inserted.
#[cfg(all(test, feature = "pg_integration_tests"))]
pub(crate) static SPEC_PROPOSALS_TEST_LOCK: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

/// One row from `atlas_managed.spec_proposals`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecProposalRow {
    pub id: String,
    /// The app whose scan queued this row (`project.apps.app_id`).
    pub app_id: String,
    pub kind: String,
    pub pathname: Option<String>,
    pub spec_id: Option<String>,
    pub status: String,
    pub created_at: String,
    pub last_attempt_at: Option<String>,
    pub consecutive_greens: i32,
    pub last_error: Option<String>,
    pub candidate_ir: Option<Value>,
    pub metadata: Value,
}

/// What [`PgDb::requeue_stale_in_flight_proposals`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StaleRequeue {
    /// Proposals put back in the queue.
    pub requeued: u64,
    /// Task runs those proposals' dead executes had recorded, stopped because
    /// they had not finished.
    pub runs_stopped: u64,
}

impl PgDb {
    // -------------------------------------------------------------------------
    // Writes
    // -------------------------------------------------------------------------

    /// Insert a new proposal row for `app_id`. Returns `Ok(true)` if a row was
    /// inserted, `Ok(false)` if the unique-on-(app_id, kind,
    /// COALESCE(pathname, spec_id)) index suppressed the insert via
    /// `ON CONFLICT DO NOTHING`.
    ///
    /// `kind` must be `"fullPage"` or `"patch"`; the CHECK constraint enforces
    /// this server-side, but callers should validate upstream so the rejection
    /// surfaces as a structured `SpecError` rather than a raw PG error.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_proposal(
        &self,
        id: &str,
        app_id: &str,
        kind: &str,
        pathname: Option<&str>,
        spec_id: Option<&str>,
        status: &str,
        metadata: &Value,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // ON CONFLICT DO NOTHING with no target catches both the PK and the
        // unique `spec_proposals_app_kind_target_uniq` index. The PK is a fresh
        // UUIDv7-shaped id from `mint_proposal_id` so PK collisions are
        // effectively zero — the only realistic conflict is the unique
        // dedup index, which is the intended swallow.
        let rows_affected = conn
            .execute(
                r#"
                INSERT INTO atlas_managed.spec_proposals (id, app_id, kind, pathname, spec_id, status, metadata)
                VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb)
                ON CONFLICT DO NOTHING
                "#,
                &[&id, &app_id, &kind, &pathname, &spec_id, &status, metadata],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG insert_proposal", &e))?;

        Ok(rows_affected == 1)
    }

    /// Update only the status of a proposal. Does NOT touch
    /// `last_attempt_at` / `last_error` — use the more specific helpers when
    /// recording attempts or failures.
    pub async fn update_proposal_status(&self, id: &str, status: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE atlas_managed.spec_proposals SET status = $2 WHERE id = $1",
            &[&id, &status],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG update_proposal_status", &e))?;
        Ok(())
    }

    /// Mark an attempt without changing status. The sweep stamps each pass
    /// over a pending proposal with it; an execute's claim stamps its own
    /// ([`Self::claim_proposal_for_execute`]).
    pub async fn update_proposal_attempt_only(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE atlas_managed.spec_proposals SET last_attempt_at = now() WHERE id = $1",
            &[&id],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG update_proposal_attempt_only", &e))?;
        Ok(())
    }

    /// Terminal-failure update: status='failed' + last_error populated +
    /// last_attempt_at bumped. Idempotent.
    pub async fn update_proposal_status_with_error(
        &self,
        id: &str,
        status: &str,
        last_error: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            r#"UPDATE atlas_managed.spec_proposals
               SET status = $2,
                   last_error = $3,
                   last_attempt_at = now()
               WHERE id = $1"#,
            &[&id, &status, &last_error],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG update_proposal_status_with_error", &e))?;
        Ok(())
    }

    /// Set `consecutive_greens` to an absolute value (e.g. 0 on red, +1 on
    /// green). The promotion sweep (Step 10) decides when to use this.
    pub async fn update_proposal_greens(
        &self,
        id: &str,
        consecutive_greens: i32,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE atlas_managed.spec_proposals SET consecutive_greens = $2 WHERE id = $1",
            &[&id, &consecutive_greens],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG update_proposal_greens", &e))?;
        Ok(())
    }

    /// Claim `app_id`'s proposal `id` for an execute: `queued → inFlight` as ONE
    /// compare-and-set. It stamps `last_attempt_at` and writes the claim's
    /// record into `metadata.execute`: a fresh `claim` token, and
    /// `staleAfterSecs`, the CLAIMANT's own bound on how long it may hold the
    /// row. Returns the token, or `None` when the row is not `queued` (or not
    /// this app's) and nothing was written.
    ///
    /// An execute starts a paid AI run, so two executes of one proposal, or an
    /// execute of a proposal already past the queue, must not both get here.
    /// Every later write the execute makes to the row matches the token
    /// ([`Self::record_execute_run`], [`Self::fail_claimed_proposal`],
    /// [`Self::stage_claimed_proposal`], [`Self::requeue_claimed_proposal`]).
    /// Once the claim is gone, because a scan re-queued the row as stale and
    /// another execute may hold it now, this execute's writes change nothing.
    pub async fn claim_proposal_for_execute(
        &self,
        app_id: &str,
        id: &str,
        stale_after: std::time::Duration,
    ) -> Result<Option<String>, String> {
        let token = uuid::Uuid::new_v4().to_string();
        let stale_after_secs = stale_after.as_secs_f64();
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let claimed = conn
            .execute(
                r#"UPDATE atlas_managed.spec_proposals
                   SET status = 'inFlight',
                       last_attempt_at = now(),
                       metadata = metadata || jsonb_build_object(
                           'execute',
                           jsonb_build_object('claim', $3::text, 'staleAfterSecs', $4::float8))
                   WHERE id = $1 AND app_id = $2 AND status = 'queued'"#,
                &[&id, &app_id, &token, &stale_after_secs],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG claim_proposal_for_execute", &e))?;
        Ok((claimed == 1).then_some(token))
    }

    /// Record on proposal `id` the task run its execute started, while the
    /// claim with `token` is still held. A scan that later finds the claim stale
    /// stops this run before it re-queues the proposal
    /// ([`Self::requeue_stale_in_flight_proposals`]). `Ok(false)` means the
    /// claim is no longer held, and nothing was written.
    pub async fn record_execute_run(
        &self,
        id: &str,
        token: &str,
        task_run_id: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let recorded = conn
            .execute(
                r#"UPDATE atlas_managed.spec_proposals
                   SET metadata = jsonb_set(metadata, '{execute,taskRunId}', to_jsonb($3::text))
                   WHERE id = $1
                     AND status = 'inFlight'
                     AND metadata->'execute'->>'claim' = $2"#,
                &[&id, &token, &task_run_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG record_execute_run", &e))?;
        Ok(recorded == 1)
    }

    /// Settle a claimed proposal as `failed` with `last_error`: stamp
    /// `last_attempt_at` and release the claim, but only while the claim with
    /// `token` is still held. `Ok(false)` means it is not, and the row was left
    /// as it was.
    pub async fn fail_claimed_proposal(
        &self,
        id: &str,
        token: &str,
        last_error: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let failed = conn
            .execute(
                r#"UPDATE atlas_managed.spec_proposals
                   SET status = 'failed',
                       last_error = $3,
                       last_attempt_at = now(),
                       metadata = metadata - 'execute'
                   WHERE id = $1
                     AND status = 'inFlight'
                     AND metadata->'execute'->>'claim' = $2"#,
                &[&id, &token, &last_error],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG fail_claimed_proposal", &e))?;
        Ok(failed == 1)
    }

    /// Settle a claimed proposal as `pendingPromotion` with its authored
    /// `candidate_ir`: clear `last_error`, stamp `last_attempt_at` and release
    /// the claim, but only while the claim with `token` is still held.
    /// `Ok(false)` means it is not, and the row was left as it was.
    pub async fn stage_claimed_proposal(
        &self,
        id: &str,
        token: &str,
        candidate_ir: &Value,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let staged = conn
            .execute(
                r#"UPDATE atlas_managed.spec_proposals
                   SET status = 'pendingPromotion',
                       candidate_ir = $3::jsonb,
                       last_error = NULL,
                       last_attempt_at = now(),
                       metadata = metadata - 'execute'
                   WHERE id = $1
                     AND status = 'inFlight'
                     AND metadata->'execute'->>'claim' = $2"#,
                &[&id, &token, candidate_ir],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG stage_claimed_proposal", &e))?;
        Ok(staged == 1)
    }

    /// Put a claimed proposal back in the queue and release the claim, but
    /// only while the claim with `token` is still held: an execute that was
    /// not admitted, or that went away before writing its outcome.
    /// `last_error` replaces the row's when given and leaves it as it was when
    /// `None`. `Ok(false)` means the claim is not held, and the row was left as
    /// it was.
    pub async fn requeue_claimed_proposal(
        &self,
        id: &str,
        token: &str,
        last_error: Option<&str>,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let requeued = conn
            .execute(
                r#"UPDATE atlas_managed.spec_proposals
                   SET status = 'queued',
                       last_error = COALESCE($3, last_error),
                       metadata = metadata - 'execute'
                   WHERE id = $1
                     AND status = 'inFlight'
                     AND metadata->'execute'->>'claim' = $2"#,
                &[&id, &token, &last_error],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG requeue_claimed_proposal", &e))?;
        Ok(requeued == 1)
    }

    /// Put back in the queue every one of `app_id`'s proposals whose claim has
    /// gone stale, and stop the task run each one recorded.
    ///
    /// A claim is stale once it has been held longer than the CLAIMANT's own
    /// bound, `metadata.execute.staleAfterSecs`, which it wrote when it claimed
    /// the row. `fallback_stale_after` is used only for a row that carries no
    /// such bound: one claimed before claims recorded it. So a scan on an
    /// instance with a shorter execute timeout never takes a live claim of one
    /// with a longer one.
    ///
    /// A stale claim belongs to an execute that died with its runner (or hung
    /// past its own bound). The re-queue releases the claim, so the dead
    /// execute's later writes, if it ever makes any, change nothing. The run it
    /// recorded (`metadata.execute.taskRunId`) is stopped unless it has
    /// finished: nothing will read its output, and a run that kept going (a
    /// Restate journal resumes one regardless of the run row's
    /// `auto_continue`) would otherwise spend the user's AI account beside the
    /// next execute's run. A stop only changes the row, as
    /// [`Self::stop_task_run_if_unfinished`] does.
    ///
    /// One statement, so the stops and the re-queue are one transaction.
    pub async fn requeue_stale_in_flight_proposals(
        &self,
        app_id: &str,
        fallback_stale_after: std::time::Duration,
        last_error: &str,
        run_stop_reason: &str,
    ) -> Result<StaleRequeue, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let fallback_secs = fallback_stale_after.as_secs_f64();
        let finished: &[&str] = &crate::database::pg::task_runs::FINISHED_TASK_RUN_STATUSES;
        let row = conn
            .query_one(
                r#"WITH stale AS (
                       SELECT id, metadata->'execute'->>'taskRunId' AS task_run_id
                       FROM atlas_managed.spec_proposals
                       WHERE app_id = $1
                         AND status = 'inFlight'
                         AND (last_attempt_at IS NULL
                              OR last_attempt_at < now() - make_interval(
                                     secs => COALESCE(
                                         (metadata->'execute'->>'staleAfterSecs')::float8, $2)))
                       FOR UPDATE
                   ),
                   stopped AS (
                       UPDATE task_runs
                       SET status = 'stopped',
                           error_message = $4,
                           updated_at = NOW(),
                           completed_at = NOW()
                       WHERE id IN (SELECT task_run_id FROM stale WHERE task_run_id IS NOT NULL)
                         AND status <> ALL($5)
                       RETURNING id
                   ),
                   requeued AS (
                       UPDATE atlas_managed.spec_proposals p
                       SET status = 'queued',
                           last_error = $3,
                           metadata = p.metadata - 'execute'
                       FROM stale
                       WHERE p.id = stale.id
                       RETURNING p.id
                   )
                   SELECT (SELECT count(*) FROM requeued)::bigint,
                          (SELECT count(*) FROM stopped)::bigint"#,
                &[
                    &app_id,
                    &fallback_secs,
                    &last_error,
                    &run_stop_reason,
                    &finished,
                ],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG requeue_stale_in_flight_proposals", &e))?;
        let count = |i: usize| -> Result<u64, String> {
            row.try_get::<_, i64>(i)
                .map(|n| n.max(0) as u64)
                .map_err(|e| {
                    crate::database::pg::pg_err("PG requeue_stale_in_flight_proposals", &e)
                })
        };
        Ok(StaleRequeue {
            requeued: count(0)?,
            runs_stopped: count(1)?,
        })
    }

    // -------------------------------------------------------------------------
    // Reads
    // -------------------------------------------------------------------------

    /// Fetch `app_id`'s proposal `id`. Returns `Ok(None)` if it is missing or
    /// belongs to another app.
    pub async fn find_proposal_by_id(
        &self,
        app_id: &str,
        id: &str,
    ) -> Result<Option<SpecProposalRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"
                SELECT
                    id,
                    app_id,
                    kind,
                    pathname,
                    spec_id,
                    status,
                    created_at::TEXT,
                    last_attempt_at::TEXT,
                    consecutive_greens,
                    last_error,
                    candidate_ir,
                    metadata
                FROM atlas_managed.spec_proposals
                WHERE id = $1 AND app_id = $2
                "#,
                &[&id, &app_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG find_proposal_by_id", &e))?;

        Ok(rows.first().map(row_to_proposal))
    }

    /// Paginated list of `app_id`'s proposals, ordered by
    /// `created_at DESC, id`.
    pub async fn list_proposals(
        &self,
        app_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SpecProposalRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let lim = limit.clamp(1, 1000);
        let off = offset.max(0);
        let rows = conn
            .query(
                r#"
                SELECT
                    id,
                    app_id,
                    kind,
                    pathname,
                    spec_id,
                    status,
                    created_at::TEXT,
                    last_attempt_at::TEXT,
                    consecutive_greens,
                    last_error,
                    candidate_ir,
                    metadata
                FROM atlas_managed.spec_proposals
                WHERE app_id = $1
                ORDER BY created_at DESC, id
                LIMIT $2 OFFSET $3
                "#,
                &[&app_id, &lim, &off],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG list_proposals", &e))?;

        Ok(rows.iter().map(row_to_proposal).collect())
    }

    /// `app_id`'s proposals in a single status, ordered by `created_at DESC`.
    /// The sweep-pending handler reads its `pendingPromotion` candidates here.
    pub async fn list_proposals_by_status(
        &self,
        app_id: &str,
        status: &str,
        limit: i64,
    ) -> Result<Vec<SpecProposalRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let lim = limit.clamp(1, 1000);
        let rows = conn
            .query(
                r#"
                SELECT
                    id,
                    app_id,
                    kind,
                    pathname,
                    spec_id,
                    status,
                    created_at::TEXT,
                    last_attempt_at::TEXT,
                    consecutive_greens,
                    last_error,
                    candidate_ir,
                    metadata
                FROM atlas_managed.spec_proposals
                WHERE app_id = $1 AND status = $2
                ORDER BY created_at DESC, id
                LIMIT $3
                "#,
                &[&app_id, &status, &lim],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG list_proposals_by_status", &e))?;

        Ok(rows.iter().map(row_to_proposal).collect())
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
)]
fn row_to_proposal(r: &tokio_postgres::Row) -> SpecProposalRow {
    SpecProposalRow {
        id: r.get(0),
        app_id: r.get(1),
        kind: r.get(2),
        pathname: r.get(3),
        spec_id: r.get(4),
        status: r.get(5),
        created_at: r.get(6),
        last_attempt_at: r.get(7),
        consecutive_greens: r.get(8),
        last_error: r.get(9),
        candidate_ir: r.get(10),
        metadata: r.get(11),
    }
}
