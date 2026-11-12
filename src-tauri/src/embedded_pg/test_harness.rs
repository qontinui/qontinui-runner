//! The PostgreSQL the runner's Postgres-backed tests run against: ONE bundled
//! cluster per test PROCESS, booted from a cached ready data directory, with a
//! FRESH database per test (plan
//! `2026-11-09-runner-postgres-backed-tests-never-run-so-hand-verified-sql-is-unretained`,
//! Phase 1).
//!
//! # Why
//!
//! Until this existed, the runner's `DATABASE_URL`-backed tests were
//! `#[ignore]`d, most behind a silent localhost fallback, so no CI lane ran them
//! and SQL verified by hand was retained nowhere. The harness boots the
//! runner's OWN bundled server — the end user's production database — inside
//! the test process, so a Postgres test runs wherever tests run: both CI legs,
//! `cargo-guard.sh test`, nextest.
//!
//! # Entry points
//!
//! - [`fresh_database`] / [`fresh_database_blocking`]: a database cloned from
//!   the canonical template, which holds the full transformed
//!   `schema.pg.sql.generated`.
//! - [`fresh_database_from`] / [`fresh_database_from_blocking`]: a database
//!   cloned from a [`DerivedTemplate`] — the canonical template after a
//!   provisioning step ran on it once per process. The bin's `PgDb` test seam
//!   derives one with `PgDb::new`, so its databases carry the DDL `PgDb::new`
//!   self-provisions as well as the schema file's.
//!
//! Each returns a [`TestDatabase`]; dropping it drops the database. All harness
//! I/O runs on the harness's own runtime thread, so the blocking forms are safe
//! from a synchronous test AND from inside a runtime, and no caller's runtime
//! is ever nested.
//!
//! # Lifetime
//!
//! - **Cache** (`<cargo target dir>/qontinui-test-pg/tpl-<key>/`): the
//!   extracted bundled install plus a cleanly shut-down data directory whose
//!   `qontinui_template` database holds the transformed schema. The key hashes
//!   the bundled PostgreSQL release, the `postgresql_embedded` release, the
//!   rendered schema SQL and [`HARNESS_VERSION`]. Built under an exclusive lock
//!   on `.lock`, and entered by its builder before that lock is released
//!   ([`enter_or_build`]); entered by every other process under a shared one,
//!   which keeps a collection from removing it mid-copy. Every entry marks it
//!   used. Worktrees on branches whose schemas differ share one target
//!   directory, so caches of several keys coexist: a build collects another
//!   key's cache only once no live root was copied from it and it has gone
//!   unused for [`TEMPLATE_IDLE_LIMIT`].
//! - **Process** (`run-<pid>-<nonce>/`): a copy of the cached data directory,
//!   started on a free loopback port. No live cluster is shared between
//!   processes, so only its owner ever stops it.
//! - **Test**: `CREATE DATABASE <unique> TEMPLATE <template>`, serialized per
//!   server. A template with a session open cannot be copied, so every template
//!   refuses connections once it is built.
//!
//! # Teardown — which mechanism covers which exit
//!
//! A `static` is never dropped, so `postgresql_embedded`'s own `Drop` (the only
//! place it acts on `temporary`) never runs for the process's cluster. The
//! harness owns teardown instead:
//!
//! | how the test process ends | Linux / macOS | Windows |
//! |---|---|---|
//! | every test passed (`main` returns) | `atexit`: `pg_ctl stop -m immediate`, then the root is removed | the same |
//! | a test failed (libtest calls `process::exit`) | the same `atexit` | `process::exit` is `ExitProcess`, and whether the C runtime runs `atexit` handlers then is not established here (the failing-child self-test logs which happened): either way the postmaster dies — stopped by `atexit` if it runs, killed by the kill-on-close job if not — and a root `atexit` did not remove waits for the next sweep |
//! | killed or crashed | the postmaster is orphaned; the next boot's sweep kills it and removes the root | the job kills the postmaster; the next sweep removes the root |
//! | any of these, on an external server ([`EXTERNAL_URL_ENV`]) | `atexit`, where it runs, drops this process's `qtpl_<nonce>*` and `qtest_<nonce>_*` databases; a killed or crashed process leaves them, and a later boot only reports them | the same, and a failed test process leaves them too if `atexit` did not run |
//!
//! The sweep runs at every boot: a `run-*` root whose recorded owner pid is gone
//! has its postmaster killed — after checking that the pid really is that
//! root's postmaster — and is removed. A pid is judged only in the PID
//! namespace that recorded it: a root whose owner recorded another one (a
//! container sharing the target directory, a second WSL distro) is presumed
//! live until its record is [`FOREIGN_OWNER_GRACE`] old, because its pid names
//! nothing here.
//!
//! Nothing sweeps an external server. Its boot names the harness databases there
//! that belong to no nonce it knows ([`foreign_harness_databases`]) and drops
//! none: a concurrent test process's databases look exactly like a killed one's.
//!
//! On Windows the harness also clears the inherit flag on the process's std
//! handles before it starts anything: `pg_ctl` and the postmaster inherit every
//! inheritable handle, and a postmaster holding CI's `| tee` pipe would hold the
//! step open until its timeout.
//!
//! # Opt-in external server
//!
//! [`EXTERNAL_URL_ENV`] (`QONTINUI_TEST_PG_URL`) runs every harness database on
//! another server — the one way to test against real pgvector, which the
//! bundled server's `bytea` transform cannot emulate. One line naming that
//! server goes to the stderr handle, each test still gets a fresh database there
//! (cloned from a per-process template), and the URL's own database only ever
//! hosts the harness's `CREATE`/`DROP DATABASE`. No harness-backed test reads
//! `DATABASE_URL`.
//!
//! # A harness that cannot boot FAILS the test
//!
//! Every test that asks for a database panics with the boot error. There is no
//! `#[ignore]`, no early return and no fallback that silently changes what ran.
//!
//! # Measurements
//!
//! The boot cost, every clone's cost and an exit-time summary (clone mean and
//! max, and the peak client connections against `max_connections`) are written
//! straight to the stderr handle. libtest captures `eprintln!` from passing
//! tests but not the handle, so CI logs carry them on both legs.

use std::collections::HashMap;
use std::future::Future;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use postgresql_embedded::{PostgreSQL, Settings, VersionReq, BOOTSTRAP_SUPERUSER};
use sha2::{Digest, Sha256};

use super::{
    apply_canonical_schema, apply_schema_sql, canonical_schema_sql, free_loopback_port,
    VectorColumns, POSTMASTER_PID,
};

/// Relocates the whole harness — template cache and per-process roots — away
/// from the default `<cargo target dir>/qontinui-test-pg` (to a RAM disk, or a
/// roomier volume). It is also the boot-failure self-test's hook: pointed at a
/// regular file, the harness cannot create its directory and must fail every
/// test that asks it for a database.
pub const HARNESS_DIR_ENV: &str = "QONTINUI_TEST_PG_DIR";

/// Runs every harness database on this EXTERNAL server instead of the bundled
/// cluster: a `postgres://` or `postgresql://` URL whose role may create
/// databases, on PostgreSQL 13 or newer (`DROP DATABASE … WITH (FORCE)`),
/// reachable without TLS. A name nothing else exports, on purpose: no
/// harness-backed test reads `DATABASE_URL`, which is set on a developer box and
/// in DB-gated CI.
pub const EXTERNAL_URL_ENV: &str = "QONTINUI_TEST_PG_URL";

/// `max_connections` of every harness cluster. Set explicitly rather than left
/// at PostgreSQL's default of 100: per-test pools (a `PgDb` pool holds up to 8)
/// under libtest's default thread count — one per core on a many-core box — can
/// pass 100. The exit-time summary reports the measured peak against it.
pub const MAX_CONNECTIONS: u32 = 400;

// Clearing PostgreSQL's default is the whole point of setting it: a compile
// error, not a test, if it is ever lowered back to the default or below.
const _: () = assert!(
    MAX_CONNECTIONS > 100,
    "MAX_CONNECTIONS must clear PostgreSQL's default of 100"
);

/// Bump when the way the template cache is BUILT changes in a way its other key
/// inputs cannot see, so every machine rebuilds rather than reusing a cache
/// built the old way.
const HARNESS_VERSION: &str = "1";

/// The `postgresql_embedded` release a cache was built with — a key input,
/// because its `initdb` invocation shapes the data directory. Pinned against
/// `Cargo.lock` by a test, so a crate upgrade cannot reuse an old cache.
const POSTGRESQL_EMBEDDED_VERSION: &str = "0.20.4";

/// The canonical template database inside the cached data directory.
const TEMPLATE_DB: &str = "qontinui_template";
const DEFAULT_DIR_NAME: &str = "qontinui-test-pg";
const LOCK_FILE: &str = ".lock";
/// Written last by a template build: its presence is what makes a cache usable.
const READY_FILE: &str = "READY";
/// Rewritten by every process that enters a template cache: its mtime is when
/// the cache was last used ([`last_used`]).
const LAST_USED_FILE: &str = "last_used";
/// How long a template cache no live root was copied from is kept after its
/// last use. A day outlasts any branch switch a developer would come back from,
/// so alternating between two schemas never rebuilds either cache.
const TEMPLATE_IDLE_LIMIT: Duration = Duration::from_secs(24 * 60 * 60);
/// The per-process root's record of who owns it (see [`Owner`]).
const OWNER_FILE: &str = "owner";
const RUN_PREFIX: &str = "run-";
const TEMPLATE_PREFIX: &str = "tpl-";
const DATA_DIR: &str = "pg-data";
const INSTALL_DIR: &str = "pg-install";
const PASSWORD_FILE: &str = "pg-pass";
/// The server log `postgresql_embedded`'s `start()` points `pg_ctl -l` at.
const START_LOG: &str = "start.log";
/// The per-command timeout handed to `postgresql_embedded` for `initdb` and
/// `pg_ctl`, which today bounds NOTHING: the crate reads `Settings::timeout`
/// only in the `execute_command` its `tokio` feature compiles in, this crate
/// builds it without that feature, and the blocking `execute_command` it gets
/// ignores the timeout.
///
/// What does hold: `pg_ctl` runs with `--wait` and no `--timeout`, so it gives
/// up after its own default wait (60 s unless `PGCTLTIMEOUT` sets another), but
/// `initdb` has no bound at all. In CI a hung `initdb` runs until the Run Rust
/// tests step's `timeout-minutes` ends the whole leg. Locally it hangs the boot
/// while that boot holds the harness directory's EXCLUSIVE cache lock, so every
/// test process sharing the directory waits behind it.
///
/// Kept, and passed, so the bound is real the moment that feature is enabled.
/// Generous for that day: a cold `initdb` on a saturated box takes tens of
/// seconds, and a timeout here fails every database test in the binary.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(300);
/// `pg_ctl start` attempts, each on a fresh port, before a bind collision is a
/// boot failure. The port comes from binding port 0 and releasing it, so another
/// process can take it in between.
const START_ATTEMPTS: usize = 3;
/// How long the exit-time `pg_ctl stop -m immediate` waits for the server.
const STOP_TIMEOUT_SECS: &str = "30";
/// The hard bound on that `pg_ctl stop` itself: its own `--timeout` plus margin.
const STOP_WAIT: Duration = Duration::from_secs(45);
/// A `run-*` root with no readable owner record is left alone this long: it may
/// be a live process's root, caught between its `mkdir` and its record's write.
const OWNERLESS_GRACE: Duration = Duration::from_secs(600);
/// A `run-*` root whose owner recorded a different PID namespace is left alone
/// until its record is this old: its pid cannot be checked from here, so the
/// root is presumed live for longer than any test process runs. Not
/// [`OWNERLESS_GRACE`], which outlasts only the moment between a root's `mkdir`
/// and its record's write, not a whole run.
const FOREIGN_OWNER_GRACE: Duration = Duration::from_secs(24 * 60 * 60);
/// How long a template may keep sessions open after it is sealed before boot
/// fails; stragglers are terminated after [`TEMPLATE_STRAGGLER_GRACE`].
const TEMPLATE_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const TEMPLATE_STRAGGLER_GRACE: Duration = Duration::from_secs(2);
/// How often the peak-connection sampler counts client backends.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);
/// How much of a server log a boot error quotes.
const LOG_TAIL_BYTES: usize = 2048;
/// How long a removal keeps retrying a root whose files are still held open (on
/// Windows, by postgres processes that are exiting).
const REMOVE_RETRY_WINDOW: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// A database created for one test. Its URL addresses it as the server's
/// superuser (the bundled cluster) or as the opted-in role (an external server).
///
/// Dropping it drops the database — in the background, `WITH (FORCE)`, so a
/// connection still open on it cannot keep it alive. A database still alive at
/// exit goes with its cluster.
pub struct TestDatabase {
    name: String,
    url: String,
    server: Arc<Server>,
}

impl TestDatabase {
    /// The database's connection URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The database's name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl std::fmt::Debug for TestDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the URL: it carries the server's password.
        f.debug_struct("TestDatabase")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let server = Arc::clone(&self.server);
        let name = std::mem::take(&mut self.name);
        harness()
            .handle
            .spawn(async move { server.drop_database(&name).await });
    }
}

/// The future a [`DerivedTemplate`]'s provisioning step returns.
pub type ProvisionFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'static>>;

/// A template derived from the canonical one by a provisioning step that runs
/// ONCE per process, before the first database is cloned from it.
///
/// The step is handed the derived template's URL and runs on the harness's
/// runtime. It must close every connection it opened before it returns: the
/// harness then refuses new connections to the template and terminates any it
/// still finds, because a template with a session open cannot be copied. A
/// failed step fails every test that asks for the template, with its error.
///
/// Declare one as a `static`. Its `name` (lowercase letters, digits and `_`)
/// must be unique within a process, and must not ask the harness for a database
/// itself: it runs while the harness holds its clone lock.
pub struct DerivedTemplate {
    name: &'static str,
    provision: fn(String) -> ProvisionFuture,
}

impl DerivedTemplate {
    /// A derived template called `name`, provisioned by `provision`.
    pub const fn new(name: &'static str, provision: fn(String) -> ProvisionFuture) -> Self {
        Self { name, provision }
    }
}

/// A fresh database cloned from the canonical template.
///
/// Panics with the harness's boot error when the cluster cannot start: a test
/// that cannot get its database fails; it is never skipped.
pub async fn fresh_database() -> TestDatabase {
    on_harness(create_fresh(None)).await
}

/// [`fresh_database`] for a synchronous caller. Also safe inside a runtime: the
/// work runs on the harness's own runtime thread, and this only waits for it.
pub fn fresh_database_blocking() -> TestDatabase {
    on_harness_blocking(create_fresh(None))
}

/// A fresh database cloned from `template`, provisioning it first if this is
/// the process's first request for it. Panics like [`fresh_database`].
pub async fn fresh_database_from(template: &'static DerivedTemplate) -> TestDatabase {
    on_harness(create_fresh(Some(template))).await
}

/// [`fresh_database_from`] for a synchronous caller; safe inside a runtime, as
/// [`fresh_database_blocking`] is.
pub fn fresh_database_from_blocking(template: &'static DerivedTemplate) -> TestDatabase {
    on_harness_blocking(create_fresh(Some(template)))
}

/// The most client connections the harness has seen open at once on this
/// process's server so far, or `None` before it has booted. Sampled every
/// [`SAMPLE_INTERVAL`] and at no other time, so a shorter spike can be missed.
pub fn peak_client_connections() -> Option<u64> {
    let server = HARNESS.get()?.server.get()?.as_ref().ok()?;
    Some(server.stats.peak())
}

// ---------------------------------------------------------------------------
// The harness runtime
// ---------------------------------------------------------------------------

/// The process-lived harness: its runtime and the server it boots on demand.
struct Harness {
    handle: tokio::runtime::Handle,
    /// The boot's outcome, failure included: a boot that failed is not retried
    /// by every later test, each of which panics with the same error instead.
    server: tokio::sync::OnceCell<Result<Arc<Server>, String>>,
}

static HARNESS: OnceLock<Harness> = OnceLock::new();

thread_local! {
    /// Set on the harness runtime's own threads, where a blocking request would
    /// wait on work queued behind itself.
    static ON_HARNESS_RUNTIME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn harness() -> &'static Harness {
    HARNESS.get_or_init(start_harness_runtime)
}

/// Build the harness runtime on a dedicated thread that parks on it for the
/// life of the process, so the runtime is never dropped — and in particular
/// never dropped from inside an async context.
fn start_harness_runtime() -> Harness {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("qontinui-test-pg".to_string())
        .spawn(move || {
            let built = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("qontinui-test-pg-rt")
                .on_thread_start(|| ON_HARNESS_RUNTIME.with(|on| on.set(true)))
                .enable_all()
                .build();
            match built {
                Ok(runtime) => {
                    let _ = sender.send(Ok(runtime.handle().clone()));
                    runtime.block_on(std::future::pending::<()>());
                }
                Err(e) => {
                    let _ = sender.send(Err(e.to_string()));
                }
            }
        });
    if let Err(e) = spawned {
        panic!("qontinui test-pg: could not spawn the harness runtime thread: {e}");
    }
    let handle = match receiver.recv() {
        Ok(Ok(handle)) => handle,
        Ok(Err(e)) => panic!("qontinui test-pg: could not build the harness runtime: {e}"),
        Err(_) => panic!("qontinui test-pg: the harness runtime thread died before it started"),
    };
    Harness {
        handle,
        server: tokio::sync::OnceCell::new(),
    }
}

/// Run `work` on the harness runtime and await it from the caller's.
async fn on_harness<T: Send + 'static>(
    work: impl Future<Output = Result<T, String>> + Send + 'static,
) -> T {
    match harness().handle.spawn(work).await {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => panic!("qontinui test-pg: {e}"),
        Err(join) if join.is_panic() => std::panic::resume_unwind(join.into_panic()),
        Err(join) => panic!("qontinui test-pg: the harness task was cancelled: {join}"),
    }
}

/// Run `work` on the harness runtime and block this thread until it is done.
fn on_harness_blocking<T: Send + 'static>(
    work: impl Future<Output = Result<T, String>> + Send + 'static,
) -> T {
    assert!(
        !ON_HARNESS_RUNTIME.with(std::cell::Cell::get),
        "qontinui test-pg: a blocking harness door was called from the harness's own runtime \
         (a DerivedTemplate provisioning step?); it would wait on work queued behind itself"
    );
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    harness().handle.spawn(async move {
        let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(work)).await;
        let _ = sender.send(outcome);
    });
    match receiver.recv() {
        Ok(Ok(Ok(value))) => value,
        Ok(Ok(Err(e))) => panic!("qontinui test-pg: {e}"),
        Ok(Err(payload)) => std::panic::resume_unwind(payload),
        Err(_) => panic!("qontinui test-pg: the harness runtime dropped the request"),
    }
}

async fn create_fresh(template: Option<&'static DerivedTemplate>) -> Result<TestDatabase, String> {
    server().await?.fresh(template).await
}

/// The booted server, booting it on the first call. Runs on the harness runtime.
async fn server() -> Result<Arc<Server>, String> {
    harness()
        .server
        .get_or_init(|| async {
            boot().await.map(Arc::new).map_err(|e| {
                let message = format!(
                    "the test PostgreSQL could not boot, so this test cannot run — it is \
                     failed, not skipped: {e}"
                );
                say(&message);
                message
            })
        })
        .await
        .clone()
}

// ---------------------------------------------------------------------------
// The server
// ---------------------------------------------------------------------------

/// One process's server: the bundled cluster it booted, or the external one.
struct Server {
    flavor: Flavor,
    /// The canonical template database on this server.
    template: String,
    /// This process's token in every database name it creates.
    nonce: String,
    next_seq: AtomicU64,
    state: tokio::sync::Mutex<ServerState>,
    stats: Arc<Stats>,
    /// The bundled cluster's process facts; `None` on an external server.
    embedded: Option<EmbeddedFacts>,
    /// Held, never dropped (the server lives in a static): teardown is the
    /// harness's, and `postgresql_embedded`'s `Drop` must not stop the cluster
    /// on its own.
    _postmaster: Option<PostgreSQL>,
    /// Windows: the kill-on-close job the postmaster is in. Never dropped; the
    /// OS closes it when this process ends, however it ends.
    _exit_guard: Option<crate::process_helpers::ChildTreeGuard>,
}

enum Flavor {
    Embedded { port: u16, password: String },
    External { base: url::Url },
}

/// Where this process's bundled cluster lives — for the child-process
/// self-tests, which check that none of it outlives the process.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EmbeddedFacts {
    root: PathBuf,
    data_dir: PathBuf,
    postmaster_pid: u32,
    port: u16,
}

/// A derived template's provisioning outcome, with the template it was
/// provisioned for so a second template under the same name is caught.
type DerivedEntry = Result<(&'static DerivedTemplate, String), String>;

struct ServerState {
    maintenance: Option<tokio_postgres::Client>,
    derived: HashMap<&'static str, DerivedEntry>,
}

impl Server {
    fn url_for(&self, database: &str) -> String {
        match &self.flavor {
            Flavor::Embedded { port, password } => {
                format!("postgresql://{BOOTSTRAP_SUPERUSER}:{password}@127.0.0.1:{port}/{database}")
            }
            Flavor::External { base } => url_with_database(base, database),
        }
    }

    fn maintenance_url(&self) -> String {
        match &self.flavor {
            Flavor::Embedded { .. } => self.url_for("postgres"),
            Flavor::External { base } => base.to_string(),
        }
    }

    fn is_embedded(&self) -> bool {
        matches!(self.flavor, Flavor::Embedded { .. })
    }

    /// The maintenance session, (re)connected if it is missing or closed.
    async fn maintenance<'s>(
        &self,
        state: &'s mut ServerState,
    ) -> Result<&'s tokio_postgres::Client, String> {
        if state
            .maintenance
            .as_ref()
            .is_none_or(tokio_postgres::Client::is_closed)
        {
            let client = connect(&self.maintenance_url()).await.map_err(|e| {
                format!("connecting to the harness server's maintenance database failed: {e}")
            })?;
            state.maintenance = Some(client);
        }
        state
            .maintenance
            .as_ref()
            .ok_or_else(|| "the maintenance session vanished".to_string())
    }

    async fn fresh(
        self: &Arc<Self>,
        template: Option<&'static DerivedTemplate>,
    ) -> Result<TestDatabase, String> {
        let requested = Instant::now();
        let mut state = self.state.lock().await;
        let source = match template {
            None => self.template.clone(),
            Some(template) => self.derived(&mut state, template).await?,
        };
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let name = format!("qtest_{}_{seq}", self.nonce);
        let queued = requested.elapsed();
        let cloning = Instant::now();
        create_database(self.maintenance(&mut state).await?, &name, &source).await?;
        drop(state);
        let took = cloning.elapsed();
        self.stats.record_clone(took);
        say(format!(
            "fresh database {name} from {source}: CREATE DATABASE {} ms (queued {} ms); peak \
             client connections so far {}",
            took.as_millis(),
            queued.as_millis(),
            self.stats.peak()
        ));
        Ok(TestDatabase {
            url: self.url_for(&name),
            name,
            server: Arc::clone(self),
        })
    }

    async fn derived(
        &self,
        state: &mut ServerState,
        template: &'static DerivedTemplate,
    ) -> Result<String, String> {
        if let Some(entry) = state.derived.get(template.name) {
            return match entry {
                Ok((owner, database)) if std::ptr::eq(*owner, template) => Ok(database.clone()),
                Ok(_) => Err(format!(
                    "two different DerivedTemplates are named {:?} in this process; a derived \
                     template's name must be unique",
                    template.name
                )),
                Err(e) => Err(e.clone()),
            };
        }
        let outcome = self.provision(state, template).await;
        state.derived.insert(
            template.name,
            outcome.clone().map(|database| (template, database)),
        );
        outcome
    }

    async fn provision(
        &self,
        state: &mut ServerState,
        template: &'static DerivedTemplate,
    ) -> Result<String, String> {
        let database = format!("{}_{}", self.template, template.name);
        if !is_identifier(template.name) || !is_identifier(&database) {
            return Err(format!(
                "derived template name {:?} is unusable: a database name here is lowercase \
                 letters, digits and `_`, at most 63 bytes ({database:?})",
                template.name
            ));
        }
        let started = Instant::now();
        create_database(self.maintenance(state).await?, &database, &self.template).await?;
        let provisioned = (template.provision)(self.url_for(&database)).await;
        // Whatever the step did, nothing may be cloned from the template until
        // it refuses connections and has none left.
        seal_template(
            self.maintenance(state).await?,
            &database,
            self.is_embedded(),
        )
        .await?;
        provisioned.map_err(|e| {
            format!(
                "provisioning derived template {:?} (database {database}) failed: {e}",
                template.name
            )
        })?;
        say(format!(
            "derived template {database} provisioned in {} ms",
            started.elapsed().as_millis()
        ));
        Ok(database)
    }

    async fn drop_database(&self, name: &str) {
        let mut state = self.state.lock().await;
        let Ok(client) = self.maintenance(&mut state).await else {
            return;
        };
        let dropped = client
            .batch_execute(&format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"))
            .await;
        if dropped.is_ok() {
            self.stats.drops.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// What the exit-time summary reports.
struct Stats {
    /// `max_connections` of the server, when the harness set it.
    connection_limit: Option<u32>,
    clones: AtomicU64,
    clone_micros: AtomicU64,
    clone_max_micros: AtomicU64,
    drops: AtomicU64,
    peak_connections: AtomicU64,
}

impl Stats {
    fn new(connection_limit: Option<u32>) -> Self {
        Self {
            connection_limit,
            clones: AtomicU64::new(0),
            clone_micros: AtomicU64::new(0),
            clone_max_micros: AtomicU64::new(0),
            drops: AtomicU64::new(0),
            peak_connections: AtomicU64::new(0),
        }
    }

    fn record_clone(&self, took: Duration) {
        let micros = u64::try_from(took.as_micros()).unwrap_or(u64::MAX);
        self.clones.fetch_add(1, Ordering::Relaxed);
        self.clone_micros.fetch_add(micros, Ordering::Relaxed);
        self.clone_max_micros.fetch_max(micros, Ordering::Relaxed);
    }

    fn observe_connections(&self, open: u64) {
        self.peak_connections.fetch_max(open, Ordering::Relaxed);
    }

    fn peak(&self) -> u64 {
        self.peak_connections.load(Ordering::Relaxed)
    }

    fn summary(&self) -> String {
        let clones = self.clones.load(Ordering::Relaxed);
        let mean_ms = self
            .clone_micros
            .load(Ordering::Relaxed)
            .checked_div(clones)
            .unwrap_or(0)
            / 1000;
        let limit = self.connection_limit.map_or_else(
            || "the external server's own limit".to_string(),
            |limit| format!("max_connections {limit}"),
        );
        format!(
            "{clones} fresh database(s) (CREATE DATABASE mean {mean_ms} ms, max {} ms; {} \
             dropped before exit); peak client connections {} of {limit}",
            self.clone_max_micros.load(Ordering::Relaxed) / 1000,
            self.drops.load(Ordering::Relaxed),
            self.peak()
        )
    }
}

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

async fn boot() -> Result<Server, String> {
    match env_nonblank(EXTERNAL_URL_ENV) {
        Some(url) => boot_external(url).await,
        None => boot_embedded().await,
    }
}

/// The bundled release and the releases URL its archive extractor is
/// registered under, read off `postgresql_embedded`'s defaults.
struct BundledDefaults {
    version: VersionReq,
    releases_url: String,
}

fn bundled_defaults() -> BundledDefaults {
    let probe = Settings::default();
    // `Settings::new()` creates two EMPTY directories under the system temp dir
    // (`tempfile::tempdir().keep()`) as the defaults for `data_dir` and for the
    // password file's parent. Remove both, so asking for the defaults leaks
    // nothing. `remove_dir` only ever removes an empty directory.
    let temp = std::env::temp_dir();
    let leaked = [Some(probe.data_dir.as_path()), probe.password_file.parent()];
    for dir in leaked.into_iter().flatten() {
        if dir.starts_with(&temp) {
            let _ = std::fs::remove_dir(dir);
        }
    }
    BundledDefaults {
        version: probe.version,
        releases_url: probe.releases_url,
    }
}

async fn boot_embedded() -> Result<Server, String> {
    let booting = Instant::now();
    keep_std_handles_out_of_children();
    let dir = harness_dir()?;
    create_private_dir_all(&dir).map_err(|e| {
        format!(
            "could not create the harness directory {}: {e}",
            dir.display()
        )
    })?;
    for line in sweep_dead_roots(&dir) {
        say(line);
    }

    let defaults = bundled_defaults();
    let version = defaults.version.to_string();
    let key = cache_key(&version, &canonical_schema_sql(VectorColumns::Bytea));
    let template_dir = dir.join(format!("{TEMPLATE_PREFIX}{key}"));
    let stats = Arc::new(Stats::new(Some(MAX_CONNECTIONS)));
    let cache_lock = open_lock(&dir)?;
    let (entered, built) = enter_or_build(
        &cache_lock,
        &template_dir,
        || build_template(&dir, &template_dir, &key, &defaults),
        || enter_template(&dir, &template_dir, &key, &stats),
    )
    .await?;

    let data_dir = entered.root.join(DATA_DIR);
    let password_file = template_dir.join(PASSWORD_FILE);
    let password = read_password(&password_file)?;
    let base = cluster_settings(
        entered.install.clone(),
        true,
        data_dir.clone(),
        password_file,
        password.clone(),
        &defaults,
    );
    let (postmaster, start_took) = start_on_free_port(&base).await?;
    let port = postmaster.settings().port;
    let pid = read_postmaster_pid(&data_dir).ok_or_else(|| {
        format!(
            "the cluster started but {} names no postmaster",
            data_dir.join(POSTMASTER_PID).display()
        )
    })?;
    with_teardown(|plan| {
        if let Some(cluster) = plan.cluster.as_mut() {
            cluster.postmaster_pid = Some(pid);
        }
    });
    write_owner(&entered.root, &Owner::this_process(&key, Some(pid)))?;
    let exit_guard = crate::process_helpers::ChildTreeGuard::attach_pid(pid);
    let exit_guard_note = if !cfg!(windows) {
        "atexit; a killed process's cluster is reaped by the next boot's sweep"
    } else if exit_guard.is_attached() {
        "atexit + kill-on-close job"
    } else {
        "atexit only — the kill-on-close job could not be attached"
    };

    let maintenance_url =
        format!("postgresql://{BOOTSTRAP_SUPERUSER}:{password}@127.0.0.1:{port}/postgres");
    let maintenance = connect(&maintenance_url)
        .await
        .map_err(|e| format!("connecting to the started cluster failed: {e}"))?;
    spawn_peak_sampler(maintenance_url, None, Arc::clone(&stats));

    let cache = built.map_or_else(
        || "hit".to_string(),
        |took| format!("built in {} ms", took.as_millis()),
    );
    say(format!(
        "bundled PostgreSQL {} ready on 127.0.0.1:{port} in {} ms (template cache {cache}; \
         data copy {} ms; pg_ctl start {} ms); root {}; postmaster pid {pid}; \
         max_connections {MAX_CONNECTIONS}; exit guard: {exit_guard_note}",
        version.trim_start_matches('='),
        booting.elapsed().as_millis(),
        entered.copy_took.as_millis(),
        start_took.as_millis(),
        entered.root.display()
    ));

    Ok(Server {
        flavor: Flavor::Embedded { port, password },
        template: TEMPLATE_DB.to_string(),
        nonce: process_nonce().to_string(),
        next_seq: AtomicU64::new(0),
        state: tokio::sync::Mutex::new(ServerState {
            maintenance: Some(maintenance),
            derived: HashMap::new(),
        }),
        stats,
        embedded: Some(EmbeddedFacts {
            root: entered.root,
            data_dir,
            postmaster_pid: pid,
            port,
        }),
        _postmaster: Some(postmaster),
        _exit_guard: Some(exit_guard),
    })
}

/// Enter the template cache in `template_dir` — `enter` copies it into this
/// process's root — building it first with `build` when it is missing. Returns
/// what `enter` returned and, when this call built the cache, how long that
/// took.
///
/// A READY cache is entered under the SHARED cache lock: a collection holds the
/// lock exclusively, so none can remove the cache mid-copy. A missing one is
/// built AND entered under the EXCLUSIVE lock, which is not let go of in
/// between. A lock cannot be downgraded in place, and in the gap a release
/// would open, another key's build could collect the new cache before any live
/// root names it: two processes whose schemas differ, sharing one harness
/// directory, then removed each other's cache in turn until neither became
/// ready.
///
/// Either way the cache is marked used ([`LAST_USED_FILE`]) as it is entered,
/// under the same lock.
async fn enter_or_build<T, Built>(
    cache_lock: &std::fs::File,
    template_dir: &Path,
    build: impl FnOnce() -> Built,
    enter: impl FnOnce() -> Result<T, String>,
) -> Result<(T, Option<Duration>), String>
where
    Built: Future<Output = Result<Duration, String>>,
{
    let lock_failed = |e: std::io::Error| {
        format!(
            "could not take the cache lock for {}: {e}",
            template_dir.display()
        )
    };
    cache_lock.lock_shared().map_err(lock_failed)?;
    if template_dir.join(READY_FILE).is_file() {
        touch_last_used(template_dir);
        let entered = enter();
        let _ = cache_lock.unlock();
        return entered.map(|entered| (entered, None));
    }
    let _ = cache_lock.unlock();
    cache_lock.lock().map_err(lock_failed)?;
    let outcome = async move {
        // Another process may have built it while this one waited.
        let built = if template_dir.join(READY_FILE).is_file() {
            None
        } else {
            Some(build().await?)
        };
        touch_last_used(template_dir);
        enter().map(|entered| (entered, built))
    }
    .await;
    let _ = cache_lock.unlock();
    outcome
}

/// Mark a template cache used now. [`LAST_USED_FILE`]'s mtime is what
/// [`collect_stale_templates`] ages a cache by; its content says the same to a
/// reader. Best effort: a cache that cannot be marked is still entered, and the
/// root copied from it still keeps it from collection while it lives.
fn touch_last_used(template_dir: &Path) {
    let path = template_dir.join(LAST_USED_FILE);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    if let Err(e) = std::fs::write(&path, format!("used_unix={now}\n")) {
        say(format!(
            "could not mark the template cache used ({}): {e}",
            path.display()
        ));
    }
}

/// A per-process root holding a fresh copy of the template's data directory.
struct Entered {
    root: PathBuf,
    install: PathBuf,
    copy_took: Duration,
}

/// Create this process's root and copy the cached data directory into it. Runs
/// under the cache lock ([`enter_or_build`]): shared, or exclusive straight
/// after this process built the cache. The root is handed to the exit-time
/// teardown before anything is copied, so a failure from here on still removes
/// it.
fn enter_template(
    dir: &Path,
    template_dir: &Path,
    key: &str,
    stats: &Arc<Stats>,
) -> Result<Entered, String> {
    let install = installed_dir(template_dir)?;
    let root = create_run_root(dir, key)?;
    arm_teardown(Teardown {
        cluster: Some(ClusterTeardown {
            pg_ctl: pg_ctl_in(&install),
            data_dir: root.join(DATA_DIR),
            root: root.clone(),
            postmaster_pid: None,
        }),
        external: None,
        stats: Arc::clone(stats),
    });
    let copying = Instant::now();
    copy_tree(&template_dir.join(DATA_DIR), &root.join(DATA_DIR)).map_err(|e| {
        format!(
            "copying the cached data directory into {} failed: {e}",
            root.display()
        )
    })?;
    Ok(Entered {
        root,
        install,
        copy_took: copying.elapsed(),
    })
}

/// Build the template cache in `template_dir`. Runs under the EXCLUSIVE cache
/// lock, so no other build runs and no process is copying a cache.
async fn build_template(
    dir: &Path,
    template_dir: &Path,
    key: &str,
    defaults: &BundledDefaults,
) -> Result<Duration, String> {
    let building = Instant::now();
    // A cache directory without READY is a crashed build's, and may still have
    // that build's postmaster running in it.
    if template_dir.exists() {
        let note = reap_postmaster(template_dir, None);
        remove_dir_all_retrying(template_dir).map_err(|e| {
            format!(
                "could not clear the incomplete template cache {} ({note}): {e}",
                template_dir.display()
            )
        })?;
    }
    for line in collect_stale_templates(dir, key) {
        say(line);
    }
    // Private: it holds the cluster's superuser password (`pg-pass`).
    create_private_dir(template_dir).map_err(|e| {
        format!(
            "could not create the template cache {}: {e}",
            template_dir.display()
        )
    })?;
    if let Err(e) = build_template_cluster(template_dir, defaults).await {
        let note = reap_postmaster(template_dir, None);
        let _ = remove_dir_all_retrying(template_dir);
        return Err(format!(
            "building the template cache in {} failed: {e} ({note})",
            template_dir.display()
        ));
    }
    let built_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    std::fs::write(
        template_dir.join(READY_FILE),
        format!(
            "key={key}\npostgresql={}\nbuilt_unix={built_at}\n",
            defaults.version
        ),
    )
    .map_err(|e| {
        format!(
            "could not mark the template cache {} ready: {e}",
            template_dir.display()
        )
    })?;
    let took = building.elapsed();
    say(format!(
        "built the template cache {} in {} ms",
        template_dir.display(),
        took.as_millis()
    ));
    Ok(took)
}

/// Extract the bundled release, `initdb`, start, create the canonical template,
/// apply the transformed schema to it — through [`apply_canonical_schema`], the
/// path every end user's fresh install takes — seal it, and shut down CLEANLY,
/// so every copy starts without crash recovery.
///
/// Built from this module's own `Settings` rather than through
/// [`super::bootstrap`]: bootstrap's failure paths deliberately leave a started
/// postmaster running (right for a production peer that may have attached,
/// wrong for a private build directory), and its attach path has no business
/// here. Its 60 s command timeout is no reason either way: like
/// [`COMMAND_TIMEOUT`], it bounds nothing while `postgresql_embedded` is built
/// without its `tokio` feature. On every failure below, the dropped
/// `PostgreSQL` stops the cluster this build started, which is ours to stop.
async fn build_template_cluster(
    template_dir: &Path,
    defaults: &BundledDefaults,
) -> Result<(), String> {
    let data_dir = template_dir.join(DATA_DIR);
    let mut installer = PostgreSQL::new(cluster_settings(
        template_dir.join(INSTALL_DIR),
        false,
        data_dir.clone(),
        template_dir.join(PASSWORD_FILE),
        uuid::Uuid::new_v4().simple().to_string(),
        defaults,
    ));
    installer
        .setup()
        .await
        .map_err(|e| format!("extracting the bundled PostgreSQL and running initdb failed: {e}"))?;
    // `setup()` resolved the versioned install directory into its settings;
    // start from those, and let go of this handle before a cluster is running
    // that its `Drop` would act on.
    let base = installer.settings().clone();
    drop(installer);

    let (cluster, _) = start_on_free_port(&base).await?;
    cluster
        .create_database(TEMPLATE_DB)
        .await
        .map_err(|e| format!("CREATE DATABASE {TEMPLATE_DB} failed: {e}"))?;
    apply_canonical_schema(&cluster.settings().url(TEMPLATE_DB)).await?;
    let maintenance = connect(&cluster.settings().url("postgres")).await?;
    seal_template(&maintenance, TEMPLATE_DB, true).await?;
    drop(maintenance);
    cluster
        .stop()
        .await
        .map_err(|e| format!("stopping the template cluster failed: {e}"))?;
    drop(cluster);

    if data_dir.join(POSTMASTER_PID).exists() {
        return Err(format!(
            "the template cluster did not shut down cleanly: {} is still there",
            data_dir.join(POSTMASTER_PID).display()
        ));
    }
    // The build cluster's own log and command line; each process writes its own.
    for leftover in [START_LOG, "postmaster.opts"] {
        let _ = std::fs::remove_file(data_dir.join(leftover));
    }
    Ok(())
}

/// The server settings every harness cluster runs with, passed as `-c` options
/// at start (nothing is written into the data directory).
///
/// The durability settings are off because a test cluster is thrown away with
/// its process — none of them changes what a query returns. Unix sockets are
/// off so a cluster leaves nothing in `/tmp` and a deep target path cannot hit
/// the socket-path length limit; every connection is TCP on the IPv4 loopback.
/// `autovacuum` is off so no background worker churns through the clones.
fn server_configuration() -> HashMap<String, String> {
    [
        ("max_connections", MAX_CONNECTIONS.to_string()),
        ("listen_addresses", "127.0.0.1".to_string()),
        ("unix_socket_directories", String::new()),
        ("fsync", "off".to_string()),
        ("synchronous_commit", "off".to_string()),
        ("full_page_writes", "off".to_string()),
        ("autovacuum", "off".to_string()),
        ("jit", "off".to_string()),
        ("lc_messages", "C".to_string()),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value))
    .collect()
}

fn cluster_settings(
    installation_dir: PathBuf,
    trust_installation_dir: bool,
    data_dir: PathBuf,
    password_file: PathBuf,
    password: String,
    defaults: &BundledDefaults,
) -> Settings {
    Settings {
        releases_url: defaults.releases_url.clone(),
        version: defaults.version.clone(),
        installation_dir,
        password_file,
        data_dir,
        host: "127.0.0.1".to_string(),
        port: 0,
        username: BOOTSTRAP_SUPERUSER.to_string(),
        password,
        temporary: false,
        timeout: Some(COMMAND_TIMEOUT),
        configuration: server_configuration(),
        trust_installation_dir,
        socket_dir: None,
    }
}

/// `pg_ctl start` on a free loopback port, retrying on a fresh port when the one
/// picked was taken in between. On a failed attempt the dropped `PostgreSQL`
/// stops whatever came up in OUR data directory, which is ours to stop.
async fn start_on_free_port(base: &Settings) -> Result<(PostgreSQL, Duration), String> {
    let log = base.data_dir.join(START_LOG);
    let mut last_failure = String::new();
    for _ in 0..START_ATTEMPTS {
        let mut settings = base.clone();
        settings.port =
            free_loopback_port().ok_or_else(|| "no free loopback TCP port".to_string())?;
        let port = settings.port;
        let mut cluster = PostgreSQL::new(settings);
        let starting = Instant::now();
        match cluster.start().await {
            Ok(()) => return Ok((cluster, starting.elapsed())),
            Err(e) => {
                let tail = log_tail(&log);
                last_failure =
                    format!("pg_ctl start on port {port} failed: {e}; server log: {tail}");
                let bind_collision =
                    tail.contains("could not bind") || tail.contains("could not create any TCP");
                if !bind_collision {
                    return Err(last_failure);
                }
            }
        }
    }
    Err(last_failure)
}

async fn boot_external(raw: String) -> Result<Server, String> {
    let booting = Instant::now();
    let base =
        url::Url::parse(&raw).map_err(|e| format!("{EXTERNAL_URL_ENV} is not a URL: {e}"))?;
    if !matches!(base.scheme(), "postgres" | "postgresql") {
        return Err(format!(
            "{EXTERNAL_URL_ENV} must be a postgres:// or postgresql:// URL, not {:?}",
            base.scheme()
        ));
    }
    let server_name = describe_server(&base);
    say(format!(
        "{EXTERNAL_URL_ENV} is set: every test database is created on the EXTERNAL server \
         {server_name}, not on the bundled cluster"
    ));
    let nonce = process_nonce().to_string();
    let template = format!("qtpl_{nonce}");
    let stats = Arc::new(Stats::new(None));
    let maintenance = connect(&raw)
        .await
        .map_err(|e| format!("connecting to {server_name} failed: {e}"))?;
    let foreign = foreign_harness_databases(&maintenance, &nonce).await;
    if let Some(line) = describe_foreign_databases(&server_name, &foreign) {
        say(line);
    }
    let pgvector: bool = maintenance
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM pg_available_extensions WHERE name = 'vector')",
            &[],
        )
        .await
        .and_then(|row| row.try_get(0))
        .map_err(|e| {
            format!(
                "asking {server_name} for pgvector failed: {}",
                describe_pg_error(&e)
            )
        })?;
    arm_teardown(Teardown {
        cluster: None,
        external: Some(ExternalTeardown {
            url: raw.clone(),
            nonce: nonce.clone(),
        }),
        stats: Arc::clone(&stats),
    });
    maintenance
        .batch_execute(&format!("CREATE DATABASE \"{template}\""))
        .await
        .map_err(|e| {
            format!(
                "CREATE DATABASE {template} on {server_name} failed: {}",
                describe_pg_error(&e)
            )
        })?;
    let vectors = if pgvector {
        VectorColumns::Native
    } else {
        VectorColumns::Bytea
    };
    apply_schema_sql(
        &url_with_database(&base, &template),
        &canonical_schema_sql(vectors),
        "the external test server",
    )
    .await?;
    seal_template(&maintenance, &template, false).await?;
    spawn_peak_sampler(
        raw,
        Some(format!("qtest\\_{nonce}\\_%")),
        Arc::clone(&stats),
    );
    say(format!(
        "external template {template} ready on {server_name} in {} ms (vector columns: {})",
        booting.elapsed().as_millis(),
        if pgvector {
            "native pgvector"
        } else {
            "bytea — the server has no pgvector"
        }
    ));
    Ok(Server {
        flavor: Flavor::External { base },
        template,
        nonce,
        next_seq: AtomicU64::new(0),
        state: tokio::sync::Mutex::new(ServerState {
            maintenance: Some(maintenance),
            derived: HashMap::new(),
        }),
        stats,
        embedded: None,
        _postmaster: None,
        _exit_guard: None,
    })
}

/// How many leftover database names [`describe_foreign_databases`] spells out.
const FOREIGN_DATABASES_NAMED: usize = 10;

/// The harness databases on a server that belong to no nonce this process
/// knows: every `qtpl_%` and `qtest_%` database but its own, sorted. The only
/// nonce a process can know is its own, so each of these is a concurrent test
/// process's or one a killed process left behind, and nothing here tells which.
async fn foreign_harness_databases(
    client: &tokio_postgres::Client,
    nonce: &str,
) -> Result<Vec<String>, String> {
    let (templates, clones) = ("qtpl\\_%", "qtest\\_%");
    let (own_templates, own_clones) = (format!("qtpl\\_{nonce}%"), format!("qtest\\_{nonce}\\_%"));
    let rows = client
        .query(
            "SELECT datname FROM pg_database \
             WHERE (datname LIKE $1 OR datname LIKE $2) \
               AND datname NOT LIKE $3 AND datname NOT LIKE $4 \
             ORDER BY datname",
            &[&templates, &clones, &own_templates, &own_clones],
        )
        .await
        .map_err(|e| describe_pg_error(&e))?;
    rows.iter()
        .map(|row| {
            row.try_get::<_, String>(0)
                .map_err(|e| describe_pg_error(&e))
        })
        .collect()
}

/// The one stderr line an external boot gives [`foreign_harness_databases`]'
/// answer: none when there is nothing to report. It only REPORTS: dropping
/// another process's databases could pull them from under a running test.
fn describe_foreign_databases(
    server_name: &str,
    found: &Result<Vec<String>, String>,
) -> Option<String> {
    match found {
        Ok(names) if names.is_empty() => None,
        Ok(names) => {
            let named = names
                .iter()
                .take(FOREIGN_DATABASES_NAMED)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            let more = names.len().saturating_sub(FOREIGN_DATABASES_NAMED);
            let more = if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            };
            Some(format!(
                "{} harness database(s) on {server_name} belong to no nonce this process knows: \
                 another test process's, or left by one killed before its teardown ran. The \
                 harness drops none of them; remove the stale ones by hand: {named}{more}",
                names.len()
            ))
        }
        Err(e) => Some(format!(
            "could not list the harness databases already on {server_name}, so any a killed \
             test process left there go unreported: {e}"
        )),
    }
}

// ---------------------------------------------------------------------------
// SQL helpers
// ---------------------------------------------------------------------------

async fn connect(url: &str) -> Result<tokio_postgres::Client, String> {
    let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .map_err(|e| describe_pg_error(&e))?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

/// `tokio_postgres::Error`'s `Display` is the bare `db error` for every
/// server-side failure; the SQLSTATE and the server's message live in the cause.
fn describe_pg_error(e: &tokio_postgres::Error) -> String {
    match e.as_db_error() {
        Some(db) => {
            let detail = db
                .detail()
                .map(|detail| format!(" (detail: {detail})"))
                .unwrap_or_default();
            format!("[{}] {}{detail}", db.code().code(), db.message())
        }
        None => {
            let mut rendered = e.to_string();
            let mut source = std::error::Error::source(e);
            while let Some(cause) = source {
                rendered.push_str(" <- ");
                rendered.push_str(&cause.to_string());
                source = cause.source();
            }
            rendered
        }
    }
}

/// `CREATE DATABASE name TEMPLATE template`. Both names are this module's own
/// validated identifiers (`CREATE DATABASE` cannot take parameters).
async fn create_database(
    client: &tokio_postgres::Client,
    name: &str,
    template: &str,
) -> Result<(), String> {
    let sql = format!("CREATE DATABASE \"{name}\" TEMPLATE \"{template}\"");
    let mut attempt = 0;
    loop {
        attempt += 1;
        match client.batch_execute(&sql).await {
            Ok(()) => return Ok(()),
            Err(e)
                if attempt < 3
                    && e.code() == Some(&tokio_postgres::error::SqlState::OBJECT_IN_USE) =>
            {
                // "source database is being accessed by other users": a session
                // is on the template. It refuses new ones, so ending the
                // stragglers ends this.
                terminate_sessions(client, template).await;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(e) => {
                return Err(format!(
                    "CREATE DATABASE {name} TEMPLATE {template} failed: {}",
                    describe_pg_error(&e)
                ))
            }
        }
    }
}

async fn terminate_sessions(client: &tokio_postgres::Client, database: &str) {
    let _ = client
        .execute(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
             WHERE datname = $1 AND pid <> pg_backend_pid()",
            &[&database],
        )
        .await;
}

/// Make `database` a template nothing can connect to, and wait until nothing is.
///
/// `IS_TEMPLATE` is set only when `mark_template` is, which is on the bundled
/// cluster. On an external server the harness drops its templates at exit, and
/// `DROP DATABASE` refuses a database marked a template; leaving the mark off
/// costs nothing there, because `CREATE DATABASE … TEMPLATE` lets a database's
/// owner copy it whether or not it is marked.
async fn seal_template(
    client: &tokio_postgres::Client,
    database: &str,
    mark_template: bool,
) -> Result<(), String> {
    let options = if mark_template {
        "ALLOW_CONNECTIONS false IS_TEMPLATE true"
    } else {
        "ALLOW_CONNECTIONS false"
    };
    client
        .batch_execute(&format!("ALTER DATABASE \"{database}\" WITH {options}"))
        .await
        .map_err(|e| {
            format!(
                "sealing template {database} failed: {}",
                describe_pg_error(&e)
            )
        })?;
    let sealed = Instant::now();
    loop {
        let open: i64 = client
            .query_one(
                "SELECT count(*)::bigint FROM pg_stat_activity \
                 WHERE datname = $1 AND pid <> pg_backend_pid()",
                &[&database],
            )
            .await
            .and_then(|row| row.try_get(0))
            .map_err(|e| {
                format!(
                    "counting sessions on template {database} failed: {}",
                    describe_pg_error(&e)
                )
            })?;
        if open == 0 {
            return Ok(());
        }
        if sealed.elapsed() >= TEMPLATE_DRAIN_TIMEOUT {
            return Err(format!(
                "template {database} still has {open} session(s) {} s after it was sealed",
                TEMPLATE_DRAIN_TIMEOUT.as_secs()
            ));
        }
        if sealed.elapsed() >= TEMPLATE_STRAGGLER_GRACE {
            terminate_sessions(client, database).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Count client backends every [`SAMPLE_INTERVAL`] into [`Stats`]' peak. On the
/// bundled cluster every client backend is this process's; on an external one
/// only those on this process's databases (`database_pattern`, a `LIKE`
/// pattern) are. Ends when the server goes away.
fn spawn_peak_sampler(url: String, database_pattern: Option<String>, stats: Arc<Stats>) {
    tokio::spawn(async move {
        let Ok(client) = connect(&url).await else {
            return;
        };
        loop {
            let counted = match &database_pattern {
                None => {
                    client
                        .query_one(
                            "SELECT count(*)::bigint FROM pg_stat_activity \
                             WHERE backend_type = 'client backend' AND pid <> pg_backend_pid()",
                            &[],
                        )
                        .await
                }
                Some(pattern) => {
                    client
                        .query_one(
                            "SELECT count(*)::bigint FROM pg_stat_activity \
                             WHERE backend_type = 'client backend' AND pid <> pg_backend_pid() \
                               AND datname LIKE $1",
                            &[pattern],
                        )
                        .await
                }
            };
            match counted.and_then(|row| row.try_get::<_, i64>(0)) {
                Ok(open) => stats.observe_connections(u64::try_from(open).unwrap_or(0)),
                Err(_) => return,
            }
            tokio::time::sleep(SAMPLE_INTERVAL).await;
        }
    });
}

fn url_with_database(base: &url::Url, database: &str) -> String {
    let mut url = base.clone();
    url.set_path(&format!("/{database}"));
    url.to_string()
}

/// `user@host:port/database` — never the password.
fn describe_server(base: &url::Url) -> String {
    let user = if base.username().is_empty() {
        "(default user)"
    } else {
        base.username()
    };
    let port = base
        .port()
        .map_or_else(|| "(default port)".to_string(), |port| port.to_string());
    format!(
        "{user}@{}:{port}/{}",
        base.host_str().unwrap_or("(default host)"),
        base.path().trim_start_matches('/')
    )
}

/// A name the harness can use unquoted-safe inside double quotes: lowercase
/// ASCII letters, digits and `_`, starting with a letter or `_`, at most 63
/// bytes (PostgreSQL's `NAMEDATALEN - 1`).
fn is_identifier(name: &str) -> bool {
    name.len() <= 63
        && name
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

// ---------------------------------------------------------------------------
// The cache and the per-process root
// ---------------------------------------------------------------------------

/// The cache key: every input that shapes a cached template, hashed.
fn cache_key(pg_version: &str, schema_sql: &str) -> String {
    let mut hasher = Sha256::new();
    for part in [
        "qontinui-test-pg",
        HARNESS_VERSION,
        POSTGRESQL_EMBEDDED_VERSION,
        pg_version,
    ] {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    hasher.update(Sha256::digest(schema_sql.as_bytes()));
    let digest = hasher.finalize();
    hex::encode(&digest[..8])
}

fn env_nonblank(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// [`HARNESS_DIR_ENV`], else `qontinui-test-pg` in the directory
/// [`cargo_target_dir_of`] finds for this test binary.
fn harness_dir() -> Result<PathBuf, String> {
    if let Some(dir) = env_nonblank(HARNESS_DIR_ENV) {
        return Ok(PathBuf::from(dir));
    }
    let exe = std::env::current_exe().map_err(|e| {
        format!(
            "cannot locate this test binary to find the cargo target directory: {e}; set \
             {HARNESS_DIR_ENV}"
        )
    })?;
    cargo_target_dir_of(&exe)
        .map(|target| target.join(DEFAULT_DIR_NAME))
        .ok_or_else(|| {
            format!(
                "no ancestor of {} is a cargo target directory (none holds cargo's \
                 CACHEDIR.TAG); set {HARNESS_DIR_ENV} to a directory the harness may use",
                exe.display()
            )
        })
}

/// Where a test binary's harness directory goes: the binary's nearest ancestor
/// holding a cargo `CACHEDIR.TAG`. For `<target>/<profile>/deps/<binary>` that
/// is `<target>`. A build with `--target <triple>` — a cross build, or the
/// host's own triple named explicitly — puts the binary in
/// `<target>/<triple>/<profile>/deps/`, and cargo tags `<target>/<triple>/` as
/// well when it creates it, so the harness lands in
/// `<target>/<triple>/qontinui-test-pg`. That is intended: the template cache
/// holds the extracted bundled PostgreSQL, which `postgresql_embedded` bundles
/// for the target triple, and the cache key does not name the triple. Found
/// from the binary rather than `CARGO_TARGET_TMPDIR`, which cargo sets for
/// integration tests and benches only — this module serves every test binary.
fn cargo_target_dir_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .skip(1)
        .find(|dir| dir.join("CACHEDIR.TAG").is_file())
        .map(Path::to_path_buf)
}

fn open_lock(dir: &Path) -> Result<std::fs::File, String> {
    let path = dir.join(LOCK_FILE);
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| format!("could not open the cache lock {}: {e}", path.display()))
}

/// The versioned install directory inside a template cache: the one holding
/// `bin/pg_ctl`.
fn installed_dir(template_dir: &Path) -> Result<PathBuf, String> {
    let base = template_dir.join(INSTALL_DIR);
    let entries = std::fs::read_dir(&base).map_err(|e| {
        format!(
            "the template cache {} has no readable {INSTALL_DIR}: {e}",
            template_dir.display()
        )
    })?;
    entries
        .flatten()
        .map(|entry| entry.path())
        .find(|install| pg_ctl_in(install).is_file())
        .ok_or_else(|| {
            format!(
                "no PostgreSQL install holding bin/pg_ctl under {}; delete {} to rebuild the \
                 cache",
                base.display(),
                template_dir.display()
            )
        })
}

fn pg_ctl_in(install: &Path) -> PathBuf {
    install
        .join("bin")
        .join(format!("pg_ctl{}", std::env::consts::EXE_SUFFIX))
}

fn read_password(path: &Path) -> Result<String, String> {
    let stored = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "could not read the template cluster's password file {}: {e}",
            path.display()
        )
    })?;
    let stored = stored.trim();
    if stored.is_empty() {
        return Err(format!(
            "the template cluster's password file {} is empty; delete its cache to rebuild it",
            path.display()
        ));
    }
    Ok(stored.to_string())
}

/// The postmaster pid on the first line of a data directory's `postmaster.pid`.
fn read_postmaster_pid(data_dir: &Path) -> Option<u32> {
    let contents = std::fs::read_to_string(data_dir.join(POSTMASTER_PID)).ok()?;
    postmaster_pid_from(&contents)
}

fn postmaster_pid_from(contents: &str) -> Option<u32> {
    contents
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
        .filter(|pid| *pid != 0)
}

/// This process's token in its root's and its databases' names.
fn process_nonce() -> &'static str {
    static NONCE: OnceLock<String> = OnceLock::new();
    NONCE.get_or_init(|| format!("{:08x}", uuid::Uuid::new_v4().as_u128() as u32))
}

/// This process's root, once created: the sweep must never take it.
static OWN_ROOT: OnceLock<PathBuf> = OnceLock::new();

fn create_run_root(dir: &Path, key: &str) -> Result<PathBuf, String> {
    let root = dir.join(format!(
        "{RUN_PREFIX}{}-{}",
        std::process::id(),
        process_nonce()
    ));
    std::fs::create_dir(&root).map_err(|e| {
        format!(
            "could not create this process's root {}: {e}",
            root.display()
        )
    })?;
    let _ = OWN_ROOT.set(root.clone());
    write_owner(&root, &Owner::this_process(key, None))?;
    Ok(root)
}

/// Who owns a per-process root: the process — its pid, and the PID namespace
/// that pid means something in — the template cache it was copied from (so a
/// stale cache in use is not collected), and its postmaster.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Owner {
    pid: u32,
    /// [`own_pid_namespace`] of the owner; `None` off Linux.
    pid_ns: Option<String>,
    template: String,
    postmaster: Option<u32>,
}

impl Owner {
    /// This process, as the owner of a root copied from `template`.
    fn this_process(template: &str, postmaster: Option<u32>) -> Self {
        Self {
            pid: std::process::id(),
            pid_ns: own_pid_namespace().map(str::to_owned),
            template: template.to_string(),
            postmaster,
        }
    }

    fn render(&self) -> String {
        let pid_ns = self
            .pid_ns
            .as_ref()
            .map(|ns| format!("pid_ns={ns}\n"))
            .unwrap_or_default();
        let postmaster = self
            .postmaster
            .map(|pid| format!("postmaster={pid}\n"))
            .unwrap_or_default();
        format!(
            "pid={}\n{pid_ns}template={}\n{postmaster}",
            self.pid, self.template
        )
    }

    fn parse(text: &str) -> Option<Self> {
        let mut fields: HashMap<&str, &str> = HashMap::new();
        for line in text.lines() {
            if let Some((name, value)) = line.split_once('=') {
                fields.insert(name.trim(), value.trim());
            }
        }
        Some(Self {
            pid: fields.get("pid")?.parse().ok()?,
            pid_ns: fields
                .get("pid_ns")
                .filter(|ns| !ns.is_empty())
                .map(|ns| (*ns).to_string()),
            template: (*fields.get("template")?).to_string(),
            postmaster: fields.get("postmaster").and_then(|pid| pid.parse().ok()),
        })
    }
}

/// This process's PID namespace — the target of `/proc/self/ns/pid`, such as
/// `pid:[4026531836]` — on Linux. `None` elsewhere, and where it cannot be read.
///
/// A pid means something only inside its namespace. A container sharing the
/// target directory, or a second WSL distro, runs its tests in another one,
/// where a live process's pid is absent from this namespace's process table or
/// names an unrelated process.
fn own_pid_namespace() -> Option<&'static str> {
    static NAMESPACE: OnceLock<Option<String>> = OnceLock::new();
    NAMESPACE
        .get_or_init(|| {
            if !cfg!(target_os = "linux") {
                return None;
            }
            std::fs::read_link("/proc/self/ns/pid")
                .ok()
                .and_then(|link| link.to_str().map(str::to_owned))
        })
        .as_deref()
}

/// Whether the process that owns `root` may still be running — so the root, and
/// the template cache it was copied from, must be left alone.
///
/// The recorded pid is judged only when the record names the same PID namespace
/// as this process — off Linux, where no process records one, none on both
/// sides. A record from another namespace, or one that names none where this
/// process has one (a Windows process's, read by a WSL process through a
/// shared target directory), cannot be checked from here: its root is presumed
/// live until the record is [`FOREIGN_OWNER_GRACE`] old.
fn owner_may_be_live(root: &Path, owner: &Owner) -> bool {
    if OWN_ROOT.get().is_some_and(|own| own == root) {
        return true;
    }
    if owner.pid_ns.as_deref() != own_pid_namespace() {
        return !modified_before(&root.join(OWNER_FILE), FOREIGN_OWNER_GRACE);
    }
    // A record naming THIS process's pid is a dead predecessor's: this
    // process's own root is matched above.
    owner.pid != std::process::id() && crate::runner_breadcrumb::pid_is_live(owner.pid)
}

fn write_owner(root: &Path, owner: &Owner) -> Result<(), String> {
    let path = root.join(OWNER_FILE);
    std::fs::write(&path, owner.render())
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

fn read_owner(root: &Path) -> Option<Owner> {
    Owner::parse(&std::fs::read_to_string(root.join(OWNER_FILE)).ok()?)
}

fn is_run_root(path: &Path) -> bool {
    path.is_dir()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(RUN_PREFIX))
}

/// Stop and remove every `run-*` root whose owner process is gone
/// ([`owner_may_be_live`]). Returns one line per root it acted on. Never takes
/// this process's own root.
fn sweep_dead_roots(dir: &Path) -> Vec<String> {
    let mut report = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return report;
    };
    for root in entries.flatten().map(|entry| entry.path()) {
        if !is_run_root(&root) || OWN_ROOT.get() == Some(&root) {
            continue;
        }
        let owner = read_owner(&root);
        let dead = match &owner {
            Some(owner) => !owner_may_be_live(&root, owner),
            None => modified_before(&root, OWNERLESS_GRACE),
        };
        if !dead {
            continue;
        }
        let stopped = reap_postmaster(&root, owner.as_ref().and_then(|owner| owner.postmaster));
        let whose = owner.map_or_else(|| "unrecorded".to_string(), |owner| owner.pid.to_string());
        report.push(match remove_dir_all_retrying(&root) {
            Ok(()) => format!(
                "swept the dead root {} (owner pid {whose}; {stopped})",
                root.display()
            ),
            Err(e) => format!(
                "could not sweep the dead root {} (owner pid {whose}; {stopped}): {e}",
                root.display()
            ),
        });
    }
    report
}

/// Remove the template caches other than `keep` that are garbage. Runs under the
/// exclusive cache lock, inside a build, so no process is building or copying
/// one:
///
/// - a cache with no READY is a crashed build's: it goes at once, with any
///   postmaster that build left running in it;
/// - a READY cache goes only when no live root was copied from it AND it has
///   gone unused for [`TEMPLATE_IDLE_LIMIT`] ([`last_used`]). Collecting every
///   cache but the builder's own made two processes whose schemas differ remove
///   each other's cache in turn, and made alternating branches rebuild on every
///   switch.
fn collect_stale_templates(dir: &Path, keep: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    let in_use: Vec<String> = paths
        .iter()
        .filter(|path| is_run_root(path))
        .filter_map(|root| read_owner(root).filter(|owner| owner_may_be_live(root, owner)))
        .map(|owner| owner.template)
        .collect();
    let mut report = Vec::new();
    for path in paths {
        let Some(key) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(TEMPLATE_PREFIX))
        else {
            continue;
        };
        if key == keep || in_use.iter().any(|used| used == key) || !path.is_dir() {
            continue;
        }
        let why = if !path.join(READY_FILE).is_file() {
            "incomplete: a crashed build's".to_string()
        } else {
            match last_used(&path).and_then(|at| at.elapsed().ok()) {
                Some(idle) if idle > TEMPLATE_IDLE_LIMIT => {
                    format!("unused for {} h", idle.as_secs() / 3600)
                }
                // Used within the limit, or its age cannot be read: kept.
                _ => continue,
            }
        };
        let stopped = reap_postmaster(&path, None);
        report.push(match remove_dir_all_retrying(&path) {
            Ok(()) => format!(
                "removed the stale template cache {} ({why}; {stopped})",
                path.display()
            ),
            Err(e) => format!(
                "could not remove the stale template cache {} ({why}): {e}",
                path.display()
            ),
        });
    }
    report
}

/// When a template cache was last entered: its [`LAST_USED_FILE`]'s mtime, or
/// — on a cache no process has marked, which only a harness predating the
/// marker leaves — its READY marker's. `None` when neither can be read.
fn last_used(template_dir: &Path) -> Option<SystemTime> {
    [LAST_USED_FILE, READY_FILE].into_iter().find_map(|marker| {
        std::fs::metadata(template_dir.join(marker))
            .and_then(|meta| meta.modified())
            .ok()
    })
}

fn modified_before(path: &Path, age: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|elapsed| elapsed > age)
}

/// Kill the postmaster still running in `root/pg-data`, if there is one: the pid
/// its `postmaster.pid` names, and `recorded` (the owner record's). A pid is
/// killed only once [`is_postmaster_of`] confirms it is that data directory's
/// postmaster, so a recycled pid is never touched.
fn reap_postmaster(root: &Path, recorded: Option<u32>) -> String {
    let data_dir = root.join(DATA_DIR);
    let mut candidates: Vec<u32> = read_postmaster_pid(&data_dir).into_iter().collect();
    if let Some(pid) = recorded {
        if !candidates.contains(&pid) {
            candidates.push(pid);
        }
    }
    let mut notes = Vec::new();
    for pid in candidates {
        if !crate::runner_breadcrumb::pid_is_live(pid) {
            continue;
        }
        if !is_postmaster_of(pid, &data_dir) {
            notes.push(format!(
                "pid {pid} is alive but is not this root's postmaster, so it was left alone"
            ));
            continue;
        }
        notes.push(if kill_and_wait(pid) {
            format!("killed its postmaster {pid}")
        } else {
            format!("its postmaster {pid} would not die")
        });
    }
    if notes.is_empty() {
        "no postmaster was running".to_string()
    } else {
        notes.join("; ")
    }
}

/// Whether `pid` is a postgres process working in `data_dir` — the postmaster
/// `chdir`s into its data directory and is started with `-D <data_dir>`.
fn is_postmaster_of(pid: u32, data_dir: &Path) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::new()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always),
    );
    let Some(process) = system.process(pid) else {
        return false;
    };
    let named_postgres = process
        .name()
        .to_string_lossy()
        .to_ascii_lowercase()
        .starts_with("postgres");
    named_postgres
        && (process.cwd().is_some_and(|cwd| same_dir(cwd, data_dir))
            || process
                .cmd()
                .iter()
                .any(|arg| same_dir(Path::new(arg), data_dir)))
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Kill `pid` outright and wait for it to go. Only ever applied to a postmaster
/// whose data directory is being thrown away; its workers exit when it does.
fn kill_and_wait(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let target = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&[target]), true);
    if let Some(process) = system.process(target) {
        process.kill();
    }
    wait_for(
        || !crate::runner_breadcrumb::pid_is_live(pid),
        Duration::from_secs(10),
    )
}

fn wait_for(mut done: impl FnMut() -> bool, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `remove_dir_all`, retried for a while: on Windows a postgres process that is
/// exiting can still hold a file open for a moment.
fn remove_dir_all_retrying(path: &Path) -> std::io::Result<()> {
    let deadline = Instant::now() + REMOVE_RETRY_WINDOW;
    loop {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// Copy a directory tree (regular files and directories only — a fresh data
/// directory holds nothing else). Directories are created private (0700 on
/// Unix), which PostgreSQL requires of the data directory itself.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    create_private_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        } else {
            return Err(std::io::Error::other(format!(
                "{} is neither a file nor a directory",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

/// Create `path`, private (0700 on Unix); fails if it exists.
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    private_dir_builder().create(path)
}

/// Create `path` and every parent it lacks, each new one private (0700 on
/// Unix); an existing directory is left as it is, and `path` existing as a file
/// fails, as with `create_dir_all`. The harness directory is created this way:
/// it holds every template cache, and with it each cluster's superuser
/// password (`tpl-*/pg-pass`, which `postgresql_embedded` writes with the
/// default file mode).
fn create_private_dir_all(path: &Path) -> std::io::Result<()> {
    private_dir_builder().recursive(true).create(path)
}

fn private_dir_builder() -> std::fs::DirBuilder {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
}

/// The end of a server log, on one line, for a boot error.
fn log_tail(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let start = bytes.len().saturating_sub(LOG_TAIL_BYTES);
            String::from_utf8_lossy(&bytes[start..])
                .trim()
                .replace(['\r', '\n'], " | ")
        }
        Err(e) => format!("<{} unreadable: {e}>", path.display()),
    }
}

/// One line straight to the stderr HANDLE. libtest captures `eprintln!` from a
/// passing test; it does not capture the handle, so these reach the CI log.
fn say(line: impl AsRef<str>) {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "qontinui test-pg: {}", line.as_ref());
}

// ---------------------------------------------------------------------------
// Std handles (Windows)
// ---------------------------------------------------------------------------

/// Clear the inherit flag on this process's std handles before the harness
/// starts any process. Rust spawns every child with `bInheritHandles = TRUE`, so
/// `pg_ctl` — and through it the postmaster and every worker it starts —
/// inherits each inheritable handle this process holds, including the std
/// handles a CI step hands the test binary: the write end of `| tee`. A
/// postmaster holding that pipe keeps `tee` from seeing EOF, and the step from
/// ending, until the postmaster dies. Rust's own children are unaffected: an
/// inherited stdio is passed to each child as a fresh inheritable duplicate.
#[cfg(windows)]
fn keep_std_handles_out_of_children() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{
        SetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
    };
    let handles = [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ];
    for handle in handles {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            continue;
        }
        // SAFETY: `handle` is one of this process's std handles, valid for the
        // life of the process; clearing its inherit flag changes nothing else.
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
    }
}

/// No-op off Windows: a Unix child gets fresh pipes on fds 1 and 2, and every
/// other descriptor Rust opens is close-on-exec, so the postmaster cannot hold
/// the test process's output.
#[cfg(not(windows))]
fn keep_std_handles_out_of_children() {}

// ---------------------------------------------------------------------------
// Teardown
// ---------------------------------------------------------------------------

/// What this process must undo at exit.
struct Teardown {
    cluster: Option<ClusterTeardown>,
    external: Option<ExternalTeardown>,
    stats: Arc<Stats>,
}

struct ClusterTeardown {
    pg_ctl: PathBuf,
    data_dir: PathBuf,
    root: PathBuf,
    postmaster_pid: Option<u32>,
}

struct ExternalTeardown {
    url: String,
    nonce: String,
}

static TEARDOWN: Mutex<Option<Teardown>> = Mutex::new(None);
static ATEXIT_REGISTERED: std::sync::Once = std::sync::Once::new();

extern "C" {
    /// The C runtime's exit-handler registration (glibc / libSystem on Unix,
    /// the UCRT on Windows MSVC). Its handlers run when `main` returns and on
    /// `std::process::exit` on Unix. On Windows `process::exit` is
    /// `ExitProcess`, and whether the UCRT runs them then is not established
    /// here: the failing-child self-test logs which happened, and the module
    /// doc says what covers either outcome.
    fn atexit(callback: extern "C" fn()) -> std::os::raw::c_int;
}

fn arm_teardown(plan: Teardown) {
    *TEARDOWN.lock().unwrap_or_else(PoisonError::into_inner) = Some(plan);
    ATEXIT_REGISTERED.call_once(|| {
        // SAFETY: `atexit` only records the pointer. The callback is a plain
        // `extern "C" fn` that captures nothing and catches every panic, so it
        // never unwinds into the C runtime.
        let registered = unsafe { atexit(teardown_at_exit) };
        if registered != 0 {
            say(
                "could not register the exit-time teardown (atexit failed); this process's \
                 cluster is left to the next boot's sweep",
            );
        }
    });
}

fn with_teardown(update: impl FnOnce(&mut Teardown)) {
    let mut plan = TEARDOWN.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(plan) = plan.as_mut() {
        update(plan);
    }
}

extern "C" fn teardown_at_exit() {
    let _ = std::panic::catch_unwind(|| {
        let plan = TEARDOWN
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(plan) = plan {
            plan.run();
        }
    });
}

impl Teardown {
    fn run(self) {
        let started = Instant::now();
        let mut done = Vec::new();
        if let Some(cluster) = &self.cluster {
            done.push(cluster.run());
        }
        if let Some(external) = &self.external {
            done.push(external.run());
        }
        say(format!(
            "teardown: {}; {}; {} ms",
            self.stats.summary(),
            done.join("; "),
            started.elapsed().as_millis()
        ));
    }
}

impl ClusterTeardown {
    fn run(&self) -> String {
        let stopped = if self.data_dir.join(POSTMASTER_PID).exists() {
            let mut command = crate::process_helpers::no_window(&self.pg_ctl);
            command
                .arg("stop")
                .arg("--pgdata")
                .arg(&self.data_dir)
                .args([
                    "--mode",
                    "immediate",
                    "--wait",
                    "--timeout",
                    STOP_TIMEOUT_SECS,
                ])
                .arg("--silent")
                .stdin(std::process::Stdio::null());
            // Bounded twice: `pg_ctl --timeout` gives up on a server that will
            // not stop, and the wrapper kills a `pg_ctl` that will not return.
            match crate::process_helpers::run_with_timeout(command, STOP_WAIT) {
                Ok(crate::process_helpers::TimedOutput::Completed(output))
                    if output.status.success() =>
                {
                    "cluster stopped".to_string()
                }
                Ok(crate::process_helpers::TimedOutput::Completed(output)) => format!(
                    "pg_ctl stop exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                Ok(crate::process_helpers::TimedOutput::TimedOut { pid, .. }) => format!(
                    "pg_ctl stop (pid {pid}) overran {} s and was killed",
                    STOP_WAIT.as_secs()
                ),
                Err(e) => format!("pg_ctl stop could not run: {e}"),
            }
        } else {
            "no cluster was running".to_string()
        };
        // A postmaster `pg_ctl` could not stop is killed outright.
        let killed = self
            .postmaster_pid
            .filter(|pid| {
                crate::runner_breadcrumb::pid_is_live(*pid)
                    && is_postmaster_of(*pid, &self.data_dir)
            })
            .map(|pid| {
                if kill_and_wait(pid) {
                    format!("; postmaster {pid} killed")
                } else {
                    format!("; postmaster {pid} would not die")
                }
            })
            .unwrap_or_default();
        let removed = match remove_dir_all_retrying(&self.root) {
            Ok(()) => format!("root {} removed", self.root.display()),
            Err(e) => format!("root {} NOT removed: {e}", self.root.display()),
        };
        format!("{stopped}{killed}; {removed}")
    }
}

impl ExternalTeardown {
    fn run(&self) -> String {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let url = self.url.clone();
        let nonce = self.nonce.clone();
        harness().handle.spawn(async move {
            let _ = sender.send(drop_external_databases(&url, &nonce).await);
        });
        match receiver.recv_timeout(Duration::from_secs(60)) {
            Ok(Ok(dropped)) => {
                format!("dropped this process's {dropped} database(s) on the external server")
            }
            Ok(Err(e)) => {
                format!("could not drop this process's databases on the external server: {e}")
            }
            Err(_) => {
                "timed out dropping this process's databases on the external server".to_string()
            }
        }
    }
}

/// Drop every database this process created on the external server: its
/// template, its derived templates and any clone still alive.
async fn drop_external_databases(url: &str, nonce: &str) -> Result<usize, String> {
    let client = connect(url).await?;
    let rows = client
        .query(
            "SELECT datname FROM pg_database WHERE datname LIKE $1 OR datname LIKE $2",
            &[&format!("qtest\\_{nonce}\\_%"), &format!("qtpl\\_{nonce}%")],
        )
        .await
        .map_err(|e| describe_pg_error(&e))?;
    let mut dropped = 0;
    for row in rows {
        let name: String = row.try_get(0).map_err(|e| describe_pg_error(&e))?;
        let outcome = client
            .batch_execute(&format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"))
            .await;
        if outcome.is_ok() {
            dropped += 1;
        }
    }
    Ok(dropped)
}

#[cfg(test)]
fn embedded_facts() -> Option<EmbeddedFacts> {
    HARNESS.get()?.server.get()?.as_ref().ok()?.embedded.clone()
}

#[cfg(test)]
mod tests {
    //! The harness's self-tests. The database ones run against the real
    //! bundled cluster this module boots; the child-process ones re-run this
    //! test binary with [`child_helper`] selected, to observe what a test
    //! PROCESS leaves behind when it ends.

    use super::*;

    async fn connect_to(database: &TestDatabase) -> tokio_postgres::Client {
        let (client, connection) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
            .await
            .unwrap_or_else(|e| panic!("connect to {}: {e}", database.name()));
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
    }

    async fn query_bool(client: &tokio_postgres::Client, sql: &str) -> bool {
        client
            .query_one(sql, &[])
            .await
            .and_then(|row| row.try_get(0))
            .unwrap_or_else(|e| panic!("{sql}: {}", describe_pg_error(&e)))
    }

    async fn query_count(client: &tokio_postgres::Client, sql: &str) -> i64 {
        client
            .query_one(sql, &[])
            .await
            .and_then(|row| row.try_get(0))
            .unwrap_or_else(|e| panic!("{sql}: {}", describe_pg_error(&e)))
    }

    // ---- against the real cluster ----

    #[tokio::test]
    async fn a_fresh_database_answers_select_1() {
        let database = fresh_database().await;
        let client = connect_to(&database).await;
        let one: i32 = client
            .query_one("SELECT 1", &[])
            .await
            .and_then(|row| row.try_get(0))
            .expect("SELECT 1");
        assert_eq!(one, 1);
    }

    /// Two fresh databases are two databases, and a database cloned after a
    /// sibling was written to starts from the TEMPLATE, not from the sibling.
    #[tokio::test]
    async fn two_fresh_databases_are_isolated() {
        const PROBE: &str = "SELECT to_regclass('public.harness_isolation_probe') IS NOT NULL";
        let first = fresh_database().await;
        let second = fresh_database().await;
        assert_ne!(
            first.name(),
            second.name(),
            "two fresh databases must be two databases"
        );
        let writer = connect_to(&first).await;
        writer
            .batch_execute(
                "CREATE TABLE public.harness_isolation_probe (marker text NOT NULL); \
                 INSERT INTO public.harness_isolation_probe VALUES ('written in the first')",
            )
            .await
            .expect("write into the first fresh database");
        assert!(
            query_bool(&writer, PROBE).await,
            "the probe table must exist where it was created"
        );

        let reader = connect_to(&second).await;
        assert!(
            !query_bool(&reader, PROBE).await,
            "a table created in {} is visible in {}: fresh databases are not isolated",
            first.name(),
            second.name()
        );

        let third = fresh_database().await;
        let late_reader = connect_to(&third).await;
        assert!(
            !query_bool(&late_reader, PROBE).await,
            "{}, cloned after {} was written to, carries its table: clones are not taken \
             from the template",
            third.name(),
            first.name()
        );
    }

    /// The canonical template holds exactly the schema file's tables — so a
    /// transform that dropped a table fails here instead of passing on a short
    /// schema.
    ///
    /// What is compared: ordinary and partitioned tables (`pg_class.relkind`
    /// `r` / `p`) in every non-system schema of a fresh CANONICAL clone, against
    /// the number of lines starting `CREATE TABLE ` in the `include_str!`'d file.
    /// The dump writes one such line per table and creates no table any other
    /// way; pgcrypto (and pgvector, on an external server) create none; and the
    /// canonical template is the one no provisioning step touches, so nothing
    /// beyond the file's own tables can be in it. (The bin's `PgDb` template is
    /// a DERIVED one and adds `PgDb::new`'s tables; it is not this.)
    #[tokio::test]
    async fn the_template_carries_every_table_the_schema_file_creates() {
        let expected = super::super::CANONICAL_SCHEMA
            .lines()
            .filter(|line| line.starts_with("CREATE TABLE "))
            .count();
        assert!(
            expected > 100,
            "the schema file should define the full canonical schema; found only {expected} \
             `CREATE TABLE` lines"
        );
        let database = fresh_database().await;
        let client = connect_to(&database).await;
        let tables = query_count(
            &client,
            "SELECT count(*)::bigint FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind IN ('r', 'p') \
               AND n.nspname NOT IN ('pg_catalog', 'information_schema') \
               AND n.nspname NOT LIKE 'pg\\_toast%' \
               AND n.nspname NOT LIKE 'pg\\_temp\\_%'",
        )
        .await;
        assert_eq!(
            usize::try_from(tables).ok(),
            Some(expected),
            "a fresh canonical database holds {tables} tables but schema.pg.sql.generated \
             creates {expected}: the template is not the full schema"
        );
    }

    #[tokio::test]
    async fn a_fresh_database_carries_the_journey_tables() {
        let database = fresh_database().await;
        let client = connect_to(&database).await;
        for table in [
            "project.journey_edge_observations",
            "project.journey_frontier",
        ] {
            let present: bool = client
                .query_one("SELECT to_regclass($1) IS NOT NULL", &[&table])
                .await
                .and_then(|row| row.try_get(0))
                .unwrap_or_else(|e| panic!("probe {table}: {}", describe_pg_error(&e)));
            assert!(
                present,
                "{table} is missing from the fresh database {}",
                database.name()
            );
        }
    }

    static PROBE_TEMPLATE: DerivedTemplate =
        DerivedTemplate::new("harness_probe", provision_probe_template);

    fn provision_probe_template(url: String) -> ProvisionFuture {
        Box::pin(async move {
            let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
                .await
                .map_err(|e| describe_pg_error(&e))?;
            let driver = tokio::spawn(connection);
            let written = client
                .batch_execute(
                    "CREATE TABLE public.derived_probe (marker text NOT NULL); \
                     INSERT INTO public.derived_probe VALUES ('provisioned once')",
                )
                .await
                .map_err(|e| describe_pg_error(&e));
            drop(client);
            let _ = driver.await;
            written
        })
    }

    /// A derived template carries what its step provisioned — exactly once per
    /// process — and the canonical template does not.
    #[tokio::test]
    async fn a_derived_template_carries_its_provisioning_and_the_canonical_one_does_not() {
        let derived = fresh_database_from(&PROBE_TEMPLATE).await;
        let derived_again = fresh_database_from(&PROBE_TEMPLATE).await;
        let canonical = fresh_database().await;
        for database in [&derived, &derived_again] {
            let client = connect_to(database).await;
            assert_eq!(
                query_count(&client, "SELECT count(*)::bigint FROM public.derived_probe").await,
                1,
                "{} should carry the one provisioned row: the step runs once per process",
                database.name()
            );
        }
        let client = connect_to(&canonical).await;
        assert!(
            !query_bool(
                &client,
                "SELECT to_regclass('public.derived_probe') IS NOT NULL"
            )
            .await,
            "the canonical clone {} carries the derived template's table",
            canonical.name()
        );
    }

    /// The blocking door inside a runtime: the nested-runtime panic the old
    /// `PgDb::new_blocking_for_test` had cannot happen, because the work runs on
    /// the harness's own runtime thread.
    #[tokio::test]
    async fn the_blocking_door_is_safe_inside_a_runtime() {
        let database = fresh_database_blocking();
        let client = connect_to(&database).await;
        assert!(query_bool(&client, "SELECT true").await);
    }

    #[test]
    fn the_blocking_door_serves_a_synchronous_test() {
        let database = fresh_database_blocking();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime for the synchronous test's own client");
        let answer = runtime.block_on(async {
            let client = connect_to(&database).await;
            query_bool(&client, "SELECT true").await
        });
        assert!(answer);
    }

    // ---- child processes: what a test process leaves behind ----

    const CHILD_MODE_ENV: &str = "QONTINUI_TEST_PG_CHILD";
    const CHILD_REPORT_ENV: &str = "QONTINUI_TEST_PG_CHILD_REPORT";
    const CHILD_TIMEOUT: Duration = Duration::from_secs(600);
    /// How long a postmaster may take to disappear after its process ended (on
    /// Windows the job's kill lands asynchronously).
    const DEATH_WAIT: Duration = Duration::from_secs(30);

    /// The libtest name of [`child_helper`] in this binary.
    fn child_test_name() -> String {
        let module = module_path!();
        let module = module.split_once("::").map_or(module, |(_, rest)| rest);
        format!("{module}::child_helper")
    }

    fn render_facts(facts: &EmbeddedFacts) -> String {
        format!(
            "root={}\ndata_dir={}\npostmaster={}\nport={}\n",
            facts.root.display(),
            facts.data_dir.display(),
            facts.postmaster_pid,
            facts.port
        )
    }

    fn parse_facts(text: &str) -> Option<EmbeddedFacts> {
        let field = |name: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
        };
        Some(EmbeddedFacts {
            root: PathBuf::from(field("root")?),
            data_dir: PathBuf::from(field("data_dir")?),
            postmaster_pid: field("postmaster")?.parse().ok()?,
            port: field("port")?.parse().ok()?,
        })
    }

    /// Inert — it passes at once — unless `QONTINUI_TEST_PG_CHILD` selects a
    /// mode. The child-process self-tests below re-run this binary with exactly
    /// this test selected:
    ///
    /// - `pass`: boot, report where the cluster is, and pass.
    /// - `fail`: the same, then fail, so libtest exits through
    ///   `std::process::exit` (on Windows, `ExitProcess`).
    /// - `boot-failure`: ask for a database from a harness that cannot boot.
    #[test]
    fn child_helper() {
        let Ok(mode) = std::env::var(CHILD_MODE_ENV) else {
            return;
        };
        match mode.as_str() {
            "pass" | "fail" => {
                let database = fresh_database_blocking();
                let facts = embedded_facts().expect("the child runs on the bundled cluster");
                let report = std::env::var(CHILD_REPORT_ENV).expect("the report path");
                std::fs::write(report, render_facts(&facts)).expect("write the child report");
                drop(database);
                if mode == "fail" {
                    panic!(
                        "child_helper: failing on purpose after a successful boot, so libtest \
                         exits through process::exit"
                    );
                }
            }
            "boot-failure" => {
                let database = fresh_database_blocking();
                panic!(
                    "child_helper: the harness booted ({}) although {HARNESS_DIR_ENV} names a \
                     regular file",
                    database.name()
                );
            }
            other => panic!("child_helper: unknown {CHILD_MODE_ENV} {other:?}"),
        }
    }

    struct ChildRun {
        status: std::process::ExitStatus,
        /// The child's stdout and stderr, labelled, for assertion messages.
        output: String,
    }

    fn run_child(mode: &str, report: &Path, harness_dir: Option<&Path>) -> ChildRun {
        let exe = std::env::current_exe().expect("this test binary's path");
        let mut command = crate::process_helpers::no_window(&exe);
        command
            .arg(child_test_name())
            .args(["--exact", "--test-threads=1"])
            .env(CHILD_MODE_ENV, mode)
            .env(CHILD_REPORT_ENV, report)
            // These tests are about the bundled cluster's teardown.
            .env_remove(EXTERNAL_URL_ENV)
            .stdin(std::process::Stdio::null());
        if let Some(dir) = harness_dir {
            command.env(HARNESS_DIR_ENV, dir);
        }
        match crate::process_helpers::run_with_timeout(command, CHILD_TIMEOUT) {
            Ok(crate::process_helpers::TimedOutput::Completed(output)) => ChildRun {
                status: output.status,
                output: format!(
                    "--- child stdout ---\n{}\n--- child stderr ---\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ),
            },
            Ok(crate::process_helpers::TimedOutput::TimedOut { pid, .. }) => panic!(
                "the {mode} child (pid {pid}) did not finish within {} s",
                CHILD_TIMEOUT.as_secs()
            ),
            Err(e) => panic!("could not run the {mode} child {}: {e}", exe.display()),
        }
    }

    fn read_child_report(report: &Path, run: &ChildRun) -> EmbeddedFacts {
        let text = std::fs::read_to_string(report)
            .unwrap_or_else(|e| panic!("the child wrote no report ({e}):\n{}", run.output));
        parse_facts(&text)
            .unwrap_or_else(|| panic!("the child's report is malformed: {text:?}\n{}", run.output))
    }

    fn postmaster_alive(facts: &EmbeddedFacts) -> bool {
        crate::runner_breadcrumb::pid_is_live(facts.postmaster_pid)
            && is_postmaster_of(facts.postmaster_pid, &facts.data_dir)
    }

    /// A test process whose tests all pass leaves neither its postmaster nor its
    /// root behind: the `atexit` teardown stops one and removes the other.
    #[test]
    fn a_test_process_that_passes_leaves_no_postmaster_and_no_root() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let report = scratch.path().join("child-report");
        let run = run_child("pass", &report, None);
        assert!(
            run.status.success(),
            "the passing child failed:\n{}",
            run.output
        );
        let facts = read_child_report(&report, &run);
        assert!(
            wait_for(|| !postmaster_alive(&facts), DEATH_WAIT),
            "postmaster {} of the exited child (data dir {}) is still running:\n{}",
            facts.postmaster_pid,
            facts.data_dir.display(),
            run.output
        );
        assert!(
            !facts.root.exists(),
            "the exited child's root {} survived it:\n{}",
            facts.root.display(),
            run.output
        );
        assert!(
            run.output.contains("teardown:"),
            "the child printed no teardown summary:\n{}",
            run.output
        );
    }

    /// A test process with a FAILING test leaves no postmaster behind either,
    /// and any root it leaves is removed by the next boot's sweep.
    ///
    /// libtest ends such a process with `std::process::exit`. On Unix that runs
    /// `atexit`, so the teardown summary must appear. On Windows it is
    /// `ExitProcess`, and whether the UCRT runs `atexit` handlers then is not
    /// established here, so this test only RECORDS it — one line straight to the
    /// stderr handle, which reaches CI's log — and asserts what holds either way:
    /// the postmaster dies and the root does not outlive the sweep.
    #[test]
    fn a_test_process_that_fails_leaves_no_postmaster_and_its_root_is_swept() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let report = scratch.path().join("child-report");
        let run = run_child("fail", &report, None);
        assert!(
            !run.status.success(),
            "the failing child exited successfully:\n{}",
            run.output
        );
        assert!(
            run.output.contains("failing on purpose"),
            "the child failed for some other reason:\n{}",
            run.output
        );
        let tore_down = run.output.contains("teardown:");
        if cfg!(windows) {
            say(format!(
                "failing-child self-test: after libtest's process::exit (ExitProcess) the \
                 child's exit-time teardown summary {}",
                if tore_down {
                    "APPEARED, so the atexit handler ran"
                } else {
                    "did NOT appear, so the atexit handler did not run"
                }
            ));
        } else {
            assert!(
                tore_down,
                "the failing child printed no teardown summary, so atexit did not run on \
                 process::exit:\n{}",
                run.output
            );
        }
        let facts = read_child_report(&report, &run);
        assert!(
            wait_for(|| !postmaster_alive(&facts), DEATH_WAIT),
            "postmaster {} of the failed child (data dir {}) is still running:\n{}",
            facts.postmaster_pid,
            facts.data_dir.display(),
            run.output
        );
        let dir = facts
            .root
            .parent()
            .expect("a root lives in the harness directory");
        let swept = sweep_dead_roots(dir);
        assert!(
            !facts.root.exists(),
            "the failed child's root {} survived the sweep ({swept:?}):\n{}",
            facts.root.display(),
            run.output
        );
    }

    /// A harness that cannot boot FAILS the test that asked it for a database,
    /// with the boot error — it never passes and is never ignored.
    #[test]
    fn a_harness_that_cannot_boot_fails_the_test_with_the_boot_error() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let not_a_directory = scratch.path().join("not-a-directory");
        std::fs::write(
            &not_a_directory,
            b"a regular file where the harness directory goes",
        )
        .expect("plant the regular file");
        let report = scratch.path().join("child-report");
        let run = run_child("boot-failure", &report, Some(&not_a_directory));
        let name = child_test_name();
        assert!(
            !run.status.success(),
            "a child whose harness cannot boot exited successfully:\n{}",
            run.output
        );
        assert!(
            run.output.contains(&format!("test {name} ... FAILED")),
            "the boot failure must FAIL the test:\n{}",
            run.output
        );
        assert!(
            !run.output.contains(&format!("test {name} ... ok"))
                && !run.output.contains(&format!("test {name} ... ignored")),
            "the boot failure must not read as ok or ignored:\n{}",
            run.output
        );
        assert!(
            run.output.contains("could not boot")
                && run.output.contains(&not_a_directory.display().to_string()),
            "the failure must carry the boot error naming the unusable path:\n{}",
            run.output
        );
    }

    // ---- pure ----

    #[test]
    fn postgresql_embedded_version_matches_the_lockfile() {
        let lockfile = include_str!("../../../Cargo.lock");
        let resolved = lockfile
            .split("[[package]]")
            .find_map(|package| {
                let mut lines = package.lines().map(str::trim);
                lines.find(|line| *line == "name = \"postgresql_embedded\"")?;
                lines
                    .find_map(|line| line.strip_prefix("version = \""))?
                    .strip_suffix('"')
            })
            .expect("Cargo.lock resolves postgresql_embedded");
        assert_eq!(
            resolved, POSTGRESQL_EMBEDDED_VERSION,
            "Cargo.lock resolves postgresql_embedded {resolved}: update \
             POSTGRESQL_EMBEDDED_VERSION so the template cache re-keys"
        );
    }

    #[test]
    fn every_cache_key_input_changes_the_key() {
        let key = cache_key("=18.6.0", "CREATE TABLE t ();");
        assert_eq!(key.len(), 16);
        assert_eq!(key, cache_key("=18.6.0", "CREATE TABLE t ();"));
        assert_ne!(key, cache_key("=18.6.1", "CREATE TABLE t ();"));
        assert_ne!(key, cache_key("=18.6.0", "CREATE TABLE u ();"));
    }

    #[test]
    fn owner_records_round_trip() {
        let bare = Owner {
            pid: 4242,
            pid_ns: None,
            template: "0123456789abcdef".to_string(),
            postmaster: None,
        };
        assert_eq!(Owner::parse(&bare.render()), Some(bare.clone()));
        let started = Owner {
            pid_ns: Some("pid:[4026531836]".to_string()),
            postmaster: Some(777),
            ..bare
        };
        assert_eq!(Owner::parse(&started.render()), Some(started));
        assert_eq!(
            Owner::parse(&Owner::this_process("k", None).render()),
            Some(Owner::this_process("k", None)),
        );
        assert_eq!(Owner::parse(""), None);
        assert_eq!(Owner::parse("pid=not-a-pid\ntemplate=x\n"), None);
    }

    #[test]
    fn a_postmaster_pid_file_yields_its_first_line() {
        assert_eq!(
            postmaster_pid_from("31488\n/data\n1755500000\n54812\n\n127.0.0.1\n1 2\nready   \n"),
            Some(31488)
        );
        assert_eq!(postmaster_pid_from(""), None);
        assert_eq!(postmaster_pid_from("0\n"), None);
        assert_eq!(postmaster_pid_from("garbage\n"), None);
    }

    #[test]
    fn database_names_are_plain_identifiers() {
        assert!(is_identifier("qtest_0a1b2c3d_17"));
        assert!(is_identifier("qontinui_template_pgdb"));
        assert!(!is_identifier(""));
        assert!(!is_identifier("1starts_with_a_digit"));
        assert!(!is_identifier("Upper"));
        assert!(!is_identifier("has-dash"));
        assert!(!is_identifier("quote\"d"));
        assert!(!is_identifier(&"x".repeat(64)));
    }

    #[test]
    fn the_server_description_never_carries_the_password() {
        let base = url::Url::parse("postgres://tester:s3cr3t-pw@db.example.test:6543/maint")
            .expect("a URL");
        let described = describe_server(&base);
        assert!(!described.contains("s3cr3t-pw"), "{described}");
        assert_eq!(described, "tester@db.example.test:6543/maint");
        assert_eq!(
            url_with_database(&base, "qtest_x_1"),
            "postgres://tester:s3cr3t-pw@db.example.test:6543/qtest_x_1"
        );
    }

    #[test]
    fn the_cluster_configuration_sets_max_connections_explicitly() {
        let configuration = server_configuration();
        assert_eq!(
            configuration.get("max_connections"),
            Some(&MAX_CONNECTIONS.to_string())
        );
        assert_eq!(
            configuration.get("unix_socket_directories"),
            Some(&String::new())
        );
    }

    #[test]
    fn the_target_dir_is_the_nearest_ancestor_with_cargos_tag() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let target = scratch.path().join("target");
        let deps = target.join("debug").join("deps");
        std::fs::create_dir_all(&deps).expect("deps directory");
        std::fs::write(target.join("CACHEDIR.TAG"), b"Signature").expect("tag");
        let exe = deps.join("qontinui_runner_lib-0123abcd");
        assert_eq!(cargo_target_dir_of(&exe), Some(target.clone()));
        assert_eq!(cargo_target_dir_of(scratch.path()), None);

        // `--target <triple>`: the binary is under `<target>/<triple>/`, which
        // cargo tags as well when it creates it, so the harness lives per
        // triple. Untagged, the triple directory is passed over for `<target>`.
        let triple = target.join("x86_64-unknown-linux-gnu");
        let cross_exe = triple
            .join("debug")
            .join("deps")
            .join("qontinui_runner_lib-0123abcd");
        std::fs::create_dir_all(cross_exe.parent().expect("deps")).expect("cross deps");
        assert_eq!(cargo_target_dir_of(&cross_exe), Some(target.clone()));
        std::fs::write(triple.join("CACHEDIR.TAG"), b"Signature").expect("triple tag");
        assert_eq!(cargo_target_dir_of(&cross_exe), Some(triple));
    }

    /// The harness directory is created private, with its missing parents, and
    /// creating it again changes nothing; a file where it goes is an error.
    #[cfg(unix)]
    #[test]
    fn the_harness_directory_is_created_private() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::tempdir().expect("scratch directory");
        let dir = scratch.path().join("parent").join("qontinui-test-pg");
        create_private_dir_all(&dir).expect("create the harness directory");
        create_private_dir_all(&dir).expect("create it again");
        for created in [dir.as_path(), dir.parent().expect("its parent")] {
            let mode = std::fs::metadata(created)
                .expect("created directory")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o700,
                "{} was created {mode:o}, not private",
                created.display()
            );
        }
        let file = scratch.path().join("a-file");
        std::fs::write(&file, b"not a directory").expect("plant a file");
        assert!(
            create_private_dir_all(&file).is_err(),
            "a file where the harness directory goes must be an error"
        );
    }

    /// An external boot's report names every harness database of another nonce
    /// and none of this process's own. It runs on whichever server the harness
    /// booted: this process's clones carry its nonce on either. On the bundled
    /// cluster nothing else is there, so the planted two are all it may find; an
    /// external server can also hold a concurrent or killed process's, so there
    /// it requires only that the planted are listed and none of this process's.
    #[tokio::test]
    async fn the_leftover_report_names_only_other_nonces_databases() {
        let own = fresh_database().await;
        let client = connect_to(&own).await;
        let nonce = process_nonce();
        let other = if nonce == "0badc0de" {
            "0badc0df"
        } else {
            "0badc0de"
        };
        let planted = [format!("qtest_{other}_1"), format!("qtpl_{other}")];
        for name in &planted {
            // From `template0`, which nothing connects to: a session open on the
            // default `template1` would refuse the copy.
            client
                .batch_execute(&format!("CREATE DATABASE \"{name}\" TEMPLATE template0"))
                .await
                .unwrap_or_else(|e| panic!("plant {name}: {e}"));
        }
        let found = foreign_harness_databases(&client, nonce).await;
        for name in &planted {
            let _ = client
                .batch_execute(&format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"))
                .await;
        }
        let found = found.expect("list the harness databases");
        assert!(
            own.name().starts_with(&format!("qtest_{nonce}_")),
            "this process's own clone {} must carry its nonce for this test to mean anything",
            own.name()
        );
        let (own_template, own_clone) = (format!("qtpl_{nonce}"), format!("qtest_{nonce}_"));
        assert!(
            planted.iter().all(|name| found.contains(name))
                && !found
                    .iter()
                    .any(|name| name.starts_with(&own_template) || name.starts_with(&own_clone)),
            "every planted database of another nonce is foreign and none of this process's \
             own is (its clone {}): {found:?}",
            own.name()
        );
        if own.server.is_embedded() {
            assert_eq!(
                found,
                planted.to_vec(),
                "nothing but the planted databases is foreign on this process's own cluster"
            );
            let line = describe_foreign_databases("the server", &Ok(found))
                .expect("a report when there are leftovers");
            assert!(
                planted.iter().all(|name| line.contains(name.as_str()))
                    && line.starts_with("2 harness database(s) on the server"),
                "the report must count and name them: {line}"
            );
        }
        assert_eq!(
            describe_foreign_databases("the server", &Ok(Vec::new())),
            None
        );
        let many: Vec<String> = (0..FOREIGN_DATABASES_NAMED + 2)
            .map(|i| format!("qtest_{other}_{i}"))
            .collect();
        let line = describe_foreign_databases("the server", &Ok(many.clone()))
            .expect("a report when there are leftovers");
        assert!(
            line.contains(&many[FOREIGN_DATABASES_NAMED - 1])
                && !line.contains(&many[FOREIGN_DATABASES_NAMED])
                && line.ends_with(" and 2 more"),
            "past {FOREIGN_DATABASES_NAMED} names the report counts the rest: {line}"
        );
        let line = describe_foreign_databases("the server", &Err("refused".to_string()))
            .expect("a failed listing is reported, not taken for none");
        assert!(line.contains("refused"), "the failure is named: {line}");
    }

    // ---- the template cache: who may collect it, and when ----

    fn set_mtime(path: &Path, at: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .and_then(|file| file.set_modified(at))
            .unwrap_or_else(|e| panic!("set the mtime of {}: {e}", path.display()));
    }

    /// A live process that is not this one — this test process's parent, the
    /// test runner — to own a root that must count as live.
    fn a_live_pid_not_ours() -> u32 {
        use sysinfo::{Pid, ProcessesToUpdate, System};
        let own = Pid::from_u32(std::process::id());
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::Some(&[own]), true);
        let parent = system
            .process(own)
            .and_then(sysinfo::Process::parent)
            .map(Pid::as_u32)
            .expect("this test process has a parent");
        assert!(
            crate::runner_breadcrumb::pid_is_live(parent),
            "the parent {parent} of this test process is not live"
        );
        parent
    }

    /// The builder of a cache enters it before letting go of the EXCLUSIVE
    /// lock: another key's build collects under that lock, and in a gap between
    /// the build and the entry it would find the new cache with no live root
    /// naming it. So while the builder enters, another descriptor on the lock —
    /// another process's — cannot take even a shared lock. A READY cache is
    /// entered under a SHARED lock, which other readers share. Both entries
    /// mark the cache used, and both leave the lock free.
    #[tokio::test]
    async fn a_built_template_is_entered_before_the_cache_lock_is_released() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let template_dir = scratch.path().join(format!("{TEMPLATE_PREFIX}race"));
        let lock = open_lock(scratch.path()).expect("the cache lock");
        let other = open_lock(scratch.path()).expect("another descriptor on the cache lock");
        let reader_shares_the_lock = || match other.try_lock_shared() {
            Ok(()) => {
                let _ = other.unlock();
                true
            }
            Err(_) => false,
        };
        let building: &Path = &template_dir;

        let (shared, built) = enter_or_build(
            &lock,
            building,
            move || async move {
                std::fs::create_dir(building).map_err(|e| e.to_string())?;
                std::fs::write(building.join(READY_FILE), "built by the test\n")
                    .map_err(|e| e.to_string())?;
                Ok::<_, String>(Duration::from_millis(1))
            },
            || Ok(reader_shares_the_lock()),
        )
        .await
        .expect("build and enter the missing cache");
        assert!(
            built.is_some(),
            "the cache was missing, so this call built it"
        );
        assert!(
            !shared,
            "the builder entered its cache after letting go of the exclusive lock: another \
             key's build could have collected the cache in between"
        );
        assert!(
            template_dir.join(LAST_USED_FILE).is_file(),
            "entering the cache it built did not mark it used"
        );

        std::fs::remove_file(template_dir.join(LAST_USED_FILE)).expect("clear the marker");
        let (shared, built) = enter_or_build(
            &lock,
            &template_dir,
            || async { Err::<Duration, _>("a READY cache is never rebuilt".to_string()) },
            || Ok(reader_shares_the_lock()),
        )
        .await
        .expect("enter the READY cache");
        assert!(built.is_none(), "a READY cache was rebuilt");
        assert!(
            shared,
            "a READY cache must be entered under the SHARED lock, which readers share"
        );
        assert!(
            template_dir.join(LAST_USED_FILE).is_file(),
            "entering a READY cache did not mark it used"
        );
        assert!(
            other.try_lock().is_ok(),
            "enter_or_build left the cache lock held"
        );
        let _ = other.unlock();
    }

    /// Caches of two keys coexist: a build collects another key's cache only
    /// when it is a crashed build's, or when no live root was copied from it
    /// AND it has gone unused for a day. Collecting every cache but the
    /// builder's own let two processes whose schemas differ remove each other's
    /// cache in turn, and made alternating branches rebuild on every switch.
    #[test]
    fn a_build_collects_only_another_keys_crashed_or_long_unused_cache() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let dir = scratch.path();
        let long_ago = SystemTime::now() - TEMPLATE_IDLE_LIMIT - Duration::from_secs(3600);
        let cache = |key: &str, ready: bool, used: Option<SystemTime>| {
            let path = dir.join(format!("{TEMPLATE_PREFIX}{key}"));
            std::fs::create_dir(&path).expect("plant a cache");
            if ready {
                std::fs::write(path.join(READY_FILE), "planted\n").expect("plant READY");
                set_mtime(&path.join(READY_FILE), long_ago);
            }
            if let Some(at) = used {
                std::fs::write(path.join(LAST_USED_FILE), "planted\n").expect("plant the marker");
                set_mtime(&path.join(LAST_USED_FILE), at);
            }
            path
        };
        let recent = cache("recent", true, Some(SystemTime::now()));
        let in_use = cache("in-use", true, Some(long_ago));
        let idle = cache("idle", true, Some(long_ago));
        let never_marked = cache("never-marked", true, None);
        let crashed = cache("crashed", false, None);
        let root = dir.join(format!("{RUN_PREFIX}live"));
        std::fs::create_dir(&root).expect("plant a live process's root");
        write_owner(
            &root,
            &Owner {
                pid: a_live_pid_not_ours(),
                ..Owner::this_process("in-use", None)
            },
        )
        .expect("record its owner");

        let report = collect_stale_templates(dir, "building");

        for (kept, why) in [
            (&recent, "it was used within the day"),
            (&in_use, "a live root was copied from it"),
        ] {
            assert!(
                kept.is_dir(),
                "{} was collected although {why}: {report:#?}",
                kept.display()
            );
        }
        for (gone, why) in [
            (&idle, "it went unused for over a day"),
            (
                &never_marked,
                "it was READY over a day ago and never marked used",
            ),
            (&crashed, "it is a crashed build's"),
        ] {
            assert!(
                !gone.exists(),
                "{} survived although {why}: {report:#?}",
                gone.display()
            );
        }
        assert_eq!(report.len(), 3, "one line per removed cache: {report:#?}");
        assert!(root.is_dir(), "a template collection removed a root");
    }

    /// A pid means something only in the PID namespace that recorded it. A root
    /// whose owner recorded ANOTHER namespace — a container sharing the target
    /// directory, a second WSL distro — is neither swept as dead nor stops
    /// keeping its cache, however dead its pid looks from here, until its record
    /// is [`FOREIGN_OWNER_GRACE`] old. A record that names no namespace counts as
    /// another one wherever this process has one.
    ///
    /// Every record here names THIS process's pid, which in this process's own
    /// namespace marks a dead predecessor's root (its own root is never in a
    /// scratch directory) — which is exactly how a live process in another
    /// namespace can look from this one.
    #[test]
    fn a_root_owned_from_another_pid_namespace_is_presumed_live() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let dir = scratch.path();
        let here = own_pid_namespace().map(str::to_owned);
        let elsewhere = Some("pid:[0]".to_string());
        assert_ne!(here, elsewhere, "pid:[0] is never a real namespace");
        let now = SystemTime::now();
        let past_grace = now - FOREIGN_OWNER_GRACE - Duration::from_secs(3600);
        let unused_long_ago = now - TEMPLATE_IDLE_LIMIT - Duration::from_secs(3600);
        // A root named `name`, owned in `pid_ns` and recorded at `recorded`, and
        // the idle cache it was copied from, which only a live root keeps.
        let plant = |name: &str, pid_ns: &Option<String>, recorded: SystemTime| {
            let root = dir.join(format!("{RUN_PREFIX}{name}"));
            std::fs::create_dir(&root).expect("plant a root");
            let owner = Owner {
                pid_ns: pid_ns.clone(),
                ..Owner::this_process(name, None)
            };
            write_owner(&root, &owner).expect("record its owner");
            set_mtime(&root.join(OWNER_FILE), recorded);
            let cache = dir.join(format!("{TEMPLATE_PREFIX}{name}"));
            std::fs::create_dir(&cache).expect("plant its cache");
            for marker in [READY_FILE, LAST_USED_FILE] {
                std::fs::write(cache.join(marker), "planted\n").expect("plant a marker");
                set_mtime(&cache.join(marker), unused_long_ago);
            }
            (root, cache)
        };
        let same = plant("same", &here, now);
        let foreign = plant("foreign", &elsewhere, now);
        let foreign_long_ago = plant("foreign-long-ago", &elsewhere, past_grace);
        let unnamed = plant("unnamed", &None, now);
        let unnamed_is_foreign = here.is_some();

        let collected = collect_stale_templates(dir, "building");
        let swept = sweep_dead_roots(dir);

        let outcome = format!("collected {collected:#?}\nswept {swept:#?}");
        for ((root, cache), live, why) in [
            (&same, false, "its pid is this process's, in this namespace"),
            (&foreign, true, "its owner recorded another PID namespace"),
            (
                &foreign_long_ago,
                false,
                "its other-namespace record is older than FOREIGN_OWNER_GRACE",
            ),
            (
                &unnamed,
                unnamed_is_foreign,
                "its record names no PID namespace",
            ),
        ] {
            for (path, what) in [(root, "root"), (cache, "cache")] {
                assert_eq!(
                    path.exists(),
                    live,
                    "the {what} {} must {} because {why}\n{outcome}",
                    path.display(),
                    if live { "survive" } else { "go" }
                );
            }
        }
    }
}
