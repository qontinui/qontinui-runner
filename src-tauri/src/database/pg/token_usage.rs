//! PostgreSQL token usage operations via Clorinde-generated queries.

use super::PgDb;
use crate::database::types::PhaseTokenUsageRow;

/// One phase's already-billed spend for a single execution.
///
/// The row shape of [`PgDb::get_execution_phase_spend`], which is the durable
/// answer to "what has this execution already cost?" — the same ledger the
/// `task_runs.total_*` aggregate in `queries/token_usage.sql` sums.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseSpendTotals {
    pub phase: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Integer cents, as stored. `phase_token_usage.cost_cents` is a bigint and
    /// the writer ROUNDS to the nearest cent (`f64::round`, not truncation),
    /// so a call under half a cent contributes 0 while one just over it
    /// contributes 1. This total is therefore an approximation in both
    /// directions, not a floor. The sub-cent figure lives in `cost_microusd`,
    /// which this reload does not read.
    ///
    /// Per row the figure is either the CLI-REPORTED cost (`cost_source =
    /// 'reported'`) or an ESTIMATE (`'estimated'`): `record_phase_token_usage_with_cache`
    /// prices tokens through `ai_pricing::calculate_cost_usd_with_cache`, which
    /// borrows a same-family price for a model the catalog cannot price and
    /// logs that it did. It used to write `0` in that case, which made this
    /// total silently unusable as prior spend.
    pub cost_cents: u64,
}

/// Where a recorded phase cost came from — `phase_token_usage.cost_source`.
///
/// The column is CHECK-constrained to exactly these two strings
/// (`ck_phase_token_usage_cost_source`, qontinui-web alembic revision
/// `kpi_01_phase_token_usage_cost_precision`). NULL in the column is
/// "unknown provenance": every row written before that revision, and any row
/// for which no cost could be determined at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostSource {
    /// The provider reported the cost (Claude CLI `total_cost_usd`).
    Reported,
    /// Computed from token counts against the `ai_pricing` table.
    Estimated,
}

impl CostSource {
    pub fn as_db_str(self) -> &'static str {
        match self {
            CostSource::Reported => "reported",
            CostSource::Estimated => "estimated",
        }
    }
}

/// The cost of one `phase_token_usage` row, in the three columns that carry it.
///
/// `cost_cents` stays the whole-cent figure every existing reader sums;
/// `cost_microusd` is the same cost in millionths of a dollar, so a call that
/// costs a fraction of a cent is not rounded to a fabricated `$0`;
/// `cost_source` says whether the number was reported or estimated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseCost {
    /// Whole cents, rounded to nearest — `round(usd * 100)`.
    pub cents: u64,
    /// Millionths of a USD — `round(usd * 1_000_000)`. `None` when no
    /// microdollar-precision figure exists; never a stand-in for zero.
    pub microusd: Option<i64>,
    /// `None` = unknown provenance.
    pub source: Option<CostSource>,
}

impl PhaseCost {
    /// A cost the provider reported, in USD.
    pub fn reported(total_cost_usd: f64) -> Self {
        Self::from_usd(total_cost_usd, CostSource::Reported)
    }

    /// A cost estimated from token counts, in USD.
    pub fn estimated(estimate_usd: f64) -> Self {
        Self::from_usd(estimate_usd, CostSource::Estimated)
    }

    /// A whole-cent figure of unknown provenance and no sub-cent precision —
    /// what a writer that only knows `cost_cents` records.
    pub fn cents_only(cents: u64) -> Self {
        Self {
            cents,
            microusd: None,
            source: None,
        }
    }

    fn from_usd(usd: f64, source: CostSource) -> Self {
        // Negative or non-finite input is clamped to 0 rather than written:
        // `as` saturates, but a NaN would otherwise read as a real $0.
        let usd = if usd.is_finite() { usd.max(0.0) } else { 0.0 };
        Self {
            cents: (usd * 100.0).round() as u64,
            microusd: Some((usd * 1_000_000.0).round() as i64),
            source: Some(source),
        }
    }
}

impl PgDb {
    /// Record token usage for a single AI call within a workflow phase.
    ///
    /// Whole-cent convenience over [`Self::create_phase_token_usage_with_cache`]
    /// for a writer that has no cache counters and no provenance for its
    /// figure (`cost_microusd` / `cost_source` are written NULL).
    pub async fn create_phase_token_usage(
        &self,
        task_run_id: &str,
        phase: &str,
        stage_index: Option<u32>,
        iteration: Option<u32>,
        model_used: Option<&str>,
        provider_used: Option<&str>,
        input_tokens: u64,
        output_tokens: u64,
        cost_cents: u64,
        duration_ms: Option<u64>,
    ) -> Result<(), String> {
        self.create_phase_token_usage_with_cache(
            task_run_id,
            phase,
            stage_index,
            iteration,
            model_used,
            provider_used,
            input_tokens,
            output_tokens,
            PhaseCost::cents_only(cost_cents),
            duration_ms,
            0,
            0,
            None,
            None,
        )
        .await
    }

    /// Record token usage with prompt cache metrics, the full cost triple
    /// (`cost_cents`, `cost_microusd`, `cost_source`) and optional UI Bridge
    /// target. The one writer of `phase_token_usage`.
    ///
    /// The cache counters are bound as `Some(n)` — including `Some(0)` —
    /// never `None`: `phase_token_usage.cache_creation_tokens` and
    /// `.cache_read_tokens` are `bigint NOT NULL DEFAULT 0`, and the
    /// Clorinde-generated INSERT names every column explicitly, so a bound
    /// NULL does not fall back to the column default — it violates NOT NULL
    /// and the whole row is rejected with an opaque `db error`. That used to
    /// be the silent-data-loss path for every call with no prompt-cache
    /// activity (the caller in `unified_workflow_executor/phase_helpers.rs`
    /// writes from a detached `tokio::spawn` that only `warn!`s on failure);
    /// the round-trip tests below pin it.
    pub async fn create_phase_token_usage_with_cache(
        &self,
        task_run_id: &str,
        phase: &str,
        stage_index: Option<u32>,
        iteration: Option<u32>,
        model_used: Option<&str>,
        provider_used: Option<&str>,
        input_tokens: u64,
        output_tokens: u64,
        cost: PhaseCost,
        duration_ms: Option<u64>,
        cache_creation_tokens: u64,
        cache_read_tokens: u64,
        target_app: Option<&str>,
        target_page_url: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let stage_index_i = stage_index.map(|v| v as i32);
        let iteration_i = iteration.map(|v| v as i32);
        let model_owned = model_used.map(|s| s.to_string());
        let provider_owned = provider_used.map(|s| s.to_string());
        let input_i = input_tokens as i64;
        let output_i = output_tokens as i64;
        let cost_i = cost.cents as i64;
        let duration_i = duration_ms.map(|v| v as i64);
        let cache_creation: Option<i64> = Some(cache_creation_tokens as i64);
        let cache_read: Option<i64> = Some(cache_read_tokens as i64);
        let target_app_owned = target_app.map(|s| s.to_string());
        let target_page_owned = target_page_url.map(|s| s.to_string());
        let cost_source: Option<&str> = cost.source.map(CostSource::as_db_str);

        qontinui_db::queries::token_usage::create_phase_token_usage()
            .bind(
                &conn,
                &task_run_id,
                &phase,
                &stage_index_i,
                &iteration_i,
                &model_owned,
                &provider_owned,
                &input_i,
                &output_i,
                &cost_i,
                &duration_i,
                &cache_creation,
                &cache_read,
                &target_app_owned,
                &target_page_owned,
                &cost.microusd,
                &cost_source,
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG insert phase_token_usage", &e))?;
        Ok(())
    }

    /// Get per-phase token usage breakdown for a task run.
    pub async fn get_phase_token_usage(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<PhaseTokenUsageRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::token_usage::get_phase_token_usage()
            .bind(&conn, &task_run_id)
            .all()
            .await
            .map_err(|e| crate::database::pg::pg_err("PG query phase_token_usage", &e))?;

        Ok(rows
            .into_iter()
            .map(|r| PhaseTokenUsageRow {
                phase: r.phase,
                stage_index: r.stage_index.map(|v| v as u32),
                iteration: r.iteration.map(|v| v as u32),
                model_used: r.model_used,
                provider_used: r.provider_used,
                input_tokens: r.input_tokens as u64,
                output_tokens: r.output_tokens as u64,
                cost_cents: r.cost_cents as u64,
                duration_ms: r.duration_ms.map(|v| v as u64),
                cache_creation_tokens: r.cache_creation_tokens.map(|v| v as u64),
                cache_read_tokens: r.cache_read_tokens.map(|v| v as u64),
                created_at: r.created_at.to_rfc3339(),
            })
            .collect())
    }

    /// Get summed token usage for a specific iteration of a task run.
    pub async fn get_iteration_token_totals(
        &self,
        task_run_id: &str,
        iteration: u32,
    ) -> Result<(u64, u64), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let iteration_i = iteration as i32;
        let row = qontinui_db::queries::token_usage::get_iteration_token_totals()
            .bind(&conn, &task_run_id, &iteration_i)
            .one()
            .await
            .map_err(|e| crate::database::pg::pg_err("PG query iteration totals", &e))?;

        Ok((row.total_input as u64, row.total_output as u64))
    }

    /// Per-phase spend already billed to one execution.
    ///
    /// `task_run_id` **is** the workflow `execution_id`. This is the reload
    /// side of the budget: `AppState::register_cost_trackers` calls it so a
    /// resumed run seeds its `BudgetTracker` from what the ledger already
    /// records instead of restarting at $0.00 consumed. A fresh execution has
    /// no rows and returns an empty `Vec`, which seeds zero — so no
    /// resume-specific branch is needed anywhere.
    ///
    /// Grouped by phase rather than a single scalar so the **per-phase** caps
    /// in `TokenBudget::phase_budgets` survive a resume as well as the run
    /// total; the totals are derived by summing these rows, so the two can
    /// never disagree.
    ///
    /// Raw SQL: the checked-in Clorinde bindings are generated from `queries/*.sql` against
    /// a live Postgres (see `.github/workflows/clorinde-bindings-fresh.yml`),
    /// so adding a `--!` block would not compile until that regeneration
    /// lands. Nothing here needs a generated type — it is one aggregate over
    /// one indexed key.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_execution_phase_spend(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<PhaseSpendTotals>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT phase,
                          COALESCE(SUM(input_tokens), 0)::bigint  AS total_input_tokens,
                          COALESCE(SUM(output_tokens), 0)::bigint AS total_output_tokens,
                          COALESCE(SUM(cost_cents), 0)::bigint    AS total_cost_cents
                   FROM phase_token_usage
                   WHERE task_run_id = $1
                   GROUP BY phase"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG query execution phase spend", &e))?;

        Ok(rows
            .into_iter()
            .map(|r| PhaseSpendTotals {
                phase: r.get::<_, String>("phase"),
                input_tokens: r.get::<_, i64>("total_input_tokens").max(0) as u64,
                output_tokens: r.get::<_, i64>("total_output_tokens").max(0) as u64,
                cost_cents: r.get::<_, i64>("total_cost_cents").max(0) as u64,
            })
            .collect())
    }

    /// Update the aggregate token totals on a task run.
    pub async fn update_task_run_token_totals(&self, task_run_id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::token_usage::update_task_run_token_totals()
            .bind(&conn, &task_run_id)
            .await
            .map_err(|e| crate::database::pg::pg_err("PG update task_run token totals", &e))?;
        Ok(())
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    //! Round-trip tests for the budget-reload reader.
    //!
    //! `#[ignore]`d to match the `database/pg/*` convention — they need a live
    //! Postgres carrying the project schema, addressed by `DATABASE_URL`.
    //! `#[ignore]` (rather than an in-test "no DB? return Ok" early return) is
    //! deliberate: an ignored test is REPORTED as ignored, so a run without a
    //! database cannot pass these off as green.
    //!
    //! Run them with (note: `database` is declared in `main.rs`, so these live
    //! in the BIN target — `--lib` matches nothing and exits 0 vacuously):
    //! `cargo test --bin qontinui-runner -- --ignored database::pg::token_usage`

    use super::*;

    #[test]
    fn phase_cost_converts_usd_to_whole_cents_and_microdollars() {
        assert_eq!(
            PhaseCost::reported(1.234_567_8),
            PhaseCost {
                cents: 123,
                microusd: Some(1_234_568),
                source: Some(CostSource::Reported),
            }
        );
        assert_eq!(
            PhaseCost::estimated(0.000_004),
            PhaseCost {
                cents: 0,
                microusd: Some(4),
                source: Some(CostSource::Estimated),
            }
        );
    }

    #[test]
    fn phase_cost_never_writes_a_negative_or_nan_figure() {
        assert_eq!(PhaseCost::reported(-3.0).microusd, Some(0));
        assert_eq!(PhaseCost::reported(f64::NAN).cents, 0);
    }

    #[test]
    fn cost_source_strings_match_the_column_check() {
        // ck_phase_token_usage_cost_source: cost_source IN ('reported','estimated')
        assert_eq!(CostSource::Reported.as_db_str(), "reported");
        assert_eq!(CostSource::Estimated.as_db_str(), "estimated");
    }

    /// NOTE: the fallback DSN here deliberately differs from the one the other
    /// `database/pg/*` and `spec_api/*` test modules hardcode
    /// (`qontinui_password`). That value is STALE — canonical dev Postgres is
    /// `qontinui-stack`'s, whose compose file defaults the password to
    /// `qontinui_dev_password` (`qontinui-stack/docker-compose.yml:42`), and
    /// the stale one fails auth with an opaque `db error`. `DATABASE_URL`
    /// still overrides.
    async fn test_pg() -> PgDb {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://qontinui_user:qontinui_dev_password@localhost:5433/qontinui_db".to_string()
        });
        PgDb::new(&url)
            .await
            .expect("PgDb::new for token_usage tests")
    }

    fn unique_run_id(label: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("ptu-{}-{}", label, nanos)
    }

    /// `phase_token_usage.task_run_id` is FK'd to `task_runs(id)`, so the
    /// parent row has to exist before any spend can be recorded.
    async fn create_task_run(pg: &PgDb, task_run_id: &str) {
        let conn = pg.pool.get().await.expect("PG pool");
        conn.execute(
            "INSERT INTO task_runs (id, task_name) VALUES ($1, $2)",
            &[&task_run_id, &"budget reload test"],
        )
        .await
        .expect("insert task_run fixture");
    }

    /// `ON DELETE CASCADE` takes the `phase_token_usage` rows with it.
    async fn cleanup_task_run(pg: &PgDb, task_run_id: &str) {
        let conn = pg.pool.get().await.expect("PG pool");
        let _ = conn
            .execute("DELETE FROM task_runs WHERE id = $1", &[&task_run_id])
            .await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn fresh_execution_has_no_prior_spend() {
        // The seeding path's zero case, against the real schema: an execution
        // with no ledger rows reads an empty Vec — which is why the production
        // path needs no "is this a resume" branch.
        let pg = test_pg().await;
        let unknown = unique_run_id("never-ran");
        let spend = pg
            .get_execution_phase_spend(&unknown)
            .await
            .expect("reading spend for an unknown execution must not error");
        assert!(
            spend.is_empty(),
            "an execution with no ledger rows must read as no spend; got {:?}",
            spend
        );
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn prior_spend_is_grouped_by_phase_and_summed() {
        let pg = test_pg().await;
        let run_id = unique_run_id("grouped");
        create_task_run(&pg, &run_id).await;

        // Two agentic calls plus one setup call — the shape a resumed run's
        // earlier attempts leave behind.
        //
        // Doubles as the regression test for the NOT NULL cache-column bug
        // fixed in `create_phase_token_usage_with_cache`: these writes carry
        // no prompt-cache activity, which is precisely the path that used to
        // be rejected by Postgres and swallowed by the caller's detached
        // `tokio::spawn`. If that regresses, these inserts fail here instead
        // of silently losing the run's spend in production.
        pg.create_phase_token_usage(
            &run_id,
            "agentic",
            None,
            Some(1),
            Some("claude-sonnet-4"),
            Some("claude_cli"),
            1_000,
            500,
            42,
            None,
        )
        .await
        .expect("insert agentic row 1");
        pg.create_phase_token_usage(
            &run_id,
            "agentic",
            None,
            Some(2),
            Some("claude-sonnet-4"),
            Some("claude_cli"),
            2_000,
            750,
            108,
            None,
        )
        .await
        .expect("insert agentic row 2");
        pg.create_phase_token_usage(
            &run_id,
            "setup",
            None,
            Some(1),
            Some("claude-sonnet-4"),
            Some("claude_cli"),
            300,
            100,
            7,
            None,
        )
        .await
        .expect("insert setup row");

        let mut spend = pg
            .get_execution_phase_spend(&run_id)
            .await
            .expect("read prior spend");
        spend.sort_by(|a, b| a.phase.cmp(&b.phase));

        assert_eq!(
            spend,
            vec![
                PhaseSpendTotals {
                    phase: "agentic".to_string(),
                    input_tokens: 3_000,
                    output_tokens: 1_250,
                    cost_cents: 150,
                },
                PhaseSpendTotals {
                    phase: "setup".to_string(),
                    input_tokens: 300,
                    output_tokens: 100,
                    cost_cents: 7,
                },
            ]
        );

        // This is exactly what `AppState::register_cost_trackers` folds into a
        // `PriorConsumption`, so assert the seeded tracker sees the whole
        // run's prior spend rather than a fresh budget.
        let prior = crate::cost_management::budget::PriorConsumption::from_phases(
            spend.into_iter().map(|r| {
                (
                    r.phase,
                    r.input_tokens + r.output_tokens,
                    r.cost_cents as f64 / 100.0,
                )
            }),
        );
        assert_eq!(prior.total_tokens, 4_650);
        assert!((prior.total_cost_usd - 1.57).abs() < 1e-9);

        cleanup_task_run(&pg, &run_id).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn prior_spend_is_scoped_to_one_execution() {
        // A shared dev DB has other runs' rows in it; the reader must not
        // seed a run's budget with a neighbour's spend.
        let pg = test_pg().await;
        let mine = unique_run_id("mine");
        let theirs = unique_run_id("theirs");
        create_task_run(&pg, &mine).await;
        create_task_run(&pg, &theirs).await;

        pg.create_phase_token_usage(&mine, "agentic", None, None, None, None, 10, 5, 1, None)
            .await
            .expect("insert own row");
        pg.create_phase_token_usage(
            &theirs, "agentic", None, None, None, None, 9_999, 9_999, 9_999, None,
        )
        .await
        .expect("insert neighbour row");

        let spend = pg
            .get_execution_phase_spend(&mine)
            .await
            .expect("read prior spend");
        assert_eq!(spend.len(), 1);
        assert_eq!(spend[0].input_tokens, 10);
        assert_eq!(spend[0].output_tokens, 5);
        assert_eq!(spend[0].cost_cents, 1);

        cleanup_task_run(&pg, &mine).await;
        cleanup_task_run(&pg, &theirs).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    #[expect(
        clippy::disallowed_methods,
        reason = "test-only Row::get on columns this test just wrote"
    )]
    async fn reported_cost_round_trips_with_microdollar_precision_and_provenance() {
        let pg = test_pg().await;
        let run_id = unique_run_id("reported");
        create_task_run(&pg, &run_id).await;

        pg.create_phase_token_usage_with_cache(
            &run_id,
            "agentic",
            None,
            Some(1),
            Some("claude-sonnet-4"),
            Some("claude_cli"),
            11,
            397,
            PhaseCost::reported(0.0042),
            None,
            6_402,
            31_002,
            None,
            None,
        )
        .await
        .expect("insert reported-cost row");

        let conn = pg.pool.get().await.expect("PG pool");
        let row = conn
            .query_one(
                "SELECT cost_cents, cost_microusd, cost_source, cache_creation_tokens, \
                        cache_read_tokens \
                 FROM phase_token_usage WHERE task_run_id = $1",
                &[&run_id],
            )
            .await
            .expect("read back");
        assert_eq!(row.get::<_, i64>("cost_cents"), 0);
        assert_eq!(row.get::<_, Option<i64>>("cost_microusd"), Some(4_200));
        assert_eq!(
            row.get::<_, Option<String>>("cost_source").as_deref(),
            Some("reported")
        );
        assert_eq!(row.get::<_, i64>("cache_creation_tokens"), 6_402);
        assert_eq!(row.get::<_, i64>("cache_read_tokens"), 31_002);

        cleanup_task_run(&pg, &run_id).await;
    }
}
