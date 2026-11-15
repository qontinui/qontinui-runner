//! Boot self-heal: two reflection measurements may hold an UNKNOWN (NULL).
//!
//! Plan `2026-11-09-reflection-reports-only-what-it-judged-and-the-dead-verifier-leaves-no-residue`,
//! Phase 1 (decision D4). Two alembic-owned `project.*` columns were created
//! unable to say "not measured", so the runner had to fabricate a value:
//!
//! - `project.convergence_snapshots.effective_fix_rate` — NOT NULL;
//! - `project.architecture_components.health_score` — NOT NULL with a server
//!   default of `1.0`, a health nobody measured.
//!
//! qontinui-web revision `reflection_rates_01_unknown_is_null` makes both
//! nullable and drops the default; alembic stays the author. This block applies
//! the same shape on runner databases that never ran that revision, so a later
//! NULL write is safe everywhere. It follows the CR-5 self-heal in
//! [`PgDb::verify_and_provision`](super::PgDb::verify_and_provision):
//!
//! - each `ALTER` runs only while `information_schema.columns` still shows the
//!   constraint or default, so a warm boot executes no DDL at all;
//! - an absent table shows no column row, so a database the migrator has not
//!   reached yet is skipped cleanly (the Plan 06 index block's posture).
//!
//! `project.component_health_snapshots.health_score` is deliberately left NOT
//! NULL: snapshots are written only for a component whose health is known.

/// The idempotent `DO` block run at every `PgDb::new` boot.
pub(crate) const REFLECTION_UNKNOWN_IS_NULL_SELF_HEAL: &str = "DO $reflunknown$
     BEGIN
       IF EXISTS (
         SELECT 1 FROM information_schema.columns
         WHERE table_schema = 'project'
           AND table_name = 'convergence_snapshots'
           AND column_name = 'effective_fix_rate'
           AND is_nullable = 'NO'
       ) THEN
         ALTER TABLE project.convergence_snapshots
           ALTER COLUMN effective_fix_rate DROP NOT NULL;
       END IF;
       IF EXISTS (
         SELECT 1 FROM information_schema.columns
         WHERE table_schema = 'project'
           AND table_name = 'architecture_components'
           AND column_name = 'health_score'
           AND is_nullable = 'NO'
       ) THEN
         ALTER TABLE project.architecture_components
           ALTER COLUMN health_score DROP NOT NULL;
       END IF;
       IF EXISTS (
         SELECT 1 FROM information_schema.columns
         WHERE table_schema = 'project'
           AND table_name = 'architecture_components'
           AND column_name = 'health_score'
           AND column_default IS NOT NULL
       ) THEN
         ALTER TABLE project.architecture_components
           ALTER COLUMN health_score DROP DEFAULT;
       END IF;
     END
     $reflunknown$;";

#[cfg(test)]
mod tests {
    use super::super::PgDb;
    use futures::FutureExt;
    use tokio_postgres::NoTls;

    /// The pre-change shape of the two tables, as alembic created them
    /// (`consolidation_phase1_15_state_machine`,
    /// `consolidation_phase1_18_architecture_components`), trimmed to the
    /// columns the round-trip touches plus their NOT NULL siblings.
    const PRE_CHANGE_DDL: &str = "
        CREATE SCHEMA IF NOT EXISTS project;
        CREATE TABLE project.convergence_snapshots (
            id text PRIMARY KEY,
            workflow_name text NOT NULL,
            convergence_score double precision NOT NULL,
            effective_fix_rate double precision NOT NULL
        );
        CREATE TABLE project.architecture_components (
            id text PRIMARY KEY,
            workflow_name text NOT NULL,
            component_path text NOT NULL,
            health_score double precision DEFAULT 1.0 NOT NULL
        );
        CREATE INDEX idx_arch_comp_health
            ON project.architecture_components USING btree (health_score);
        CREATE TABLE project.component_health_snapshots (
            id text PRIMARY KEY,
            health_score double precision NOT NULL
        );";

    /// `(is_nullable, column_default, pg_attribute xmin)` for one column. The
    /// catalog row's `xmin` moves on any `ALTER COLUMN … DROP NOT NULL`, so an
    /// unchanged `xmin` proves a boot ran no DDL on the column.
    async fn column_state(
        client: &tokio_postgres::Client,
        table: &str,
        column: &str,
    ) -> (String, Option<String>, String) {
        let row = client
            .query_one(
                "SELECT c.is_nullable::text, c.column_default::text, a.xmin::text
                   FROM information_schema.columns c
                   JOIN pg_attribute a
                     ON a.attrelid = format('%I.%I', c.table_schema, c.table_name)::regclass
                    AND a.attname = c.column_name
                  WHERE c.table_schema = 'project'
                    AND c.table_name::text = $1::text
                    AND c.column_name::text = $2::text",
                &[&table, &column],
            )
            .await
            .unwrap_or_else(|e| panic!("read {table}.{column}: {e}"));
        let read = |i: usize| -> String {
            row.try_get(i)
                .unwrap_or_else(|e| panic!("{table}.{column} col {i}: {e}"))
        };
        let default: Option<String> = row
            .try_get(1)
            .unwrap_or_else(|e| panic!("{table}.{column} default: {e}"));
        (read(0), default, read(2))
    }

    async fn connect(url: &str) -> tokio_postgres::Client {
        let (client, connection) = tokio_postgres::connect(url, NoTls)
            .await
            .unwrap_or_else(|e| panic!("connect to the disposable PG: {e}"));
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
    }

    /// The disposable server from `DATABASE_URL`, refusing an unset variable
    /// and the dev database `qontinui_db`. The test never writes to that
    /// database itself: it creates, uses and drops its own scratch database.
    fn fixture_url() -> url::Url {
        let raw = std::env::var("DATABASE_URL").expect(
            "refusing to run: DATABASE_URL is unset; point it at a DISPOSABLE Postgres \
             (qontinui-claude-config/scripts/ephemeral-db.sh start pgvector)",
        );
        let url = url::Url::parse(&raw).expect("DATABASE_URL must be a URL");
        let dbname = url.path().trim_start_matches('/');
        assert_ne!(
            dbname, "qontinui_db",
            "refusing to run against the dev database qontinui_db; use a disposable one"
        );
        url
    }

    #[tokio::test]
    #[ignore = "needs a DISPOSABLE PG via DATABASE_URL (ephemeral-db.sh start pgvector); never qontinui_db"]
    async fn boot_self_heal_lets_reflection_columns_hold_unknown() {
        let admin_url = fixture_url();
        let admin = connect(admin_url.as_str()).await;
        let scratch = format!("refl_unknown_{}", uuid::Uuid::new_v4().simple());
        admin
            .batch_execute(&format!("CREATE DATABASE {scratch}"))
            .await
            .expect("create scratch database");
        let mut scratch_url = admin_url.clone();
        scratch_url.set_path(&format!("/{scratch}"));

        let outcome = std::panic::AssertUnwindSafe(round_trip(scratch_url.to_string()))
            .catch_unwind()
            .await;

        admin
            .batch_execute(&format!("DROP DATABASE IF EXISTS {scratch} WITH (FORCE)"))
            .await
            .expect("drop scratch database");
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    async fn round_trip(url: String) {
        let client = connect(&url).await;

        // A database the migrator has not reached: the block skips cleanly.
        drop(PgDb::new(&url).await.expect("boot with the tables absent"));

        client
            .batch_execute(PRE_CHANGE_DDL)
            .await
            .expect("load the pre-change schema");
        let pre = column_state(&client, "convergence_snapshots", "effective_fix_rate").await;
        assert_eq!(pre.0, "NO", "fixture must start NOT NULL");
        let pre = column_state(&client, "architecture_components", "health_score").await;
        assert_eq!((pre.0.as_str(), pre.1.as_deref()), ("NO", Some("1.0")));
        assert!(
            client
                .execute(
                    "INSERT INTO project.convergence_snapshots VALUES ('pre', 'wf', 0.5, NULL)",
                    &[],
                )
                .await
                .is_err(),
            "the pre-change shape must refuse an unknown rate"
        );

        // First boot heals both columns.
        drop(PgDb::new(&url).await.expect("first boot"));
        let rate = column_state(&client, "convergence_snapshots", "effective_fix_rate").await;
        let health = column_state(&client, "architecture_components", "health_score").await;
        assert_eq!(rate.0, "YES", "effective_fix_rate must be nullable");
        assert_eq!(health.0, "YES", "health_score must be nullable");
        assert_eq!(health.1, None, "health_score's 1.0 default must be gone");
        let snap = column_state(&client, "component_health_snapshots", "health_score").await;
        assert_eq!(snap.0, "NO", "component_health_snapshots stays NOT NULL");
        let index: i64 = client
            .query_one(
                "SELECT count(*) FROM pg_indexes \
                 WHERE schemaname = 'project' AND indexname = 'idx_arch_comp_health'",
                &[],
            )
            .await
            .expect("read indexes")
            .try_get(0)
            .expect("index count");
        assert_eq!(index, 1, "idx_arch_comp_health stays");

        // Second boot is a no-op: no catalog row is rewritten.
        drop(PgDb::new(&url).await.expect("second boot"));
        assert_eq!(
            column_state(&client, "convergence_snapshots", "effective_fix_rate").await,
            rate,
            "second boot must not touch effective_fix_rate"
        );
        assert_eq!(
            column_state(&client, "architecture_components", "health_score").await,
            health,
            "second boot must not touch health_score"
        );

        // An unknown is now storable, and an omitted health is NULL, not 1.0.
        client
            .execute(
                "INSERT INTO project.convergence_snapshots VALUES ('c1', 'wf', 0.5, NULL)",
                &[],
            )
            .await
            .expect("a NULL effective_fix_rate must insert");
        let health: Option<f64> = client
            .query_one(
                "INSERT INTO project.architecture_components (id, workflow_name, component_path) \
                 VALUES ('a1', 'wf', 'src/x.rs') RETURNING health_score",
                &[],
            )
            .await
            .expect("insert a component with no health")
            .try_get(0)
            .expect("health_score column");
        assert_eq!(health, None, "an unmeasured component's health is UNKNOWN");
    }
}
