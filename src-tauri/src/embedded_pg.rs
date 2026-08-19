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
//!
//! ## Attaching to an already-running cluster
//!
//! The data dir is fixed (`<data_root>/pg-data`) but the port is ephemeral, so a
//! *second* runner process on the same machine (a temp/dev runner alongside the
//! primary) cannot simply start its own postmaster — the data dir is locked by
//! the first one. Instead [`bootstrap`] first reads PostgreSQL's own
//! `postmaster.pid` (no extra bookkeeping files, no lock files) and, when it
//! reports a `ready` cluster on a port that actually accepts TCP, **attaches**
//! to it: same cluster, same password (`pg-pass`), no `start()`.
//!
//! Ownership is modelled explicitly by [`PgHandle`]:
//!
//! * [`PgHandle::Owned`] — we ran `pg_ctl start`; we stop it on exit.
//! * [`PgHandle::Attached`] — somebody else's postmaster; we must **never**
//!   stop it, or a temp runner exiting would take down the primary's database.
//!
//! That distinction is not cosmetic. `postgresql_embedded`'s
//! `PostgreSQL::is_running()` is merely "does `postmaster.pid` exist", and its
//! `Drop` runs `pg_ctl stop -m fast` whenever status is `Started` — so merely
//! *holding* a `PostgreSQL` value pointing at a foreign running cluster is
//! enough to kill it when that value drops. The `Attached` variant therefore
//! carries no `PostgreSQL` at all, and the provisioning error paths below
//! deliberately leak (`mem::forget`) a handle rather than let it drop next to a
//! live foreign postmaster.
//!
//! Every attach precondition fails *open*: a missing / truncated / unparseable /
//! `starting` / stale pid file, or a port that refuses connections, all fall
//! through to the normal provisioning path. A stale pid file can never wedge a
//! cold boot.

use postgresql_embedded::{PostgreSQL, Settings};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Holds the running managed instance for the process lifetime so it is not
/// dropped early (which would stop the server); stopped explicitly on exit via
/// [`stop_on_exit`].
static MANAGED_PG: OnceLock<Mutex<Option<PgHandle>>> = OnceLock::new();

/// How long to wait for a TCP connection to a candidate already-running cluster
/// before deciding it is not actually reachable and provisioning instead.
const ATTACH_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Single bounded retry delay for the boot race: two runners starting at the
/// same instant can both see no (or a `starting`) pid file. The loser waits
/// once and re-probes; there is deliberately no retry *loop*.
const ATTACH_RETRY_DELAY: Duration = Duration::from_millis(1500);

/// Our relationship to the `postgres` server behind [`ManagedPg::url`].
///
/// The whole point of the split is [`stop`]: stopping a cluster we merely
/// joined would shut down the database of the runner that owns it.
pub enum PgHandle {
    /// We ran `initdb`/`pg_ctl start` ourselves and own the process. Boxed
    /// because `PostgreSQL` is large next to the `Attached` variant.
    Owned(Box<PostgreSQL>),
    /// Another process on this machine already had the cluster running and we
    /// joined it over TCP. Holds **no** `PostgreSQL` — see the module docs:
    /// that type's `Drop` would `pg_ctl stop` the foreign cluster.
    Attached {
        /// Loopback port read from the running cluster's `postmaster.pid`.
        port: u16,
    },
}

impl PgHandle {
    /// True when this process joined a cluster it does not own (and therefore
    /// must not stop).
    pub fn is_attached(&self) -> bool {
        matches!(self, PgHandle::Attached { .. })
    }
}

/// A running managed PostgreSQL instance plus the connection URL for the
/// runner's database. Keep [`Self::handle`] alive for the whole process; call
/// [`stop`] on app exit.
pub struct ManagedPg {
    /// Ownership-aware handle to the server. For [`PgHandle::Owned`],
    /// dropping/stopping it shuts the server down; [`PgHandle::Attached`] is
    /// inert on drop by construction.
    pub handle: PgHandle,
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

// ---------------------------------------------------------------------------
// Attach: reading PostgreSQL's own `postmaster.pid`
// ---------------------------------------------------------------------------

/// `postmaster.pid` is one value per line, in the order fixed by PostgreSQL's
/// `src/include/utils/pidfile.h` (unchanged since 9.6, and still current in 18):
///
/// ```text
/// 1  postmaster PID
/// 2  data directory
/// 3  start time (epoch seconds)
/// 4  port number
/// 5  first Unix socket dir (empty on Windows)
/// 6  first listen_addresses
/// 7  shared memory key
/// 8  postmaster status ("starting" / "ready" / "stopping" / "standby")
/// ```
///
/// 0-based index of the port line.
const PID_LINE_PORT: usize = 3;
/// 0-based index of the postmaster status line.
const PID_LINE_STATUS: usize = 7;
/// The only status we will attach to. PostgreSQL space-pads this line so it can
/// be rewritten in place, hence the `trim`.
const PM_STATUS_READY: &str = "ready";

/// The file name PostgreSQL writes into the data dir while a postmaster holds it.
const POSTMASTER_PID: &str = "postmaster.pid";

/// Pure parse of a `postmaster.pid` body: `Some(port)` **only** when the file is
/// a complete, well-formed lock file for a postmaster that has finished
/// starting.
///
/// Returns `None` — never an error — for every degenerate shape (short file,
/// garbage PID, unparseable/zero port, any status other than `ready`), because
/// the only sane response to a stale or half-written pid file is to fall
/// through to normal provisioning rather than wedge the boot.
fn ready_port_from_postmaster_pid(contents: &str) -> Option<u16> {
    let lines: Vec<&str> = contents.lines().collect();
    // The status line is the last one PostgreSQL writes; a shorter file is a
    // postmaster that has not got that far (or a truncated leftover).
    if lines.len() <= PID_LINE_STATUS {
        return None;
    }
    // Line 1 must look like a PID; anything else means this is not a pid file.
    let pid: u32 = lines[0].trim().parse().ok()?;
    if pid == 0 {
        return None;
    }
    let port: u16 = lines[PID_LINE_PORT].trim().parse().ok()?;
    if port == 0 {
        return None;
    }
    if lines[PID_LINE_STATUS].trim() != PM_STATUS_READY {
        return None;
    }
    Some(port)
}

/// Whether a `postmaster.pid` is present at all (regardless of its contents).
/// Used only to decide whether the boot race is worth one bounded retry.
fn postmaster_pid_present(data_dir: &Path) -> bool {
    data_dir.join(POSTMASTER_PID).exists()
}

/// Probe `data_dir` for a cluster that is already running and accepting
/// connections: `Some(port)` when `postmaster.pid` says `ready` **and** a TCP
/// connection to `127.0.0.1:<port>` succeeds within [`ATTACH_PROBE_TIMEOUT`].
///
/// Both halves are required. The pid file alone is not evidence of liveness —
/// a hard-killed postmaster leaves the file behind — and connecting is the
/// cheapest honest liveness check available without credentials.
async fn probe_running_cluster(data_dir: &Path) -> Option<u16> {
    let pid_path = data_dir.join(POSTMASTER_PID);
    // A concurrent in-place rewrite can hand us a partial read; that simply
    // fails to parse and we fall through.
    let contents = tokio::fs::read_to_string(&pid_path).await.ok()?;
    let port = ready_port_from_postmaster_pid(&contents)?;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    match tokio::time::timeout(ATTACH_PROBE_TIMEOUT, tokio::net::TcpStream::connect(addr)).await {
        Ok(Ok(stream)) => {
            drop(stream);
            Some(port)
        }
        Ok(Err(e)) => {
            tracing::debug!(
                port,
                "embedded Postgres pid file reports ready but the port refuses connections \
                 ({e}) — provisioning instead"
            );
            None
        }
        Err(_) => {
            tracing::debug!(
                port,
                "embedded Postgres attach probe timed out — provisioning instead"
            );
            None
        }
    }
}

/// Complete an attach: point `settings` at the already-running cluster's port,
/// make sure `db_name` exists, run the same schema gate as the owned path, and
/// return a non-owning [`ManagedPg`].
///
/// `settings` is taken by value on purpose — the caller must not keep using a
/// port-mutated copy for provisioning if this fails.
async fn attach_to_running(
    mut settings: Settings,
    db_name: &str,
    port: u16,
) -> Result<ManagedPg, String> {
    settings.port = port;
    let url = settings.url(db_name);
    let maintenance_url = settings.url("postgres");

    ensure_database(&maintenance_url, db_name).await?;

    if !schema_applied(&url).await? {
        if let Err(e) = apply_canonical_schema(&url).await {
            // A peer that booted the same fresh cluster microseconds earlier
            // may have applied the schema between our probe and our apply; the
            // dump runs in one implicit transaction, so the loser's apply
            // aborts wholesale. Re-probe before reporting failure.
            if !schema_applied(&url).await.unwrap_or(false) {
                return Err(e);
            }
            tracing::info!("embedded PG schema was applied concurrently by another runner");
        }
    }

    tracing::info!(
        port,
        db = db_name,
        "Attached to the already-running embedded PostgreSQL owned by another runner process \
         (this process will NOT stop it)"
    );

    Ok(ManagedPg {
        handle: PgHandle::Attached { port },
        url,
    })
}

/// Create `db_name` on an attached cluster if it is missing.
///
/// The owned path gets this from `PostgreSQL::database_exists` /
/// `create_database`, which need the crate's handle (and hence the binaries);
/// an attached instance has neither, so it goes over the wire against the
/// `postgres` maintenance database.
async fn ensure_database(maintenance_url: &str, db_name: &str) -> Result<(), String> {
    // `CREATE DATABASE` cannot be parameterised, so refuse anything that is not
    // a plain identifier rather than interpolating it.
    if db_name.is_empty()
        || !db_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(format!(
            "embedded Postgres attach: refusing to create database with unsafe name {db_name:?}"
        ));
    }

    let (client, connection) = tokio_postgres::connect(maintenance_url, tokio_postgres::NoTls)
        .await
        .map_err(|e| {
            format!("connect to the attached embedded PG maintenance database failed: {e}")
        })?;
    let conn_task = tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::warn!("attached embedded PG maintenance connection closed with error: {e}");
        }
    });

    let result = async {
        let exists: bool = client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)",
                &[&db_name],
            )
            .await
            .map(|row| row.get(0))
            .map_err(|e| format!("attached embedded PG database probe failed: {e}"))?;
        if !exists {
            client
                .batch_execute(&format!("CREATE DATABASE \"{db_name}\""))
                .await
                .map_err(|e| {
                    format!("attached embedded PG create database {db_name} failed: {e}")
                })?;
        }
        Ok(())
    }
    .await;

    drop(client); // closes the connection so the driver task can finish
    let _ = conn_task.await;
    result
}

/// Release a `PostgreSQL` handle **without** letting its `Drop` run.
///
/// `postgresql_embedded`'s `Drop` calls `pg_ctl stop -m fast` whenever
/// `postmaster.pid` exists, which — next to a cluster owned by *another* runner
/// process — would shut that runner's database down. On the error paths where a
/// foreign postmaster is live, leaking the handle (a few hundred bytes, once,
/// at a failed boot) is strictly better than that.
fn release_without_stopping(handle: PostgreSQL) {
    std::mem::forget(handle);
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
/// - If another process on this machine already has the cluster running and
///   `ready`, this **attaches** to it instead of starting a second postmaster
///   against a locked data dir (see the module docs). The returned handle is
///   then [`PgHandle::Attached`] and this process will not stop the server.
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

    // ---- Attach path: join a cluster another runner already has running ----
    //
    // Runs BEFORE any `start()`. The data dir is fixed but the port is
    // ephemeral, so this is the only way a second runner on this machine can
    // reach the cluster at all — its own `pg_ctl start` would fail on the
    // locked data dir. Every failure below falls through to provisioning, so a
    // stale pid file cannot wedge a cold boot.
    let data_dir = settings.data_dir.clone();
    let mut attach_port = probe_running_cluster(&data_dir).await;
    if attach_port.is_none() && postmaster_pid_present(&data_dir) {
        // A pid file exists but did not qualify — most often a peer that is
        // still `starting`. Give the boot race exactly ONE bounded retry, then
        // provision. No loop.
        tracing::debug!(
            "embedded Postgres pid file present but not attachable — retrying once before \
             provisioning"
        );
        tokio::time::sleep(ATTACH_RETRY_DELAY).await;
        attach_port = probe_running_cluster(&data_dir).await;
    }
    if let Some(port) = attach_port {
        match attach_to_running(settings.clone(), db_name, port).await {
            Ok(managed) => return Ok(managed),
            Err(e) => {
                // Do not try to start our own postmaster on top of a live
                // foreign one; `start()` would fail anyway and `Drop` would
                // stop *their* cluster on the way out.
                return Err(format!(
                    "embedded Postgres is already running on port {port} but could not be \
                     attached to: {e}"
                ));
            }
        }
    }

    // Build the URL before `settings` is moved into `PostgreSQL::new`. The port
    // and password are fixed on `settings` at this point, so the URL is stable
    // for the lifetime of the started server.
    let url = settings.url(db_name);
    let port = settings.port;
    let settings_for_attach = settings.clone();

    let mut handle = PostgreSQL::new(settings);
    if let Err(e) = handle.setup().await {
        let msg = format!("embedded Postgres setup (initdb) failed: {e}");
        // If a foreign postmaster came up meanwhile, dropping `handle` here
        // would stop it. Leak instead.
        if probe_running_cluster(&data_dir).await.is_some() {
            release_without_stopping(handle);
        }
        return Err(msg);
    }
    if let Err(e) = handle.start().await {
        // We may simply have lost the boot race: a peer that was `starting`
        // when we probed can be `ready` by now. One final attach attempt —
        // still bounded, still no loop.
        if let Some(port) = probe_running_cluster(&data_dir).await {
            release_without_stopping(handle);
            tracing::info!(
                port,
                "lost the embedded Postgres start race ({e}) — attaching to the winner instead"
            );
            return attach_to_running(settings_for_attach, db_name, port).await;
        }
        // No live cluster: `handle`'s Drop is a no-op (or a harmless stop
        // against a dead pid file), so let it drop normally.
        return Err(format!("embedded Postgres start failed: {e}"));
    }

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

    Ok(ManagedPg {
        handle: PgHandle::Owned(Box::new(handle)),
        url,
    })
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
///
/// Storing a [`PgHandle::Attached`] is meaningful too: it records that this
/// process must NOT stop the server at exit.
pub fn store_handle(handle: PgHandle) {
    if handle.is_attached() {
        tracing::info!(
            "embedded PostgreSQL handle stored as ATTACHED — this process will not stop the \
             server at exit"
        );
    }
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
///
/// An [`PgHandle::Attached`] handle is a deliberate **no-op**: the postmaster
/// belongs to another runner process on this machine, and stopping it here
/// would take that runner's database down with us.
async fn stop(handle: PgHandle) {
    match handle {
        PgHandle::Owned(mut handle) => {
            if let Err(e) = handle.stop().await {
                tracing::warn!("embedded Postgres stop failed (non-fatal on exit): {e}");
            }
        }
        PgHandle::Attached { port } => {
            tracing::info!(
                port,
                "embedded Postgres was attached, not owned — leaving it running for its owner"
            );
        }
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

    /// A well-formed `postmaster.pid` body. `status` is space-padded exactly as
    /// PostgreSQL pads it (the line is rewritten in place), so the parser must
    /// trim. Line 5 (socket dir) is empty, as it is on Windows.
    fn pid_file(pid: &str, port: &str, status: &str) -> String {
        format!(
            "{pid}\n\
             C:\\Users\\x\\AppData\\Local\\com.qontinui.runner\\embedded-pg\\pg-data\n\
             1755500000\n\
             {port}\n\
             \n\
             127.0.0.1\n\
             5432001   1730234880\n\
             {status}   \n"
        )
    }

    #[test]
    fn ready_pid_file_yields_its_port() {
        assert_eq!(
            ready_port_from_postmaster_pid(&pid_file("31488", "54812", "ready")),
            Some(54812)
        );
    }

    #[test]
    fn starting_status_does_not_attach() {
        assert_eq!(
            ready_port_from_postmaster_pid(&pid_file("31488", "54812", "starting")),
            None,
            "a postmaster that has not finished starting must not be attached to"
        );
    }

    #[test]
    fn non_ready_statuses_do_not_attach() {
        for status in ["stopping", "standby", "", "readyish"] {
            assert_eq!(
                ready_port_from_postmaster_pid(&pid_file("31488", "54812", status)),
                None,
                "status {status:?} must not attach"
            );
        }
    }

    #[test]
    fn truncated_pid_file_does_not_attach() {
        // Fewer than the 8 documented lines — a postmaster mid-write, or a
        // clipped leftover. Must return None and must not panic.
        let full = pid_file("31488", "54812", "ready");
        let lines: Vec<&str> = full.lines().collect();
        for keep in 0..lines.len() {
            let truncated = lines[..keep].join("\n");
            assert_eq!(
                ready_port_from_postmaster_pid(&truncated),
                None,
                "a {keep}-line pid file must not attach"
            );
        }
        assert_eq!(ready_port_from_postmaster_pid(""), None);
        assert_eq!(ready_port_from_postmaster_pid("\n\n\n"), None);
    }

    #[test]
    fn non_numeric_port_does_not_attach() {
        assert_eq!(
            ready_port_from_postmaster_pid(&pid_file("31488", "not-a-port", "ready")),
            None
        );
        // Out of u16 range, and zero, are equally unusable.
        assert_eq!(
            ready_port_from_postmaster_pid(&pid_file("31488", "70000", "ready")),
            None
        );
        assert_eq!(
            ready_port_from_postmaster_pid(&pid_file("31488", "0", "ready")),
            None
        );
    }

    #[test]
    fn garbage_pid_line_does_not_attach() {
        assert_eq!(
            ready_port_from_postmaster_pid(&pid_file("not-a-pid", "54812", "ready")),
            None
        );
        assert_eq!(
            ready_port_from_postmaster_pid(&pid_file("0", "54812", "ready")),
            None
        );
    }

    /// The liveness half of the probe: a pid file can claim `ready` long after
    /// the postmaster was hard-killed. A refused connection must fall through
    /// to provisioning rather than hand back a dead port.
    #[tokio::test]
    async fn ready_pid_file_with_refused_port_does_not_attach() {
        let dir = std::env::temp_dir().join(format!(
            "qr-embedded-pg-stale-pid-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("temp data dir");
        // A port that was free a moment ago and has nothing listening on it.
        let dead_port = free_loopback_port().expect("a free loopback port");
        std::fs::write(
            dir.join(POSTMASTER_PID),
            pid_file("31488", &dead_port.to_string(), "ready"),
        )
        .expect("write stale pid file");

        assert!(postmaster_pid_present(&dir));
        assert_eq!(
            probe_running_cluster(&dir).await,
            None,
            "a stale ready pid file whose port refuses connections must not attach"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No pid file at all (cold boot) is the common case and must be silent.
    #[tokio::test]
    async fn missing_pid_file_does_not_attach() {
        let dir = std::env::temp_dir().join(format!(
            "qr-embedded-pg-no-pid-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp data dir");

        assert!(!postmaster_pid_present(&dir));
        assert_eq!(probe_running_cluster(&dir).await, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `ready` pid file pointing at a port that *does* accept TCP attaches —
    /// the positive half of the liveness probe, with a plain listener standing
    /// in for the postmaster (the probe only connects, it never speaks the
    /// wire protocol).
    #[tokio::test]
    async fn ready_pid_file_with_live_port_attaches() {
        let dir = std::env::temp_dir().join(format!(
            "qr-embedded-pg-live-pid-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp data dir");

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
        let port = listener.local_addr().unwrap().port();
        std::fs::write(
            dir.join(POSTMASTER_PID),
            pid_file("31488", &port.to_string(), "ready"),
        )
        .expect("write pid file");

        assert_eq!(probe_running_cluster(&dir).await, Some(port));

        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

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

    /// The point of the attach path: a SECOND concurrent bootstrap over the
    /// same `data_root` — a temp/dev runner beside the primary — must join the
    /// running cluster instead of failing on the locked data dir, and must
    /// leave it running when it exits.
    #[tokio::test]
    async fn second_concurrent_bootstrap_attaches_and_does_not_stop_the_owner() {
        let _guard = PG_TEST_LOCK.lock().await;
        let root =
            std::env::temp_dir().join(format!("qr-embedded-pg-test-attach-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root); // clean any prior run

        // Instance A owns the cluster.
        let owner = bootstrap(root.clone(), "qontinui_test3")
            .await
            .expect("owner boot should succeed");
        assert!(
            !owner.handle.is_attached(),
            "the first boot must OWN the cluster"
        );
        assert!(
            root.join("pg-data").join(POSTMASTER_PID).is_file(),
            "a started cluster must have written postmaster.pid"
        );

        // Instance B boots while A is still running: it must attach.
        let joiner = bootstrap(root.clone(), "qontinui_test3")
            .await
            .expect("second concurrent boot should attach, not fail");
        assert!(
            joiner.handle.is_attached(),
            "a second boot over a live cluster must ATTACH, not start a second postmaster"
        );
        assert_eq!(
            joiner.url, owner.url,
            "the attached URL must name the same port/password/database as the owner's"
        );

        // The attached URL really works.
        let (client, conn) = connect_test_client(&joiner.url).await;
        let one: i32 = client
            .query_one("SELECT 1", &[])
            .await
            .expect("trivial query over the attached connection")
            .get(0);
        assert_eq!(one, 1);
        drop(client);
        let _ = conn.await;

        // B exits. This must NOT stop A's cluster — the whole ownership model.
        stop(joiner.handle).await;

        let (client, conn) = connect_test_client(&owner.url).await;
        let one: i32 = client
            .query_one("SELECT 1", &[])
            .await
            .expect("the owner's cluster must still be up after the attached instance exits")
            .get(0);
        assert_eq!(one, 1);
        drop(client);
        let _ = conn.await;

        stop(owner.handle).await;
        let _ = std::fs::remove_dir_all(&root);
    }
}
