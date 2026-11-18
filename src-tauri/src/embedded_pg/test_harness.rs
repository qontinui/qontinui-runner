//! The PostgreSQL the runner's Postgres-backed tests run against: ONE bundled
//! cluster per test PROCESS, booted from a cached ready data directory, with a
//! FRESH database per test (plan
//! `2026-11-09-runner-postgres-backed-tests-never-run-so-hand-verified-sql-is-unretained`,
//! Phase 1).
//!
//! # Why
//!
//! Until this existed, the runner's `DATABASE_URL`-backed tests were
//! `#[ignore]`d, or compiled only under the `pg_integration_tests` feature, most
//! behind a silent localhost fallback, so no CI lane ran them and SQL verified
//! by hand was retained nowhere. The harness boots the
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
//! - **Directory** (`<cargo target dir>/qontinui-test-pg/`, or
//!   [`HARNESS_DIR_ENV`]'s): created private and marked as the harness's
//!   ([`MADE_MARKER`]), like every cache in it. On Unix every boot refuses it
//!   unless it is private to the tester ([`require_private`]), and one
//!   [`HARNESS_DIR_ENV`] names unless no other user could swap the tree above
//!   it ([`require_safe_ancestors`]). Windows is not checked.
//! - **Cache** (`<cargo target dir>/qontinui-test-pg/tpl-<key>/`): the
//!   extracted bundled install plus a cleanly shut-down data directory whose
//!   `qontinui_template` database holds the transformed schema. The key hashes
//!   the bundled PostgreSQL release, the `postgresql_embedded` release, the
//!   rendered schema SQL, the OS and architecture it was built for, and
//!   [`HARNESS_VERSION`]. Built under an exclusive lock
//!   on `.lock`, and entered by its builder before that lock is released
//!   ([`enter_or_build`]); entered by every other process under a shared one,
//!   which keeps a collection from removing it mid-copy. Whoever takes the
//!   lock names itself in `.lock.holder` ([`LockHolder`]), so a process
//!   waiting for it says who holds it ([`take_cache_lock`]). Every entry marks
//!   it used. Worktrees on branches whose schemas differ share one target
//!   directory, so caches of several keys coexist: a build collects another
//!   key's cache only once no live root was copied from it and it has gone
//!   unused for [`TEMPLATE_IDLE_LIMIT`].
//! - **Process** (`run-<pid>-<nonce>/`): a copy of the cached data directory,
//!   started on a free loopback port. No live cluster is shared between
//!   processes, so only its owner stops it — unless it outlives that owner, when
//!   the next boot's sweep ends it.
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
//! | every test passed (`main` returns) | `atexit` ends the postmaster ([`end_postmaster`]), then removes the root | the same |
//! | a test failed (libtest calls `process::exit`) | the same `atexit` | `process::exit` is `ExitProcess`, and whether the C runtime runs `atexit` handlers then is not established here (the failing-child self-test logs which happened): the postmaster is ended by `atexit` if it runs, else by the kill-on-close job, else — where the job could not be attached — by the next boot's sweep; a root `atexit` did not remove waits for the next sweep |
//! | killed or crashed | the postmaster is orphaned; the next boot's sweep ends it ([`end_postmaster`]) and removes the root | the job kills the postmaster — or, where it could not be attached, the next boot's sweep ends it — and the next sweep removes the root |
//! | any of these, on an external server ([`EXTERNAL_URL_ENV`]) | `atexit`, where it runs, drops this process's `qtpl_<nonce>*` and `qtest_<nonce>_*` databases; a killed or crashed process leaves them, and a later boot only reports them | the same, and a failed test process leaves them too if `atexit` did not run |
//!
//! The sweep runs at every boot: a `run-*` root whose owner is gone
//! ([`owner_may_be_live`]) has its postmaster ended, and is removed. Ending a
//! postmaster ([`end_postmaster`]) acts only on a process proven to work in
//! that data directory, and what keeps a pid recycled after the proof from
//! being hit depends on the OS:
//!
//! - **Linux**: the process is held by a pidfd from before the proof until it
//!   is gone. It is sent SIGQUIT — what `pg_ctl stop -m immediate` sends —
//!   through the pidfd, which lets it remove its System V and dynamic shared
//!   memory segments as it exits, and SIGKILL only if it has not exited within
//!   30 s (`QUIT_WAIT`). A signal through a pidfd reaches the process the pidfd
//!   was opened on or none, never a later holder of its pid.
//! - **Windows**: one handle is opened before the proof, the proof is tied to
//!   that handle's process (by its creation time, or its image), and the
//!   process is terminated and waited for through the same handle.
//! - **macOS and other Unix, and Linux without pidfds (before 5.3)**: the proof
//!   and the action each name the pid. `pg_ctl stop -m immediate` is used when
//!   `postmaster.pid` names the proven pid (`pg_ctl` signals whatever that
//!   file names), and a kill by pid follows a second proof, so a pid recycled
//!   between a proof and the signal after it can still be hit there.
//!
//! A pid is judged only in the PID namespace that recorded it: a root whose
//! owner recorded another one (a container sharing the target directory, a
//! second WSL distro) is presumed live until its record is
//! [`FOREIGN_OWNER_GRACE`] old, because its pid names nothing here.
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
/// roomier volume). The harness runs PostgreSQL out of that directory and keeps
/// each cluster's superuser password in it, so on Unix the boot fails on a
/// directory that another user owns or that anyone else can reach
/// ([`require_private`]), and on one with an ancestor another user could swap
/// the tree through ([`require_safe_ancestors`]). The harness changes the mode
/// only of a directory it made itself ([`MADE_MARKER`]): an existing directory
/// named here is never marked or chmodded, and is refused unless it is already
/// private. Name a directory that does not exist yet, such as
/// `/dev/shm/<you>/qontinui-test-pg`, and the harness creates it, with any
/// parent it lacks, private. Windows is not checked. It is also the
/// boot-failure self-test's hook: pointed at a regular file, the harness cannot
/// boot and must fail every test that asks it for a database.
pub const HARNESS_DIR_ENV: &str = "QONTINUI_TEST_PG_DIR";

/// Runs every harness database on this EXTERNAL server instead of the bundled
/// cluster: a `postgres://` or `postgresql://` URL whose role may create
/// databases, on PostgreSQL 13 or newer (`DROP DATABASE … WITH (FORCE)`),
/// reachable without TLS. The role must also be able to create the extensions
/// the canonical schema creates for itself — `pgcrypto`, and `vector` where the
/// server offers it: the harness's template is a copy of `template0`, so an
/// extension an administrator pre-installed in `template1` is not there, and
/// the schema's `CREATE EXTENSION` runs for real. A name nothing else exports,
/// on purpose: no harness-backed test reads `DATABASE_URL`, which is set on a
/// developer box and in DB-gated CI.
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
///
/// `2`: a cache is created private (0700 on Unix), so a `1` cache, created with
/// the default mode around its superuser password, is never entered again.
///
/// `3`: a cache is marked as the harness's ([`MADE_MARKER`]), which is what lets
/// entering it make it private again; a `2` cache has no mark, and opened up
/// later it would only be refused.
const HARNESS_VERSION: &str = "3";

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
/// How long a `pg_ctl stop -m immediate` ([`stop_immediately`]) waits for the
/// server, where a postmaster is ended that way ([`end_postmaster`]).
#[cfg(unix)]
const STOP_TIMEOUT_SECS: &str = "30";
/// The hard bound on that `pg_ctl stop` itself: its own `--timeout` plus margin.
#[cfg(unix)]
const STOP_WAIT: Duration = Duration::from_secs(45);
/// How long a postmaster sent SIGQUIT through a pidfd — what `pg_ctl stop -m
/// immediate` sends — may take to exit before it is killed: the same bound
/// `pg_ctl` would give it ([`STOP_TIMEOUT_SECS`]).
#[cfg(target_os = "linux")]
const QUIT_WAIT: Duration = Duration::from_secs(30);
/// How long a postmaster may take to be gone once it was killed or terminated,
/// or once `pg_ctl` reported it stopped.
const POSTMASTER_EXIT_WAIT: Duration = Duration::from_secs(10);
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
/// The file the harness writes into each directory it creates and may later
/// make private again: the harness directory and every template cache, and an
/// existing harness directory at the DEFAULT location it finds already private
/// to this user ([`prepare_harness_dir`]); never a directory [`HARNESS_DIR_ENV`]
/// names that the harness did not make. The harness changes the mode of no directory that
/// lacks it, and counts as the mark only a regular file this user owns
/// ([`require_private`]).
const MADE_MARKER: &str = ".made-by-qontinui-test-pg";
/// The file beside the cache lock naming the process that took the lock last
/// ([`LockHolder`]), so a process waiting for the lock can say who holds it.
const LOCK_HOLDER_FILE: &str = ".lock.holder";
/// How old a partial holder file whose text is not a record must be before it
/// is taken for a dead writer's ([`sweep_holder_partials`]): a write and a
/// rename take milliseconds.
const HOLDER_PARTIAL_GRACE: Duration = Duration::from_secs(60);
/// How long a process waits for the template cache lock, and how it says so.
/// A builder holds the lock exclusively for a whole build, whose `initdb` has
/// no bound at all ([`COMMAND_TIMEOUT`]), so a build that never ends would
/// otherwise stall every process sharing the directory, silently and for ever.
/// A line on the stderr handle after 30 s and every 60 s after, each naming
/// the holder; and a boot error once ONE holder has held the lock for 10 min
/// while this process watched — the clock restarts whenever the holder
/// changes, so a queue of healthy builds of other keys is waited out however
/// long it is ([`take_cache_lock`]). 10 min is twice [`COMMAND_TIMEOUT`], the
/// bound meant for one `initdb` or `pg_ctl`, so a slow build is waited out;
/// and it is inside CI's 20-minute `Run Rust tests` step timeout, so a process
/// waiting behind a lock held that long fails its boot with this error. The
/// holder itself is not bounded by it: a process whose own build never ends
/// still ends only with the step.
const LOCK_PATIENCE: LockPatience = LockPatience {
    first_report: Duration::from_secs(30),
    report_every: Duration::from_secs(60),
    give_up: Duration::from_secs(600),
    poll: Duration::from_millis(250),
};

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// A database created for one test. Its URL addresses it as the server's
/// superuser (the bundled cluster) or as the opted-in role (an external server).
///
/// Dropping it drops the database — in the background, `WITH (FORCE)`, so a
/// connection still open on it cannot keep it alive. A database still alive at
/// exit goes with the bundled cluster, whose root the exit-time teardown
/// removes. On an external server ([`EXTERNAL_URL_ENV`]) that teardown drops
/// it instead, where `atexit` runs; a killed process leaves it there, and a
/// later boot only reports it.
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
        // A step that panics has failed like any other: its panic is caught
        // here, so the template is still sealed and the failure recorded, and
        // every later request fails with the panic's own message — not by
        // creating the template again and failing on the one left behind.
        let provisioned = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
            (template.provision)(self.url_for(&database)),
        ))
        .await
        .unwrap_or_else(|panic| {
            Err(format!(
                "the step panicked: {}",
                panic_message(panic.as_ref())
            ))
        });
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
    let HarnessDir { path: dir, named } = harness_dir()?;
    prepare_harness_dir(&dir, named)?;
    for line in sweep_dead_roots(&dir) {
        say(line);
    }

    let defaults = bundled_defaults();
    let version = defaults.version.to_string();
    let key = cache_key(
        PLATFORM,
        &version,
        &canonical_schema_sql(VectorColumns::Bytea),
    );
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
/// Either way the cache is entered under the same lock through
/// [`enter_private`]: required private, marked used, then entered. Each lock is
/// waited for audibly and for a bounded time ([`take_cache_lock`],
/// [`LOCK_PATIENCE`]).
async fn enter_or_build<T, Built>(
    cache_lock: &std::fs::File,
    template_dir: &Path,
    build: impl FnOnce() -> Built,
    enter: impl FnOnce() -> Result<T, String>,
) -> Result<(T, Option<Duration>), String>
where
    Built: Future<Output = Result<Duration, String>>,
{
    take_cache_lock(cache_lock, false, template_dir, LOCK_PATIENCE, say).await?;
    if template_dir.join(READY_FILE).is_file() {
        let entered = enter_private(template_dir, enter);
        let _ = cache_lock.unlock();
        return entered.map(|entered| (entered, None));
    }
    let _ = cache_lock.unlock();
    take_cache_lock(cache_lock, true, template_dir, LOCK_PATIENCE, say).await?;
    let outcome = async move {
        // Another process may have built it while this one waited.
        let built = if template_dir.join(READY_FILE).is_file() {
            let (dir, key) = lock_site(template_dir);
            write_lock_holder(&dir, &key, "entering");
            None
        } else {
            Some(build().await?)
        };
        enter_private(template_dir, enter).map(|entered| (entered, built))
    }
    .await;
    let _ = cache_lock.unlock();
    outcome
}

/// How long, and how audibly, [`take_cache_lock`] waits ([`LOCK_PATIENCE`]).
#[derive(Clone, Copy, Debug)]
struct LockPatience {
    /// The first "still waiting" line comes after this long.
    first_report: Duration,
    /// Then one comes every this long.
    report_every: Duration,
    /// The wait fails the boot once one holder has held the lock this long.
    give_up: Duration,
    /// How often the lock is tried again.
    poll: Duration,
}

/// Take the template cache lock — shared, or exclusive when `exclusive` — to
/// enter or build the cache in `template_dir`, then name this process as its
/// holder ([`write_lock_holder`]: `entering` for a shared lock, `building` for
/// an exclusive one).
///
/// While the lock is held elsewhere it says so through `report`, first after
/// `patience.first_report` and then every `patience.report_every`, each line
/// naming the holder [`LOCK_HOLDER_FILE`] names. Past `patience.give_up` it
/// gives up with an error naming the cache and that holder — but only once ONE
/// holder has held the lock that long while this process watched: the clock
/// restarts whenever the named holder changes, so a queue of healthy holders,
/// each well inside the bound, is waited out however long it is. A holder the
/// file does not name — no file, an unreadable one, or one naming a process
/// that is no longer live ([`LockHolder::is_live`]), such as a holder from
/// before the file existed — is timed as one unnamed holder. Liveness is
/// judged whenever the file's text changes and again at every progress line,
/// so a named holder that dies while another process keeps the lock (a second
/// shared holder, or one that never named itself) becomes unnamed by the next
/// line. Timed with `tokio::time`, so a test can run the wait on paused time.
///
/// Once it has the lock it removes the partial holder files that processes no
/// longer live left behind ([`sweep_holder_partials`]), then names itself.
async fn take_cache_lock(
    lock: &std::fs::File,
    exclusive: bool,
    template_dir: &Path,
    patience: LockPatience,
    mut report: impl FnMut(String),
) -> Result<(), String> {
    let kind = if exclusive { "exclusive" } else { "shared" };
    let (dir, key) = lock_site(template_dir);
    let started = tokio::time::Instant::now();
    let mut next_report = patience.first_report;
    let mut watched: Option<LockHolder> = None;
    let mut watched_since = started;
    let mut last_read: Option<LockHolder> = None;
    let mut last_live = false;
    loop {
        let attempt = if exclusive {
            lock.try_lock()
        } else {
            lock.try_lock_shared()
        };
        match attempt {
            Ok(()) => {
                for line in sweep_holder_partials(&dir) {
                    report(line);
                }
                write_lock_holder(&dir, &key, if exclusive { "building" } else { "entering" });
                return Ok(());
            }
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(format!(
                    "could not take the {kind} cache lock for {}: {e}",
                    template_dir.display()
                ));
            }
        }
        let now = tokio::time::Instant::now();
        let waited = now - started;
        let report_due = waited >= next_report;
        // Liveness is judged when the file's text changes and at every progress
        // line, not on every poll: the file names only the last process to take
        // the lock, which may die while another keeps it.
        let read = read_lock_holder(&dir);
        if read != last_read || report_due {
            last_live = read.as_ref().is_some_and(LockHolder::is_live);
            last_read = read;
        }
        let holder = last_read.clone().filter(|_| last_live);
        let same = match (&holder, &watched) {
            (Some(holder), Some(watched)) => holder.is_same_process(watched),
            (None, None) => true,
            _ => false,
        };
        if !same {
            watched_since = now;
        }
        watched = holder;
        let held = now - watched_since;
        let named = watched.as_ref().map_or_else(
            || "a process that has not named itself".to_string(),
            LockHolder::describe,
        );
        if held >= patience.give_up {
            return Err(format!(
                "gave up waiting for the {kind} cache lock for {}: it has been held by {named} \
                 for {} s (this process waited {} s in all), and this boot fails rather than \
                 wait without a bound",
                template_dir.display(),
                held.as_secs(),
                (now - started).as_secs()
            ));
        }
        if report_due {
            report(format!(
                "still waiting, after {} s, for the {kind} cache lock for {}: held by {named} \
                 for {} s; this boot gives up once one holder has held it for {} s",
                waited.as_secs(),
                template_dir.display(),
                held.as_secs(),
                patience.give_up.as_secs()
            ));
            next_report += patience.report_every;
        }
        tokio::time::sleep(patience.poll.min(patience.give_up.saturating_sub(held))).await;
    }
}

/// The directory holding the cache lock for the template cache in
/// `template_dir`, and that cache's key: what [`write_lock_holder`] records.
fn lock_site(template_dir: &Path) -> (PathBuf, String) {
    let dir = template_dir
        .parent()
        .map_or_else(PathBuf::new, Path::to_path_buf);
    let name = template_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let key = name
        .strip_prefix(TEMPLATE_PREFIX)
        .map_or_else(|| name.clone(), str::to_owned);
    (dir, key)
}

/// The process that took the template cache lock last, as it recorded itself
/// in [`LOCK_HOLDER_FILE`] once it had the lock: what a waiting process names.
///
/// One lock serves every cache key, and a SHARED lock has many holders at once,
/// of whom the file names only the last to take it; one that has let go may
/// be named while an earlier one still holds it, and a process that has just
/// taken the lock is named only once it has written the file. An EXCLUSIVE
/// holder is named for as long as it holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LockHolder {
    pid: u32,
    /// [`own_pid_namespace`] of the holder; `None` off Linux.
    pid_ns: Option<String>,
    /// [`process_start`] of the holder, which tells it from a later process
    /// given the same pid; `None` where none is read.
    started: Option<u64>,
    /// The cache key it is working on.
    key: String,
    /// `entering`, `building` or `collecting`.
    action: String,
}

impl LockHolder {
    fn this_process(key: &str, action: &str) -> Self {
        let pid = std::process::id();
        Self {
            pid,
            pid_ns: own_pid_namespace().map(str::to_owned),
            started: process_start(pid),
            key: key.to_string(),
            action: action.to_string(),
        }
    }

    fn render(&self) -> String {
        format!(
            "pid={}\npid_ns={}\nstarted={}\nkey={}\naction={}\n",
            self.pid,
            self.pid_ns.as_deref().unwrap_or_default(),
            self.started.map(|at| at.to_string()).unwrap_or_default(),
            self.key,
            self.action
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
            started: fields.get("started").and_then(|at| at.parse().ok()),
            key: (*fields.get("key")?).to_string(),
            action: (*fields.get("action")?).to_string(),
        })
    }

    /// Whether the recorded process may still be running: a holder in another
    /// PID namespace cannot be checked from here and counts as live, however
    /// old its record; otherwise its pid must be live and, where both starts
    /// are read, have started when the holder did — this process's own pid
    /// included, since a record of this process with its own start IS this
    /// process. Unlike [`owner_may_be_live`] it neither ages a foreign record
    /// out nor counts this process's pid as a dead predecessor's: what it
    /// decides is only whom a waiting process names and times, and which
    /// partial holder files are swept, so it errs towards live, which keeps a
    /// file and costs at most a name.
    fn is_live(&self) -> bool {
        if self.pid_ns.as_deref() != own_pid_namespace() {
            return true;
        }
        crate::runner_breadcrumb::pid_is_live(self.pid)
            && match (self.started, process_start(self.pid)) {
                (Some(recorded), Some(now)) => recorded == now,
                _ => true,
            }
    }

    /// Whether `other` records the same process, whatever it was doing.
    fn is_same_process(&self, other: &Self) -> bool {
        self.pid == other.pid && self.pid_ns == other.pid_ns && self.started == other.started
    }

    /// `pid N (building KEY)`.
    fn describe(&self) -> String {
        format!("pid {} ({} {})", self.pid, self.action, self.key)
    }
}

/// Record this process as the cache lock's holder in `dir`, working on the
/// cache `key` (`action`: `entering`, `building` or `collecting`). Written to
/// a file of this process's own and renamed into place, so a waiter never
/// reads half of one. Best effort: a holder that cannot name itself still
/// holds the lock, so it removes the record left by whoever took the lock
/// before — which would otherwise name that process, possibly still live, as
/// the holder — and a waiter then names this one as unnamed. Removing it is
/// sound because this process holds the lock: at worst it also removes the
/// record a concurrent SHARED taker just wrote, which costs that taker its
/// name in a waiter's line, nothing more.
fn write_lock_holder(dir: &Path, key: &str, action: &str) {
    let path = dir.join(LOCK_HOLDER_FILE);
    let partial = dir.join(format!(
        "{LOCK_HOLDER_FILE}.{}-{}",
        std::process::id(),
        process_nonce()
    ));
    let written = std::fs::write(&partial, LockHolder::this_process(key, action).render())
        .and_then(|()| std::fs::rename(&partial, &path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&partial);
        let stale = match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                format!("; the previous holder's record could not be removed either: {e}")
            }
            _ => String::new(),
        };
        say(format!(
            "could not name this process as the cache lock's holder ({}): {e}{stale}",
            path.display()
        ));
    }
}

fn read_lock_holder(dir: &Path) -> Option<LockHolder> {
    LockHolder::parse(&std::fs::read_to_string(dir.join(LOCK_HOLDER_FILE)).ok()?)
}

/// Remove the partial holder files (`.lock.holder.<pid>-<nonce>`,
/// [`write_lock_holder`]) in `dir` that a process no longer live left behind,
/// by dying between the write and the rename. Run under the cache lock, which
/// its caller has just taken. One line per removal or failure.
///
/// - A file whose text reads as a record of the pid in its name is judged as a
///   waiter judges a named holder ([`LockHolder::is_live`]): removed only when
///   that process is gone — a record from another PID namespace counts as live
///   and is kept.
/// - A file whose text does not read as a record — empty, or cut short by a
///   death or by a writer still writing — says nothing of its namespace, so
///   its pid cannot be judged from here alone: it is removed only once it is
///   older than [`HOLDER_PARTIAL_GRACE`], far longer than any write and
///   rename take, AND its pid is not live in this namespace. A writer in
///   another namespace mid-write is protected by the grace, not by its pid.
fn sweep_holder_partials(dir: &Path) -> Vec<String> {
    let prefix = format!("{LOCK_HOLDER_FILE}.");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.strip_prefix(&prefix))
            .and_then(|rest| rest.split_once('-'))
            .and_then(|(pid, _)| pid.parse::<u32>().ok())
        else {
            continue;
        };
        let path = entry.path();
        let keep = match std::fs::read_to_string(&path)
            .ok()
            .as_deref()
            .and_then(LockHolder::parse)
            .filter(|holder| holder.pid == pid)
        {
            Some(holder) => holder.is_live(),
            None => {
                !modified_before(&path, HOLDER_PARTIAL_GRACE)
                    || crate::runner_breadcrumb::pid_is_live(pid)
            }
        };
        if keep {
            continue;
        }
        lines.extend(partial_removal_line(
            &path,
            pid,
            std::fs::remove_file(&path),
        ));
    }
    lines
}

/// What [`sweep_holder_partials`] says of removing the partial holder file
/// `path` that pid `pid` left, given how the removal went. A `NotFound` says
/// nothing: the sweep also runs under a SHARED lock, so another process
/// entering a cache at the same moment may have removed the same file first,
/// and it is gone either way.
fn partial_removal_line(path: &Path, pid: u32, removed: std::io::Result<()>) -> Option<String> {
    match removed {
        Ok(()) => Some(format!(
            "removed {}, a partial cache-lock holder file pid {pid} left when it died",
            path.display()
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(format!(
            "could not remove {}, a partial cache-lock holder file pid {pid} left: {e}",
            path.display()
        )),
    }
}

/// Enter the template cache in `template_dir` with `enter` once it is proven
/// private ([`require_private`]): it holds the binaries the cluster runs and
/// its superuser password. Marks it used ([`LAST_USED_FILE`]) first.
fn enter_private<T>(
    template_dir: &Path,
    enter: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    require_private(template_dir, false)?;
    touch_last_used(template_dir);
    enter()
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
    write_lock_holder(dir, key, "collecting");
    for line in collect_stale_templates(dir, key) {
        say(line);
    }
    write_lock_holder(dir, key, "building");
    // Private: it holds the cluster's superuser password (`pg-pass`). Marked as
    // the harness's, so entering it may make it private again ([`MADE_MARKER`]).
    create_private_dir(template_dir).map_err(|e| {
        format!(
            "could not create the template cache {}: {e}",
            template_dir.display()
        )
    })?;
    mark_made(template_dir)?;
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
/// picked was taken in between ([`retry_port_collisions`]). On a failed attempt
/// the dropped `PostgreSQL` stops whatever came up in OUR data directory, which
/// is ours to stop.
async fn start_on_free_port(base: &Settings) -> Result<(PostgreSQL, Duration), String> {
    retry_port_collisions(&base.data_dir.join(START_LOG), |port| {
        let mut settings = base.clone();
        settings.port = port;
        async move {
            let mut cluster = PostgreSQL::new(settings);
            let starting = Instant::now();
            match cluster.start().await {
                Ok(()) => Ok((cluster, starting.elapsed())),
                Err(e) => Err((e.to_string(), cluster)),
            }
        }
    })
    .await
}

/// Run `start` on a free loopback port up to [`START_ATTEMPTS`] times, again only
/// while it fails because another process took the port in between.
///
/// Each failure is judged by the lines its own attempt wrote to `log`. Every
/// attempt appends to that log (`pg_ctl -l`), so the length it had before the
/// attempt is where the attempt's lines begin: an earlier attempt's collision
/// must not make a later attempt's different failure look like one, and be
/// retried instead of reported. `start` hands back what a failed attempt left
/// (`G`), which is dropped only once the log is read: dropping a cluster that
/// did come up stops it, and its shutdown lines would follow the failure's.
async fn retry_port_collisions<T, G, Fut>(
    log: &Path,
    mut start: impl FnMut(u16) -> Fut,
) -> Result<T, String>
where
    Fut: std::future::Future<Output = Result<T, (String, G)>>,
{
    let mut last_failure = String::new();
    for _ in 0..START_ATTEMPTS {
        let port = free_loopback_port().ok_or_else(|| "no free loopback TCP port".to_string())?;
        let written_before = log_len(log);
        match start(port).await {
            Ok(started) => return Ok(started),
            Err((e, failed)) => {
                let tail = log_tail(log, written_before);
                drop(failed);
                last_failure =
                    format!("pg_ctl start on port {port} failed: {e}; server log: {tail}");
                if !is_port_collision(&tail) {
                    return Err(last_failure);
                }
            }
        }
    }
    Err(last_failure)
}

/// Whether a start attempt's server log says the port was already taken.
fn is_port_collision(log: &str) -> bool {
    log.contains("could not bind") || log.contains("could not create any TCP")
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
    // From `template0`, which nothing connects to and nothing customises: a
    // session on the default `template1` would refuse the copy, and whatever a
    // server keeps in `template1` would land in every test database.
    maintenance
        .batch_execute(&format!(
            "CREATE DATABASE \"{template}\" TEMPLATE template0"
        ))
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
    .await
    .map_err(external_schema_error)?;
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

/// `error`, from applying the canonical schema to an external server, with the
/// remedy added when it is about an extension: the schema creates `pgcrypto`,
/// and `vector` where the server offers it, for itself, and the template is a
/// copy of `template0`, so a role that may not create them fails here even on
/// a server whose `template1` carries them ([`EXTERNAL_URL_ENV`]).
fn external_schema_error(error: String) -> String {
    if !error.to_ascii_lowercase().contains("extension") {
        return error;
    }
    format!(
        "{error}. The canonical schema creates pgcrypto, and vector where the server offers \
         it, for itself, and the harness's template is a copy of template0, so an extension \
         pre-installed in template1 does not reach it: the role {EXTERNAL_URL_ENV} names must \
         be able to create pgcrypto, and vector where the server offers it"
    )
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

/// The operating system and architecture a template cache is built for.
#[derive(Clone, Copy, Debug)]
struct Platform {
    os: &'static str,
    arch: &'static str,
}

/// The platform this test binary was built for, and so runs on.
const PLATFORM: Platform = Platform {
    os: std::env::consts::OS,
    arch: std::env::consts::ARCH,
};

/// The cache key: every input that shapes a cached template, hashed.
///
/// The platform is one. A cache holds that platform's extracted binaries and a
/// data directory in its layout, and a target directory can be shared across
/// platforms (a Windows checkout's, read from WSL): without it in the key, a
/// process on one would enter the other's cache, find no `pg_ctl` it can run
/// ([`installed_dir`]), and be told to delete a cache the other still uses.
fn cache_key(platform: Platform, pg_version: &str, schema_sql: &str) -> String {
    let mut hasher = Sha256::new();
    for part in [
        "qontinui-test-pg",
        HARNESS_VERSION,
        POSTGRESQL_EMBEDDED_VERSION,
        platform.os,
        platform.arch,
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

/// Where the harness lives, and whether [`HARNESS_DIR_ENV`] chose it.
struct HarnessDir {
    path: PathBuf,
    /// Named by [`HARNESS_DIR_ENV`]: it can be anywhere, so its ancestors are
    /// checked as well ([`require_safe_ancestors`]). The default sits in the
    /// cargo target directory beside the test binaries, whose ancestors it
    /// shares: whoever could swap the tree under it could swap the binaries.
    named: bool,
}

/// [`HARNESS_DIR_ENV`], else `qontinui-test-pg` in the directory
/// [`cargo_target_dir_of`] finds for this test binary.
fn harness_dir() -> Result<HarnessDir, String> {
    if let Some(dir) = env_nonblank(HARNESS_DIR_ENV) {
        return Ok(HarnessDir {
            path: PathBuf::from(dir),
            named: true,
        });
    }
    let exe = std::env::current_exe().map_err(|e| {
        format!(
            "cannot locate this test binary to find the cargo target directory: {e}; set \
             {HARNESS_DIR_ENV}"
        )
    })?;
    cargo_target_dir_of(&exe)
        .map(|target| HarnessDir {
            path: target.join(DEFAULT_DIR_NAME),
            named: false,
        })
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
/// `<target>/<triple>/qontinui-test-pg`. That is intended: each triple's test
/// binaries get caches of their own. The bundled PostgreSQL a cache extracts is
/// the one `postgresql_embedded` chooses in its build script, through a
/// build-dependency, so it follows the HOST triple, which is the target's only
/// when the two are the same; and the cache key names only the test binary's
/// OS and architecture ([`PLATFORM`]), not its environment (`gnu` or `musl`).
/// Found from the binary rather than `CARGO_TARGET_TMPDIR`, which cargo sets
/// for integration tests and benches only — this module serves every test
/// binary.
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
    create_private_dir(&root).map_err(|e| {
        format!(
            "could not create this process's root {}: {e}",
            root.display()
        )
    })?;
    let _ = OWN_ROOT.set(root.clone());
    write_owner(&root, &Owner::this_process(key, None))?;
    Ok(root)
}

/// Who owns a per-process root: the process — its pid, the PID namespace that
/// pid means something in, and when it started — the template cache it was
/// copied from (so a stale cache in use is not collected), and its postmaster.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Owner {
    pid: u32,
    /// [`own_pid_namespace`] of the owner; `None` off Linux.
    pid_ns: Option<String>,
    /// [`process_start`] of the owner, which tells it from a later process
    /// given the same pid; `None` where none is read, and in a record written
    /// before it was.
    started: Option<u64>,
    template: String,
    postmaster: Option<u32>,
}

impl Owner {
    /// This process, as the owner of a root copied from `template`.
    fn this_process(template: &str, postmaster: Option<u32>) -> Self {
        let pid = std::process::id();
        Self {
            pid,
            pid_ns: own_pid_namespace().map(str::to_owned),
            started: process_start(pid),
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
        let started = self
            .started
            .map(|started| format!("started={started}\n"))
            .unwrap_or_default();
        let postmaster = self
            .postmaster
            .map(|pid| format!("postmaster={pid}\n"))
            .unwrap_or_default();
        format!(
            "pid={}\n{pid_ns}{started}template={}\n{postmaster}",
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
            started: fields.get("started").and_then(|at| at.parse().ok()),
            template: (*fields.get("template")?).to_string(),
            postmaster: fields.get("postmaster").and_then(|pid| pid.parse().ok()),
        })
    }
}

/// When the process `pid` started, exactly enough to tell it from a later
/// process given the same pid: on Linux the `starttime` field of
/// `/proc/<pid>/stat`, in clock ticks after boot. `None` where it cannot be
/// read.
#[cfg(target_os = "linux")]
fn process_start(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name, field 2, is parenthesised and may hold spaces and
    // parentheses: the fields after its last `)` start at field 3, so
    // `starttime`, field 22, is the 20th of them.
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// When the process `pid` started, exactly enough to tell it from a later
/// process given the same pid: on Windows its creation time, in 100 ns ticks.
/// `None` where it cannot be read.
#[cfg(windows)]
fn process_start(pid: u32) -> Option<u64> {
    use windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;
    ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)
        .ok()?
        .created()
}

/// When the process `pid` started: not read on platforms other than Linux and
/// Windows, so an owner there is judged by its pid alone.
#[cfg(not(any(target_os = "linux", windows)))]
fn process_start(_pid: u32) -> Option<u64> {
    None
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
///
/// A recorded pid that is free means a dead owner. One that is in use means a
/// live owner only if the process using it started when the owner did
/// ([`process_start`]): a pid is handed out again once its process is gone, at
/// once on Windows, and the later process is not the owner. A record with no
/// start — written before starts were recorded, or where none is read — is
/// judged by the pid alone, as is a live pid whose start cannot be read.
fn owner_may_be_live(root: &Path, owner: &Owner) -> bool {
    if OWN_ROOT.get().is_some_and(|own| own == root) {
        return true;
    }
    if owner.pid_ns.as_deref() != own_pid_namespace() {
        return !modified_before(&root.join(OWNER_FILE), FOREIGN_OWNER_GRACE);
    }
    // A record naming THIS process's pid is a dead predecessor's: this
    // process's own root is matched above.
    if owner.pid == std::process::id() || !crate::runner_breadcrumb::pid_is_live(owner.pid) {
        return false;
    }
    match (owner.started, process_start(owner.pid)) {
        (Some(recorded), Some(now)) => recorded == now,
        _ => true,
    }
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

/// End the postmaster still running in `root/pg-data`, if there is one
/// ([`end_postmasters`]).
fn reap_postmaster(root: &Path, recorded: Option<u32>) -> String {
    let notes = end_postmasters(&root.join(DATA_DIR), recorded);
    if notes.is_empty() {
        "no postmaster was running".to_string()
    } else {
        notes.join("; ")
    }
}

/// End every postmaster still running in `data_dir` ([`end_postmaster`]): the
/// pid its `postmaster.pid` names, and `recorded` (an owner record's, or this
/// process's own postmaster at exit). One note per pid that was running.
fn end_postmasters(data_dir: &Path, recorded: Option<u32>) -> Vec<String> {
    let named = read_postmaster_pid(data_dir);
    let mut candidates: Vec<u32> = named.into_iter().collect();
    if let Some(pid) = recorded {
        if !candidates.contains(&pid) {
            candidates.push(pid);
        }
    }
    candidates
        .into_iter()
        .filter_map(|pid| end_postmaster(pid, data_dir, named == Some(pid)))
        .collect()
}

/// End `pid` once it is proven to be a postmaster of `data_dir`
/// ([`is_postmaster_of`]), and say what happened; `None` when no process has
/// `pid`. `named`: `data_dir`'s `postmaster.pid` names it.
///
/// On Linux the process is held by a pidfd from before the proof until it is
/// gone ([`end_held`]): a signal through the pidfd reaches the process it was
/// opened on or none, so a pid freed and taken by another process after the
/// proof is never signalled. Only a kernel without pidfds (`ENOSYS`, before
/// 5.3) gets the unheld path ([`end_unheld`]), window and all.
#[cfg(target_os = "linux")]
fn end_postmaster(pid: u32, data_dir: &Path, named: bool) -> Option<String> {
    match Pidfd::open(pid) {
        Ok(held) => Some(end_held(&held, pid, data_dir)),
        Err(e) if e.raw_os_error() == Some(libc::ESRCH) => None,
        Err(e) if e.raw_os_error() == Some(libc::ENOSYS) => end_unheld(pid, data_dir, named),
        Err(e) => Some(format!(
            "pid {pid} could not be held by a pidfd ({e}), so it was left alone"
        )),
    }
}

/// End `pid` once it is proven to be a postmaster of `data_dir`
/// ([`postmaster_identity`]), and say what happened; `None` when no process
/// has `pid`. On Windows it is held by one process handle from before the proof
/// until it is gone ([`end_held`]); `postmaster.pid` is not consulted.
#[cfg(windows)]
fn end_postmaster(pid: u32, data_dir: &Path, _named: bool) -> Option<String> {
    end_held(pid, data_dir)
}

/// End `pid` once it is proven to be a postmaster of `data_dir`
/// ([`is_postmaster_of`]), and say what happened; `None` when no process has
/// `pid`. `named`: `data_dir`'s `postmaster.pid` names it. On Unix other than
/// Linux nothing holds the process ([`end_unheld`]).
#[cfg(all(unix, not(target_os = "linux")))]
fn end_postmaster(pid: u32, data_dir: &Path, named: bool) -> Option<String> {
    end_unheld(pid, data_dir, named)
}

/// Linux: end `pid`, held by `held` — opened on it before this runs — once it
/// is proven to be a postmaster of `data_dir`. It is sent SIGQUIT through the
/// pidfd, which is what `pg_ctl stop -m immediate` sends: the postmaster then
/// runs its own exit cleanup, removing its System V interlock segment and its
/// dynamic shared memory, which a kill would leave behind for good once its
/// data directory is removed. SIGKILL follows through the same pidfd only if it
/// has not exited within [`QUIT_WAIT`].
///
/// The proof reads `/proc/<pid>` after the pidfd was opened, and a signal
/// through a pidfd fails once the process it was opened on is gone. So when a
/// signal is delivered, that process was alive from the open through the
/// proof, held `pid` throughout, and is the process the proof read: a pid freed
/// and taken by another process in between gets no signal at all.
#[cfg(target_os = "linux")]
fn end_held(held: &Pidfd, pid: u32, data_dir: &Path) -> String {
    if !is_postmaster_of(pid, data_dir) {
        return format!(
            "pid {pid} is alive but is not this root's postmaster, so it was left alone"
        );
    }
    let gone = |e: &std::io::Error| e.raw_os_error() == Some(libc::ESRCH);
    let unquit = match held.signal(libc::SIGQUIT) {
        Ok(()) if held.exits_within(QUIT_WAIT) => {
            return format!("stopped its postmaster {pid} (SIGQUIT through a pidfd)");
        }
        Ok(()) => format!("SIGQUIT did not end it within {} s", QUIT_WAIT.as_secs()),
        Err(e) if gone(&e) => return format!("its postmaster {pid} exited"),
        Err(e) => format!("SIGQUIT could not be sent: {e}"),
    };
    match held.signal(libc::SIGKILL) {
        Ok(()) if held.exits_within(POSTMASTER_EXIT_WAIT) => {
            format!("killed its postmaster {pid} ({unquit})")
        }
        Ok(()) => format!("its postmaster {pid} would not die ({unquit})"),
        Err(e) if gone(&e) => format!("its postmaster {pid} exited ({unquit})"),
        Err(e) => format!("its postmaster {pid} could not be killed ({unquit}; {e})"),
    }
}

/// Windows: end `pid` once it is proven to be a postmaster of `data_dir`,
/// through ONE handle opened before the proof. The proof is tied to that
/// handle's process — its creation time is the start sysinfo reported at the
/// proof, or its image is the executable the proof read — and the process is
/// then terminated and waited for through the same handle. A handle names its
/// process for as long as it is open, whatever is given the pid later, so a
/// pid freed and taken by another process after the proof is never
/// terminated. (Windows frees a postmaster's shared memory with its last
/// handle, so terminating one leaks nothing.)
#[cfg(windows)]
fn end_held(pid: u32, data_dir: &Path) -> Option<String> {
    use windows_sys::Win32::System::Threading::{
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };
    let access = PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
    let held = match ProcessHandle::open(pid, access) {
        Ok(held) => held,
        Err(e) => {
            return crate::runner_breadcrumb::pid_is_live(pid)
                .then(|| format!("pid {pid} could not be opened ({e}), so it was left alone"));
        }
    };
    let Some((started, exe)) = postmaster_identity(pid, data_dir) else {
        return Some(format!(
            "pid {pid} is alive but is not this root's postmaster, so it was left alone"
        ));
    };
    let created = held.created().and_then(unix_seconds_of_filetime);
    let tied = created == Some(started) || (exe.is_some() && held.image() == exe);
    if !tied {
        return Some(format!(
            "pid {pid} could not be tied to the process its handle holds, so it was left alone"
        ));
    }
    Some(match held.terminate() {
        Ok(()) if held.exits_within(POSTMASTER_EXIT_WAIT) => {
            format!("terminated its postmaster {pid} (through a handle held across the proof)")
        }
        Ok(()) => format!("its postmaster {pid} would not die"),
        Err(_) if held.exits_within(Duration::ZERO) => format!("its postmaster {pid} exited"),
        Err(e) => format!("its postmaster {pid} could not be terminated ({e})"),
    })
}

/// A Windows `FILETIME` (100 ns ticks since 1601) as whole seconds since the
/// Unix epoch, computed the way sysinfo computes a process's `start_time`.
#[cfg(windows)]
fn unix_seconds_of_filetime(ticks: u64) -> Option<u64> {
    (ticks / 10_000_000).checked_sub(11_644_473_600)
}

/// Unix without a held process (other than Linux, or Linux without pidfds):
/// end `pid` once it is proven to be a postmaster of `data_dir`. When
/// `postmaster.pid` names it (`named`) it is stopped with `pg_ctl stop -m
/// immediate` ([`stop_postmaster`]); `pg_ctl` signals whatever that file names.
/// A postmaster the stop did not end is killed once it is proven again. Every
/// step names the pid, so a pid freed and taken by another process between a
/// proof and the signal after it can still be hit here.
#[cfg(unix)]
fn end_unheld(pid: u32, data_dir: &Path, named: bool) -> Option<String> {
    if !crate::runner_breadcrumb::pid_is_live(pid) {
        return None;
    }
    if !is_postmaster_of(pid, data_dir) {
        return Some(format!(
            "pid {pid} is alive but is not this root's postmaster, so it was left alone"
        ));
    }
    let not_stopped = if named {
        match stop_postmaster(pid, data_dir) {
            Ok(()) => {
                return Some(format!(
                    "stopped its postmaster {pid} (pg_ctl stop -m immediate)"
                ));
            }
            Err(e) => e,
        }
    } else {
        "postmaster.pid does not name it, so pg_ctl cannot stop it".to_string()
    };
    // While the stop ran, the postmaster may have exited and its pid been
    // taken by another process.
    if !(crate::runner_breadcrumb::pid_is_live(pid) && is_postmaster_of(pid, data_dir)) {
        return Some(format!("its postmaster {pid} exited ({not_stopped})"));
    }
    Some(if kill_and_wait(pid) {
        format!("killed its postmaster {pid} ({not_stopped})")
    } else {
        format!("its postmaster {pid} would not die ({not_stopped})")
    })
}

/// A process held by a pidfd (`pidfd_open(2)`, Linux 5.3): a signal sent
/// through it reaches that process, or fails with `ESRCH` once the process is
/// gone — never another process given its pid since — and it turns readable
/// when the process exits.
#[cfg(target_os = "linux")]
struct Pidfd(std::os::fd::OwnedFd);

#[cfg(target_os = "linux")]
impl Pidfd {
    fn open(pid: u32) -> std::io::Result<Self> {
        use std::os::fd::FromRawFd;
        let pid = libc::pid_t::try_from(pid)
            .map_err(|_| std::io::Error::from_raw_os_error(libc::ESRCH))?;
        // SAFETY: pidfd_open reads only its two integer arguments, and returns
        // a new descriptor or -1.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) };
        let fd = std::os::fd::RawFd::try_from(fd)
            .ok()
            .filter(|fd| *fd >= 0)
            .ok_or_else(std::io::Error::last_os_error)?;
        // SAFETY: `fd` is the descriptor pidfd_open just returned; nothing else
        // owns it.
        Ok(Self(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) }))
    }

    /// Send `signal` to the held process: `ESRCH` once it is gone.
    fn signal(&self, signal: libc::c_int) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        // SAFETY: pidfd_send_signal reads the descriptor, the signal number,
        // a null `siginfo_t` pointer (the kernel then fills one in as `kill`
        // would) and zero flags.
        let sent = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.0.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0_u32,
            )
        };
        if sent == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// Whether the held process exits within `limit`: the pidfd turns
    /// readable when it does.
    fn exits_within(&self, limit: Duration) -> bool {
        use std::os::fd::AsRawFd;
        let deadline = Instant::now() + limit;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let timeout = libc::c_int::try_from(left.as_millis()).unwrap_or(libc::c_int::MAX);
            let mut ready = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll reads and writes exactly the one `pollfd` it is
            // given, which lives on this frame.
            match unsafe { libc::poll(&mut ready, 1, timeout) } {
                0 => return false,
                polled if polled > 0 => return true,
                _ if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                    && Instant::now() < deadline => {}
                _ => return false,
            }
        }
    }
}

/// A process held by a Windows process handle, which names that one process for
/// as long as it is open, whatever is given its pid later.
#[cfg(windows)]
struct ProcessHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl ProcessHandle {
    fn open(
        pid: u32,
        access: windows_sys::Win32::System::Threading::PROCESS_ACCESS_RIGHTS,
    ) -> std::io::Result<Self> {
        use windows_sys::Win32::System::Threading::OpenProcess;
        // SAFETY: OpenProcess has no preconditions; a null handle is its
        // failure, handled here.
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }

    /// Its creation time, in 100 ns ticks since 1601: unique to the process
    /// among all that held its pid.
    fn created(&self) -> Option<u64> {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::GetProcessTimes;
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: the handle was opened with PROCESS_QUERY_LIMITED_INFORMATION,
        // and each pointer is to a FILETIME on this frame.
        let read =
            unsafe { GetProcessTimes(self.0, &mut created, &mut exited, &mut kernel, &mut user) };
        if read == 0 {
            None
        } else {
            Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
        }
    }

    /// The full path of its executable.
    fn image(&self) -> Option<PathBuf> {
        use std::os::windows::ffi::OsStringExt;
        use windows_sys::Win32::System::Threading::{
            QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        };
        let mut path = vec![0_u16; 32_768];
        let mut len = u32::try_from(path.len()).unwrap_or(u32::MAX);
        // SAFETY: `path` holds `len` u16s; the call writes at most that many
        // and stores in `len` how many it wrote.
        let read = unsafe {
            QueryFullProcessImageNameW(self.0, PROCESS_NAME_WIN32, path.as_mut_ptr(), &mut len)
        };
        let len = usize::try_from(len).ok()?;
        (read != 0 && len <= path.len())
            .then(|| PathBuf::from(std::ffi::OsString::from_wide(&path[..len])))
    }

    fn terminate(&self) -> std::io::Result<()> {
        use windows_sys::Win32::System::Threading::TerminateProcess;
        // SAFETY: the handle was opened with PROCESS_TERMINATE.
        if unsafe { TerminateProcess(self.0, 1) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Whether the held process exits within `limit`.
    fn exits_within(&self, limit: Duration) -> bool {
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        // `u32::MAX` is INFINITE: the longest finite wait is one less.
        let millis = u32::try_from(limit.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: the handle was opened with PROCESS_SYNCHRONIZE.
        unsafe { WaitForSingleObject(self.0, millis) == WAIT_OBJECT_0 }
    }
}

#[cfg(windows)]
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by `open` and is closed once, here.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

/// Whether `pid` is a postgres process working in `data_dir` — the postmaster
/// `chdir`s into its data directory and is started with `-D <data_dir>`.
#[cfg(any(unix, test))]
fn is_postmaster_of(pid: u32, data_dir: &Path) -> bool {
    postmaster_identity(pid, data_dir).is_some()
}

/// The proof that `pid` is a postgres process working in `data_dir`, and what
/// it read of the process: the start sysinfo reports (seconds since the Unix
/// epoch) and the executable, if readable. `None` when it is not.
fn postmaster_identity(pid: u32, data_dir: &Path) -> Option<(u64, Option<PathBuf>)> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::new()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always)
            .with_exe(UpdateKind::Always),
    );
    let process = system.process(pid)?;
    let named_postgres = process
        .name()
        .to_string_lossy()
        .to_ascii_lowercase()
        .starts_with("postgres");
    let in_data_dir = process.cwd().is_some_and(|cwd| same_dir(cwd, data_dir))
        || process
            .cmd()
            .iter()
            .any(|arg| same_dir(Path::new(arg), data_dir));
    (named_postgres && in_data_dir)
        .then(|| (process.start_time(), process.exe().map(Path::to_path_buf)))
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Stop the postmaster `pid` of `data_dir` with `pg_ctl stop -m immediate`
/// ([`stop_immediately`]), run from the install the postmaster itself runs
/// from. `Ok` once the pid is gone.
#[cfg(unix)]
fn stop_postmaster(pid: u32, data_dir: &Path) -> Result<(), String> {
    let pg_ctl = executable_of(pid)
        .and_then(|exe| {
            exe.parent()
                .map(|bin| bin.join(format!("pg_ctl{}", std::env::consts::EXE_SUFFIX)))
        })
        .filter(|pg_ctl| pg_ctl.is_file())
        .ok_or_else(|| "no pg_ctl beside its executable".to_string())?;
    stop_immediately(&pg_ctl, data_dir)?;
    if wait_for(
        || !crate::runner_breadcrumb::pid_is_live(pid),
        POSTMASTER_EXIT_WAIT,
    ) {
        Ok(())
    } else {
        Err("pg_ctl stop reported it stopped, but it is still running".to_string())
    }
}

/// The executable `pid` runs, where this process can read it.
#[cfg(unix)]
fn executable_of(pid: u32) -> Option<PathBuf> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::new().with_exe(UpdateKind::Always),
    );
    system.process(pid)?.exe().map(Path::to_path_buf)
}

/// `pg_ctl stop -m immediate` on `data_dir`. Bounded twice: `pg_ctl --timeout`
/// gives up on a server that will not stop, and the wrapper kills a `pg_ctl`
/// that will not return. `Ok` when `pg_ctl` reports the server stopped.
#[cfg(unix)]
fn stop_immediately(pg_ctl: &Path, data_dir: &Path) -> Result<(), String> {
    let mut command = crate::process_helpers::no_window(pg_ctl);
    command
        .arg("stop")
        .arg("--pgdata")
        .arg(data_dir)
        .args([
            "--mode",
            "immediate",
            "--wait",
            "--timeout",
            STOP_TIMEOUT_SECS,
        ])
        .arg("--silent")
        .stdin(std::process::Stdio::null());
    match crate::process_helpers::run_with_timeout(command, STOP_WAIT) {
        Ok(crate::process_helpers::TimedOutput::Completed(output)) if output.status.success() => {
            Ok(())
        }
        Ok(crate::process_helpers::TimedOutput::Completed(output)) => Err(format!(
            "pg_ctl stop exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Ok(crate::process_helpers::TimedOutput::TimedOut { pid, .. }) => Err(format!(
            "pg_ctl stop (pid {pid}) overran {} s and was killed",
            STOP_WAIT.as_secs()
        )),
        Err(e) => Err(format!("pg_ctl stop could not run: {e}")),
    }
}

/// Kill `pid` outright, by pid, and wait for it to go. Only ever applied to a
/// postmaster whose data directory is being thrown away, and that a stop could
/// not end; its workers exit when it does.
#[cfg(unix)]
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
        POSTMASTER_EXIT_WAIT,
    )
}

/// Whether `done` holds within `limit`, polled every 100 ms.
#[cfg(any(unix, test))]
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

/// Enter the harness directory, refusing one this process cannot safely run
/// PostgreSQL from and keep a cluster's superuser password in.
///
/// - A directory [`HARNESS_DIR_ENV`] names (`named`) must have safe ancestors
///   ([`require_safe_ancestors`]), judged before anything is created and again
///   after: an ancestor that came into being in between — another user
///   squatting `/tmp/<you>` in the sticky `/tmp` — was not there to judge the
///   first time, and the creation accepts an existing directory whoever owns
///   it.
/// - A missing directory is created private, with any parent it lacks, and
///   marked as the harness's ([`MADE_MARKER`]). Only the create that made it
///   counts as making it: one that appears between the checks and the create
///   is found, not made.
/// - An existing one at the DEFAULT location (`!named`) that is already
///   private to this user and carries no mark is marked now
///   ([`found_private`]): a harness from before the mark made it and left none,
///   and opened later it would otherwise only be refused, although the harness
///   made it. An existing directory [`HARNESS_DIR_ENV`] names is never marked:
///   the tester made it, and the harness never changes its mode.
///
/// Either way it must then be private ([`require_private`]).
fn prepare_harness_dir(dir: &Path, named: bool) -> Result<(), String> {
    if named {
        require_safe_ancestors(dir)?;
    }
    #[cfg(test)]
    if let Some(hook) = BETWEEN_ANCESTOR_CHECK_AND_CREATE.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
    // The parents first, then the directory itself with one create: whether
    // THIS call made it is the create's own answer, so a directory that
    // appears in between — made by someone else — is never taken for the
    // harness's and marked.
    let cannot_create = |e: std::io::Error| {
        format!(
            "could not create the harness directory {}: {e}",
            dir.display()
        )
    };
    if let Some(parent) = dir.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        create_private_dir_all(parent).map_err(cannot_create)?;
    }
    let made = match create_private_dir(dir) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => false,
        Err(e) => return Err(cannot_create(e)),
    };
    if named {
        require_safe_ancestors(dir)?;
    }
    if made || (!named && found_private(dir)) {
        mark_made(dir)?;
    }
    require_private(dir, named)
}

// Test seam: run once, by the next `prepare_harness_dir` on this thread,
// between its first ancestor check and its create, so a test can make an
// ancestor appear in that window. Compiles to nothing outside `cfg(test)`.
#[cfg(test)]
thread_local! {
    static BETWEEN_ANCESTOR_CHECK_AND_CREATE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Mark `dir`, which the harness has just created, or found private to this
/// user at the default location ([`found_private`]), as the harness's own
/// ([`MADE_MARKER`]).
fn mark_made(dir: &Path) -> Result<(), String> {
    let marker = dir.join(MADE_MARKER);
    std::fs::write(
        &marker,
        "made by the qontinui test-pg harness, which makes only directories holding this \
         file private again\n",
    )
    .map_err(|e| format!("could not mark {} as the harness's: {e}", dir.display()))
}

/// Whether `dir`, which the harness found rather than made, is private to this
/// process's effective user — owned by it, with no access for its group or
/// others — and carries no mark that counts (`is_marked`). Never on Windows,
/// where nothing is checked.
fn found_private(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        std::fs::metadata(dir).is_ok_and(|meta| {
            meta.is_dir() && privacy_problem(meta.uid(), meta.mode(), me).is_none()
        }) && !is_marked(dir, me)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        false
    }
}

/// Whether `dir` holds a [`MADE_MARKER`] that counts ([`marker_counts`]).
#[cfg(unix)]
fn is_marked(dir: &Path, me: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(dir.join(MADE_MARKER))
        .is_ok_and(|meta| marker_counts(meta.file_type().is_file(), meta.uid(), me))
}

/// Whether a [`MADE_MARKER`] that is a regular file (`is_file`, read without
/// following a symlink) owned by uid `owner` is the harness's mark for uid
/// `me`: only a regular file `me` owns is. Anyone who can write into a
/// directory can plant a file there, or a symlink to one, but only `me` — or
/// root — can make one `me` owns.
#[cfg(any(unix, test))]
fn marker_counts(is_file: bool, owner: u32, me: u32) -> bool {
    is_file && owner == me
}

/// Refuse `dir` unless it is private to this user, making it private again
/// first only when it holds the harness's mark and nobody else could have
/// written into it.
///
/// The harness runs PostgreSQL out of the template caches in its directory and
/// keeps each cluster's superuser password there (`tpl-*/pg-pass`), so a
/// directory another user owns, or one others can reach, could hand this process
/// their binaries to run or hand them the password. On Unix:
///
/// - a directory holding the harness's mark (`is_marked`) — the harness
///   directory or a template cache — that this process's effective user owns
///   and that is open to its group or others only to read or search is set to
///   0700 again;
/// - one such directory that its group or others can WRITE is refused, not
///   chmodded: what they wrote into it while it was open would survive the
///   chmod. For a directory [`HARNESS_DIR_ENV`] names (`named`) the error says
///   to point it at a directory that does not exist yet, never to remove the
///   one the tester named; otherwise it names both remedies: remove it, after
///   which the harness makes it again, or point [`HARNESS_DIR_ENV`] at a
///   directory that does not exist yet — the way out where removal would only
///   loop, on a filesystem that holds no modes;
/// - a directory with no mark that counts — such as an existing directory
///   [`HARNESS_DIR_ENV`] names, which the harness never marks, or one it found
///   open — is never chmodded: the harness does not change the mode of a
///   directory it did not make;
/// - then the directory must be owned by that user and open to no one else
///   ([`privacy_problem`]), or the boot fails with an error naming it and the
///   remedy.
///
/// Windows is not checked: its access lists are not modes, and the harness
/// reads none.
fn require_private(dir: &Path, named: bool) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let owner_and_mode = |dir: &Path| {
            std::fs::metadata(dir)
                .map(|meta| (meta.uid(), meta.mode() & 0o7777))
                .map_err(|e| {
                    format!(
                        "could not read the owner and mode of {}: {e}",
                        dir.display()
                    )
                })
        };
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        let (owner, mode) = owner_and_mode(dir)?;
        let made = is_marked(dir, me);
        let fresh_dir = format!(
            "point {HARNESS_DIR_ENV} at a directory that does not exist yet, which the \
             harness then creates"
        );
        if made && owner == me && mode & 0o022 != 0 {
            let remedy = if named {
                fresh_dir.clone()
            } else {
                format!("remove it, and the harness makes it again, or {fresh_dir}")
            };
            return Err(format!(
                "{} carries the harness's mark but is writable by its group or others (mode \
                 {:o}), and whatever was written into it while it was open would survive \
                 making it private again, so the harness does not use it: {remedy}",
                dir.display(),
                mode & 0o7777
            ));
        }
        let unchanged = if made && owner == me && mode & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).err()
        } else {
            None
        };
        let (owner, mode) = owner_and_mode(dir)?;
        if let Some(problem) = privacy_problem(owner, mode, me) {
            let tried = unchanged
                .map(|e| format!(" (setting it to 0700 failed: {e})"))
                .unwrap_or_default();
            let remedy = match (made, named) {
                (true, false) => {
                    format!("remove it, and the harness makes it again, or {fresh_dir}")
                }
                (false, false) => format!(
                    "make it private yourself (chmod 700; the harness changes the mode only \
                     of a directory it made), remove it if it is an old harness directory or \
                     cache, or {fresh_dir}"
                ),
                (_, true) => format!(
                    "make it private yourself (chmod 700; the harness never changes the mode \
                     of a directory {HARNESS_DIR_ENV} names that it did not make), or \
                     {fresh_dir}"
                ),
            };
            return Err(format!(
                "{} is {problem}{tried}. The harness runs PostgreSQL out of it and keeps a \
                 cluster's superuser password in it, so it must be private to this user: \
                 {remedy}",
                dir.display()
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Why a directory owned by uid `owner`, with permission bits `mode`, is not
/// private to uid `me`: another user owns it, or its group or others have any
/// access. `None` when it is private.
#[cfg(any(unix, test))]
fn privacy_problem(owner: u32, mode: u32, me: u32) -> Option<String> {
    if owner != me {
        Some(format!("owned by uid {owner}, not by this user (uid {me})"))
    } else if mode & 0o077 != 0 {
        Some(format!(
            "open to its group or to others (mode {:o})",
            mode & 0o777
        ))
    } else {
        None
    }
}

/// Refuse a harness directory with an ancestor through which another user
/// could swap the tree under it — between the check that a cache is private and
/// the start of the `pg_ctl` in it. Every ancestor that exists, as the path
/// names it and as it resolves through symlinks (whoever can write the
/// directory holding a symlink can swap the symlink), must be owned by this
/// process's effective user or by root, and not writable by others unless it
/// is sticky ([`ancestor_problem`], which says why its group's write is not
/// judged). Where a symlink's target
/// itself passes through another symlink, the directory holding that second
/// one is in neither chain and is not judged. The nearest ancestor is judged
/// first, and the first that fails fails the boot; ancestors that do not exist
/// yet the harness creates private. Only a directory [`HARNESS_DIR_ENV`] names
/// is checked ([`HarnessDir`]). Windows is not checked.
fn require_safe_ancestors(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        let absolute = std::path::absolute(dir).map_err(|e| {
            format!(
                "could not resolve the harness directory {}: {e}",
                dir.display()
            )
        })?;
        let Some(existing) = absolute.ancestors().find(|path| path.exists()) else {
            return Ok(());
        };
        let resolved = std::fs::canonicalize(existing)
            .map_err(|e| format!("could not resolve {}: {e}", existing.display()))?;
        // The directory itself, when it exists, is `require_private`'s to judge.
        let skip = usize::from(existing == absolute.as_path());
        let as_named = absolute.ancestors().filter(|path| path.exists()).skip(skip);
        for ancestor in as_named.chain(resolved.ancestors().skip(skip)) {
            let meta = std::fs::metadata(ancestor)
                .map_err(|e| format!("could not read {}: {e}", ancestor.display()))?;
            if let Some(problem) = ancestor_problem(meta.uid(), meta.mode() & 0o7777, me) {
                return Err(format!(
                    "{} cannot be the harness directory: its ancestor {} is {problem}, so \
                     another user could swap the tree under it. Point {HARNESS_DIR_ENV} at a \
                     directory whose every ancestor is this user's or root's and not writable \
                     by others, unless sticky, such as /dev/shm/<you>/qontinui-test-pg",
                    dir.display(),
                    ancestor.display()
                ));
            }
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Why an ancestor of the harness directory, owned by uid `owner` with
/// permission bits `mode`, would let a user other than uid `me` swap the tree
/// under it: a user other than `me` and root owns it, or others can write to it
/// and it is not sticky. `None` when it is safe.
///
/// Group write is not judged. On a system with a private group per user and
/// umask 0002 — the common default — every directory a tester makes is
/// writable by a group whose only member is the tester, so refusing group
/// write would refuse every named directory below one, while sharing a group
/// with other users is the tester's own choice. The harness directory itself
/// is judged more strictly ([`require_private`]).
#[cfg(any(unix, test))]
fn ancestor_problem(owner: u32, mode: u32, me: u32) -> Option<String> {
    if owner != me && owner != 0 {
        Some(format!(
            "owned by uid {owner}, neither this user (uid {me}) nor root"
        ))
    } else if mode & 0o002 != 0 && mode & 0o1000 == 0 {
        Some(format!(
            "writable by others and not sticky (mode {:o})",
            mode & 0o7777
        ))
    } else {
        None
    }
}

/// The text of a panic's payload, where it carries one.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".to_string())
}

/// How long a server log is now, in bytes: where whatever is written to it next
/// begins. A log that does not exist yet is empty.
fn log_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

/// The end of what a server log gained after its first `from` bytes
/// ([`log_len`]), on one line, for a boot error. A log shorter than `from` was
/// replaced rather than appended to, so it is read whole.
fn log_tail(path: &Path, from: u64) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let written = usize::try_from(from)
                .ok()
                .and_then(|from| bytes.get(from..))
                .unwrap_or(&bytes);
            let start = written.len().saturating_sub(LOG_TAIL_BYTES);
            String::from_utf8_lossy(&written[start..])
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
        // This process's postmaster is ended as the sweep ends a dead root's
        // ([`end_postmaster`]): proven to work in this data directory, then
        // signalled through a pidfd on Linux, or terminated through a process
        // handle on Windows, each held from before the proof until the process
        // is gone. A postmaster that died early and whose pid another process
        // has taken since is therefore never signalled there. On other Unix the
        // proof and the signal each name the pid, and that window remains.
        let ended = end_postmasters(&self.data_dir, self.postmaster_pid);
        let stopped = if ended.is_empty() {
            "no cluster was running".to_string()
        } else {
            ended.join("; ")
        };
        let removed = match remove_dir_all_retrying(&self.root) {
            Ok(()) => format!("root {} removed", self.root.display()),
            Err(e) => format!("root {} NOT removed: {e}", self.root.display()),
        };
        format!("{stopped}; {removed}")
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
    /// How long a child may take to boot, and then to finish
    /// ([`run_child_within`]). Comfortably above [`LOCK_PATIENCE`]'s bound, so
    /// a child that waits behind ONE holder keeping the cache lock past that
    /// bound gives up by itself, and says why, before it is killed. That is
    /// all it guarantees: the bound is per holder, so a child behind a queue of
    /// holders, or one that then builds the cache itself (`initdb` has no
    /// bound), can still be killed first — and then the failure carries what
    /// the child said, its "still waiting" lines among it.
    const CHILD_TIMEOUT: Duration = Duration::from_secs(900);
    const _: () = assert!(
        CHILD_TIMEOUT.as_secs() >= LOCK_PATIENCE.give_up.as_secs() + 240,
        "a child must outlive one holder's hold on the cache lock"
    );
    /// How long the `slow-hang` child waits after booting before it reports:
    /// longer than the overrun self-test's after-boot limit
    /// ([`OVERRUN_LIMIT`]).
    const SLOW_REPORT_DELAY: Duration = Duration::from_secs(12);
    /// The overrun self-test's after-boot limit.
    const OVERRUN_LIMIT: Duration = Duration::from_secs(10);
    const _: () = assert!(
        SLOW_REPORT_DELAY.as_secs() > OVERRUN_LIMIT.as_secs(),
        "the slow child must report after a spawn-timed limit would have expired"
    );
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

    /// Run only as a child process: it is `#[ignore]`d, so a default run of
    /// the whole binary skips it, and it FAILS unless `QONTINUI_TEST_PG_CHILD`
    /// selects a mode, so no run of it can count as a pass that exercised
    /// nothing — a run that includes ignored tests (`--include-ignored`,
    /// nextest's `--run-ignored all`) sees it fail. The child-process self-tests below re-run this binary with
    /// exactly this test selected (`--ignored --exact`, [`child_command`]):
    ///
    /// - `pass`: boot, report where the cluster is, and pass.
    /// - `fail`: the same, then fail, so libtest exits through
    ///   `std::process::exit` (on Windows, `ExitProcess`).
    /// - `hang`: boot, report, and wait to be killed — failing if it is not
    ///   within [`CHILD_TIMEOUT`]. Its report is renamed into place, so a parent
    ///   polling for it never reads half of one.
    /// - `slow-hang`: as `hang`, but it waits [`SLOW_REPORT_DELAY`] after
    ///   booting before it reports, so a limit timed from the spawn rather than
    ///   from the report would expire before the boot signal arrives.
    /// - `boot-failure`: ask for a database from a harness that cannot boot.
    #[test]
    #[ignore = "child-process helper; run by its parent test"]
    fn child_helper() {
        let mode = std::env::var(CHILD_MODE_ENV).unwrap_or_else(|_| {
            panic!(
                "child_helper: run only by its parent test, which sets {CHILD_MODE_ENV}; run \
                 alone it would pass having exercised nothing"
            )
        });
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
            "hang" | "slow-hang" => {
                let _database = fresh_database_blocking();
                let facts = embedded_facts().expect("the child runs on the bundled cluster");
                if mode == "slow-hang" {
                    std::thread::sleep(SLOW_REPORT_DELAY);
                }
                let report = PathBuf::from(std::env::var(CHILD_REPORT_ENV).expect("report path"));
                let partial = report.with_extension("partial");
                std::fs::write(&partial, render_facts(&facts)).expect("write the child report");
                std::fs::rename(&partial, &report).expect("move the child report into place");
                std::thread::sleep(CHILD_TIMEOUT);
                panic!(
                    "child_helper: the hang child was not killed within {} s",
                    CHILD_TIMEOUT.as_secs()
                );
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

    /// This test binary, to run [`child_helper`] alone in `mode` (`None`: with
    /// no mode set). The helper is `#[ignore]`d, hence `--ignored`.
    fn child_command(mode: Option<&str>) -> std::process::Command {
        let exe = std::env::current_exe().expect("this test binary's path");
        let mut command = crate::process_helpers::no_window(&exe);
        command
            .arg(child_test_name())
            .args(["--ignored", "--exact", "--test-threads=1"])
            // These tests are about the bundled cluster's teardown.
            .env_remove(EXTERNAL_URL_ENV)
            .stdin(std::process::Stdio::null());
        match mode {
            Some(mode) => command.env(CHILD_MODE_ENV, mode),
            None => command.env_remove(CHILD_MODE_ENV),
        };
        command
    }

    fn run_child(mode: &str, report: &Path, harness_dir: Option<&Path>) -> ChildRun {
        run_child_within(mode, report, harness_dir, CHILD_TIMEOUT)
    }

    /// Run [`child_helper`] in `mode`, its report at `report`: for at most
    /// [`CHILD_TIMEOUT`] until it has booted — it has written its report, the
    /// boot signal — or exited, and then for at most `after_boot`. Its stdout
    /// and stderr go to files beside the report, so a child that overruns
    /// still shows what it said — a lock wait's lines and its give-up among
    /// them.
    ///
    /// One that overruns either phase is killed and FAILS the test, with that
    /// output. The kill takes its process tree: on Unix its process group,
    /// which the postmaster `pg_ctl` starts in a session of its own has left;
    /// on Windows its kill-on-close job, which takes the postmaster with it.
    /// Either way its cluster is then ended, once proven its, and its root
    /// removed ([`ReapChildCluster::of_child`]): the one its report names, and
    /// any root in the harness directory whose owner record is the child's,
    /// which covers a child killed after its postmaster started but before its
    /// report.
    fn run_child_within(
        mode: &str,
        report: &Path,
        harness_dir: Option<&Path>,
        after_boot: Duration,
    ) -> ChildRun {
        let stdout_path = report.with_extension("stdout");
        let stderr_path = report.with_extension("stderr");
        let file = |path: &Path| {
            std::fs::File::create(path)
                .unwrap_or_else(|e| panic!("the {mode} child's {}: {e}", path.display()))
        };
        let mut command = child_command(Some(mode));
        command
            .env(CHILD_REPORT_ENV, report)
            .stdout(file(&stdout_path))
            .stderr(file(&stderr_path));
        if let Some(dir) = harness_dir {
            command.env(HARNESS_DIR_ENV, dir);
        }
        let output = || {
            let read = |path: &Path| std::fs::read_to_string(path).unwrap_or_default();
            format!(
                "--- child stdout ---\n{}\n--- child stderr ---\n{}",
                read(&stdout_path),
                read(&stderr_path)
            )
        };
        crate::process_helpers::ChildTreeGuard::arm(&mut command);
        let mut child = command
            .spawn()
            .unwrap_or_else(|e| panic!("could not run the {mode} child: {e}"));
        let tree = crate::process_helpers::ChildTreeGuard::attach_armed(&child);
        let booted = wait_for(
            || report.is_file() || !matches!(child.try_wait(), Ok(None)),
            CHILD_TIMEOUT,
        );
        let (phase, limit) = if booted {
            wait_for(|| !matches!(child.try_wait(), Ok(None)), after_boot);
            ("after booting", after_boot)
        } else {
            ("to boot", CHILD_TIMEOUT)
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                tree.disarm();
                ChildRun {
                    status,
                    output: output(),
                }
            }
            outcome => {
                let pid = child.id();
                // Read while the child is unreaped, so the pid is still its.
                let started = process_start(pid);
                drop(tree);
                let _ = child.kill();
                let _ = child.wait();
                let _reap = ReapChildCluster::of_child(report, pid, started, harness_dir);
                panic!(
                    "the {mode} child (pid {pid}) did not finish within {} s {phase} \
                     ({outcome:?}):\n{}",
                    limit.as_secs(),
                    output()
                )
            }
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

    /// Ends a child's postmaster, once it is proven to be the child's
    /// ([`reap_postmaster`]), and removes the child's root when a child-process
    /// self-test ends — above all when it ends in a panic, which is when the
    /// child left them: a red run must not leave a live cluster on a shared or
    /// self-hosted runner. A no-op when both are already gone.
    struct ReapChildCluster(Vec<(PathBuf, Option<u32>)>);

    impl ReapChildCluster {
        /// Armed with the cluster the child reported, if it reported one: taken
        /// before anything about the child is asserted, so no assertion can
        /// fail ahead of it and leave that cluster running.
        fn reported(report: &Path) -> Self {
            Self(
                std::fs::read_to_string(report)
                    .ok()
                    .as_deref()
                    .and_then(parse_facts)
                    .map(|facts| (facts.root, Some(facts.postmaster_pid)))
                    .into_iter()
                    .collect(),
            )
        }

        /// [`Self::reported`], and every root in the child's harness directory
        /// (`named_dir`, else the one this process resolves, which the child
        /// shares) that the child itself created: a child killed between its
        /// postmaster's start and its report leaves a root and a cluster that
        /// no report names.
        ///
        /// A root is taken only when its owner record ([`OWNER_FILE`]) names
        /// the child's `pid`, this process's PID namespace (the child's), and
        /// the child's `started` ([`process_start`], read while the child was
        /// still unreaped, so its pid could not yet name another process). A
        /// root with no record, or a record naming another namespace, another
        /// start, or none, is left alone for the dead-owner sweep
        /// ([`sweep_dead_roots`]): it may be a live peer's whose pid is the
        /// same number. With no `started` — off Linux and Windows — nothing is
        /// taken this way. Each postmaster is still ended only once proven a
        /// postmaster of that root's data directory.
        fn of_child(
            report: &Path,
            pid: u32,
            started: Option<u64>,
            named_dir: Option<&Path>,
        ) -> Self {
            let mut reap = Self::reported(report);
            let dir = named_dir
                .map(Path::to_path_buf)
                .or_else(|| harness_dir().ok().map(|dir| dir.path));
            let prefix = format!("{RUN_PREFIX}{pid}-");
            let named: Vec<PathBuf> = dir
                .and_then(|dir| std::fs::read_dir(dir).ok())
                .into_iter()
                .flatten()
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
                .map(|entry| entry.path())
                .collect();
            let own_ns = own_pid_namespace().map(str::to_owned);
            for root in named {
                let created_by_the_child = started.is_some()
                    && read_owner(&root).is_some_and(|owner| {
                        owner.pid == pid && owner.pid_ns == own_ns && owner.started == started
                    });
                if created_by_the_child && !reap.0.iter().any(|(known, _)| same_dir(known, &root)) {
                    reap.0.push((root, None));
                }
            }
            reap
        }
    }

    impl Drop for ReapChildCluster {
        fn drop(&mut self) {
            for (root, postmaster) in self.0.drain(..) {
                let ended = reap_postmaster(&root, postmaster);
                let removed = if !root.exists() {
                    "already gone".to_string()
                } else {
                    match remove_dir_all_retrying(&root) {
                        Ok(()) => "removed".to_string(),
                        Err(e) => format!("NOT removed ({e})"),
                    }
                };
                if std::thread::panicking() {
                    say(format!(
                        "after a failed child-process self-test: {ended}; root {} {removed}",
                        root.display()
                    ));
                }
            }
        }
    }

    /// A test process whose tests all pass leaves neither its postmaster nor its
    /// root behind: the `atexit` teardown stops one and removes the other.
    #[test]
    fn a_test_process_that_passes_leaves_no_postmaster_and_no_root() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let report = scratch.path().join("child-report");
        let run = run_child("pass", &report, None);
        let _reap = ReapChildCluster::reported(&report);
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
        let _reap = ReapChildCluster::reported(&report);
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

    /// A child that overruns its limit FAILS its test with what it said, and
    /// leaves no cluster behind: killing its process group does not reach the
    /// postmaster, which `pg_ctl` starts in a session of its own, so
    /// [`run_child_within`] ends that cluster before it fails. The short limit
    /// starts only once the child has booted (its report is its boot signal),
    /// so however long it takes to build or enter the bundled cache — it always
    /// runs on the bundled cluster, whatever server the parent uses — is
    /// bounded by [`CHILD_TIMEOUT`] alone. The child is `slow-hang`, which
    /// reports [`SLOW_REPORT_DELAY`] after it booted: a limit timed from the
    /// spawn would expire first, and the failure would be a boot timeout. Unix
    /// only: on Windows the child's kill-on-close job takes the cluster with
    /// it.
    #[cfg(unix)]
    #[test]
    fn a_child_that_overruns_fails_with_its_output_and_leaves_no_cluster() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let report = scratch.path().join("child-report");
        let overran = std::panic::catch_unwind(|| {
            run_child_within("slow-hang", &report, None, OVERRUN_LIMIT);
        });
        // Ends what a broken run_child_within left, after the asserts below.
        let _reap = ReapChildCluster::reported(&report);
        let failure = panic_message(
            overran
                .expect_err("a slow-hang child must overrun its limit")
                .as_ref(),
        );
        let text = std::fs::read_to_string(&report).unwrap_or_else(|e| {
            panic!(
                "the hang child never booted within {} s, so nothing here is proven ({e}):\n\
                 {failure}",
                CHILD_TIMEOUT.as_secs()
            )
        });
        let facts = parse_facts(&text).expect("the hang child's report");
        assert!(
            failure.contains(&format!(
                "did not finish within {} s after booting",
                OVERRUN_LIMIT.as_secs()
            )) && failure.contains("--- child stderr ---")
                && failure.contains("bundled PostgreSQL"),
            "the failure carries what the child said, its boot line among it: {failure}"
        );
        assert!(
            wait_for(|| !postmaster_alive(&facts), DEATH_WAIT),
            "postmaster {} of the child that overran (data dir {}) is still running",
            facts.postmaster_pid,
            facts.data_dir.display()
        );
        assert!(
            !facts.root.exists(),
            "the root {} of the child that overran survived it",
            facts.root.display()
        );
    }

    /// A test process that is KILLED runs no exit-time teardown, so its
    /// postmaster outlives it, and the next boot's sweep must STOP that
    /// postmaster, not kill it ([`end_postmaster`]: on Linux SIGQUIT through a
    /// pidfd, which is what `pg_ctl stop -m immediate` sends; `pg_ctl` itself
    /// elsewhere). A stopped postmaster removes its System V segment and its
    /// dynamic shared memory as it exits, and a killed one leaves both behind
    /// for good once the sweep has removed its data directory. On Linux both
    /// are checked — the segment in `/proc/sysvipc/shm`, and the `/dev/shm`
    /// files of the cluster's dynamic shared memory ([`dsm_files_of`]) —
    /// present while the child runs and gone after the sweep. What a failing
    /// run leaves of either is removed before anything is asserted.
    ///
    /// The child boots in the shared harness directory, where another boot
    /// running this same code — another test's child — may sweep the root
    /// first. So the end state is asserted, and this sweep's own line only when
    /// it is the one that took the root: it must not say the postmaster was
    /// killed.
    #[cfg(unix)]
    #[test]
    fn a_killed_test_process_has_its_postmaster_stopped_by_the_next_sweep() {
        /// Kills the hang child if the test ends before it is killed on purpose.
        struct Hanging(std::process::Child);
        impl Drop for Hanging {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        let scratch = tempfile::tempdir().expect("scratch directory");
        let report = scratch.path().join("child-report");
        let output_path = scratch.path().join("child-output");
        let output_file = std::fs::File::create(&output_path).expect("the child's output file");
        let mut command = child_command(Some("hang"));
        command
            .env(CHILD_REPORT_ENV, &report)
            .stdout(output_file.try_clone().expect("share the output file"))
            .stderr(output_file);
        // Declared before the child, so dropped after it: killing the child
        // orphans its postmaster, which this then ends.
        let mut reap = ReapChildCluster(Vec::new());
        let mut child = Hanging(command.spawn().expect("spawn the hang child"));
        let output = || {
            format!(
                "--- child output ---\n{}",
                std::fs::read_to_string(&output_path).unwrap_or_default()
            )
        };
        wait_for(
            || report.is_file() || !matches!(child.0.try_wait(), Ok(None)),
            CHILD_TIMEOUT,
        );
        let text = std::fs::read_to_string(&report)
            .unwrap_or_else(|e| panic!("the hang child wrote no report ({e}):\n{}", output()));
        let facts = parse_facts(&text)
            .unwrap_or_else(|| panic!("the child's report is malformed: {text:?}\n{}", output()));
        reap.0
            .push((facts.root.clone(), Some(facts.postmaster_pid)));
        #[cfg(target_os = "linux")]
        let (segment, shm_files) = {
            let segment = sysv_segment_of(&facts.data_dir).unwrap_or_else(|| {
                panic!(
                    "{} names no System V segment, so nothing can show the stop freed it",
                    facts.data_dir.join(POSTMASTER_PID).display()
                )
            });
            assert!(
                sysv_segment_listed(segment),
                "the running postmaster's segment {segment:?} is not in /proc/sysvipc/shm, so \
                 its absence after the sweep would prove nothing"
            );
            let shm_files = dsm_files_of(facts.postmaster_pid);
            assert!(
                !shm_files.is_empty(),
                "postmaster {} maps no /dev/shm/PostgreSQL.* file, so nothing can show the stop \
                 freed its dynamic shared memory",
                facts.postmaster_pid
            );
            (segment, shm_files)
        };

        child.0.kill().expect("kill the hang child");
        child.0.wait().expect("reap the hang child");
        assert!(
            postmaster_alive(&facts),
            "postmaster {} died with the killed child, so the sweep has nothing to stop and \
             this test proves nothing:\n{}",
            facts.postmaster_pid,
            output()
        );

        let dir = facts
            .root
            .parent()
            .expect("a root lives in the harness directory");
        let swept = sweep_dead_roots(dir);
        let root = facts.root.display().to_string();
        let postmaster_gone = wait_for(|| !postmaster_alive(&facts), DEATH_WAIT);
        #[cfg(target_os = "linux")]
        let (segment_left, files_left) = {
            let segment_left = sysv_segment_listed(segment);
            let files_left: Vec<&PathBuf> = shm_files.iter().filter(|file| file.exists()).collect();
            if postmaster_gone {
                if segment_left {
                    // SAFETY: `shmctl(IPC_RMID)` with a null buffer only marks
                    // the segment for removal. It is the segment the dead
                    // postmaster above created, which nothing attaches now
                    // that it is gone.
                    unsafe {
                        libc::shmctl(segment.1, libc::IPC_RMID, std::ptr::null_mut());
                    }
                }
                for file in &files_left {
                    let _ = std::fs::remove_file(file);
                }
            }
            (segment_left, files_left)
        };
        assert!(
            postmaster_gone,
            "postmaster {} of the killed child is still running after the sweep ({swept:?})",
            facts.postmaster_pid
        );
        assert!(
            !facts.root.exists(),
            "the killed child's root {root} survived the sweep ({swept:?})"
        );
        for line in swept.iter().filter(|line| line.contains(&root)) {
            assert!(
                !line.contains("killed its postmaster") && !line.contains("would not die"),
                "the sweep did not stop the killed child's postmaster: {line}"
            );
        }
        #[cfg(target_os = "linux")]
        assert!(
            !segment_left && files_left.is_empty(),
            "the killed child's postmaster was not stopped: its System V segment {segment:?} \
             left behind: {segment_left}; its /dev/shm files left behind: {files_left:?} \
             ({swept:?})"
        );
    }

    /// The `/dev/shm` files of a running cluster's dynamic shared memory: the
    /// control segment its postmaster maps, and every segment that control
    /// segment lists as in use. Those are the ones the cluster's backends
    /// created, the statistics area's among them; the postmaster maps none of
    /// them, and a killed postmaster leaves them behind with its own.
    ///
    /// The control segment is read as PostgreSQL 18's `dsm.c` lays it out on a
    /// 64-bit target: a 16-byte header (magic, item count, capacity), then
    /// 40-byte items, each starting with a segment's handle and reference
    /// count. An odd handle names the main shared memory region, not a file. A
    /// mapped file without that magic is taken as it is.
    #[cfg(target_os = "linux")]
    fn dsm_files_of(postmaster: u32) -> Vec<PathBuf> {
        const CONTROL_MAGIC: u32 = 0x9a50_3d32;
        const HEADER: usize = 16;
        const ITEM: usize = 40;
        let maps = std::fs::read_to_string(format!("/proc/{postmaster}/maps")).unwrap_or_default();
        let mut files = Vec::new();
        for mapped in maps
            .lines()
            .filter_map(|line| line.split_whitespace().nth(5))
            .filter(|path| path.starts_with("/dev/shm/PostgreSQL."))
        {
            let control = std::fs::read(mapped).unwrap_or_default();
            let word = |at: usize| {
                control
                    .get(at..at + 4)
                    .map(|bytes| u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            };
            if word(0) == Some(CONTROL_MAGIC) {
                let items = (word(4).unwrap_or(0) as usize).min(control.len() / ITEM);
                for item in (0..items).map(|index| HEADER + ITEM * index) {
                    if let (Some(handle), Some(refcnt)) = (word(item), word(item + 4)) {
                        if refcnt > 0 && handle.is_multiple_of(2) {
                            files.push(PathBuf::from(format!("/dev/shm/PostgreSQL.{handle}")));
                        }
                    }
                }
            }
            files.push(PathBuf::from(mapped));
        }
        files.sort();
        files.dedup();
        files
    }

    /// The System V segment a running postmaster's `postmaster.pid` names: its
    /// seventh line holds the segment's key, then its id. The file writes the
    /// key as an unsigned long and `/proc/sysvipc/shm` as a signed int, so it
    /// is compared as the 32 bits of a `key_t`.
    #[cfg(target_os = "linux")]
    fn sysv_segment_of(data_dir: &Path) -> Option<(u32, i32)> {
        let contents = std::fs::read_to_string(data_dir.join(POSTMASTER_PID)).ok()?;
        let mut fields = contents.lines().nth(6)?.split_whitespace();
        let key = fields.next()?.parse::<u64>().ok()? as u32;
        Some((key, fields.next()?.parse().ok()?))
    }

    /// Whether the kernel still holds the System V segment `(key, id)`.
    #[cfg(target_os = "linux")]
    fn sysv_segment_listed((key, id): (u32, i32)) -> bool {
        std::fs::read_to_string("/proc/sysvipc/shm")
            .expect("read /proc/sysvipc/shm")
            .lines()
            .skip(1)
            .any(|line| {
                let mut fields = line.split_whitespace();
                let listed_key = fields.next().and_then(|field| field.parse::<i64>().ok());
                listed_key.map(|listed| listed as u32) == Some(key)
                    && fields.next().and_then(|field| field.parse().ok()) == Some(id)
            })
    }

    /// The child-process helper FAILS when no parent chose its mode, so no run
    /// of it can count as a pass that exercised nothing.
    #[test]
    fn the_child_helper_fails_unless_a_parent_chose_its_mode() {
        let output =
            match crate::process_helpers::run_with_timeout(child_command(None), CHILD_TIMEOUT) {
                Ok(crate::process_helpers::TimedOutput::Completed(output)) => output,
                Ok(crate::process_helpers::TimedOutput::TimedOut { pid, .. }) => panic!(
                    "the child with no mode (pid {pid}) did not finish within {} s",
                    CHILD_TIMEOUT.as_secs()
                ),
                Err(e) => panic!("could not run the child with no mode: {e}"),
            };
        let shown = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !output.status.success()
                && shown.contains(&format!("{} ... FAILED", child_test_name()))
                && shown.contains("run only by its parent test"),
            "the helper must fail, saying why, when no parent chose its mode:\n{shown}"
        );
    }

    /// A harness that cannot boot FAILS the test that asked it for a database,
    /// with the boot error — it never passes and is never ignored. Which boot
    /// error depends on where the scratch directory is: under an ancestor
    /// others can write, the ancestor refusal comes first
    /// ([`require_safe_ancestors`]); otherwise the harness cannot create its
    /// directory where the regular file is. Either names the path, and that is
    /// all this asserts of the error.
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
        let linux = Platform {
            os: "linux",
            arch: "x86_64",
        };
        let key = cache_key(linux, "=18.6.0", "CREATE TABLE t ();");
        assert_eq!(key.len(), 16);
        assert_eq!(key, cache_key(linux, "=18.6.0", "CREATE TABLE t ();"));
        assert_ne!(key, cache_key(linux, "=18.6.1", "CREATE TABLE t ();"));
        assert_ne!(key, cache_key(linux, "=18.6.0", "CREATE TABLE u ();"));
        // One target directory can serve a Windows process and a WSL one.
        let windows = Platform {
            os: "windows",
            ..linux
        };
        assert_ne!(key, cache_key(windows, "=18.6.0", "CREATE TABLE t ();"));
        let arm = Platform {
            arch: "aarch64",
            ..linux
        };
        assert_ne!(key, cache_key(arm, "=18.6.0", "CREATE TABLE t ();"));
    }

    #[test]
    fn owner_records_round_trip() {
        let bare = Owner {
            pid: 4242,
            pid_ns: None,
            started: None,
            template: "0123456789abcdef".to_string(),
            postmaster: None,
        };
        assert_eq!(Owner::parse(&bare.render()), Some(bare.clone()));
        let full = Owner {
            pid_ns: Some("pid:[4026531836]".to_string()),
            started: Some(133_877_312_051_234_567),
            postmaster: Some(777),
            ..bare.clone()
        };
        assert_eq!(Owner::parse(&full.render()), Some(full));
        // A record written before starts were recorded has no `started` line.
        assert_eq!(
            Owner::parse("pid=4242\ntemplate=0123456789abcdef\n"),
            Some(bare)
        );
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

    /// A harness directory and template caches the harness made — they hold its
    /// mark ([`MADE_MARKER`]) — that something has since opened to group or
    /// others for reading are made private again as the harness enters them, a
    /// READY cache and a just-built one alike: creating a directory that exists
    /// leaves its mode as it is, and every cache holds its cluster's superuser
    /// password (`pg-pass`). One its group or others could WRITE is refused
    /// with the remedy and keeps its mode, and so is one whose mark is a
    /// symlink rather than a regular file: either could have been planted.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_directory_the_harness_made_is_made_private_again_on_entry() {
        use std::os::unix::fs::PermissionsExt;
        let mode_of = |path: &Path| {
            std::fs::metadata(path)
                .unwrap_or_else(|e| panic!("read the mode of {}: {e}", path.display()))
                .permissions()
                .mode()
                & 0o777
        };
        let widen = |path: &Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .unwrap_or_else(|e| panic!("widen {}: {e}", path.display()));
            assert_eq!(mode_of(path), mode, "{} was not widened", path.display());
        };
        let scratch = tempfile::tempdir().expect("scratch directory");
        let dir = scratch.path().join("qontinui-test-pg");
        let ready = dir.join(format!("{TEMPLATE_PREFIX}ready"));
        std::fs::create_dir_all(&ready).expect("plant the READY cache");
        std::fs::write(ready.join(READY_FILE), "built by the harness\n").expect("mark it READY");
        for made in [&dir, &ready] {
            mark_made(made).expect("mark it as the harness's");
            widen(made, 0o755);
        }

        prepare_harness_dir(&dir, false).expect("enter the harness directory");
        assert_eq!(
            mode_of(&dir),
            0o700,
            "entering the harness directory it made left it readable to other users"
        );

        let lock = open_lock(&dir).expect("the cache lock");
        enter_or_build(
            &lock,
            &ready,
            || async { Err::<Duration, _>("a READY cache is never rebuilt".to_string()) },
            || Ok(()),
        )
        .await
        .expect("enter the READY cache");
        assert_eq!(
            mode_of(&ready),
            0o700,
            "entering a READY cache it made left it, and its pg-pass, readable to other users"
        );

        let missing = dir.join(format!("{TEMPLATE_PREFIX}missing"));
        let building: &Path = &missing;
        enter_or_build(
            &lock,
            building,
            // As `build_template` makes a cache, but opened up afterwards.
            move || async move {
                std::fs::create_dir(building).map_err(|e| e.to_string())?;
                mark_made(building)?;
                std::fs::set_permissions(building, std::fs::Permissions::from_mode(0o755))
                    .map_err(|e| e.to_string())?;
                std::fs::write(building.join(READY_FILE), "built by the test\n")
                    .map_err(|e| e.to_string())?;
                Ok::<_, String>(Duration::from_millis(1))
            },
            || Ok(()),
        )
        .await
        .expect("build and enter the missing cache");
        assert_eq!(
            mode_of(&missing),
            0o700,
            "entering the cache it built left it readable to other users"
        );

        assert!(
            require_private(&dir.join("not-there"), false).is_err(),
            "a directory whose mode cannot be read must be refused, not passed over"
        );

        // Marked, but writable by others: what they wrote survives a chmod.
        for writable in [0o775, 0o757] {
            widen(&dir, writable);
            let refused = prepare_harness_dir(&dir, false)
                .expect_err("a marked directory others could write must be refused");
            assert!(
                refused.contains(&dir.display().to_string())
                    && refused.contains("writable by its group or others")
                    && refused.contains("remove it")
                    && refused.contains(&format!("point {HARNESS_DIR_ENV} at a directory"))
                    && refused.contains("does not exist yet"),
                "the error names the directory and both remedies, removing it or naming a \
                 fresh one (a filesystem that holds no modes makes removal loop): {refused}"
            );
            assert_eq!(
                mode_of(&dir),
                writable,
                "the harness chmodded a marked directory others could write"
            );
        }

        // A mark that is a symlink to a regular file does not count.
        let planted = scratch.path().join("planted");
        std::fs::create_dir(&planted).expect("plant the directory");
        let target = scratch.path().join("a-mark-elsewhere");
        std::fs::write(&target, "planted\n").expect("plant the symlink's target");
        std::os::unix::fs::symlink(&target, planted.join(MADE_MARKER))
            .expect("plant the mark as a symlink");
        widen(&planted, 0o755);
        let refused = require_private(&planted, false)
            .expect_err("a directory whose mark is a symlink is not the harness's");
        assert!(
            refused.contains("chmod 700"),
            "it is refused as a directory the harness did not make: {refused}"
        );
        assert_eq!(
            mode_of(&planted),
            0o755,
            "the harness chmodded a directory whose mark is a symlink"
        );
    }

    /// An existing harness directory that is already private to this user but
    /// carries no mark — the default location a harness from before the mark
    /// made — is marked on entry, so a later entry that finds it opened makes
    /// it private again rather than refusing it. One found open is not marked
    /// (`a_directory_the_harness_did_not_make_is_refused_and_left_as_it_is`).
    #[cfg(unix)]
    #[test]
    fn an_existing_private_harness_directory_is_marked_on_entry() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::tempdir().expect("scratch directory");
        let dir = scratch.path().join("qontinui-test-pg");
        create_private_dir(&dir).expect("plant a harness directory from before the mark");
        assert!(!dir.join(MADE_MARKER).exists(), "the fixture has no mark");

        prepare_harness_dir(&dir, false).expect("enter the existing private directory");
        assert!(
            dir.join(MADE_MARKER).is_file(),
            "an existing private harness directory was not marked on entry"
        );

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))
            .expect("open it to be read by others");
        prepare_harness_dir(&dir, false)
            .expect("a marked directory opened for reading is made private again");
        let mode = std::fs::metadata(&dir)
            .expect("the harness directory")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "it was not made private again");
    }

    /// An existing directory HARNESS_DIR_ENV names is the tester's, so the
    /// harness never marks or chmods it: private, it is used and left unmarked;
    /// opened, it is refused with a remedy that never says to remove it. A
    /// named directory the harness made is marked, but found writable it too
    /// is refused without telling the tester to remove what they named.
    #[cfg(unix)]
    #[test]
    fn an_existing_named_directory_is_never_marked_or_chmodded() {
        use std::os::unix::fs::PermissionsExt;
        let mode_of = |path: &Path| {
            std::fs::metadata(path)
                .unwrap_or_else(|e| panic!("read the mode of {}: {e}", path.display()))
                .permissions()
                .mode()
                & 0o777
        };
        let set_mode = |path: &Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .unwrap_or_else(|e| panic!("set the mode of {}: {e}", path.display()));
        };
        let scratch = tempfile::tempdir().expect("scratch directory");

        let found = scratch.path().join("the-testers-own");
        create_private_dir(&found).expect("plant a private directory the tester made");
        prepare_harness_dir(&found, true).expect("a private named directory is used");
        assert!(
            !found.join(MADE_MARKER).exists(),
            "the harness marked a named directory it did not make"
        );
        set_mode(&found, 0o755);
        let refused = prepare_harness_dir(&found, true)
            .expect_err("a named directory others can read must be refused");
        assert_eq!(
            mode_of(&found),
            0o755,
            "the harness chmodded a named directory it did not make"
        );
        assert!(
            refused.contains("chmod 700")
                && refused.contains(HARNESS_DIR_ENV)
                && !refused.contains("remove it"),
            "the remedy names chmod and a fresh directory, never removal: {refused}"
        );

        let made = scratch.path().join("made-by-the-harness");
        prepare_harness_dir(&made, true).expect("a missing named directory is created");
        assert!(
            made.join(MADE_MARKER).is_file(),
            "a directory it made is marked"
        );
        set_mode(&made, 0o775);
        let refused = prepare_harness_dir(&made, true)
            .expect_err("a marked named directory others can write must be refused");
        assert_eq!(mode_of(&made), 0o775, "it was chmodded, not refused");
        assert!(
            refused.contains("does not exist yet") && !refused.contains("remove it"),
            "the remedy points at a fresh directory, never at removing the named one: \
             {refused}"
        );
    }

    /// An ancestor that comes into being between the ancestor check and the
    /// create — another user squatting a missing parent — is judged by the
    /// second check after the create, and refuses the boot. The window is
    /// opened by a test-only seam (`BETWEEN_ANCESTOR_CHECK_AND_CREATE`),
    /// which here makes the missing parent, open to others (0757).
    #[cfg(unix)]
    #[test]
    fn an_ancestor_that_appears_between_the_check_and_the_create_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::tempdir().expect("scratch directory");
        let parent = scratch.path().join("appears");
        let named = parent.join("qontinui-test-pg");
        let squat = parent.clone();
        BETWEEN_ANCESTOR_CHECK_AND_CREATE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                std::fs::create_dir(&squat).expect("make the parent in the window");
                std::fs::set_permissions(&squat, std::fs::Permissions::from_mode(0o757))
                    .expect("open it to others");
            }));
        });
        let refused = prepare_harness_dir(&named, true);
        assert!(
            BETWEEN_ANCESTOR_CHECK_AND_CREATE.with(|hook| hook.borrow().is_none()),
            "the seam did not run, so the window was never opened"
        );
        let refused = refused.expect_err("an ancestor that appeared in the window must fail");
        assert!(
            refused.contains(&format!("its ancestor {} is", parent.display()))
                && refused.contains("not sticky"),
            "the error names the ancestor that appeared: {refused}"
        );
    }

    /// A named directory that comes into being between the ancestor check and
    /// the create — made by someone other than this call — was found, not
    /// made, so the harness does not mark it: only the create's own success
    /// counts as making it. The window is opened by the test-only seam
    /// (`BETWEEN_ANCESTOR_CHECK_AND_CREATE`), which here makes the named
    /// directory itself, private.
    #[cfg(unix)]
    #[test]
    fn a_named_directory_that_appears_in_the_window_is_not_marked() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let named = scratch.path().join("appears-itself");
        let squat = named.clone();
        BETWEEN_ANCESTOR_CHECK_AND_CREATE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                create_private_dir(&squat).expect("make the named directory in the window");
            }));
        });
        let entered = prepare_harness_dir(&named, true);
        assert!(
            BETWEEN_ANCESTOR_CHECK_AND_CREATE.with(|hook| hook.borrow().is_none()),
            "the seam did not run, so the window was never opened"
        );
        entered.expect("a private directory the tester owns is used");
        assert!(
            !named.join(MADE_MARKER).exists(),
            "the harness marked a named directory that appeared in the window, which it did \
             not make"
        );
    }

    /// Only a regular file this user owns is the harness's mark: not a symlink
    /// or anything else, and not a file another user owns, who could plant
    /// one in a directory open to them.
    #[test]
    fn a_mark_counts_only_as_a_regular_file_this_user_owns() {
        let me = 1000;
        assert!(marker_counts(true, me, me));
        assert!(
            !marker_counts(false, me, me),
            "a symlink or a directory is no mark"
        );
        for other in [0, 1001] {
            assert!(
                !marker_counts(true, other, me),
                "a file uid {other} owns is no mark for uid {me}"
            );
        }
    }

    /// A failure to apply the canonical schema to an external server says what
    /// the role must be able to do when it is about an extension, and passes
    /// any other failure on as it is.
    #[test]
    fn an_external_extension_failure_names_what_the_role_must_create() {
        let denied = "applying canonical schema to the external test server failed: \
                      permission denied to create extension \"vector\" (SQLSTATE 42501)";
        let explained = external_schema_error(denied.to_string());
        assert!(
            explained.starts_with(denied)
                && explained.contains("template0")
                && explained.contains("pgcrypto")
                && explained.contains(EXTERNAL_URL_ENV),
            "the error names the extensions, why, and the role: {explained}"
        );
        let other = "applying canonical schema to the external test server failed: syntax error";
        assert_eq!(external_schema_error(other.to_string()), other);
    }

    /// A directory the harness did not make is never chmodded: one open to its
    /// group or others FAILS the boot — as the harness directory and as a
    /// template cache alike — with an error naming it and the remedy, and keeps
    /// the mode it had. Every fixture is made inside a scratch directory the
    /// test owns. (A directory another user owns, or one the harness made and
    /// cannot chmod, needs a fixture no unprivileged test can make; the
    /// predicate's own test covers those arms.)
    #[cfg(unix)]
    #[tokio::test]
    async fn a_directory_the_harness_did_not_make_is_refused_and_left_as_it_is() {
        use std::os::unix::fs::PermissionsExt;
        let mode_of = |path: &Path| {
            std::fs::metadata(path)
                .unwrap_or_else(|e| panic!("read the mode of {}: {e}", path.display()))
                .permissions()
                .mode()
                & 0o777
        };
        let plant_open = |path: &Path| {
            std::fs::create_dir(path).unwrap_or_else(|e| panic!("plant {}: {e}", path.display()));
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o775))
                .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
        };
        let scratch = tempfile::tempdir().expect("scratch directory");

        // An existing directory at the default location (`named` false), as a
        // project directory a tester made open there would be.
        let found = scratch.path().join("found");
        plant_open(&found);
        let refused = prepare_harness_dir(&found, false)
            .expect_err("a directory open to others that the harness did not make must fail");
        assert!(
            refused.contains(&found.display().to_string())
                && refused.contains("chmod 700")
                && refused.contains(HARNESS_DIR_ENV),
            "the error names the directory and the remedy: {refused}"
        );
        assert_eq!(
            mode_of(&found),
            0o775,
            "the harness changed the mode of a directory it did not make"
        );
        assert!(
            !found.join(MADE_MARKER).exists(),
            "the harness marked as its own a directory it did not make"
        );

        // A READY cache without the mark, inside a harness directory the
        // harness made (and marked).
        let dir = scratch.path().join("qontinui-test-pg");
        prepare_harness_dir(&dir, false).expect("make the harness directory");
        assert!(
            dir.join(MADE_MARKER).is_file() && mode_of(&dir) == 0o700,
            "a harness directory the harness makes is private and marked"
        );
        let unmarked = dir.join(format!("{TEMPLATE_PREFIX}unmarked"));
        plant_open(&unmarked);
        std::fs::write(unmarked.join(READY_FILE), "made by hand\n").expect("mark it READY");
        let lock = open_lock(&dir).expect("the cache lock");
        let entered = std::cell::Cell::new(false);
        let refused = enter_or_build(
            &lock,
            &unmarked,
            || async { Err::<Duration, _>("a READY cache is never rebuilt".to_string()) },
            || {
                entered.set(true);
                Ok(())
            },
        )
        .await
        .expect_err("a cache open to others that the harness did not make must not be entered");
        assert!(
            !entered.get() && refused.contains(&unmarked.display().to_string()),
            "the cache was refused before it was entered, by name: {refused}"
        );
        assert_eq!(
            mode_of(&unmarked),
            0o775,
            "the harness changed the mode of a cache it did not make"
        );
    }

    /// A harness directory that HARNESS_DIR_ENV names is refused, before
    /// anything is created, when an ancestor would let another user swap the
    /// tree under it — here one this user owns that others can write (0757) —
    /// and the error names that ancestor. That holds for an ancestor the path
    /// names on its way through a symlink, and for one only the symlink's
    /// target reaches: a private directory holding a symlink to a directory
    /// under an open one. The default location is not ancestor-checked
    /// ([`HarnessDir`]): it is created, private and marked. (An ancestor
    /// another user owns, or root's, cannot be made by a test; the predicate's
    /// own test covers those arms.)
    #[cfg(unix)]
    #[test]
    fn a_named_harness_directory_under_an_open_ancestor_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::tempdir().expect("scratch directory");
        let open = scratch.path().join("open");
        std::fs::create_dir(&open).expect("plant the ancestor");
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o757))
            .expect("open it to others");
        let open = std::fs::canonicalize(&open).expect("resolve the ancestor");
        let named = open.join("qontinui-test-pg");

        let refused = prepare_harness_dir(&named, true)
            .expect_err("a named directory under an ancestor others can write must fail");
        assert!(
            refused.contains(&format!("its ancestor {} is", open.display()))
                && refused.contains("not sticky"),
            "the error names the ancestor and what is wrong with it: {refused}"
        );
        assert!(
            !named.exists(),
            "a refused harness directory must not have been created"
        );

        // Through a symlink in that open directory to a private one: whoever
        // can write `open` can point the symlink elsewhere.
        let private = scratch.path().join("private");
        std::fs::create_dir(&private).expect("plant the symlink's target");
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700))
            .expect("make the target private");
        let link = open.join("link");
        std::os::unix::fs::symlink(&private, &link).expect("plant the symlink");
        let through_link = link.join("qontinui-test-pg");
        let refused = prepare_harness_dir(&through_link, true)
            .expect_err("a named directory through a symlink in an open directory must fail");
        assert!(
            refused.contains(&format!("its ancestor {} is", open.display())),
            "the error names the open directory holding the symlink: {refused}"
        );
        assert!(
            !private.join("qontinui-test-pg").exists(),
            "a refused harness directory must not have been created"
        );

        // Through a symlink in a PRIVATE directory to one under the open
        // directory: every ancestor the path names is safe, and only the
        // chain the symlink resolves to passes through `open`.
        let target = open.join("target");
        std::fs::create_dir(&target).expect("plant the symlink's target under the open one");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700))
            .expect("make the target private");
        let holder = scratch.path().join("holder");
        std::fs::create_dir(&holder).expect("plant the private directory");
        std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o700))
            .expect("make it private");
        let link = holder.join("link");
        std::os::unix::fs::symlink(&target, &link).expect("plant the symlink");
        let resolved_only = link.join("qontinui-test-pg");
        let refused = prepare_harness_dir(&resolved_only, true).expect_err(
            "a named directory whose symlink resolves under an open directory must fail",
        );
        assert!(
            refused.contains(&format!("its ancestor {} is", open.display())),
            "the error names the open directory the symlink resolves under: {refused}"
        );
        assert!(
            !target.join("qontinui-test-pg").exists(),
            "a refused harness directory must not have been created"
        );

        prepare_harness_dir(&named, false)
            .expect("the default location's ancestors are the test binaries' own");
        assert!(
            named.join(MADE_MARKER).is_file(),
            "a harness directory the harness makes is marked as its own"
        );
    }

    /// A directory is private to a user only when that user owns it and its
    /// group and others have no access at all: read, write or search.
    #[test]
    fn a_directory_is_private_only_to_its_owner_with_no_other_access() {
        let me = 1000;
        assert_eq!(privacy_problem(me, 0o700, me), None);
        assert_eq!(
            privacy_problem(me, 0o1700, me),
            None,
            "the sticky bit opens nothing"
        );
        for open in [0o740, 0o720, 0o710, 0o704, 0o702, 0o701, 0o775, 0o777] {
            let problem = privacy_problem(me, open, me)
                .unwrap_or_else(|| panic!("mode {open:o} is open to its group or others"));
            assert!(
                problem.contains(&format!("{open:o}")),
                "the problem names the mode: {problem}"
            );
        }
        for owner in [0, 1001] {
            let problem = privacy_problem(owner, 0o700, me)
                .unwrap_or_else(|| panic!("a directory uid {owner} owns is not this user's"));
            assert!(
                problem.contains(&format!("uid {owner}")),
                "the problem names the owner: {problem}"
            );
        }
    }

    /// An ancestor of a named harness directory is safe only when no user but
    /// this one can swap the tree under it: this user or root owns it, and
    /// others cannot write to it unless it is sticky (`/tmp`, `/dev/shm`). Its
    /// group's write is not judged ([`ancestor_problem`]): a directory made
    /// under umask 0002 (0775) is safe.
    #[test]
    fn an_ancestor_is_safe_only_when_no_other_user_can_swap_the_tree_under_it() {
        let me = 1000;
        for (owner, mode) in [
            (me, 0o755),
            (me, 0o700),
            (me, 0o775),
            (me, 0o770),
            (0, 0o755),
            (0, 0o775),
            (0, 0o711),
            (0, 0o1777),
            (me, 0o1777),
        ] {
            assert_eq!(
                ancestor_problem(owner, mode, me),
                None,
                "an ancestor uid {owner} owns with mode {mode:o} is safe"
            );
        }
        for (owner, mode) in [
            (me, 0o757),
            (me, 0o777),
            (me, 0o702),
            (0, 0o777),
            (0, 0o757),
        ] {
            let problem = ancestor_problem(owner, mode, me).unwrap_or_else(|| {
                panic!("an ancestor uid {owner} owns with mode {mode:o} can be swapped by others")
            });
            assert!(
                problem.contains(&format!("{mode:o}")) && problem.contains("not sticky"),
                "the problem names the mode: {problem}"
            );
        }
        for owner in [1001, 65534] {
            let problem = ancestor_problem(owner, 0o755, me)
                .unwrap_or_else(|| panic!("an ancestor uid {owner} owns is not safe"));
            assert!(
                problem.contains(&format!("uid {owner}")),
                "the problem names the owner: {problem}"
            );
        }
    }

    /// A process waiting for the cache lock says so — first after
    /// `first_report`, then every `report_every` — naming the holder the
    /// holder file names ([`LOCK_HOLDER_FILE`]). When that holder changes it
    /// restarts its clock, and it gives up only once ONE holder has held the
    /// lock past `give_up`, with an error naming that holder and neither
    /// calling it wedged nor inviting anyone to end it. Run on paused time
    /// with the real [`LOCK_PATIENCE`], so every instant is exact however
    /// loaded the box is.
    #[tokio::test(start_paused = true)]
    async fn a_cache_lock_wait_names_its_holder_and_times_each_holder_alone() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let template_dir = scratch.path().join(format!("{TEMPLATE_PREFIX}waiting"));
        let named = template_dir.display().to_string();
        let holder_file = scratch.path().join(LOCK_HOLDER_FILE);
        let waiter = open_lock(scratch.path()).expect("the cache lock");
        let holder = open_lock(scratch.path()).expect("another descriptor on the cache lock");
        holder.lock().expect("hold the lock, as a long build would");

        let first = LockHolder::this_process("first-key", "building");
        std::fs::write(&holder_file, first.render()).expect("name the first holder");
        let second_pid = a_live_pid_not_ours();
        let second = LockHolder {
            pid: second_pid,
            pid_ns: own_pid_namespace().map(str::to_owned),
            started: process_start(second_pid),
            key: "second-key".to_string(),
            action: "collecting".to_string(),
        };
        assert!(
            second.is_live(),
            "the second holder must be live to be timed"
        );

        let begun = tokio::time::Instant::now();
        let mut lines: Vec<String> = Vec::new();
        let refused = tokio::time::timeout(
            Duration::from_secs(7200),
            take_cache_lock(&waiter, false, &template_dir, LOCK_PATIENCE, |line| {
                // The lock changes hands as the third line (150 s) is said.
                if lines.len() == 2 {
                    std::fs::write(&holder_file, second.render()).expect("name the second holder");
                }
                lines.push(line);
            }),
        )
        .await
        .expect("the wait must give up by itself, not hang")
        .expect_err("a lock one holder keeps past the bound must fail the boot");
        let waited = begun.elapsed();

        let first_named = format!("held by pid {} (building first-key)", std::process::id());
        let second_named = format!("held by pid {second_pid} (collecting second-key)");
        assert!(
            lines
                .first()
                .is_some_and(|line| line.contains("after 30 s")),
            "the first line comes after first_report, not before: {lines:?}"
        );
        assert!(
            lines.iter().take(3).all(|line| line.contains(&first_named))
                && lines
                    .iter()
                    .skip(3)
                    .all(|line| line.contains(&second_named))
                && lines.len() > 3
                && lines.iter().all(|line| line.contains(&named)),
            "every line names the cache and the holder at that moment: {lines:?}"
        );
        assert!(
            waited >= Duration::from_secs(750) && waited < Duration::from_secs(760),
            "the clock restarts when the holder changes (150 s in), so the wait gives up 600 s \
             after that, not 600 s after it began: it gave up after {waited:?}"
        );
        assert!(
            refused.contains("gave up")
                && refused.contains(&named)
                && refused.contains(&format!("{second_named} for 600 s")),
            "the error names the cache, the holder and how long that holder held it: {refused}"
        );
        for said in lines.iter().chain([&refused]) {
            assert!(
                !said.contains("wedged") && !said.contains("ended") && !said.contains("kill"),
                "the wait judges no holder and invites no one to end it: {said}"
            );
        }

        // A holder the file does not name — none, or a process that is gone
        // — is timed as one unnamed holder.
        let mut gone =
            std::process::Command::new(std::env::current_exe().expect("this test binary's path"))
                .args(["--list", "--exact", "no-such-test"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("spawn a process that exits at once");
        gone.wait().expect("reap it");
        let gone_pid = gone.id();
        std::fs::write(
            &holder_file,
            LockHolder {
                pid: gone_pid,
                ..LockHolder::this_process("gone-key", "building")
            }
            .render(),
        )
        .expect("name a holder that is gone");
        let short = LockPatience {
            give_up: Duration::from_secs(60),
            ..LOCK_PATIENCE
        };
        let refused = take_cache_lock(&waiter, false, &template_dir, short, drop)
            .await
            .expect_err("an unnamed holder past the bound fails the boot");
        assert!(
            refused.contains("held by a process that has not named itself for 60 s")
                && !refused.contains(&format!("pid {gone_pid}")),
            "a holder that is gone is not named: {refused}"
        );

        // A lock let go in time is taken, and its taker names itself.
        let mut holding = Some(holder);
        take_cache_lock(&waiter, true, &template_dir, LOCK_PATIENCE, |_| {
            holding.take();
        })
        .await
        .expect("a lock let go in time is taken");
        let taker = read_lock_holder(scratch.path()).expect("the taker named itself");
        assert!(
            taker.is_same_process(&LockHolder::this_process("waiting", "building"))
                && taker.key == "waiting"
                && taker.action == "building",
            "the taker of an exclusive lock names itself, building its key: {taker:?}"
        );
        let _ = waiter.unlock();
    }

    /// A named holder that dies while the lock stays held — by another shared
    /// holder, or one that never named itself — is re-judged at the next
    /// progress line, and from then on is named as a process that has not
    /// named itself, its clock restarted; the file's text never changed. Run
    /// on paused time; the holder is a `sleep` this test kills as the first
    /// line is said. Unix only, for `sleep`.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn a_named_holder_that_dies_while_the_lock_is_held_becomes_unnamed() {
        struct Sleeper(Option<std::process::Child>);
        impl Sleeper {
            fn end(&mut self) {
                if let Some(mut child) = self.0.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        impl Drop for Sleeper {
            fn drop(&mut self) {
                self.end();
            }
        }

        let scratch = tempfile::tempdir().expect("scratch directory");
        let template_dir = scratch.path().join(format!("{TEMPLATE_PREFIX}waiting"));
        let waiter = open_lock(scratch.path()).expect("the cache lock");
        let holder = open_lock(scratch.path()).expect("another descriptor on the cache lock");
        holder
            .lock_shared()
            .expect("hold the lock, as a holder that never named itself would");

        let mut sleeper = Sleeper(Some(
            std::process::Command::new("sleep")
                .arg("3600")
                .spawn()
                .expect("spawn the named holder"),
        ));
        let pid = sleeper.0.as_ref().map(std::process::Child::id).unwrap_or(0);
        let named = LockHolder {
            pid,
            pid_ns: own_pid_namespace().map(str::to_owned),
            started: process_start(pid),
            key: "short-lived".to_string(),
            action: "entering".to_string(),
        };
        assert!(
            named.is_live(),
            "the named holder must be live to begin with"
        );
        std::fs::write(scratch.path().join(LOCK_HOLDER_FILE), named.render())
            .expect("name the holder");

        let mut lines: Vec<String> = Vec::new();
        let refused = tokio::time::timeout(
            Duration::from_secs(7200),
            take_cache_lock(
                &waiter,
                true,
                &template_dir,
                LockPatience {
                    give_up: Duration::from_secs(300),
                    ..LOCK_PATIENCE
                },
                |line| {
                    if lines.is_empty() {
                        sleeper.end();
                    }
                    lines.push(line);
                },
            ),
        )
        .await
        .expect("the wait must give up by itself, not hang")
        .expect_err("a lock nobody lets go of must fail the boot");

        let first = format!("held by pid {pid} (entering short-lived)");
        let unnamed = "held by a process that has not named itself";
        assert!(
            lines.first().is_some_and(|line| line.contains(&first)),
            "the first line names the live holder: {lines:?}"
        );
        assert!(
            lines.len() > 1 && lines.iter().skip(1).all(|line| line.contains(unnamed)),
            "from the next line on, the dead holder is unnamed: {lines:?}"
        );
        assert!(
            lines
                .get(1)
                .is_some_and(|line| line.contains(&format!("{unnamed} for 0 s"))),
            "its clock restarts when it becomes unnamed: {lines:?}"
        );
        assert!(
            refused.contains(&format!("{unnamed} for 300 s")),
            "the give-up names the unnamed holder: {refused}"
        );
        drop(holder);
    }

    /// The process that takes the cache lock removes the partial holder files
    /// processes no longer live left behind, and never a live process's, which
    /// may be mid-write. A file that reads as a record is judged by that record
    /// (pid and start); one that does not is removed only once it is older
    /// than [`HOLDER_PARTIAL_GRACE`] and its pid is not live here. The two
    /// dead cut-short cases (one fresh, kept; one aged, removed) name
    /// `u32::MAX`, which the test first asserts is not live, so they do not
    /// depend on a reaped pid staying free; the third, aged, names the test
    /// runner's own live pid (this test process's parent), and is kept.
    #[tokio::test]
    async fn a_lock_taker_sweeps_only_dead_processes_partial_holder_files() {
        let gone = || {
            let mut child =
                std::process::Command::new(std::env::current_exe().expect("this test binary"))
                    .args(["--list", "--exact", "no-such-test"])
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .expect("spawn a process that exits at once");
            child.wait().expect("reap it");
            child.id()
        };
        let age = |path: &Path| {
            let aged = SystemTime::now() - HOLDER_PARTIAL_GRACE - Duration::from_secs(60);
            std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .and_then(|file| file.set_modified(aged))
                .unwrap_or_else(|e| panic!("age {}: {e}", path.display()));
        };
        let scratch = tempfile::tempdir().expect("scratch directory");
        let dir = scratch.path();
        let partial = |pid: u32, nonce: &str| dir.join(format!("{LOCK_HOLDER_FILE}.{pid}-{nonce}"));
        let record = |pid: u32| {
            LockHolder {
                pid,
                ..LockHolder::this_process("partial", "building")
            }
            .render()
        };
        assert!(
            !crate::runner_breadcrumb::pid_is_live(u32::MAX),
            "u32::MAX must name no process"
        );

        let dead_recorded = partial(gone(), "a");
        let aged_cut_short = partial(u32::MAX, "b");
        let fresh_cut_short = partial(u32::MAX, "e");
        let own = partial(std::process::id(), "c");
        let live_aged_cut_short = partial(a_live_pid_not_ours(), "d");
        std::fs::write(&dead_recorded, record(gone_pid_of(&dead_recorded))).expect("plant");
        std::fs::write(&aged_cut_short, "pid=").expect("plant");
        std::fs::write(&fresh_cut_short, "").expect("plant");
        std::fs::write(&own, record(std::process::id())).expect("plant");
        std::fs::write(&live_aged_cut_short, "").expect("plant");
        age(&aged_cut_short);
        age(&live_aged_cut_short);
        std::fs::write(dir.join(LOCK_HOLDER_FILE), record(std::process::id())).expect("plant");

        let lock = open_lock(dir).expect("the cache lock");
        let mut lines = Vec::new();
        take_cache_lock(
            &lock,
            true,
            &dir.join(format!("{TEMPLATE_PREFIX}sweeping")),
            LOCK_PATIENCE,
            |line| lines.push(line),
        )
        .await
        .expect("an uncontested lock is taken");
        let _ = lock.unlock();

        for removed in [&dead_recorded, &aged_cut_short] {
            assert!(
                !removed.exists(),
                "a dead process's partial holder file {} survived: {lines:?}",
                removed.display()
            );
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains(&removed.display().to_string())),
                "its removal was not said: {lines:?}"
            );
        }
        assert!(
            fresh_cut_short.exists(),
            "a cut-short partial younger than the grace was removed: it may be mid-write"
        );
        for kept in [&own, &live_aged_cut_short] {
            assert!(
                kept.exists(),
                "a live process's partial holder file {} was removed",
                kept.display()
            );
        }
        assert!(
            dir.join(LOCK_HOLDER_FILE).is_file(),
            "the holder file itself is not a partial"
        );
    }

    /// The root scan of [`ReapChildCluster::of_child`] takes only a root whose
    /// owner record is the child's — its pid, this namespace, its start. Four
    /// roots are planted under the same pid, none with a postmaster: one whose
    /// record is the child's (taken wherever a start is read), and three it
    /// leaves because each may be a live peer's — one whose record names
    /// another start, one whose record names another PID namespace, and one
    /// with no record at all.
    #[test]
    fn the_child_root_scan_takes_only_roots_the_child_created() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let dir = scratch.path();
        let pid = std::process::id();
        let started = process_start(pid);
        let plant = |nonce: &str, owner: Option<Owner>| {
            let root = dir.join(format!("{RUN_PREFIX}{pid}-{nonce}"));
            std::fs::create_dir(&root).expect("plant a root");
            if let Some(owner) = owner {
                write_owner(&root, &owner).expect("write its owner record");
            }
            root
        };
        let childs = Owner::this_process("scan", None);
        let other_start = Owner {
            started: Some(childs.started.map_or(1, |at| at + 1)),
            ..childs.clone()
        };
        let other_ns = Owner {
            pid_ns: Some("pid:[another]".to_string()),
            ..childs.clone()
        };
        let created = plant("created", Some(childs));
        let peer_start = plant("peer-start", Some(other_start));
        let peer_ns = plant("peer-ns", Some(other_ns));
        let unrecorded = plant("unrecorded", None);

        drop(ReapChildCluster::of_child(
            &dir.join("no-report"),
            pid,
            started,
            Some(dir),
        ));

        assert_eq!(
            !created.exists(),
            started.is_some(),
            "a root whose record is the child's is taken wherever a start is read"
        );
        for left in [&peer_start, &peer_ns, &unrecorded] {
            assert!(
                left.exists(),
                "{} is not the child's, and was taken",
                left.display()
            );
        }
    }

    /// A partial holder file another process removed first — a second taker
    /// sweeping under the shared lock at the same moment — is gone either way:
    /// its `NotFound` says nothing, while any other failure is said.
    #[test]
    fn a_partial_another_taker_removed_first_is_not_a_failure() {
        let path = Path::new("/nowhere/.lock.holder.7-x");
        assert_eq!(
            partial_removal_line(
                path,
                7,
                Err(std::io::Error::from(std::io::ErrorKind::NotFound))
            ),
            None,
            "a partial already removed is not a failure"
        );
        let refused = partial_removal_line(
            path,
            7,
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        )
        .expect("any other failure is said");
        assert!(refused.contains("could not remove"), "{refused}");
        assert!(
            partial_removal_line(path, 7, Ok(())).is_some_and(|line| line.starts_with("removed ")),
            "a removal is said"
        );
    }

    /// A process that takes the lock but cannot name itself removes the record
    /// its predecessor left, so a waiter names it as unnamed rather than name
    /// that predecessor, which may still be live. The write is made to fail by
    /// a directory where this process's partial file goes.
    #[test]
    fn a_holder_that_cannot_name_itself_removes_the_previous_record() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let dir = scratch.path();
        let previous = LockHolder {
            pid: a_live_pid_not_ours(),
            ..LockHolder::this_process("earlier", "entering")
        };
        std::fs::write(dir.join(LOCK_HOLDER_FILE), previous.render())
            .expect("plant the previous holder's record");
        let partial = dir.join(format!(
            "{LOCK_HOLDER_FILE}.{}-{}",
            std::process::id(),
            process_nonce()
        ));
        std::fs::create_dir(&partial).expect("block this process's partial file");
        write_lock_holder(dir, "now", "building");
        assert!(
            read_lock_holder(dir).is_none(),
            "the previous holder's record survived a failed write, so a waiter would name it"
        );
    }

    /// The pid in a partial holder file's name.
    fn gone_pid_of(partial: &Path) -> u32 {
        partial
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(&format!("{LOCK_HOLDER_FILE}.")))
            .and_then(|rest| rest.split_once('-'))
            .and_then(|(pid, _)| pid.parse().ok())
            .expect("a partial holder file's name carries a pid")
    }

    static PANICKING_TEMPLATE: DerivedTemplate =
        DerivedTemplate::new("harness_panics", provision_by_panicking);

    fn provision_by_panicking(_url: String) -> ProvisionFuture {
        Box::pin(async { planted_provisioning_panic() })
    }

    fn planted_provisioning_panic() -> Result<(), String> {
        panic!("the planted provisioning panic")
    }

    /// A provisioning step that PANICS fails the request that ran it and every
    /// later one, each with the panic's own message: the template it left
    /// behind is not created again, which would fail on that leftover and hide
    /// the panic.
    #[test]
    fn a_provisioning_step_that_panics_fails_every_request_with_its_panic() {
        for request in ["first", "second"] {
            let failure =
                std::panic::catch_unwind(|| fresh_database_from_blocking(&PANICKING_TEMPLATE))
                    .expect_err("a template whose provisioning panicked must not be handed out");
            let message = panic_message(failure.as_ref());
            assert!(
                message.contains("the planted provisioning panic"),
                "the {request} request must fail with the step's own panic, not a \
                 leftover's: {message}"
            );
        }
    }

    /// A start attempt's failure is judged by the lines that attempt wrote to the
    /// server log, which every attempt appends to: a port collision is retried,
    /// and a different failure in the attempt after it is reported, carrying
    /// only that attempt's lines — not retried because the collision is still in
    /// the log.
    #[tokio::test]
    async fn a_start_failure_is_judged_by_what_its_own_attempt_logged() {
        use std::io::Write as _;
        let scratch = tempfile::tempdir().expect("scratch directory");
        let log = scratch.path().join(START_LOG);
        let append = |line: &str| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
                .and_then(|mut file| file.write_all(line.as_bytes()))
                .expect("append to the server log");
        };
        let attempts = std::cell::Cell::new(0);
        let failure = retry_port_collisions::<(), (), _>(&log, |_port| {
            attempts.set(attempts.get() + 1);
            append(if attempts.get() == 1 {
                "LOG:  could not bind IPv4 address \"127.0.0.1\": Address already in use\n\
                 FATAL:  could not create any TCP/IP sockets\n"
            } else {
                "FATAL:  data directory \"pg-data\" has invalid permissions\n"
            });
            std::future::ready(Err(("the server did not start".to_string(), ())))
        })
        .await
        .expect_err("no attempt starts a server");
        assert_eq!(
            attempts.get(),
            2,
            "a collision is retried once, and the different failure after it is reported, not \
             retried: {failure}"
        );
        assert!(
            failure.contains("invalid permissions") && !failure.contains("could not bind"),
            "the report must carry the failing attempt's lines and not the earlier collision's: \
             {failure}"
        );

        let whole = log_tail(&log, u64::MAX);
        assert!(
            whole.contains("could not bind") && whole.contains("invalid permissions"),
            "a log shorter than the offset (replaced, not appended to) is read whole: {whole}"
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
        // Another nonce of this run's own: fixed names would collide with a
        // concurrent run's on a shared external server, and a run killed before
        // its drops would leave them to fail every later run there.
        let other =
            std::iter::repeat_with(|| format!("{:08x}", uuid::Uuid::new_v4().as_u128() as u32))
                .find(|other| other != nonce)
                .expect("an endless supply of candidates");
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
            // As `build_template` makes a cache: private, and marked as the
            // harness's.
            move || async move {
                create_private_dir(building).map_err(|e| e.to_string())?;
                mark_made(building)?;
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
        // The record names that live process with its own start, as the
        // process would have recorded itself: `this_process` supplies this
        // process's start, which no other pid shares.
        let live = a_live_pid_not_ours();
        write_owner(
            &root,
            &Owner {
                pid: live,
                started: process_start(live),
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

    /// A recorded owner whose pid is in use is live only if the process using it
    /// started when the owner did: a pid is handed out again once its process is
    /// gone (at once on Windows), and the later process is not the owner. A
    /// record with no start is judged by the pid alone.
    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn an_owner_whose_pid_a_later_process_holds_is_dead() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let root = scratch.path().join(format!("{RUN_PREFIX}reused"));
        std::fs::create_dir(&root).expect("plant a root");
        let pid = a_live_pid_not_ours();
        let started = process_start(pid).expect("the start of a live process can be read");
        let owner = |started| Owner {
            pid,
            started,
            ..Owner::this_process("reused", None)
        };
        assert!(
            owner_may_be_live(&root, &owner(Some(started))),
            "the live process that started when the record says is the owner"
        );
        assert!(
            !owner_may_be_live(&root, &owner(Some(started + 1))),
            "a live pid whose process started at another time is a later process: the \
             recorded owner is dead"
        );
        assert!(
            owner_may_be_live(&root, &owner(None)),
            "a record with no start falls back to the pid alone"
        );
    }

    /// A pidfd sees the process it holds exit, and signals nothing once that
    /// process is gone: `ESRCH`, where a signal by pid would reach whatever
    /// process holds the pid by then. No test can make the kernel hand that pid
    /// to another process on demand, so this shows the held signal failing, not
    /// a later process being spared.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_pidfd_sees_its_process_exit_and_signals_nothing_after() {
        /// Kills the sleeping child if the test ends before it is reaped.
        struct Sleeping(std::process::Child);
        impl Drop for Sleeping {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Sleeping(
            std::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .expect("spawn sleep"),
        );
        let held = Pidfd::open(child.0.id()).expect("hold the child with a pidfd");
        assert!(
            !held.exits_within(Duration::from_millis(200)),
            "a sleeping process has not exited"
        );
        held.signal(libc::SIGTERM)
            .expect("signal the held child through its pidfd");
        assert!(
            held.exits_within(Duration::from_secs(10)),
            "the held child did not exit after SIGTERM"
        );
        let _ = child.0.wait();
        let refused = held
            .signal(libc::SIGTERM)
            .expect_err("a process that is gone is not signalled");
        assert_eq!(refused.raw_os_error(), Some(libc::ESRCH), "{refused}");
    }
}
