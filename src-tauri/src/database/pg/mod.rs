//! PostgreSQL database layer for qontinui-runner.
//!
//! Runs alongside SQLite during migration. New tables and queries are added here
//! via Clorinde-generated code. Callers prefer PG when available, fall back to SQLite.

pub mod token_analytics;
pub mod token_usage;

use tracing::{info, warn};

/// PostgreSQL connection pool backed by deadpool-postgres.
pub struct PgDb {
    pool: deadpool_postgres::Pool,
}

impl PgDb {
    /// Connect to PostgreSQL using the given connection URL.
    ///
    /// The URL should include `?options=-c search_path=runner,public` to scope
    /// queries to the runner schema by default.
    ///
    /// Returns `Err` if the connection string is invalid or initial connectivity fails.
    pub async fn new(database_url: &str) -> Result<Self, String> {
        let pg_config: tokio_postgres::Config = database_url
            .parse()
            .map_err(|e| format!("Invalid PostgreSQL URL: {}", e))?;

        let mgr_config = deadpool_postgres::ManagerConfig {
            recycling_method: deadpool_postgres::RecyclingMethod::Fast,
        };

        let mgr = deadpool_postgres::Manager::from_config(pg_config, tokio_postgres::NoTls, mgr_config);

        let pool = deadpool_postgres::Pool::builder(mgr)
            .max_size(16)
            .build()
            .map_err(|e| format!("Failed to create PG pool: {}", e))?;

        // Verify connectivity
        let conn = pool
            .get()
            .await
            .map_err(|e| format!("PostgreSQL connection failed: {}", e))?;

        // Verify the runner schema exists
        let row = conn
            .query_one("SELECT 1 FROM information_schema.schemata WHERE schema_name = 'runner'", &[])
            .await
            .map_err(|e| format!("Failed to verify runner schema: {}", e))?;

        if row.get::<_, i32>(0) != 1 {
            return Err("Runner schema does not exist in PostgreSQL".to_string());
        }

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
}
