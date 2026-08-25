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
//!
//! ## Known limitation — the ownership model closes only ONE direction
//!
//! `Attached` → `Owned` is closed: an attached process can no longer stop a
//! cluster it joined, on any path (see [`release_without_stopping`] for the
//! error paths, which leak rather than drop).
//!
//! `Owned` → `Attached` is **not** closed. The owner stopping its cluster
//! — the ordinary [`stop_on_exit`] at app quit, or a failure after `start()`
//! succeeded — pulls the server out from under any peer that has attached. An
//! attached peer holds a `deadpool` with no reconnect logic, so it 503s for the
//! rest of its life. Failure paths at least leave the server up (they release
//! instead of stopping), but a clean owner exit legitimately stops it: there is
//! no reference count over the cluster, and inventing one would need exactly
//! the extra bookkeeping this design avoids. In practice the owner is the
//! long-lived primary runner and the attached peers are short-lived temp
//! runners, so the surviving direction is the one that matters; a peer that
//! outlives the primary must be restarted.

use postgresql_embedded::{PostgreSQL, Settings};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Holds the running managed instance for the process lifetime so it is not
/// dropped early (which would stop the server); stopped explicitly on exit via
/// [`stop_on_exit`].
static MANAGED_PG: OnceLock<Mutex<Option<PgHandle>>> = OnceLock::new();

// ---------------------------------------------------------------------------
// Which database arm this process actually took
// ---------------------------------------------------------------------------

/// Which of `main.rs`'s mutually exclusive database arms this process ended up
/// on. Recorded so `/health` can answer *which* database this process is on,
/// not merely whether one answers.
///
/// This exists because the arm is otherwise unobservable from outside the
/// process. `/health` reported a bare `database.reachable`, which is `true` for
/// an external docker-compose Postgres, for an embedded cluster this process
/// started, and for one it attached to alike — three states with completely
/// different blast radii. Notably it is what makes the attach path *checkable*:
/// "a second runner attached to the primary's cluster instead of degrading" is
/// a claim about which arm was taken, and log-scraping was previously the only
/// way to establish it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DbArm {
    /// Nothing has been resolved yet (boot has not reached the PG arm).
    Unknown,
    /// A PostgreSQL reachable at the profile-resolved URL — a developer's
    /// docker-compose cluster, or an explicitly configured one.
    External,
    /// A bundled cluster this process ran `initdb`/`pg_ctl start` for. This
    /// process stops it at exit.
    EmbeddedOwned,
    /// A bundled cluster another runner process on this machine already had
    /// running, joined over TCP. This process must never stop it.
    EmbeddedAttached,
    /// No database. DB-backed routes serve 503.
    Degraded,
}

impl DbArm {
    /// Stable wire name for `/health`. Kebab-case so the embedded pair reads as
    /// one family; parsed by tests and by the P1 verification procedure.
    pub fn as_str(self) -> &'static str {
        match self {
            DbArm::Unknown => "unknown",
            DbArm::External => "external",
            DbArm::EmbeddedOwned => "embedded-owned",
            DbArm::EmbeddedAttached => "embedded-attached",
            DbArm::Degraded => "degraded",
        }
    }

    fn from_code(code: u8) -> DbArm {
        match code {
            1 => DbArm::External,
            2 => DbArm::EmbeddedOwned,
            3 => DbArm::EmbeddedAttached,
            4 => DbArm::Degraded,
            _ => DbArm::Unknown,
        }
    }

    fn code(self) -> u8 {
        match self {
            DbArm::Unknown => 0,
            DbArm::External => 1,
            DbArm::EmbeddedOwned => 2,
            DbArm::EmbeddedAttached => 3,
            DbArm::Degraded => 4,
        }
    }
}

/// Atomic rather than a `Mutex`: `/health` reads this on every request and must
/// never be able to block on a lock held by the boot path.
static DB_ARM: AtomicU8 = AtomicU8::new(0);

/// The embedded cluster's loopback port, or 0 when this process is not on an
/// embedded arm. Reported beside [`DbArm`] so two runners on one machine can be
/// shown to be talking to the *same* cluster, which is the whole claim the
/// attach path makes.
static EMBEDDED_PORT: AtomicU16 = AtomicU16::new(0);

/// Record which database arm this process took. Called from `main.rs` for the
/// external and degraded arms; the two embedded arms are set by
/// [`store_handle`] from the handle itself, so they cannot drift from the
/// ownership the rest of this module enforces.
pub fn set_db_arm(arm: DbArm) {
    DB_ARM.store(arm.code(), Ordering::Relaxed);
    if !matches!(arm, DbArm::EmbeddedOwned | DbArm::EmbeddedAttached) {
        EMBEDDED_PORT.store(0, Ordering::Relaxed);
    }
}

/// Which database arm this process took. [`DbArm::Unknown`] before the boot
/// path reaches the PG block.
pub fn db_arm() -> DbArm {
    DbArm::from_code(DB_ARM.load(Ordering::Relaxed))
}

/// The embedded cluster's loopback port, or `None` when this process is not on
/// an embedded arm.
pub fn embedded_port() -> Option<u16> {
    match EMBEDDED_PORT.load(Ordering::Relaxed) {
        0 => None,
        port => Some(port),
    }
}

/// Env lever that relocates the whole embedded-PG data root.
///
/// WHY (manual-test-loop iter 18, item 2): the root was hardcoded to
/// `%LOCALAPPDATA%/com.qontinui.runner/embedded-pg`, so EVERY runner on the box
/// — the operator's primary and every throwaway test runner — provisioned or
/// *attached to* the same cluster. `bootstrap` derives `pg-install`, `pg-data`
/// and `pg-pass` from this one path, and the attach probe reads
/// `pg-data/postmaster.pid`, so a fixed root means a temp runner cannot avoid
/// joining the machine-shared database. Iteration 17 was consequently forced to
/// write two real `error_events` rows (ids 4 and 5) into the operator's cluster
/// just to exercise an error route.
///
/// Point this at a scratch directory and the runner gets a private cluster —
/// private install, private data dir, private password file, and an attach
/// probe that can only ever find *its own* postmaster.
pub const EMBEDDED_PG_DIR_ENV: &str = "QONTINUI_EMBEDDED_PG_DIR";

/// The machine-shared default root, used whenever [`EMBEDDED_PG_DIR_ENV`] is
/// unset or blank. Unchanged from the value that was inlined in `main.rs`.
pub fn default_data_root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("com.qontinui.runner")
        .join("embedded-pg")
}

/// Pure resolver behind [`data_root`], split out so the override semantics are
/// unit-testable without mutating process env (which races every other test in
/// the binary).
///
/// A whitespace-only override is treated as UNSET rather than as a relative
/// path of one space: `QONTINUI_EMBEDDED_PG_DIR=` in a shell wrapper is how an
/// operator spells "no override", and silently provisioning a cluster in the
/// process CWD would be the worst possible reading of it.
pub fn resolve_data_root(override_value: Option<&str>, default_root: PathBuf) -> PathBuf {
    match override_value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => default_root,
    }
}

/// The embedded-PG data root this process should use.
pub fn data_root() -> PathBuf {
    let override_value = std::env::var(EMBEDDED_PG_DIR_ENV).ok();
    resolve_data_root(override_value.as_deref(), default_data_root())
}

/// The data root actually handed to [`bootstrap`], for `/health`. `None` until
/// the boot path reaches the embedded arm — the external and degraded arms
/// never set it, and must not report a path they are not using.
static DATA_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Record the resolved root. Idempotent; first writer wins (boot calls it once).
pub fn set_data_root(root: &std::path::Path) {
    let _ = DATA_ROOT.set(root.to_path_buf());
}

/// The embedded cluster's data root as a string, or `None` off the embedded
/// arms. Reported by `/health` beside `embeddedPort` so "this temp runner has
/// its own cluster" is checkable from outside the process instead of inferred
/// from a port number that says nothing about *which* data dir is behind it.
pub fn embedded_data_root() -> Option<String> {
    DATA_ROOT.get().map(|p| p.display().to_string())
}

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

    /// The loopback port the cluster behind this handle is listening on.
    ///
    /// For [`PgHandle::Owned`] this reads back the `Settings` the handle was
    /// built from rather than a separately-tracked copy, so it cannot drift
    /// from the port `start()` actually used.
    pub fn port(&self) -> u16 {
        match self {
            PgHandle::Owned(pg) => pg.settings().port,
            PgHandle::Attached { port } => *port,
        }
    }

    /// The [`DbArm`] this handle represents.
    pub fn arm(&self) -> DbArm {
        match self {
            PgHandle::Owned(_) => DbArm::EmbeddedOwned,
            PgHandle::Attached { .. } => DbArm::EmbeddedAttached,
        }
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

    // Re-read `pg-pass` HERE rather than trusting the value `bootstrap`
    // resolved before probing. On a genuinely cold boot with two runners, the
    // password resolved up there is `Settings::default()`'s fresh random one
    // (the file did not exist yet) and the peer writes the real password at
    // initdb inside our probe/retry window — attaching with the stale random
    // password would fail `password authentication failed` and degrade us. A
    // `ready` pid file proves initdb completed, so the file exists and is
    // authoritative by now.
    let stored = std::fs::read_to_string(&settings.password_file).map_err(|e| {
        format!(
            "embedded Postgres is running on port {port} but its password file {} could not be \
             read: {e}",
            settings.password_file.display()
        )
    })?;
    let stored = stored.trim();
    if stored.is_empty() {
        return Err(format!(
            "embedded Postgres is running on port {port} but its password file {} is empty — \
             the superuser password is unrecoverable",
            settings.password_file.display()
        ));
    }
    settings.password = stored.to_string();

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
            // Check-then-act: the attach window opens as soon as the owner's
            // postmaster reports `ready`, which is BEFORE the owner runs its
            // own `create_database` — so the two genuinely overlap and the
            // loser gets 42P04. That outcome is a success for our purposes
            // (the database exists), and treating it as an error would degrade
            // one of the two runners for no reason.
            if let Err(e) = client
                .batch_execute(&format!("CREATE DATABASE \"{db_name}\""))
                .await
            {
                let duplicate =
                    e.code() == Some(&tokio_postgres::error::SqlState::DUPLICATE_DATABASE);
                if !duplicate {
                    return Err(format!(
                        "attached embedded PG create database {db_name} failed: {e}"
                    ));
                }
                tracing::info!(
                    db = db_name,
                    "embedded PG database was created concurrently by another runner"
                );
            }
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
/// `postmaster.pid` merely *exists* — it never checks who started that
/// postmaster. So dropping a handle beside a cluster owned by another runner
/// process shuts that runner's database down.
///
/// Note how much weaker `Drop`'s predicate is than
/// [`probe_running_cluster`]'s: the probe additionally demands a parseable
/// `ready` status and a TCP connect inside a timeout. Every state in the gap —
/// a peer in crash/WAL recovery (`starting`), a live peer whose probe timed out
/// under load, a pid file caught mid in-place-rewrite — is a live foreign
/// postmaster that the probe reports as absent. Guarding the leak on the probe
/// is therefore *not* sound, and any probe-then-drop is TOCTOU-racy besides.
/// Callers on the pre-`start()`-success paths leak **unconditionally**: this
/// process demonstrably does not own a running postmaster there, so there is
/// nothing legitimate for `Drop` to stop.
///
/// The cost is one leaked handle (a few hundred bytes, once, at a failed boot).
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

    // From here to a successful `start()`, this process owns no running
    // postmaster — so `handle` must never be *dropped*, only released. See
    // [`release_without_stopping`]: `Drop` stops whatever `postmaster.pid`
    // names, which on these paths can only be a peer's cluster (a `starting`
    // peer in crash recovery, a live peer whose probe timed out, a pid file
    // caught mid-rewrite). The leak is unconditional and therefore also
    // TOCTOU-free — no probe result can go stale between the check and the drop.
    //
    // Accepted consequence: if `pg_ctl start -w` reports a timeout while the
    // postmaster actually did come up, we leak a handle to a cluster we DO own
    // and never stop it at exit, orphaning a `postgres` process. That is
    // strictly better than killing a peer's database, and it self-heals — the
    // next boot attaches to that same cluster instead of fighting it.
    let mut handle = PostgreSQL::new(settings);
    if let Err(e) = handle.setup().await {
        release_without_stopping(handle);
        return Err(format!("embedded Postgres setup (initdb) failed: {e}"));
    }
    if let Err(e) = handle.start().await {
        // We may simply have lost the boot race: a peer that was `starting`
        // when we probed can be `ready` by now. One final attach attempt —
        // still bounded, still no loop. Probe first, release unconditionally,
        // and only then branch.
        let winner = probe_running_cluster(&data_dir).await;
        release_without_stopping(handle);
        if let Some(port) = winner {
            tracing::info!(
                port,
                "lost the embedded Postgres start race ({e}) — attaching to the winner instead"
            );
            return attach_to_running(settings_for_attach, db_name, port).await;
        }
        return Err(format!("embedded Postgres start failed: {e}"));
    }

    // Past this point the server IS running and a peer may already have
    // attached to it (the attach window opens the moment the postmaster reports
    // `ready`, which is before we get here). An attached peer builds a
    // `deadpool` against this server and has no reconnect logic, so stopping it
    // out from under them 503s that runner for the rest of its life. Every
    // failure below therefore releases the handle instead of dropping it: this
    // process is degrading anyway, and an orphaned `postgres` is a far cheaper
    // outcome than a peer's database vanishing. `main.rs` degrades on the Err
    // exactly as before.
    macro_rules! fail_without_stopping {
        ($handle:expr, $msg:expr) => {{
            release_without_stopping($handle);
            return Err($msg);
        }};
    }

    // Each await is bound to a local before the `match`: a scrutinee's
    // temporaries (here, a future borrowing `handle`) live to the end of the
    // match statement, which would forbid moving `handle` inside an arm.
    let exists = handle.database_exists(db_name).await;
    match exists {
        Ok(true) => {}
        Ok(false) => {
            let created = handle.create_database(db_name).await;
            if let Err(e) = created {
                // Concurrency: a peer that attached the instant we reported
                // `ready` can create the database between our probe and our
                // create (SQLSTATE 42P04). The crate wraps its sqlx error in an
                // opaque `CreateDatabaseError(String)`, so the SQLSTATE is not
                // inspectable through this API — re-probe instead, which is
                // both version-proof and stricter (it asserts the end state we
                // actually need).
                let reprobe = handle.database_exists(db_name).await;
                match reprobe {
                    Ok(true) => tracing::info!(
                        db = db_name,
                        "embedded PG database was created concurrently by another runner"
                    ),
                    _ => fail_without_stopping!(
                        handle,
                        format!("embedded Postgres create_database({db_name}) failed: {e}")
                    ),
                }
            }
        }
        Err(e) => fail_without_stopping!(
            handle,
            format!("embedded Postgres database_exists({db_name}) failed: {e}")
        ),
    }

    // The schema gate is deliberately decoupled from database creation: gating
    // the apply on "database was just created" would let a crash between
    // `create_database` and the schema batch leave an existing-but-empty
    // database that skips schema apply forever. Instead, probe for the schema
    // and apply whenever it is missing. `batch_execute` runs the whole dump in
    // one implicit transaction, so a crashed/failed apply rolls back and this
    // probe stays false on the next boot — self-healing.
    match schema_applied(&url).await {
        Ok(true) => {}
        Ok(false) => {
            if let Err(e) = apply_canonical_schema(&url).await {
                // Same concurrent-apply tolerance as the attach path: the dump
                // runs in one implicit transaction, so the loser aborts
                // wholesale and the winner's schema is complete.
                if !schema_applied(&url).await.unwrap_or(false) {
                    fail_without_stopping!(handle, e);
                }
                tracing::info!("embedded PG schema was applied concurrently by another runner");
            }
        }
        Err(e) => fail_without_stopping!(handle, e),
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
    // Derive the reported arm from the handle rather than letting the caller
    // pass one: the Owned/Attached distinction is the ownership invariant this
    // module enforces, and `/health` must not be able to disagree with it.
    EMBEDDED_PORT.store(handle.port(), Ordering::Relaxed);
    DB_ARM.store(handle.arm().code(), Ordering::Relaxed);
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

    // ---- Data-root isolation (manual-test-loop iter 18, item 2) ----

    #[test]
    fn data_root_override_wins_over_the_shared_default() {
        let shared = PathBuf::from("C:/Users/x/AppData/Local/com.qontinui.runner/embedded-pg");
        let isolated = resolve_data_root(Some("C:/tmp/iter18-pg"), shared.clone());
        assert_eq!(
            isolated,
            PathBuf::from("C:/tmp/iter18-pg"),
            "QONTINUI_EMBEDDED_PG_DIR must relocate the whole root; without this a temp \
             runner provisions or ATTACHES to the machine-shared cluster"
        );
        assert_ne!(
            isolated, shared,
            "an override that still resolves to the shared root isolates nothing"
        );
    }

    #[test]
    fn unset_data_root_falls_back_to_the_shared_default() {
        let shared = PathBuf::from("C:/Users/x/AppData/Local/com.qontinui.runner/embedded-pg");
        assert_eq!(
            resolve_data_root(None, shared.clone()),
            shared,
            "with the var unset the operator's runner must keep its existing cluster"
        );
    }

    #[test]
    fn blank_data_root_override_is_treated_as_unset() {
        let shared = PathBuf::from("C:/Users/x/AppData/Local/com.qontinui.runner/embedded-pg");
        for blank in ["", "   ", "\t", "\n"] {
            assert_eq!(
                resolve_data_root(Some(blank), shared.clone()),
                shared,
                "{blank:?} must read as no override, never as a relative path"
            );
        }
    }

    #[test]
    fn override_is_trimmed_before_use() {
        // A wrapper script that interpolates a path can leave trailing
        // whitespace; `PathBuf::from("C:/tmp/pg ")` is a DIFFERENT directory on
        // some filesystems and an invalid one on Windows.
        assert_eq!(
            resolve_data_root(Some("  C:/tmp/iter18-pg  "), PathBuf::from("/shared")),
            PathBuf::from("C:/tmp/iter18-pg")
        );
    }

    #[test]
    fn default_data_root_is_the_historical_shared_path() {
        // Guards the fallback: this is the path every existing runner's cluster
        // already lives at, so drifting it would orphan real data.
        let root = default_data_root();
        assert!(
            root.ends_with(PathBuf::from("com.qontinui.runner").join("embedded-pg")),
            "default root drifted: {}",
            root.display()
        );
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
        // A privileged, reserved port: nothing on this box can bind it without
        // elevation, so the connect is deterministically refused. (Taking a
        // port from `free_loopback_port()` would be racy — ~9 concurrent
        // sessions run here and any of them can bind it between the probe and
        // ours.)
        let dead_port: u16 = 1;
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

        // The two handles must also AGREE about which cluster they are on --
        // this is exactly what `/health` reports as `database.embeddedPort`,
        // and the whole claim of the attach path is that there is ONE cluster.
        // `Owned::port()` reads back the started `Settings`, so a postmaster
        // that ended up on a different port than we asked for would show here.
        assert_eq!(owner.handle.arm(), DbArm::EmbeddedOwned);
        assert_eq!(joiner.handle.arm(), DbArm::EmbeddedAttached);
        assert_eq!(
            joiner.handle.port(),
            owner.handle.port(),
            "attach means one cluster: both handles must name the same port"
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

    /// The failed-boot path must not take a live peer's cluster down with it.
    ///
    /// This is the case the attach probe CANNOT see: the peer's pid file says
    /// `starting` (crash/WAL recovery — the common state for a runner that was
    /// killed rather than stopped), so `probe_running_cluster` reports nothing,
    /// while `PostgreSQL::drop`'s predicate (does `postmaster.pid` exist) is
    /// satisfied and would fire `pg_ctl stop -m fast` at the *live* server.
    ///
    /// The test drives it against a genuinely running postmaster rather than a
    /// bare TCP listener on a fake PID: `pg_ctl stop` acts on the PID in the
    /// pid file, so a stand-in listener would survive the bug and the assertion
    /// would prove nothing.
    #[tokio::test]
    async fn failed_boot_beside_a_starting_peer_does_not_stop_it() {
        let _guard = PG_TEST_LOCK.lock().await;
        let root =
            std::env::temp_dir().join(format!("qr-embedded-pg-test-nostop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root); // clean any prior run

        // A real, running, owned cluster stands in for the peer.
        let peer = bootstrap(root.clone(), "qontinui_test4")
            .await
            .expect("peer boot should succeed");

        // Rewrite its status line to `starting`, leaving the PID and port
        // intact — exactly what a postmaster in recovery publishes. PostgreSQL
        // only rewrites this line at state transitions, so the running server
        // is unaffected.
        let pid_path = root.join("pg-data").join(POSTMASTER_PID);
        let original = std::fs::read_to_string(&pid_path).expect("read the peer's pid file");
        let mut lines: Vec<String> = original.lines().map(str::to_string).collect();
        assert!(
            lines.len() > PID_LINE_STATUS,
            "a running postmaster must publish all 8 lines"
        );
        lines[PID_LINE_STATUS] = "starting".to_string();
        std::fs::write(&pid_path, format!("{}\n", lines.join("\n"))).expect("rewrite pid file");
        assert_eq!(
            probe_running_cluster(&root.join("pg-data")).await,
            None,
            "a `starting` peer must be invisible to the attach probe — that is the point"
        );

        // Our boot cannot attach (status is not `ready`) and cannot start (the
        // data dir is locked), so it fails. With the handle merely dropped, its
        // `Drop` would stop the peer here.
        let err = bootstrap(root.clone(), "qontinui_test4")
            .await
            .err()
            .expect("boot beside a locked data dir must fail");
        assert!(
            err.contains("start failed") || err.contains("setup"),
            "unexpected failure mode: {err}"
        );

        // The peer must still be serving.
        let (client, conn) = connect_test_client(&peer.url).await;
        let one: i32 = client
            .query_one("SELECT 1", &[])
            .await
            .expect("the peer's cluster must survive a failed boot next to it")
            .get(0);
        assert_eq!(one, 1);
        drop(client);
        let _ = conn.await;

        // Restore the status line so the ordinary stop path sees a normal file.
        let _ = std::fs::write(&pid_path, original);
        stop(peer.handle).await;
        let _ = std::fs::remove_dir_all(&root);
    }

    // -- the /health arm observable -------------------------------------

    #[test]
    fn db_arm_wire_names_round_trip() {
        for arm in [
            DbArm::Unknown,
            DbArm::External,
            DbArm::EmbeddedOwned,
            DbArm::EmbeddedAttached,
            DbArm::Degraded,
        ] {
            assert_eq!(
                DbArm::from_code(arm.code()),
                arm,
                "{} must survive the atomic round-trip",
                arm.as_str()
            );
        }
        // The wire names are read by the P1 verification procedure and by
        // anything watching /health; pin them rather than letting a rename
        // silently break a caller.
        assert_eq!(DbArm::External.as_str(), "external");
        assert_eq!(DbArm::EmbeddedOwned.as_str(), "embedded-owned");
        assert_eq!(DbArm::EmbeddedAttached.as_str(), "embedded-attached");
        assert_eq!(DbArm::Degraded.as_str(), "degraded");
        assert_eq!(DbArm::Unknown.as_str(), "unknown");
    }

    /// An out-of-range byte must read as `Unknown`, not panic or alias a real
    /// arm: the atomic is the only thing between `/health` and a torn value.
    #[test]
    fn unknown_arm_code_reads_as_unknown() {
        assert_eq!(DbArm::from_code(9), DbArm::Unknown);
        assert_eq!(DbArm::from_code(u8::MAX), DbArm::Unknown);
    }

    /// Leaving an embedded arm must clear the port. Otherwise a runner that
    /// booted embedded and then degraded would keep advertising a port it can
    /// no longer reach, which is worse than reporting nothing.
    ///
    /// Shares the process-global arm state with nothing else in this suite
    /// (no other test calls `set_db_arm` or `store_handle`), so it is
    /// order-independent.
    #[test]
    fn leaving_an_embedded_arm_clears_the_port() {
        EMBEDDED_PORT.store(54812, Ordering::Relaxed);
        set_db_arm(DbArm::EmbeddedAttached);
        assert_eq!(db_arm(), DbArm::EmbeddedAttached);
        assert_eq!(
            embedded_port(),
            Some(54812),
            "an embedded arm must keep the port it was recorded with"
        );

        set_db_arm(DbArm::Degraded);
        assert_eq!(db_arm(), DbArm::Degraded);
        assert_eq!(
            embedded_port(),
            None,
            "a degraded runner must not advertise an embedded port it cannot serve"
        );

        set_db_arm(DbArm::Unknown);
    }

    /// The port an `Attached` handle reports is the one it joined -- the value
    /// `/health` publishes so two runners can be shown to share a cluster.
    #[test]
    fn attached_handle_reports_its_joined_port() {
        let handle = PgHandle::Attached { port: 54812 };
        assert_eq!(handle.port(), 54812);
        assert_eq!(handle.arm(), DbArm::EmbeddedAttached);
        assert!(handle.is_attached());
    }
}
