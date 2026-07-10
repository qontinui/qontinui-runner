//! Managed (embedded) PostgreSQL for standalone end-user runs.
//!
//! ## Why
//!
//! The runner connects to PostgreSQL via `PgDb::new(&url)`. Historically that
//! URL pointed at a developer's docker-compose Postgres (`localhost:5432`); on
//! a machine without it, boot hard-panicked (`main.rs`) before the window was
//! ever created — so the shipped app "installed but didn't open" for every
//! end-user who lacked a database (task #5).
//!
//! This module starts a **private PostgreSQL owned by the app** under its
//! per-user data dir when no external DB is configured/reachable, so the
//! installer is self-contained. The binaries ride inside the app via the
//! `postgresql_embedded` crate's `bundled` feature (embedded at *compile* time
//! — no runtime download / no network on first launch).
//!
//! ## Scope
//!
//! This is **purely a provisioning concern**. The entire query layer
//! (`tokio-postgres`, `deadpool-postgres`, clorinde-generated queries) is
//! unchanged: this module only produces a connection URL that is handed to the
//! existing `PgDb::new`. Dev machines with a configured profile or a reachable
//! docker-compose DB never reach this module — the caller falls back here only
//! when the resolved URL is the *unconfigured* localhost default AND is
//! unreachable (see `main.rs`).
//!
//! ## Lifecycle
//!
//! [`bootstrap`] runs `initdb` on first launch (idempotent thereafter) and
//! starts the server on an ephemeral loopback port. The returned
//! [`ManagedPg::handle`] owns the server process and MUST be kept alive for the
//! process lifetime and stopped on app exit (`RunEvent::Exit`) so no orphaned
//! `postgres` process lingers holding the port.

use postgresql_embedded::{PostgreSQL, Settings};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Holds the running managed instance for the process lifetime so it is not
/// dropped early (which would stop the server); stopped explicitly on exit via
/// [`stop_on_exit`].
static MANAGED_PG: OnceLock<Mutex<Option<PostgreSQL>>> = OnceLock::new();

/// A running managed PostgreSQL instance plus the connection URL for the
/// runner's database. Keep [`Self::handle`] alive for the whole process; call
/// [`stop`] on app exit.
pub struct ManagedPg {
    /// Owns the `postgres` server process. Dropping/stopping it shuts the
    /// server down.
    pub handle: PostgreSQL,
    /// `postgres://user:password@127.0.0.1:<ephemeral>/<db_name>` — hand
    /// straight to `PgDb::new`.
    pub url: String,
}

/// Best-effort free loopback TCP port. Small TOCTOU window between probe and
/// `start()`; acceptable for a local single-user server (the crate would also
/// pick a port, but choosing it here lets us log/report it deterministically).
fn free_loopback_port() -> Option<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}

/// Boot a managed PostgreSQL under `data_root`, provision the cluster on first
/// run, ensure `db_name` exists, and return the running handle + connection URL.
///
/// - `data_root` should be the app's per-user data dir (e.g. Tauri's
///   `app_local_data_dir()`), NOT a temp dir — the cluster persists across
///   launches.
/// - `setup()` runs `initdb` (first launch only; a no-op on an existing
///   cluster). `start()` launches the server. `temporary=false` persists data.
/// - The superuser password persists in `pg-pass` under `data_root` and is read
///   back on every boot after the first (see below) — the crate alone would
///   regenerate it randomly each launch and lock us out of our own cluster.
pub async fn bootstrap(data_root: PathBuf, db_name: &str) -> Result<ManagedPg, String> {
    // Struct-update form (not `default()` + field reassignment) to satisfy
    // clippy::field_reassign_with_default.
    let mut settings = Settings {
        installation_dir: data_root.join("pg-install"),
        data_dir: data_root.join("pg-data"),
        password_file: data_root.join("pg-pass"),
        host: "127.0.0.1".to_string(),
        temporary: false, // persist the cluster across launches
        // The crate's default per-command timeout (initdb / pg_ctl) is 5s,
        // which flakes under CI contention and on slow end-user disks — first
        // initdb on a cold machine can legitimately take tens of seconds.
        timeout: Some(std::time::Duration::from_secs(60)),
        ..Settings::default()
    };
    // Prefer an explicit free loopback port; fall back to whatever Default chose.
    if let Some(port) = free_loopback_port() {
        settings.port = port;
    }

    // Read the stored superuser password back from `password_file`. The
    // `postgresql_embedded` crate (0.20.x) writes that file once at initdb and
    // NEVER reads it back — while `Settings::default()` generates a fresh
    // random password on every construction. Without this read-back, every
    // boot after the first authenticates with a password the cluster has never
    // seen (`FATAL: password authentication failed`) and the install bricks.
    // `pg-pass` holds the cluster's real password — treat it as the source of
    // truth whenever it exists (first boot: absent, the fresh random password
    // is used and written by initdb, unchanged behaviour).
    //
    // Every degenerate combination of (password file, cluster state) is handled
    // explicitly below: silently falling back to a fresh random password can
    // never authenticate against an initialized cluster, and that silent
    // fallback was exactly the original undiagnosable brick. Errors here
    // surface through `main.rs`'s existing degrade path with an actionable
    // message. Same initialized-probe the crate's `is_initialized` uses.
    let cluster_initialized = settings.data_dir.join("postgresql.conf").exists();
    match std::fs::read_to_string(&settings.password_file) {
        Ok(stored) => {
            let stored = stored.trim();
            if stored.is_empty() {
                if cluster_initialized {
                    return Err(format!(
                        "embedded Postgres password file {} is empty but the cluster is \
                         initialized — the superuser password is unrecoverable; delete the \
                         embedded-pg directory to re-provision",
                        settings.password_file.display()
                    ));
                }
                // The crate's initialize() writes the password file only when
                // it does NOT exist, so a 0-byte leftover would poison every
                // future initdb: remove it so initdb regenerates it.
                tracing::warn!(
                    path = %settings.password_file.display(),
                    "embedded Postgres password file is empty before initdb — removing it so \
                     initdb regenerates it"
                );
                std::fs::remove_file(&settings.password_file).map_err(|e| {
                    format!(
                        "embedded Postgres could not remove empty password file {}: {e}",
                        settings.password_file.display()
                    )
                })?;
            } else {
                if !stored.chars().all(|c| c.is_ascii_alphanumeric()) {
                    // The crate only ever generates alphanumeric passwords;
                    // anything else is hand-tampering, and `Settings::url()`
                    // does no percent-encoding.
                    tracing::warn!(
                        path = %settings.password_file.display(),
                        "embedded Postgres password contains non-alphanumeric characters — \
                         the connection URL may be malformed"
                    );
                }
                settings.password = stored.to_string();
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if cluster_initialized {
                return Err(format!(
                    "embedded Postgres cluster at {} is initialized but its password file {} \
                     is missing — the superuser password is unrecoverable; delete the \
                     embedded-pg directory to re-provision",
                    settings.data_dir.display(),
                    settings.password_file.display()
                ));
            }
            // First boot: the crate writes the file at initdb.
        }
        Err(e) => {
            return Err(format!(
                "embedded Postgres password file {} exists but could not be read: {e}",
                settings.password_file.display()
            ));
        }
    }

    // Build the URL before `settings` is moved into `PostgreSQL::new`. The port
    // and password are fixed on `settings` at this point, so the URL is stable
    // for the lifetime of the started server.
    let url = settings.url(db_name);
    let port = settings.port;

    let mut handle = PostgreSQL::new(settings);
    handle
        .setup()
        .await
        .map_err(|e| format!("embedded Postgres setup (initdb) failed: {e}"))?;
    handle
        .start()
        .await
        .map_err(|e| format!("embedded Postgres start failed: {e}"))?;

    let exists = handle
        .database_exists(db_name)
        .await
        .map_err(|e| format!("embedded Postgres database_exists({db_name}) failed: {e}"))?;
    if !exists {
        handle
            .create_database(db_name)
            .await
            .map_err(|e| format!("embedded Postgres create_database({db_name}) failed: {e}"))?;
    }

    // The schema gate is deliberately decoupled from database creation: gating
    // the apply on "database was just created" would let a crash between
    // `create_database` and the schema batch leave an existing-but-empty
    // database that skips schema apply forever. Instead, probe for the schema
    // and apply whenever it is missing. `batch_execute` runs the whole dump in
    // one implicit transaction, so a crashed/failed apply rolls back and this
    // probe stays false on the next boot — self-healing.
    if !schema_applied(&url).await? {
        apply_canonical_schema(&url).await?;
    }

    tracing::info!(
        port,
        db = db_name,
        "Embedded PostgreSQL ready (standalone mode — no external DB configured)"
    );

    Ok(ManagedPg { handle, url })
}

/// Apply the bundled canonical schema to a freshly-created embedded database.
///
/// The schema (`schema.pg.sql.generated`, a `pg_dump`-style dump of the full
/// 362-table canonical schema) is embedded at compile time. Two transforms make
/// it apply to a **pgvector-free** Postgres:
///
///   1. `public.vector(N)` column types → `bytea`. The runner stores every
///      embedding as `bytea` and computes cosine similarity in Rust
///      (`database::embeddings`), and none of the six `vector` columns
///      (in `project.domain_knowledge` / `execution_issues` /
///      `project_embeddings`) are ever read or written as a vector by the
///      runner — so `bytea` is behaviourally identical here and needs no
///      extension.
///   2. Drop the three `ivfflat`/`vector_cosine_ops` indexes on those columns
///      (they require pgvector and only accelerate similarity queries the
///      runner never issues).
///
/// Applied whenever [`schema_applied`] reports the schema missing (fresh
/// database, or a prior apply that crashed/rolled back).
async fn apply_canonical_schema(url: &str) -> Result<(), String> {
    const SCHEMA: &str = include_str!("../schema.pg.sql.generated");
    let body: String = SCHEMA
        .replace("public.vector(384)", "bytea")
        .replace("public.vector(512)", "bytea")
        // The dump emits `CREATE SCHEMA public;`, which errors on a fresh
        // database (the `public` schema already exists). Make every schema
        // creation idempotent so the one-shot apply cannot trip on it.
        .replace("CREATE SCHEMA ", "CREATE SCHEMA IF NOT EXISTS ")
        .lines()
        .filter(|l| !l.contains("ivfflat") && !l.contains("public.vector_cosine_ops"))
        .collect::<Vec<_>>()
        .join("\n");

    // pgcrypto is a STANDARD contrib extension shipped with Postgres (unlike
    // pgvector). The schema uses `public.gen_random_bytes()` / `digest()` in
    // column DEFAULTs, which must exist before the CREATE TABLEs. The dump
    // doesn't emit the extension (the dev/CI Postgres already has it), so
    // provision it into `public` for the fresh embedded cluster.
    let transformed =
        format!("CREATE EXTENSION IF NOT EXISTS pgcrypto WITH SCHEMA public;\n{body}");

    let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .map_err(|e| format!("connect to embedded PG for schema apply failed: {e}"))?;
    let conn_task = tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::warn!("embedded PG schema-apply connection closed with error: {e}");
        }
    });

    let result = client.batch_execute(&transformed).await.map_err(|e| {
        let detail = e
            .as_db_error()
            .map(|db| {
                format!(
                    "{} (SQLSTATE {}){}{}",
                    db.message(),
                    db.code().code(),
                    db.detail()
                        .map(|d| format!("; detail: {d}"))
                        .unwrap_or_default(),
                    db.where_()
                        .map(|w| format!("; where: {w}"))
                        .unwrap_or_default(),
                )
            })
            .unwrap_or_else(|| e.to_string());
        format!("applying canonical schema to embedded PG failed: {detail}")
    });

    drop(client); // closes the connection so the driver task can finish
    let _ = conn_task.await;
    result
}

/// Whether the canonical schema is present in the database at `url`, probed
/// via a sentinel table (`project.domain_knowledge` — one of the dump's own
/// tables). Because [`apply_canonical_schema`] runs the whole dump in a single
/// implicit transaction, this is all-or-nothing: the sentinel existing means
/// the full schema landed, and a crashed/failed apply leaves it absent.
async fn schema_applied(url: &str) -> Result<bool, String> {
    let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .map_err(|e| format!("connect to embedded PG for schema probe failed: {e}"))?;
    let conn_task = tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::warn!("embedded PG schema-probe connection closed with error: {e}");
        }
    });

    let result = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
             WHERE table_schema = 'project' AND table_name = 'domain_knowledge')",
            &[],
        )
        .await
        .map(|row| row.get::<_, bool>(0))
        .map_err(|e| format!("embedded PG schema probe failed: {e}"));

    drop(client); // closes the connection so the driver task can finish
    let _ = conn_task.await;
    result
}

/// Store the managed handle so it is kept alive for the process lifetime and
/// can be stopped cleanly on exit. Call once, right after [`bootstrap`].
pub fn store_handle(handle: PostgreSQL) {
    let slot = MANAGED_PG.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(handle);
    }
}

/// Stop the managed PostgreSQL (if one was started) so no orphaned `postgres`
/// process lingers. Call from the Tauri `RunEvent::Exit` hook. Best-effort and
/// self-contained (uses its own short-lived runtime so it does not depend on
/// any other runtime still being alive at shutdown).
pub fn stop_on_exit() {
    let Some(slot) = MANAGED_PG.get() else {
        return;
    };
    let handle = slot.lock().ok().and_then(|mut g| g.take());
    let Some(handle) = handle else {
        return;
    };
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt.block_on(stop(handle)),
        Err(e) => tracing::warn!("embedded Postgres stop skipped — runtime build failed: {e}"),
    }
}

/// Stop a managed PostgreSQL instance. Best-effort: logs on failure rather than
/// propagating (shutdown must not hang the app).
async fn stop(mut handle: PostgreSQL) {
    if let Err(e) = handle.stop().await {
        tracing::warn!("embedded Postgres stop failed (non-fatal on exit): {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes the embedded-PG tests: each boots a full PostgreSQL server
    /// (initdb + start), and two clusters extracting/booting concurrently is
    /// unnecessary risk (disk, port, and archive-extraction contention).
    /// Works regardless of `--test-threads`.
    static PG_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Connect to `url` and spawn the driver task — the shared boilerplate for
    /// asserting against a booted embedded PG. Callers `drop(client)` then
    /// await the handle so the driver task can finish.
    async fn connect_test_client(
        url: &str,
    ) -> (tokio_postgres::Client, tokio::task::JoinHandle<()>) {
        let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
            .await
            .expect("connect to embedded PG");
        let driver = tokio::spawn(async move {
            let _ = connection.await;
        });
        (client, driver)
    }

    /// End-to-end: initdb → start → create db → apply the transformed canonical
    /// schema → connect → verify. Runs offline (the `bundled` archive is
    /// embedded at compile time, so `setup()` extracts from bytes, no network).
    /// Guards the whole pgvector-free schema-apply path against runtime
    /// restore errors (the reason this fix exists).
    #[tokio::test]
    async fn boots_and_applies_transformed_schema() {
        let _guard = PG_TEST_LOCK.lock().await;
        let root = std::env::temp_dir().join(format!("qr-embedded-pg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root); // clean any prior run

        let managed = bootstrap(root.clone(), "qontinui_test")
            .await
            .expect("embedded PG should boot and apply the schema");

        let (client, conn) = connect_test_client(&managed.url).await;

        // The pgvector-free transform applied: the former `public.vector`
        // column is now `bytea`.
        let dtype: String = client
            .query_one(
                "SELECT data_type FROM information_schema.columns \
                 WHERE table_schema = 'project' AND table_name = 'domain_knowledge' \
                   AND column_name = 'content_embedding'",
                &[],
            )
            .await
            .expect("domain_knowledge.content_embedding should exist after schema apply")
            .get(0);
        assert_eq!(
            dtype, "bytea",
            "vector column should be transformed to bytea"
        );

        // The full schema applied (not just a handful of self-heal tables).
        let n: i64 = client
            .query_one(
                "SELECT count(*)::bigint FROM information_schema.tables \
                 WHERE table_schema IN ('project','agent','coord','auth','cloud','public')",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert!(
            n > 100,
            "expected the full canonical schema (>100 tables), got {n}"
        );

        drop(client);
        let _ = conn.await;
        stop(managed.handle).await;
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Regression: a SECOND bootstrap over the same `data_root` must
    /// authenticate. `Settings::default()` regenerates a random password every
    /// construction and the crate never reads `password_file` back, so without
    /// the read-back in [`bootstrap`] every boot after the first failed with
    /// `FATAL: password authentication failed` — bricking each standalone
    /// install on its second launch. (The single-boot test above can never
    /// catch this.)
    #[tokio::test]
    async fn second_boot_reuses_stored_password() {
        let _guard = PG_TEST_LOCK.lock().await;
        let root = std::env::temp_dir().join(format!(
            "qr-embedded-pg-test-second-boot-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root); // clean any prior run

        // First boot: initdb writes the real password to pg-pass.
        let managed = bootstrap(root.clone(), "qontinui_test2")
            .await
            .expect("first boot should succeed");
        assert!(
            root.join("pg-pass").is_file(),
            "initdb should have written the password file"
        );
        stop(managed.handle).await;

        // Second boot, same data_root: must read the stored password back and
        // authenticate against the existing cluster. (Schema apply is skipped —
        // the schema probe finds it already present — this test is about
        // authentication.)
        let managed = bootstrap(root.clone(), "qontinui_test2")
            .await
            .expect("second boot over the same data_root should authenticate");

        // The returned URL must actually authenticate end-to-end.
        let (client, conn) = connect_test_client(&managed.url).await;
        let one: i32 = client
            .query_one("SELECT 1", &[])
            .await
            .expect("trivial query on second boot")
            .get(0);
        assert_eq!(one, 1);

        drop(client);
        let _ = conn.await;
        stop(managed.handle).await;
        let _ = std::fs::remove_dir_all(&root);
    }
}
