//! PostgreSQL database layer for qontinui-runner.
//!
//! Runs alongside SQLite during migration. New tables and queries are added here
//! via Clorinde-generated code. Callers prefer PG when available, fall back to SQLite.

pub mod activity_timeline;
pub mod observations;
pub mod settings;
pub mod task_run_events;
pub mod task_runs;
pub mod token_analytics;
pub mod token_usage;
pub mod watchers;
pub mod workflows;

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
        Ok(Self { pool })
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
