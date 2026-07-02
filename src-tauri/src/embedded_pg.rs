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
pub async fn bootstrap(data_root: PathBuf, db_name: &str) -> Result<ManagedPg, String> {
    let mut settings = Settings::default();
    settings.installation_dir = data_root.join("pg-install");
    settings.data_dir = data_root.join("pg-data");
    settings.password_file = data_root.join("pg-pass");
    settings.host = "127.0.0.1".to_string();
    settings.temporary = false; // persist the cluster across launches
    if let Some(port) = free_loopback_port() {
        settings.port = port;
    }

    // Build the URL before `settings` is moved into `PostgreSQL::new`. The port
    // and generated password are fixed on `settings` at this point, so the URL
    // is stable for the lifetime of the started server.
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

    tracing::info!(
        port,
        db = db_name,
        "Embedded PostgreSQL ready (standalone mode — no external DB configured)"
    );

    Ok(ManagedPg { handle, url })
}

/// Stop a managed PostgreSQL instance on app exit. Best-effort: logs on
/// failure rather than propagating (shutdown must not hang the app).
pub async fn stop(mut handle: PostgreSQL) {
    if let Err(e) = handle.stop().await {
        tracing::warn!("embedded Postgres stop failed (non-fatal on exit): {e}");
    }
}
