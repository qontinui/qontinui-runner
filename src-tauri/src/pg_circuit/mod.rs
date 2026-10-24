//! The runner's PostgreSQL checkout point, behind one circuit breaker.
//!
//! Plan `2026-10-19-runner-pg-unreachable-costs-every-caller-a-pool-timeout`.
//!
//! THE DEFECT. Every `pool.get()` is bounded at 5 s (create, wait, recycle),
//! and nothing remembered that the last attempt failed, so with PostgreSQL
//! unreachable EVERY checkout paid the full connect attempt again. On Windows
//! a connect to a dead loopback port is ~2 s of SYN retries per resolved
//! address, so each DB-backed call cost 2–5 s, and `/health` paid it too.
//!
//! THE FIX. [`GuardedPool`] wraps the deadpool pool and an [`crate::circuit`]
//! breaker. It lives in the LIB crate because `env_agent` (lib) receives a
//! clone of the boot pool, and a bin-side breaker could not cover it.
//!
//! * The first connect-class failure ([`classify`], plan D2) opens the
//!   breaker. Every later caller gets [`PgCheckoutError::Unavailable`]
//!   immediately, with no I/O.
//! * A caller already WAITING in `get()` when the breaker trips is released at
//!   once (the checkout is raced against the trip; deadpool's `get()` is
//!   drop-safe, which the assumption tests below pin).
//! * A wait timeout (`Timeout(Wait)`) is NEVER read as unreachability. It is
//!   reported as [`PgCheckoutError::Saturated`] and counted.
//! * A trip records its [`CauseClass`]: a refused connect is evidence the
//!   database is unreachable, but a connect TIMEOUT is not — the server may be
//!   alive behind a stalled process or box — so only `connect_refused` lets a
//!   reader say "unreachable".
//! * While the breaker is not Closed, ONE background prober per pool is the
//!   only actor that touches the network. Its trials back off 1 s, 2 s, 4 s,
//!   8 s, then hold at 15 s, and a successful trial closes the breaker. No
//!   caller is used as the probe while a prober can run.
//! * The prober runs on an explicit process-lived runtime
//!   ([`GuardedPool::start_background`]) or on a caller's MULTI-THREAD
//!   runtime. A current-thread runtime only runs while it is `block_on`'d, so
//!   nothing is ever spawned there; with no runtime to use (a CLI), the first
//!   caller after `retry_at` runs one single-flight trial inline.
//!
//! The method layer above keeps returning `Result<T, String>`.
//! [`PgCheckoutError::Unavailable`]'s `Display` starts with the stable token
//! [`DB_UNAVAILABLE_TOKEN`], and [`is_db_unavailable`] classifies those
//! strings. HTTP routing does not parse anything: `pg_guard` decides by the
//! breaker's STATE before a handler runs.

#[cfg(test)]
pub mod test_support;

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use crate::circuit::{
    Admission, Backoff, Circuit, CircuitConfig, CircuitSnapshot, CircuitState, NonTripKind,
    Outcome, ProbeMode,
};

/// The stable prefix of every "database unavailable" error string. It is the
/// same code `pg_guard`'s 503 envelope serves, and a test pins the literal.
pub const DB_UNAVAILABLE_TOKEN: &str = "DATABASE_UNAVAILABLE:";

/// The longest the prober ever waits between trials. Pinned below `/health`'s
/// 30 s staleness bound by a test, so an Open snapshot cannot go stale between
/// two trials.
pub const PROBER_BACKOFF_CAP: Duration = Duration::from_secs(15);

/// Bound on one default trial (checkout + `SELECT 1`). Matches the pool's own
/// 5 s create timeout, and is applied by the trial itself because not every
/// pool carries timeouts (`env_agent::publish_pg_pool_from_url`).
pub const DEFAULT_TRIAL_TIMEOUT: Duration = Duration::from_secs(5);

/// Bound on a replacement trial installed with
/// [`GuardedPool::install_trial_until_first_success`] (the degraded-boot
/// provisioning run, which also executes DDL).
pub const INSTALLED_TRIAL_TIMEOUT: Duration = Duration::from_secs(120);

/// The PostgreSQL breaker configuration (plan D2/D3).
pub const PG_CIRCUIT_CONFIG: CircuitConfig = CircuitConfig {
    failure_threshold: 1,
    success_threshold: 1,
    backoff: Backoff {
        initial: Duration::from_secs(1),
        factor: 2,
        cap: PROBER_BACKOFF_CAP,
    },
    half_open_max_probes: 1,
    probe_mode: ProbeMode::Background,
};

/// Why the breaker opened: the runner booted without its database, or it
/// lost the database later.
pub const CAUSE_BOOT: &str = "boot";
pub const CAUSE_RUNTIME: &str = "runtime";

/// What KIND of evidence the latest connect-class failure was (plan D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CauseClass {
    /// A client-side connect failure with no server reply (refused, reset):
    /// the database is unreachable.
    ConnectRefused,
    /// The connect or startup did not complete in time. The server may be
    /// silent — or this process or box may be stalled — so this is NOT
    /// evidence of unreachability.
    ConnectTimeout,
    /// A trial failed in a way that names neither (e.g. the degraded-boot
    /// provisioning run).
    Unknown,
}

impl CauseClass {
    pub fn as_str(self) -> &'static str {
        match self {
            CauseClass::ConnectRefused => "connect_refused",
            CauseClass::ConnectTimeout => "connect_timeout",
            CauseClass::Unknown => "unknown",
        }
    }
}

/// `true` when an error string (as the `database/pg` method layer returns it)
/// carries the database-unavailable token.
pub fn is_db_unavailable(message: &str) -> bool {
    message.contains(DB_UNAVAILABLE_TOKEN)
}

/// The facts behind a fail-fast rejection.
#[derive(Debug, Clone, PartialEq)]
pub struct DbUnavailable {
    /// When the outage began (the ORIGINAL trip).
    pub since: Option<SystemTime>,
    /// [`CAUSE_BOOT`] or [`CAUSE_RUNTIME`].
    pub cause: String,
    pub cause_class: CauseClass,
    pub last_error: Option<String>,
    /// Until the prober's next trial.
    pub retry_after: Option<Duration>,
    /// The connect timeout a `connect_timeout` ran out of, for the message.
    pub connect_timeout: Option<Duration>,
}

/// RFC 3339 (UTC, seconds) for a wall-clock instant.
pub fn rfc3339(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl DbUnavailable {
    /// The one human-readable reason, worded by what the breaker actually
    /// knows (also `pg_guard`'s 503 message). A connect that merely TIMED OUT
    /// is never called "unreachable".
    pub fn reason(&self) -> String {
        match (self.cause.as_str(), self.cause_class) {
            (CAUSE_BOOT, _) => "this runner booted without its database".to_string(),
            (_, CauseClass::ConnectRefused) => {
                "PostgreSQL is unreachable (the connection was refused)".to_string()
            }
            (_, CauseClass::ConnectTimeout) => match self.connect_timeout {
                Some(t) => format!("PostgreSQL did not answer within {} ms", t.as_millis()),
                None => "PostgreSQL did not answer in time".to_string(),
            },
            (_, CauseClass::Unknown) => "PostgreSQL is not available".to_string(),
        }
    }
}

impl std::fmt::Display for DbUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{DB_UNAVAILABLE_TOKEN} {}", self.reason())?;
        if let Some(since) = self.since {
            write!(f, " (since {})", rfc3339(since))?;
        }
        if let Some(r) = self.retry_after {
            write!(f, "; next reconnect attempt in {} ms", r.as_millis())?;
        }
        if let Some(e) = &self.last_error {
            write!(f, "; last error: {e}")?;
        }
        Ok(())
    }
}

/// Why a checkout failed.
#[derive(Debug)]
pub enum PgCheckoutError {
    /// The breaker is open. Returned immediately, with no I/O.
    Unavailable(DbUnavailable),
    /// Every slot was busy for the whole wait timeout. Never trips the
    /// breaker.
    Saturated { waited: Option<Duration> },
    /// Any other pool error (the server answered with an error, a post-create
    /// hook failed, a programming error).
    Pool(deadpool_postgres::PoolError),
}

impl std::fmt::Display for PgCheckoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PgCheckoutError::Unavailable(u) => u.fmt(f),
            PgCheckoutError::Saturated { waited } => match waited {
                Some(w) => write!(
                    f,
                    "PG pool saturated: no free connection slot within {} ms",
                    w.as_millis()
                ),
                None => write!(f, "PG pool saturated: no free connection slot"),
            },
            PgCheckoutError::Pool(e) => write!(f, "{}", render_pool_error(e)),
        }
    }
}

impl std::error::Error for PgCheckoutError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PgCheckoutError::Pool(e) => Some(e),
            _ => None,
        }
    }
}

impl PgCheckoutError {
    pub fn is_unavailable(&self) -> bool {
        matches!(self, PgCheckoutError::Unavailable(_))
    }
}

/// A pool error rendered with its SQLSTATE when the server sent one (the
/// bare `Display` of `tokio_postgres::Error` is the useless `db error`).
fn render_pool_error(e: &deadpool_postgres::PoolError) -> String {
    match e {
        deadpool_postgres::PoolError::Backend(be) => match be.as_db_error() {
            Some(db) => format!("{e}: [{}] {}", db.code().code(), db.message()),
            None => {
                let mut out = e.to_string();
                let mut src = std::error::Error::source(be);
                while let Some(cause) = src {
                    out.push_str(&format!(" <- {cause}"));
                    src = cause.source();
                }
                out
            }
        },
        other => other.to_string(),
    }
}

/// What a client-side failure (no server reply) says, read off the
/// `io::Error` in its `source()` chain. Only a refused / reset / aborted
/// socket is evidence the database is unreachable; tokio-postgres reports its
/// own `connect_timeout=` (and an OS `ETIMEDOUT`) as a `TimedOut` io error,
/// which is a TIMEOUT; DNS, TLS and client-config failures name neither.
pub fn client_failure_class(e: &(dyn std::error::Error + 'static)) -> CauseClass {
    use std::io::ErrorKind;
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = cur {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            return match io.kind() {
                ErrorKind::ConnectionRefused
                | ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted => CauseClass::ConnectRefused,
                ErrorKind::TimedOut => CauseClass::ConnectTimeout,
                _ => CauseClass::Unknown,
            };
        }
        cur = err.source();
    }
    CauseClass::Unknown
}

/// Plan D2: what a pool error says about reachability. Only connect-class
/// evidence trips the breaker (served policy `verification-and-evidence`
/// `an-unknown-input-must-not-fire-a-detector`: the breaker is a detecting
/// consumer).
///
/// | Outcome | Meaning | Trips? |
/// |---|---|---|
/// | `Timeout(Create)` | connect/startup did not complete in time | yes, `connect_timeout` |
/// | `Backend(e)`, no `DbError` | client-side connect failure | yes, classed by its `io::ErrorKind` |
/// | `Backend(e)` with a `DbError` | the server ANSWERED | no (backend error) |
/// | `Timeout(Wait)` | no permit within the wait timeout | never |
/// | `Timeout(Recycle)` | not returned by `get()` in deadpool 0.12.3 | no (defensive) |
/// | `PostCreateHook` | the server answered, then `SET search_path` failed | no |
/// | `Closed`, `NoRuntimeSpecified` | programming error | no |
pub fn classify(e: &deadpool_postgres::PoolError) -> (Outcome, Option<CauseClass>) {
    use deadpool_postgres::{PoolError, TimeoutType};
    match e {
        PoolError::Timeout(TimeoutType::Create) => (
            Outcome::TripFailure(render_pool_error(e)),
            Some(CauseClass::ConnectTimeout),
        ),
        PoolError::Backend(be) if be.as_db_error().is_none() => (
            Outcome::TripFailure(render_pool_error(e)),
            Some(client_failure_class(be)),
        ),
        PoolError::Backend(_) | PoolError::PostCreateHook(_) => {
            (Outcome::NonTripFailure(NonTripKind::BackendError), None)
        }
        PoolError::Timeout(TimeoutType::Wait) => {
            (Outcome::NonTripFailure(NonTripKind::WaitTimeout), None)
        }
        PoolError::Timeout(TimeoutType::Recycle)
        | PoolError::Closed
        | PoolError::NoRuntimeSpecified => (Outcome::NonTripFailure(NonTripKind::Ambiguous), None),
    }
}

tokio::task_local! {
    /// Set inside a prober trial: checkouts made by the trial itself (and by
    /// anything it awaits inline, such as the degraded-boot provisioning run)
    /// go straight to the inner pool instead of being rejected by the breaker
    /// they are trying to close. Private to this module: only a trial enters
    /// it, so no ordinary caller can bypass the breaker.
    static TRIAL_SCOPE: ();
}

fn in_trial() -> bool {
    TRIAL_SCOPE.try_with(|_| ()).is_ok()
}

/// What one probe observed: a prober trial, or a [`GuardedPool::probe_now`].
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeObservation {
    pub at: SystemTime,
    pub at_instant: Instant,
    pub duration: Duration,
    /// `Some(true)` the server answered; `Some(false)` a connect-class
    /// failure; `None` the probe could not tell (the pool was saturated, or
    /// the probe ran out its bound with the breaker still Closed).
    pub reachable: Option<bool>,
    pub error: Option<String>,
}

pub type TrialFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
/// A replacement trial. Returns `Ok` when the database is reachable (and, for
/// the provisioning trial, provisioned).
pub type TrialFn = Arc<dyn Fn() -> TrialFuture + Send + Sync>;

/// Build a [`TrialFn`] from an async closure.
pub fn trial_fn<F, Fut>(f: F) -> TrialFn
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    Arc::new(move || -> TrialFuture { Box::pin(f()) })
}

/// A trial failure, with what it says about the cause when it says anything.
type TrialResult = Result<(), (Option<CauseClass>, String)>;

struct Shared {
    circuit: Arc<Circuit>,
    prober_running: AtomicBool,
    background: OnceLock<tokio::runtime::Handle>,
    installed_trial: Mutex<Option<TrialFn>>,
    last_probe: Mutex<Option<ProbeObservation>>,
    cause_class: Mutex<CauseClass>,
    trial_timeout: Duration,
}

/// The guarded pool. Cheap to clone; clones share one breaker and one prober.
#[derive(Clone)]
pub struct GuardedPool {
    inner: deadpool_postgres::Pool,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for GuardedPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuardedPool")
            .field("state", &self.shared.circuit.state())
            .finish()
    }
}

/// Resets the prober flag however the prober task ends — including when the
/// runtime it was spawned on drops the task, polled or not — so a later
/// checkout can respawn it. Created BEFORE the spawn and moved into the task,
/// so even a task dropped before its first poll clears the flag.
struct ProberFlag {
    shared: Arc<Shared>,
    armed: bool,
}

impl Drop for ProberFlag {
    fn drop(&mut self) {
        if self.armed {
            self.shared.prober_running.store(false, Ordering::Release);
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl GuardedPool {
    /// Wrap a pool with the PostgreSQL breaker configuration.
    pub fn new(inner: deadpool_postgres::Pool) -> Self {
        Self::with_circuit(
            inner,
            Arc::new(Circuit::new(PG_CIRCUIT_CONFIG)),
            DEFAULT_TRIAL_TIMEOUT,
        )
    }

    /// Wrap a pool with an explicit breaker (tests use short backoffs).
    pub fn with_circuit(
        inner: deadpool_postgres::Pool,
        circuit: Arc<Circuit>,
        trial_timeout: Duration,
    ) -> Self {
        Self {
            inner,
            shared: Arc::new(Shared {
                circuit,
                prober_running: AtomicBool::new(false),
                background: OnceLock::new(),
                installed_trial: Mutex::new(None),
                last_probe: Mutex::new(None),
                cause_class: Mutex::new(CauseClass::Unknown),
                trial_timeout,
            }),
        }
    }

    /// Give the pool its process-lived runtime for the prober, and start the
    /// prober if the breaker is already open (a degraded boot). The first call
    /// wins; later calls only (re)start the prober.
    pub fn start_background(&self, handle: tokio::runtime::Handle) {
        let _ = self.shared.background.set(handle);
        self.ensure_prober();
    }

    /// Check out a connection.
    ///
    /// * Breaker Closed: the inner checkout is raced against a trip, so a
    ///   caller already waiting when another caller trips the breaker is
    ///   released at once with `Unavailable`. Its error is classified (D2).
    /// * Breaker Open or HalfOpen: `Unavailable`, immediately, no I/O — except
    ///   that when no prober can run and the next trial is due, this caller
    ///   runs that one trial inline first.
    pub async fn get(&self) -> Result<deadpool_postgres::Object, PgCheckoutError> {
        if in_trial() {
            return self.inner.get().await.map_err(PgCheckoutError::Pool);
        }
        let circuit = &self.shared.circuit;
        if let Admission::Reject(snapshot) = circuit.allow() {
            if !self.ensure_prober() && self.try_inline_trial().await {
                // The inline trial closed the breaker: fall through.
            } else {
                return Err(PgCheckoutError::Unavailable(self.unavailable(snapshot)));
            }
        }
        let checkout = self.inner.get();
        tokio::pin!(checkout);
        let result = tokio::select! {
            biased;
            r = &mut checkout => r,
            _ = circuit.wait_opened() => {
                self.ensure_prober();
                return Err(PgCheckoutError::Unavailable(self.unavailable(circuit.snapshot())));
            }
        };
        match result {
            Ok(obj) => Ok(obj),
            Err(e) => Err(self.record_error(e)),
        }
    }

    /// The facts behind the current outage, or `None` while Closed.
    pub fn unavailable_now(&self) -> Option<DbUnavailable> {
        if self.is_available() {
            return None;
        }
        Some(self.unavailable(self.shared.circuit.snapshot()))
    }

    fn unavailable(&self, s: CircuitSnapshot) -> DbUnavailable {
        DbUnavailable {
            since: s.since,
            cause: s.cause.unwrap_or_else(|| CAUSE_RUNTIME.to_string()),
            cause_class: self.cause_class(),
            last_error: s.last_error,
            retry_after: s.retry_after,
            connect_timeout: self.inner.timeouts().create,
        }
    }

    fn set_cause_class(&self, c: CauseClass) {
        *lock(&self.shared.cause_class) = c;
    }

    /// The class of the latest connect-class failure (meaningful while the
    /// breaker is not Closed).
    pub fn cause_class(&self) -> CauseClass {
        *lock(&self.shared.cause_class)
    }

    fn record_error(&self, e: deadpool_postgres::PoolError) -> PgCheckoutError {
        let (outcome, class) = classify(&e);
        let circuit = &self.shared.circuit;
        match outcome {
            Outcome::TripFailure(_) => {
                if circuit.is_closed() {
                    self.set_cause_class(class.unwrap_or(CauseClass::Unknown));
                }
                circuit.record(outcome);
                self.ensure_prober();
                PgCheckoutError::Unavailable(self.unavailable(circuit.snapshot()))
            }
            Outcome::NonTripFailure(NonTripKind::WaitTimeout) => {
                circuit.record(outcome);
                PgCheckoutError::Saturated {
                    waited: self.inner.timeouts().wait,
                }
            }
            other => {
                circuit.record(other);
                PgCheckoutError::Pool(e)
            }
        }
    }

    /// `true` while the breaker is Closed (one atomic load).
    pub fn is_available(&self) -> bool {
        self.shared.circuit.is_closed()
    }

    pub fn breaker_state(&self) -> CircuitState {
        self.shared.circuit.state()
    }

    pub fn breaker_snapshot(&self) -> CircuitSnapshot {
        self.shared.circuit.snapshot()
    }

    /// The most recent probe observation (a prober trial or a `probe_now`).
    pub fn last_probe(&self) -> Option<ProbeObservation> {
        lock(&self.shared.last_probe).clone()
    }

    pub fn status(&self) -> deadpool_postgres::Status {
        self.inner.status()
    }

    /// Open the breaker from outside (the degraded-boot path: no database at
    /// boot). Spawns nothing — the prober starts with
    /// [`GuardedPool::start_background`] or on a later checkout.
    pub fn force_open(&self, cause: &str, last_error: impl Into<String>) {
        self.set_cause_class(CauseClass::Unknown);
        self.shared.circuit.force_open(cause, last_error);
    }

    /// Replace the prober's trial until it first succeeds, then fall back to
    /// the default checkout + `SELECT 1`. Checkouts the trial makes are not
    /// refused by the breaker (a private task-local trial scope), so a
    /// provisioning run can use the ordinary `PgDb` methods. Bounded by
    /// [`INSTALLED_TRIAL_TIMEOUT`].
    pub fn install_trial_until_first_success(&self, trial: TrialFn) {
        *lock(&self.shared.installed_trial) = Some(trial);
    }

    /// Where a prober may run: the background handle, else the caller's
    /// runtime when it is multi-thread. Never a current-thread runtime, which
    /// only runs while something is `block_on`'d on it.
    fn prober_runtime(&self) -> Option<tokio::runtime::Handle> {
        if let Some(h) = self.shared.background.get() {
            return Some(h.clone());
        }
        let h = tokio::runtime::Handle::try_current().ok()?;
        match h.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::CurrentThread => None,
            _ => Some(h),
        }
    }

    /// Start the background prober if the breaker is not Closed and none is
    /// running. Returns `true` when a prober is running (or the breaker is
    /// Closed), `false` when no runtime can host one.
    pub fn ensure_prober(&self) -> bool {
        if self.shared.circuit.is_closed() {
            return true;
        }
        if self.shared.prober_running.load(Ordering::Acquire) {
            return true;
        }
        let Some(handle) = self.prober_runtime() else {
            return false;
        };
        if self.shared.prober_running.swap(true, Ordering::AcqRel) {
            return true;
        }
        let this = self.clone();
        let flag = ProberFlag {
            shared: self.shared.clone(),
            armed: true,
        };
        handle.spawn(async move { this.run_prober(flag).await });
        true
    }

    async fn run_prober(self, mut flag: ProberFlag) {
        let circuit = self.shared.circuit.clone();
        loop {
            if circuit.is_closed() {
                // Give the slot back, then look once more: a trip between the
                // check above and this clear saw the flag still set and
                // spawned nothing, so this prober must keep going.
                self.shared.prober_running.store(false, Ordering::Release);
                if circuit.is_closed() || self.shared.prober_running.swap(true, Ordering::AcqRel) {
                    flag.armed = false;
                    return;
                }
                continue;
            }
            if let Some(wait) = circuit.retry_after() {
                if !wait.is_zero() {
                    tokio::time::sleep(wait).await;
                }
            }
            let Some(permit) = circuit.begin_trial() else {
                if circuit.is_closed() {
                    continue; // the exit path above re-checks the race
                }
                // A `probe_now` or an inline trial holds the slot; look again.
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            };
            let result = self.run_trial().await;
            self.finish(permit, result);
        }
    }

    fn finish(&self, permit: crate::circuit::TrialPermit, result: TrialResult) {
        if let Err((Some(class), _)) = &result {
            self.set_cause_class(*class);
        }
        self.shared
            .circuit
            .finish_trial(permit, result.map_err(|(_, msg)| msg));
    }

    /// No prober can run (no runtime to host one): when the next trial is
    /// due, run it inline on this caller, single-flight. Returns `true` when
    /// it closed the breaker.
    async fn try_inline_trial(&self) -> bool {
        let circuit = &self.shared.circuit;
        if circuit.retry_after().is_some_and(|d| !d.is_zero()) {
            return false;
        }
        let Some(permit) = circuit.begin_trial() else {
            return false;
        };
        let result = self.run_trial().await;
        self.finish(permit, result);
        circuit.is_closed()
    }

    /// Run one trial (the installed one, else the default), record the
    /// observation, and return its verdict.
    async fn run_trial(&self) -> TrialResult {
        let installed = lock(&self.shared.installed_trial).clone();
        let started = Instant::now();
        let result = match &installed {
            Some(trial) => {
                let fut = TRIAL_SCOPE.scope((), trial());
                match tokio::time::timeout(INSTALLED_TRIAL_TIMEOUT, fut).await {
                    Ok(r) => r.map_err(|e| (None, e)),
                    Err(_) => Err((
                        None,
                        format!("trial exceeded {} s", INSTALLED_TRIAL_TIMEOUT.as_secs()),
                    )),
                }
            }
            None => self.default_trial().await,
        };
        if installed.is_some() && result.is_ok() {
            *lock(&self.shared.installed_trial) = None;
        }
        match &result {
            Ok(()) => self.observe(started, Some(true), None),
            Err((_, m)) => self.observe(started, Some(false), Some(m.clone())),
        };
        result
    }

    /// Checkout + `SELECT 1` on the inner pool, bounded. A reply from the
    /// server — even an error reply — proves reachability.
    async fn default_trial(&self) -> TrialResult {
        let inner = self.inner.clone();
        let probe = async move {
            let conn = match inner.get().await {
                Ok(c) => c,
                Err(e) => {
                    return match classify(&e) {
                        (Outcome::NonTripFailure(NonTripKind::BackendError), _) => Ok(()),
                        (_, class) => Err((class, render_pool_error(&e))),
                    }
                }
            };
            match conn.simple_query("SELECT 1").await {
                Ok(_) => Ok(()),
                Err(e) if e.as_db_error().is_some() => Ok(()),
                Err(e) => Err((Some(client_failure_class(&e)), format!("SELECT 1: {e}"))),
            }
        };
        match tokio::time::timeout(self.shared.trial_timeout, probe).await {
            Ok(r) => r,
            Err(_) => Err((
                Some(CauseClass::ConnectTimeout),
                format!(
                    "probe exceeded {} ms",
                    self.shared.trial_timeout.as_millis()
                ),
            )),
        }
    }

    fn observe(
        &self,
        started: Instant,
        reachable: Option<bool>,
        error: Option<String>,
    ) -> ProbeObservation {
        let obs = ProbeObservation {
            at: SystemTime::now(),
            at_instant: Instant::now(),
            duration: started.elapsed(),
            reachable,
            error,
        };
        *lock(&self.shared.last_probe) = Some(obs.clone());
        obs
    }

    /// A FRESH reachability read for gating consumers (`/restart-readiness`)
    /// and for the `/health` refresher.
    ///
    /// * Closed: a real checkout + `SELECT 1` through the breaker, so a
    ///   connect-class failure trips it. A saturated pool, or a probe that
    ///   runs out its bound (one second past the trial timeout, so the pool's
    ///   own classified error normally arrives first), is `reachable: None` —
    ///   the probe could not tell — and trips nothing.
    /// * Open: an immediate single-flight trial (bypassing the backoff), or —
    ///   when one is already in flight — a bounded wait for it.
    pub async fn probe_now(&self) -> ProbeObservation {
        let started = Instant::now();
        let circuit = self.shared.circuit.clone();
        if circuit.is_closed() {
            let probe = async {
                let conn = match self.get().await {
                    Ok(c) => c,
                    Err(e @ PgCheckoutError::Saturated { .. }) => {
                        return (None, Some(e.to_string()))
                    }
                    Err(e) => return (Some(false), Some(e.to_string())),
                };
                match conn.simple_query("SELECT 1").await {
                    Ok(_) => (Some(true), None),
                    Err(e) if e.as_db_error().is_some() => (Some(true), None),
                    Err(e) => (Some(false), Some(format!("SELECT 1: {e}"))),
                }
            };
            let bound = self.shared.trial_timeout + Duration::from_secs(1);
            let (reachable, error) = match tokio::time::timeout(bound, probe).await {
                Ok(r) => r,
                Err(_) => (
                    None,
                    Some(format!("probe exceeded {} ms", bound.as_millis())),
                ),
            };
            return self.observe(started, reachable, error);
        }
        if let Some(permit) = circuit.begin_trial() {
            let r = self.run_trial().await;
            self.finish(permit, r);
            self.ensure_prober();
            return self
                .last_probe()
                .unwrap_or_else(|| self.observe(started, None, None));
        }
        match tokio::time::timeout(self.shared.trial_timeout, circuit.wait_closed()).await {
            Ok(()) => self.observe(started, Some(true), None),
            Err(_) => {
                let e = circuit
                    .snapshot()
                    .last_error
                    .unwrap_or_else(|| "database unavailable".to_string());
                self.observe(started, Some(false), Some(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{closed_port, test_pool, Mode, PgStandIn};
    use super::*;
    use deadpool_postgres::{PoolError, TimeoutType};

    /// "Immediate": the fail-fast path is an atomic load and a lock, so this
    /// bound only has to absorb scheduler noise on a loaded CI box (the plan
    /// names 50 ms; 100 ms keeps the assertion meaningful — a caller that
    /// paid a connect attempt takes 300 ms or more here — without flaking).
    const FAST_FAIL_BOUND: Duration = Duration::from_millis(100);

    /// The test breaker: the production shape with short delays.
    fn test_circuit() -> Arc<Circuit> {
        Arc::new(Circuit::new(CircuitConfig {
            backoff: Backoff {
                initial: Duration::from_millis(200),
                factor: 2,
                cap: Duration::from_millis(800),
            },
            ..PG_CIRCUIT_CONFIG
        }))
    }

    /// A guarded test pool with NO background runtime.
    fn bare(url: &str, create_ms: u64, wait_ms: u64, max: usize) -> GuardedPool {
        GuardedPool::with_circuit(
            test_pool(url, create_ms, wait_ms, max),
            test_circuit(),
            Duration::from_millis(create_ms + 200),
        )
    }

    /// A guarded test pool whose prober runs on the test's runtime (which is
    /// driven for the whole test, so it stands in for a process-lived one).
    fn guarded(url: &str, create_ms: u64, wait_ms: u64, max: usize) -> GuardedPool {
        let g = bare(url, create_ms, wait_ms, max);
        g.start_background(tokio::runtime::Handle::current());
        g
    }

    // ---- Phase 1: assumption checks against a real deadpool pool ----------

    #[tokio::test]
    async fn assumption_silent_server_is_a_create_timeout() {
        let pg = PgStandIn::start(Mode::Silent).await;
        let pool = test_pool(&pg.url(), 300, 300, 4);
        let err = pool
            .get()
            .await
            .expect_err("a silent server never completes startup");
        assert!(
            matches!(err, PoolError::Timeout(TimeoutType::Create)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn assumption_closed_port_is_a_client_side_backend_error() {
        let url = format!("postgres://fake:fake@127.0.0.1:{}/fake", closed_port());
        let pool = test_pool(&url, 3000, 3000, 4);
        let err = pool.get().await.expect_err("nothing listens there");
        match &err {
            PoolError::Backend(e) => {
                assert!(e.as_db_error().is_none(), "no server replied: {e:?}");
                println!("closed port -> Backend (client-side): {e}");
            }
            // Windows SYN retries can outrun a short create timeout; D2 trips
            // on both, so both are accepted and the observed one is printed.
            PoolError::Timeout(TimeoutType::Create) => {
                println!("closed port -> Timeout(Create)");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(classify(&err).0, Outcome::TripFailure(_)));
    }

    #[tokio::test]
    async fn assumption_a_held_slot_is_a_wait_timeout() {
        let pg = PgStandIn::start(Mode::Fake).await;
        let pool = test_pool(&pg.url(), 1000, 300, 1);
        let _held = pool
            .get()
            .await
            .expect("fake pg accepts the first checkout");
        let err = pool.get().await.expect_err("the only slot is held");
        assert!(
            matches!(err, PoolError::Timeout(TimeoutType::Wait)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn assumption_fake_pg_serves_select_1() {
        let pg = PgStandIn::start(Mode::Fake).await;
        let pool = test_pool(&pg.url(), 1000, 1000, 1);
        let conn = pool
            .get()
            .await
            .expect("fake pg completes startup + post_create");
        let msgs = conn.simple_query("SELECT 1").await.expect("SELECT 1");
        let row = msgs
            .iter()
            .find_map(|m| match m {
                tokio_postgres::SimpleQueryMessage::Row(r) => Some(r.get(0).map(str::to_string)),
                _ => None,
            })
            .expect("one row");
        assert_eq!(row.as_deref(), Some("1"));
    }

    /// D3's waiter race rests on this: dropping a `get()` future mid-create
    /// leaks no permit and no phantom object.
    #[tokio::test]
    async fn assumption_get_is_drop_safe_mid_create() {
        let pg = PgStandIn::start(Mode::Silent).await;
        let pool = test_pool(&pg.url(), 2000, 2000, 1);
        let in_flight = tokio::time::timeout(Duration::from_millis(150), pool.get()).await;
        assert!(in_flight.is_err(), "the get future was dropped mid-create");
        pg.set_mode(Mode::Fake);
        let conn = tokio::time::timeout(Duration::from_secs(2), pool.get())
            .await
            .expect("not stuck waiting for a leaked permit")
            .expect("checkout succeeds");
        drop(conn);
        assert_eq!(pool.status().size, 1, "exactly one object, no phantom");
    }

    #[test]
    fn classification_follows_the_d2_table() {
        assert!(matches!(
            classify(&PoolError::Timeout(TimeoutType::Create)),
            (Outcome::TripFailure(_), Some(CauseClass::ConnectTimeout))
        ));
        assert_eq!(
            classify(&PoolError::Timeout(TimeoutType::Wait)),
            (Outcome::NonTripFailure(NonTripKind::WaitTimeout), None)
        );
        assert_eq!(
            classify(&PoolError::Timeout(TimeoutType::Recycle)),
            (Outcome::NonTripFailure(NonTripKind::Ambiguous), None)
        );
        assert_eq!(
            classify(&PoolError::Closed),
            (Outcome::NonTripFailure(NonTripKind::Ambiguous), None)
        );
        assert_eq!(
            classify(&PoolError::NoRuntimeSpecified),
            (Outcome::NonTripFailure(NonTripKind::Ambiguous), None)
        );
        assert_eq!(
            classify(&PoolError::PostCreateHook(
                deadpool_postgres::HookError::Message("search_path".into())
            )),
            (Outcome::NonTripFailure(NonTripKind::BackendError), None)
        );
    }

    #[test]
    fn a_client_side_failure_is_classed_by_its_io_kind() {
        use std::io::{Error, ErrorKind};
        let refused = Error::new(ErrorKind::ConnectionRefused, "refused");
        let reset = Error::new(ErrorKind::ConnectionReset, "reset");
        let timed_out = Error::new(ErrorKind::TimedOut, "connect timeout");
        let other = Error::new(ErrorKind::NotFound, "dns: no such host");
        assert_eq!(client_failure_class(&refused), CauseClass::ConnectRefused);
        assert_eq!(client_failure_class(&reset), CauseClass::ConnectRefused);
        assert_eq!(client_failure_class(&timed_out), CauseClass::ConnectTimeout);
        assert_eq!(client_failure_class(&other), CauseClass::Unknown);
        // Nested: the io error sits under a wrapper, as in tokio_postgres::Error.
        #[derive(Debug)]
        struct Wrap(Error);
        impl std::fmt::Display for Wrap {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "error connecting to server")
            }
        }
        impl std::error::Error for Wrap {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let wrapped = Wrap(Error::new(ErrorKind::TimedOut, "t"));
        assert_eq!(client_failure_class(&wrapped), CauseClass::ConnectTimeout);
    }

    #[test]
    fn the_unavailable_token_is_a_pinned_literal() {
        assert_eq!(DB_UNAVAILABLE_TOKEN, "DATABASE_UNAVAILABLE:");
        let e = PgCheckoutError::Unavailable(DbUnavailable {
            since: Some(SystemTime::UNIX_EPOCH),
            cause: CAUSE_RUNTIME.into(),
            cause_class: CauseClass::ConnectRefused,
            last_error: Some("connection refused".into()),
            retry_after: Some(Duration::from_millis(1500)),
            connect_timeout: Some(Duration::from_secs(5)),
        });
        let s = format!("PG pool error: {e}");
        assert!(
            s.starts_with("PG pool error: DATABASE_UNAVAILABLE: "),
            "{s}"
        );
        assert!(is_db_unavailable(&s));
        assert!(s.contains("unreachable"), "{s}");
        assert!(s.contains("since 1970-01-01T00:00:00Z"), "{s}");
        assert!(s.contains("1500 ms"), "{s}");
        assert!(s.contains("connection refused"), "{s}");
        assert!(!is_db_unavailable(
            &PgCheckoutError::Saturated { waited: None }.to_string()
        ));
        // A connect TIMEOUT never claims "unreachable".
        let t = DbUnavailable {
            since: None,
            cause: CAUSE_RUNTIME.into(),
            cause_class: CauseClass::ConnectTimeout,
            last_error: None,
            retry_after: None,
            connect_timeout: Some(Duration::from_secs(5)),
        }
        .to_string();
        assert!(t.contains("did not answer within 5000 ms"), "{t}");
        assert!(!t.contains("unreachable"), "{t}");
        let boot = DbUnavailable {
            since: None,
            cause: CAUSE_BOOT.into(),
            cause_class: CauseClass::Unknown,
            last_error: None,
            retry_after: None,
            connect_timeout: None,
        };
        assert!(boot.to_string().contains("booted without its database"));
    }

    #[test]
    fn the_prober_cap_stays_below_the_health_staleness_bound() {
        assert_eq!(PG_CIRCUIT_CONFIG.backoff.cap, PROBER_BACKOFF_CAP);
        assert!(PROBER_BACKOFF_CAP < Duration::from_secs(30));
    }

    // ---- Phase 3: acceptance ----------------------------------------------

    /// Calls 2..=20 after the first failure must each be fast and must not
    /// touch the network themselves.
    async fn assert_fail_fast_after_first_failure(g: &GuardedPool, pg: Option<&PgStandIn>) {
        let first = g.get().await.expect_err("first call fails");
        assert!(first.is_unavailable(), "{first}");
        assert_eq!(g.breaker_state(), CircuitState::Open);
        let accepts_after_first = pg.map(PgStandIn::accepts);
        let fast_start = Instant::now();
        for i in 2..=20 {
            let t = Instant::now();
            let e = g.get().await.expect_err("still down");
            assert!(e.is_unavailable(), "call {i}: {e}");
            assert!(
                t.elapsed() < FAST_FAIL_BOUND,
                "call {i} took {:?}",
                t.elapsed()
            );
        }
        if let (Some(pg), Some(before)) = (pg, accepts_after_first) {
            // Only prober trials may connect: at most one per backoff step
            // (initial 200 ms) plus one.
            let budget = 1 + fast_start.elapsed().as_millis() as u64 / 200;
            assert!(
                pg.accepts() - before <= budget,
                "callers connected: {} accepts over budget {budget}",
                pg.accepts() - before
            );
        }
    }

    #[tokio::test]
    async fn silent_db_first_call_pays_once_then_everyone_fails_fast() {
        let pg = PgStandIn::start(Mode::Silent).await;
        let g = guarded(&pg.url(), 300, 300, 8);
        let t = Instant::now();
        let first = g.get().await.expect_err("silent");
        assert!(
            t.elapsed() >= Duration::from_millis(250),
            "{:?}",
            t.elapsed()
        );
        assert!(first.is_unavailable());
        assert_eq!(g.cause_class(), CauseClass::ConnectTimeout);
        assert!(!first.to_string().contains("unreachable"), "{first}");
        // A fresh breaker, to measure calls 2..20 from a clean trip.
        let g = guarded(&pg.url(), 300, 300, 8);
        assert_fail_fast_after_first_failure(&g, Some(&pg)).await;
    }

    #[tokio::test]
    async fn refused_port_trips_and_later_calls_do_not_each_pay_the_refuse() {
        let url = format!("postgres://fake:fake@127.0.0.1:{}/fake", closed_port());
        let g = guarded(&url, 3000, 3000, 8);
        assert_fail_fast_after_first_failure(&g, None).await;
        // Windows may report a refused loopback connect as a timeout; either
        // way the class is a connect class, never Unknown.
        assert_ne!(g.cause_class(), CauseClass::Unknown);
    }

    #[tokio::test]
    async fn recovery_closes_the_breaker_and_re_trip_opens_it_again() {
        let pg = PgStandIn::start(Mode::Silent).await;
        let g = guarded(&pg.url(), 300, 300, 1);
        let _ = g.get().await.expect_err("silent");
        assert_eq!(g.breaker_state(), CircuitState::Open);
        let first_since = g.breaker_snapshot().since;
        pg.set_mode(Mode::Fake);
        tokio::time::timeout(Duration::from_millis(1800), g.shared.circuit.wait_closed())
            .await
            .expect("closed within backoff + 1 s");
        let conn = g.get().await.expect("closed breaker serves checkouts");
        assert!(conn.simple_query("SELECT 1").await.is_ok());
        assert!(g.last_probe().is_some_and(|p| p.reachable == Some(true)));
        // Detach the live connection so the next checkout must connect.
        let _ = deadpool_postgres::Object::take(conn);
        pg.set_mode(Mode::Silent);
        tokio::time::sleep(Duration::from_millis(1100)).await; // `since` has second resolution
        let e = g.get().await.expect_err("silent again");
        assert!(e.is_unavailable());
        assert_eq!(g.breaker_state(), CircuitState::Open);
        assert_ne!(
            g.breaker_snapshot().since,
            first_since,
            "a new outage, a new since"
        );
        assert_eq!(g.breaker_snapshot().trips, 2);
    }

    #[tokio::test]
    async fn a_wait_timeout_is_reported_and_never_trips() {
        let pg = PgStandIn::start(Mode::Fake).await;
        let g = guarded(&pg.url(), 1000, 300, 1);
        let _held = g.get().await.expect("first checkout");
        let e = g.get().await.expect_err("the only slot is held");
        assert!(matches!(e, PgCheckoutError::Saturated { .. }), "{e}");
        assert!(!is_db_unavailable(&e.to_string()));
        assert_eq!(g.breaker_state(), CircuitState::Closed);
        assert_eq!(g.breaker_snapshot().wait_timeouts, 1);
        assert!(g.breaker_snapshot().wait_timeout_last_at.is_some());
    }

    #[tokio::test]
    async fn concurrent_waiters_are_released_when_the_breaker_trips() {
        let pg = PgStandIn::start(Mode::Silent).await;
        // One slot: one caller is creating (300 ms create timeout), seven wait
        // for the slot with a 5 s wait timeout. Without the race they would
        // each run out a full wait timeout.
        let g = guarded(&pg.url(), 300, 5000, 1);
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let g2 = g.clone();
            tasks.push(tokio::spawn(async move {
                let r = g2.get().await;
                (r.map(|_| ()), Instant::now())
            }));
        }
        tokio::time::timeout(Duration::from_secs(2), g.shared.circuit.wait_opened())
            .await
            .expect("trips within the create timeout");
        let tripped_at = Instant::now();
        for t in tasks {
            let (r, done) = t.await.unwrap();
            let e = r.expect_err("down");
            assert!(e.is_unavailable(), "{e}");
            assert!(
                done.saturating_duration_since(tripped_at) < FAST_FAIL_BOUND,
                "a waiter was held {:?} past the trip",
                done.saturating_duration_since(tripped_at)
            );
        }
    }

    #[tokio::test]
    async fn a_server_error_reply_does_not_trip() {
        let pg = PgStandIn::start(Mode::AuthError).await;
        let g = guarded(&pg.url(), 1000, 1000, 2);
        let e = g.get().await.expect_err("auth refused");
        match &e {
            PgCheckoutError::Pool(PoolError::Backend(be)) => {
                assert_eq!(be.as_db_error().map(|d| d.code().code()), Some("28P01"));
            }
            other => panic!("expected the server's own error, got {other}"),
        }
        assert!(e.to_string().contains("28P01"), "{e}");
        assert_eq!(g.breaker_state(), CircuitState::Closed);
        assert_eq!(g.breaker_snapshot().backend_errors, 1);
    }

    #[tokio::test]
    async fn probe_now_trips_when_closed_and_trials_when_open() {
        let pg = PgStandIn::start(Mode::Silent).await;
        let g = guarded(&pg.url(), 300, 300, 2);
        let obs = g.probe_now().await;
        assert_eq!(obs.reachable, Some(false));
        assert_eq!(
            g.breaker_state(),
            CircuitState::Open,
            "a fresh probe feeds the breaker"
        );
        pg.set_mode(Mode::Fake);
        let obs = g.probe_now().await;
        assert_eq!(obs.reachable, Some(true), "{obs:?}");
        assert!(g.is_available());
    }

    /// D4: the degraded-boot trial checks out through the guarded pool, and
    /// its own breaker must not refuse it.
    #[tokio::test]
    async fn an_installed_trial_runs_until_it_first_succeeds_and_is_not_refused() {
        let pg = PgStandIn::start(Mode::Fake).await;
        let g = bare(&pg.url(), 1000, 1000, 2);
        g.force_open(CAUSE_BOOT, "no database at boot");
        assert_eq!(g.breaker_snapshot().cause.as_deref(), Some(CAUSE_BOOT));
        // Outside the trial, callers are still refused.
        assert!(g.get().await.expect_err("open").is_unavailable());
        let runs = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (g2, runs2) = (g.clone(), runs.clone());
        g.install_trial_until_first_success(trial_fn(move || {
            let (g3, runs3) = (g2.clone(), runs2.clone());
            async move {
                runs3.fetch_add(1, Ordering::SeqCst);
                let c = g3.get().await.map_err(|e| e.to_string())?;
                c.simple_query("SELECT 1")
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
        }));
        g.start_background(tokio::runtime::Handle::current());
        tokio::time::timeout(Duration::from_secs(3), g.shared.circuit.wait_closed())
            .await
            .expect("the installed trial closes the breaker");
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert!(lock(&g.shared.installed_trial).is_none());
    }

    /// D3: a breaker tripped on a current-thread runtime that is then dropped
    /// spawns nothing there, and still recovers once a background handle is
    /// given.
    #[test]
    fn a_trip_on_a_dropped_current_thread_runtime_recovers_through_the_background_handle() {
        let main_rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let pg = main_rt.block_on(PgStandIn::start(Mode::Silent));
        let g = main_rt.block_on(async { bare(&pg.url(), 300, 300, 2) });
        {
            let boot_rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let e = boot_rt.block_on(g.get()).expect_err("silent");
            assert!(e.is_unavailable());
            assert!(
                !g.shared.prober_running.load(Ordering::SeqCst),
                "nothing spawned on a current-thread runtime"
            );
        } // boot_rt dropped here
        pg.set_mode(Mode::Fake);
        main_rt.block_on(async {
            g.start_background(tokio::runtime::Handle::current());
            tokio::time::timeout(Duration::from_secs(3), g.shared.circuit.wait_closed())
                .await
                .expect("recovers through the background handle");
        });
        assert!(g.is_available());
    }

    /// D3: with no runtime that can host a prober, the first caller after
    /// `retry_at` runs the trial inline.
    #[tokio::test]
    async fn with_no_prober_runtime_a_due_caller_runs_the_trial_inline() {
        let pg = PgStandIn::start(Mode::Silent).await;
        let g = bare(&pg.url(), 300, 300, 2); // #[tokio::test] is current-thread
        assert!(g.get().await.expect_err("silent").is_unavailable());
        assert!(!g.shared.prober_running.load(Ordering::SeqCst));
        pg.set_mode(Mode::Fake);
        // Before retry_at: still fail-fast, no trial.
        let t = Instant::now();
        assert!(g.get().await.expect_err("not yet due").is_unavailable());
        assert!(t.elapsed() < FAST_FAIL_BOUND);
        tokio::time::sleep(Duration::from_millis(250)).await;
        let conn = g
            .get()
            .await
            .expect("the due caller's inline trial closed it");
        assert!(conn.simple_query("SELECT 1").await.is_ok());
        assert!(g.is_available());
    }
}
