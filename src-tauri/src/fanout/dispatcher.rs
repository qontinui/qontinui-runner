//! The fan-out dispatcher: a queue, a cap and a spawn.
//!
//! Every mutation — create, cancel, PATCH, release and the admission tick —
//! runs under ONE async mutex over the in-memory book, so the cap is exact by
//! construction: no two admissions can each read "one slot free" and both
//! spawn. The spawn ITSELF runs outside that mutex: a tick records each
//! admission (`admitted`, persisted) under the lock, releases it, spawns, and
//! re-takes it to record what the spawn came to. A member being spawned is
//! `admitted`, so it counts toward the cap and is never picked twice; it is
//! marked in-flight so liveness does not release it and an operator release
//! is refused until its spawn settles; and a cancel that lands meanwhile is
//! honoured when a spawn that did not happen returns it. A spawn whose outcome
//! is lost — its task panicked, or the loop driving it was aborted — is
//! outcome-UNKNOWN, never a refusal: the member stays `admitted` under its
//! pinned session id and the next tick's liveness reconcile decides whether it
//! ran, so a prompt is never spawned twice. A spawn is waited for at most
//! [`SPAWN_CALL_TIMEOUT`]; one that answers later (or after its loop died) is
//! recorded by the next tick, and a late `Spawned` re-adopts a member liveness
//! released `spawn_unconfirmed` meanwhile — a running `claude` is never left
//! untracked, at the cost of the run briefly sitting one over its cap. Every
//! transition is
//! written through to the store; the book is reloaded from it at runner start
//! and its `admitted` members reconciled against the terminals that survived
//! ([`FanoutDispatcher::boot`]).
//!
//! The three seams — [`FanoutStore`], [`FanoutHost`], [`FanoutEvents`] — are
//! traits so the admission rules are tested against an in-memory ledger and a
//! recording spawner (see `tests` below); production wires PostgreSQL
//! (`database::pg::fanout`) and the Tauri host (`fanout::host`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tracing::{info, warn};
use uuid::Uuid;

use super::model::{
    clamp_max_concurrent, derived_run_state, reason, ConfigDirPolicy, FanoutMember, FanoutRun,
    MemberState, RunState, RunView,
};

/// The durable ledger.
#[async_trait]
pub(crate) trait FanoutStore: Send + Sync {
    /// Insert a run and all of its members, atomically.
    async fn insert_run(&self, run: &FanoutRun, members: &[FanoutMember]) -> Result<(), String>;
    /// Persist the run-level fields that change (`max_concurrent`, `state`).
    async fn update_run(&self, run: &FanoutRun) -> Result<(), String>;
    /// Persist one member's mutable fields.
    async fn update_member(&self, run_id: Uuid, member: &FanoutMember) -> Result<(), String>;
    /// Every `active` run `owner_instance` owns, with its members in index order.
    async fn load_active_runs(
        &self,
        owner_instance: &str,
    ) -> Result<Vec<(FanoutRun, Vec<FanoutMember>)>, String>;
}

/// Everything one member spawn needs. The tenant is the RUN's, admitted once at
/// create; the session id is minted fresh at this admission.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MemberSpawnRequest {
    pub run_id: Uuid,
    pub index: u32,
    pub title: String,
    pub prompt: String,
    pub working_dir: String,
    pub tenant_id: Option<Uuid>,
    pub config_dir_policy: ConfigDirPolicy,
    pub claude_session_id: String,
}

/// What one spawn attempt came to.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SpawnOutcome {
    /// The member's terminal is running its prompt.
    Spawned { terminal_id: String },
    /// Coord's device drain deferred it: stays queued, nothing is a failure.
    DeferredByDrain { reason: String },
    /// The `parallel_fanout` admission bound was occupied: stays queued.
    BoundOccupied { detail: String },
    /// A resource-guard, authorization or spawn error. The member goes
    /// `refused` with this reason and returns to the queue after a backoff
    /// that grows while the same kind of refusal repeats.
    Refused { reason: String },
}

/// What the host can say about an admitted member's session.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Liveness {
    /// A live terminal hosts it (possibly under a new terminal id, after a
    /// restore rebound the session).
    Live { terminal_id: String },
    /// The session carries the runner-local finished marker.
    Finished,
    /// Its terminal exited (present and dead), or its record is closed.
    Exited,
    /// Its lifecycle record is open and unfinished, but no terminal answers
    /// for it — a session mid-respawn, or one session restore has not rebound
    /// yet. Held for [`LIVENESS_GRACE`] before it is read as an exit.
    Unconfirmed,
    /// No lifecycle record exists and no terminal answers: the session may
    /// never have started.
    Unrecorded,
    /// The host cannot tell (no terminal manager, an unreadable lifecycle
    /// store). Never releases a slot: an unknown is not a death.
    Unknown,
}

/// The process the members run in.
#[async_trait]
pub(crate) trait FanoutHost: Send + Sync {
    /// `Some(reason)` while autonomous spawns are deferred by coord's device
    /// drain. A cheap pre-check: the spawn itself re-checks as the authority.
    fn drain_deferral(&self) -> Option<String>;
    /// This runner's current `parallel_fanout` bound. It resolves per device
    /// (the runner's own registry row), not per tenant.
    async fn fanout_bound(&self) -> u32;
    /// Spawn one member.
    async fn spawn_fanout_member(&self, req: MemberSpawnRequest) -> SpawnOutcome;
    /// Is the session `claude_session_id` (last seen on `terminal_id`) alive?
    fn liveness(&self, claude_session_id: &str, terminal_id: Option<&str>) -> Liveness;
}

/// Where a changed run is announced (the `fanout-changed` Tauri event).
pub(crate) trait FanoutEvents: Send + Sync {
    fn changed(&self, run: &RunView);
}

/// A run to create: the already-admitted tenant and the previewed members.
#[derive(Debug, Clone)]
pub(crate) struct NewRun {
    pub tenant_id: Option<Uuid>,
    pub template_slug: Option<String>,
    pub template_version: Option<i32>,
    pub requested_max_concurrent: u32,
    pub config_dir_policy: ConfigDirPolicy,
    pub working_dir: String,
    pub members: Vec<FanoutMember>,
}

/// A create or PATCH result: the run, and how its cap relates to the bound.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CapOutcome {
    pub run: RunView,
    /// This runner's `parallel_fanout` bound the cap was clamped against.
    pub fanout_bound: u32,
    /// What the caller asked for, when the clamp changed it.
    pub clamped_from: Option<u32>,
}

/// Why the book cannot be served: it does not reflect the ledger yet.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NotLoaded {
    /// `true` once a load was attempted and failed (PostgreSQL unreadable);
    /// `false` while the boot settle has not run the first load yet.
    pub load_failed: bool,
    /// The operator-facing reason.
    pub reason: String,
}

/// Why an operator operation was refused.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OpError {
    /// No run with that id on this runner.
    NotFound(String),
    /// The operation does not apply in the member's current state.
    Conflict(String),
    /// The ledger write failed; nothing changed.
    Store(String),
    /// The ledger has not been loaded into the book yet (or the last load
    /// failed): what this runner holds is UNKNOWN, so nothing is answered from
    /// — or changed in — the book.
    NotLoaded(NotLoaded),
}

/// Whether the book reflects the durable ledger. Until it is `Loaded`, an
/// empty book is not "no runs" — it is "not read yet".
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LedgerState {
    /// [`FanoutDispatcher::boot`] has not run yet (the boot settle).
    Pending,
    /// The last load attempt failed with this error; it is retried each tick.
    Failed(String),
    /// Active runs are loaded and reconciled.
    Loaded,
}

impl LedgerState {
    /// Why the book cannot be served, or `None` once it is loaded.
    pub(crate) fn not_loaded(&self) -> Option<NotLoaded> {
        match self {
            LedgerState::Loaded => None,
            LedgerState::Pending => Some(NotLoaded {
                load_failed: false,
                reason: "the fan-out ledger has not been loaded yet — the runner loads and \
                         reconciles its active runs from PostgreSQL after the boot settle, so its \
                         runs are UNKNOWN until then"
                    .to_string(),
            }),
            LedgerState::Failed(e) => Some(NotLoaded {
                load_failed: true,
                reason: format!(
                    "the fan-out ledger could not be loaded from PostgreSQL ({e}) — this \
                     runner's runs are UNKNOWN until a load succeeds (retried every tick)"
                ),
            }),
        }
    }
}

/// How long a completed run stays listed after it completes.
const COMPLETED_RETENTION: chrono::Duration = chrono::Duration::hours(24);

/// A run loaded at runner start with no write of any kind (create, admission,
/// release, refusal, drain deferral, PATCH — every write stamps `updated_at`)
/// for longer than this is not resumed: its waiting members are cancelled with
/// [`reason::STALE_AFTER_RESTART`]. Owner instance names are reused — a temp
/// runner torn down days ago and a new one under the same name share a key —
/// and nobody is waiting on a queue that old; re-creating the run from the
/// prompt modal is one click. Admitted members are still reconciled against
/// their sessions as usual.
///
/// Never applied to a run that is demonstrably still in use: one with an
/// admitted member whose session reads live (or unconfirmed — mid-respawn),
/// or one paused by coord's device drain (a member deferred `runner_draining`,
/// or this device drained now). A drain is an intentional pause and the
/// documented way to quiesce a box before a rebuild restart, however long it
/// lasted.
pub(crate) const STALE_RUN_AGE: chrono::Duration = chrono::Duration::hours(24);

/// How long a spawn may stay in flight before its outcome is treated as
/// UNKNOWN. The admission's own wait is `fanout::host::ADMISSION_WAIT` (250 ms);
/// the spawn after it resolves the claude binary, picks an account and
/// allocates a worktree over the network, none of which takes minutes. An
/// in-flight marker older than this is one whose settle will never be recorded
/// — the loop that would record it panicked or was aborted — and is handed to
/// the liveness reconcile like a panicked spawn ([`SpawnSettle::Unknown`]).
pub(crate) const SPAWN_SETTLE_BOUND: chrono::Duration = chrono::Duration::minutes(10);

/// How long the admission loop waits for one spawn's answer before settling it
/// outcome-UNKNOWN ([`SpawnSettle::Unknown`]). Deliberately SHORTER than
/// [`SPAWN_SETTLE_BOUND`], so a slow spawn always settles through this path
/// (and the in-flight marker is only ever expired for a loop that died).
///
/// A timed-out spawn is not aborted — cutting `TerminalManager::create` off
/// mid-way could leave a child nothing records. It runs on, and whatever it
/// comes to is queued as a LATE settle that the next tick records
/// ([`FanoutDispatcher::record_spawn`]): a late `Spawned` for a member the
/// liveness reconcile meanwhile released `spawn_unconfirmed` RE-ADOPTS it, so a
/// running `claude` is never left untracked.
pub(crate) const SPAWN_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// How long an admitted member may read [`Liveness::Unconfirmed`] before the
/// slot is released as an exit. Covers a respawn and the restore rebind, which
/// run after the boot settle.
pub(crate) const LIVENESS_GRACE: chrono::Duration = chrono::Duration::seconds(120);

/// The first retry delay after a refusal; it doubles while the same kind of
/// refusal repeats, up to [`RETRY_MAX`].
const RETRY_BASE: chrono::Duration = chrono::Duration::seconds(5);
const RETRY_MAX: chrono::Duration = chrono::Duration::minutes(5);

/// A refused member's backoff, keyed on the kind of refusal.
#[derive(Debug, Clone, PartialEq)]
struct RetryState {
    /// [`reason_class`] of the last refusal.
    class: String,
    /// Consecutive refusals of that class.
    attempts: u32,
    next_at: DateTime<Utc>,
}

/// The stable part of a refusal reason: up to the first `:`, else all of it.
/// `resource_guard:critical: commit charge at 97%` and `… at 98%` are the same
/// refusal; a different class restarts the backoff.
fn reason_class(why: &str) -> String {
    why.split(':').next().unwrap_or(why).trim().to_string()
}

/// The delay before attempt `attempts + 1`: `RETRY_BASE · 2^(attempts-1)`,
/// capped at [`RETRY_MAX`].
fn retry_delay(attempts: u32) -> chrono::Duration {
    let shift = attempts.saturating_sub(1).min(16);
    let secs = RETRY_BASE.num_seconds().saturating_mul(1_i64 << shift);
    chrono::Duration::seconds(secs.min(RETRY_MAX.num_seconds()))
}

struct RunEntry {
    run: FanoutRun,
    members: Vec<FanoutMember>,
    /// When the run became `completed` in this process, for retention.
    completed_at: Option<DateTime<Utc>>,
    /// Backoff per refused member index. In-memory: a restart retries at once.
    retry: HashMap<u32, RetryState>,
    /// Member indices whose spawn is in flight outside the book lock, with
    /// when it was admitted — so a marker whose settle was lost expires
    /// ([`SPAWN_SETTLE_BOUND`]) instead of pinning the member forever.
    spawning: HashMap<u32, DateTime<Utc>>,
    /// Since when (and under which release reason) a member has read
    /// [`Liveness::Unconfirmed`].
    unconfirmed: HashMap<u32, (DateTime<Utc>, String)>,
    /// An operator cancelled the run's waiting members: a member whose spawn
    /// was in flight and did not happen is cancelled too, never re-queued.
    cancel_requested: bool,
}

impl RunEntry {
    fn new(run: FanoutRun, members: Vec<FanoutMember>) -> Self {
        Self {
            run,
            members,
            completed_at: None,
            retry: HashMap::new(),
            spawning: HashMap::new(),
            unconfirmed: HashMap::new(),
            cancel_requested: false,
        }
    }

    /// The wire view, with each refused member's backoff.
    fn view(&self) -> RunView {
        let mut v = RunView::of(&self.run, &self.members);
        for m in v.members.iter_mut() {
            if let Some(r) = self.retry.get(&m.index) {
                m.refusals = r.attempts;
                if m.state == MemberState::Refused {
                    m.next_retry_at = Some(r.next_at.to_rfc3339());
                }
            }
        }
        v
    }

    /// The most recent write to the run or any of its members.
    fn last_activity(&self) -> DateTime<Utc> {
        self.members
            .iter()
            .flat_map(|m| [m.admitted_at, m.released_at, m.updated_at])
            .chain([self.run.updated_at])
            .flatten()
            .fold(self.run.created_at, Ord::max)
    }

    /// Whether member `index`'s spawn is in flight and still inside
    /// [`SPAWN_SETTLE_BOUND`].
    fn spawn_in_flight(&self, index: u32, now: DateTime<Utc>) -> bool {
        self.spawning
            .get(&index)
            .is_some_and(|at| now.signed_duration_since(*at) < SPAWN_SETTLE_BOUND)
    }
}

#[derive(Default)]
struct Book {
    runs: BTreeMap<Uuid, RunEntry>,
    /// The store's active runs are loaded and reconciled.
    booted: bool,
}

/// One admission recorded under the lock, to be spawned outside it.
struct PendingSpawn {
    req: MemberSpawnRequest,
}

/// How one recorded admission settled.
#[derive(Debug)]
enum SpawnSettle {
    /// Not attempted (an earlier spawn in the batch halted it).
    NotAttempted,
    /// The host answered.
    Outcome(SpawnOutcome),
    /// No answer: the spawn task panicked (or its marker expired). Whether a
    /// terminal was created is UNKNOWN, so the member is neither refused nor
    /// re-queued — liveness decides.
    Unknown(String),
}

/// A spawn that answered after the admission loop stopped waiting for it (it
/// timed out, or the loop was aborted). Recorded by the next tick.
#[derive(Debug)]
struct LateSettle {
    req: MemberSpawnRequest,
    outcome: SpawnOutcome,
}

pub(crate) struct FanoutDispatcher {
    owner_instance: String,
    store: Arc<dyn FanoutStore>,
    host: Arc<dyn FanoutHost>,
    events: Arc<dyn FanoutEvents>,
    book: tokio::sync::Mutex<Book>,
    /// The views last published, newest run first — what the read routes
    /// serve, so a read never waits behind the book.
    published: RwLock<Arc<Vec<RunView>>>,
    /// Whether `published` reflects the ledger. Written under the book lock;
    /// read lock-free beside `published`.
    ledger: RwLock<LedgerState>,
    wake: tokio::sync::Notify,
    /// Seconds added to the wall clock — moved only by tests, to step through
    /// backoffs and grace windows without sleeping.
    clock_skew_secs: AtomicI64,
    /// How long one spawn is waited for ([`SPAWN_CALL_TIMEOUT`]; shortened by
    /// tests).
    spawn_call_timeout: std::time::Duration,
    /// Spawn outcomes that arrived after nobody was waiting for them, drained
    /// at the start of every tick. Shared with the detached spawn tasks, so an
    /// outcome lands here even when the loop that started the spawn is gone.
    late_settles: Arc<std::sync::Mutex<Vec<LateSettle>>>,
}

impl FanoutDispatcher {
    pub(crate) fn new(
        owner_instance: String,
        store: Arc<dyn FanoutStore>,
        host: Arc<dyn FanoutHost>,
        events: Arc<dyn FanoutEvents>,
    ) -> Self {
        Self {
            owner_instance,
            store,
            host,
            events,
            book: tokio::sync::Mutex::new(Book::default()),
            published: RwLock::new(Arc::new(Vec::new())),
            ledger: RwLock::new(LedgerState::Pending),
            wake: tokio::sync::Notify::new(),
            clock_skew_secs: AtomicI64::new(0),
            spawn_call_timeout: SPAWN_CALL_TIMEOUT,
            late_settles: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Record every late spawn outcome queued since the last tick.
    async fn record_late_settles(&self) {
        let late = match self.late_settles.lock() {
            Ok(mut g) => std::mem::take(&mut *g),
            Err(p) => std::mem::take(&mut *p.into_inner()),
        };
        for LateSettle { req, outcome } in late {
            self.record_spawn(&req, SpawnSettle::Outcome(outcome)).await;
        }
    }

    fn now(&self) -> DateTime<Utc> {
        Utc::now() + chrono::Duration::seconds(self.clock_skew_secs.load(Ordering::SeqCst))
    }

    /// Ask the admission loop to tick now rather than at its next interval.
    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    /// Resolves on the next [`Self::wake`].
    pub(crate) async fn woken(&self) {
        self.wake.notified().await;
    }

    /// The ledger's load state.
    pub(crate) fn ledger_state(&self) -> LedgerState {
        match self.ledger.read() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }

    fn set_ledger_state(&self, state: LedgerState) {
        match self.ledger.write() {
            Ok(mut g) => *g = state,
            Err(p) => *p.into_inner() = state,
        }
    }

    /// Every run this runner holds, newest first — or, until the ledger is
    /// loaded, the reason it cannot say. An unloaded book is never served as
    /// an empty list: that would read as "no runs".
    pub(crate) fn list(&self) -> Result<Arc<Vec<RunView>>, NotLoaded> {
        if let Some(why) = self.ledger_state().not_loaded() {
            return Err(why);
        }
        Ok(match self.published.read() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        })
    }

    /// One run (`Ok(None)`: the loaded ledger has no such run).
    pub(crate) fn get(&self, id: Uuid) -> Result<Option<RunView>, NotLoaded> {
        Ok(self.list()?.iter().find(|r| r.id == id).cloned())
    }

    /// Refuse an operation while the book does not reflect the ledger.
    /// `booted` is only ever read and written under the book lock.
    fn require_booted(&self, book: &Book) -> Result<(), OpError> {
        if book.booted {
            return Ok(());
        }
        Err(OpError::NotLoaded(
            self.ledger_state().not_loaded().unwrap_or(NotLoaded {
                load_failed: false,
                reason: "the fan-out ledger has not been loaded yet".to_string(),
            }),
        ))
    }

    /// Create a run. Persisted before it is admitted to the book, so a run
    /// the ledger could not record is never queued at all.
    ///
    /// Refused until the ledger is loaded. A run created inside the boot
    /// window would be inserted into PG, picked up by the concurrent
    /// [`Self::boot`] load, possibly admitted (spawned) by the first tick after
    /// it — and then overwritten in the book by this call's all-queued copy,
    /// so the next tick would spawn its members a second time. `booted` is
    /// read under the book lock and never goes back to false, so checking it
    /// once up front closes that window.
    pub(crate) async fn create(&self, new: NewRun) -> Result<CapOutcome, OpError> {
        {
            let book = self.book.lock().await;
            self.require_booted(&book)?;
        }
        let bound = self.host.fanout_bound().await;
        let max_concurrent = clamp_max_concurrent(new.requested_max_concurrent, bound);
        let run = FanoutRun {
            id: Uuid::new_v4(),
            tenant_id: new.tenant_id,
            template_slug: new.template_slug,
            template_version: new.template_version,
            max_concurrent,
            config_dir_policy: new.config_dir_policy,
            working_dir: new.working_dir,
            created_at: self.now(),
            state: RunState::Active,
            owner_instance: self.owner_instance.clone(),
            updated_at: None,
        };
        self.store
            .insert_run(&run, &new.members)
            .await
            .map_err(OpError::Store)?;
        let view = RunView::of(&run, &new.members);
        {
            let mut book = self.book.lock().await;
            book.runs.insert(run.id, RunEntry::new(run, new.members));
            self.publish(&book);
        }
        info!(
            run_id = %view.id,
            members = view.members.len(),
            max_concurrent,
            "fanout: run created"
        );
        self.events.changed(&view);
        self.wake();
        Ok(CapOutcome {
            run: view,
            fanout_bound: bound,
            clamped_from: (max_concurrent != new.requested_max_concurrent)
                .then_some(new.requested_max_concurrent),
        })
    }

    /// Cancel every member that has not been admitted (`queued`, and `refused`,
    /// which is a queued member whose last attempt failed). An admitted
    /// member's session is never touched; one whose spawn is still in flight
    /// is cancelled if that spawn does not happen.
    pub(crate) async fn cancel(&self, id: Uuid) -> Result<RunView, OpError> {
        let mut book = self.book.lock().await;
        self.require_booted(&book)?;
        let entry = book
            .runs
            .get_mut(&id)
            .ok_or_else(|| OpError::NotFound(format!("no fan-out run {id}")))?;
        let mut next = entry.members.clone();
        let mut changed = Vec::new();
        let now = self.now();
        for (pos, m) in next.iter_mut().enumerate() {
            if matches!(m.state, MemberState::Queued | MemberState::Refused) {
                m.state = MemberState::Cancelled;
                m.reason = Some(reason::CANCELLED.to_string());
                m.updated_at = Some(now);
                changed.push(pos);
            }
        }
        for &pos in &changed {
            self.store
                .update_member(id, &next[pos])
                .await
                .map_err(OpError::Store)?;
            entry.retry.remove(&next[pos].index);
            entry.members[pos] = next[pos].clone();
        }
        entry.cancel_requested = true;
        let view = self.settle_run_state(entry).await;
        self.publish(&book);
        drop(book);
        self.events.changed(&view);
        Ok(view)
    }

    /// Set `max_concurrent`, clamped to this runner's `parallel_fanout` bound.
    pub(crate) async fn set_max_concurrent(
        &self,
        id: Uuid,
        requested: u32,
    ) -> Result<CapOutcome, OpError> {
        let bound = self.host.fanout_bound().await;
        let max_concurrent = clamp_max_concurrent(requested, bound);
        let mut book = self.book.lock().await;
        self.require_booted(&book)?;
        let entry = book
            .runs
            .get_mut(&id)
            .ok_or_else(|| OpError::NotFound(format!("no fan-out run {id}")))?;
        let mut next = entry.run.clone();
        next.max_concurrent = max_concurrent;
        next.updated_at = Some(self.now());
        self.store.update_run(&next).await.map_err(OpError::Store)?;
        entry.run = next;
        let view = entry.view();
        self.publish(&book);
        drop(book);
        self.events.changed(&view);
        self.wake();
        Ok(CapOutcome {
            run: view,
            fanout_bound: bound,
            clamped_from: (max_concurrent != requested).then_some(requested),
        })
    }

    /// Release member `index`'s slot. The session keeps running; only its
    /// claim on the cap ends. Refused while the member's spawn is in flight.
    pub(crate) async fn release(&self, id: Uuid, index: u32) -> Result<RunView, OpError> {
        let mut book = self.book.lock().await;
        self.require_booted(&book)?;
        let entry = book
            .runs
            .get_mut(&id)
            .ok_or_else(|| OpError::NotFound(format!("no fan-out run {id}")))?;
        let pos = entry
            .members
            .iter()
            .position(|m| m.index == index)
            .ok_or_else(|| OpError::NotFound(format!("run {id} has no member {index}")))?;
        let current = &entry.members[pos];
        if current.state != MemberState::Admitted {
            return Err(OpError::Conflict(format!(
                "member {index} is {}, not admitted — only an admitted member holds a slot",
                current.state.as_str()
            )));
        }
        let now = self.now();
        if entry.spawn_in_flight(index, now) {
            return Err(OpError::Conflict(format!(
                "member {index} is being spawned — its slot can be released once the spawn \
                 settles"
            )));
        }
        let mut next = current.clone();
        mark_released(&mut next, reason::OPERATOR_RELEASE, now);
        next.updated_at = Some(now);
        self.store
            .update_member(id, &next)
            .await
            .map_err(OpError::Store)?;
        entry.members[pos] = next;
        entry.unconfirmed.remove(&index);
        let view = self.settle_run_state(entry).await;
        self.publish(&book);
        drop(book);
        self.events.changed(&view);
        self.wake();
        Ok(view)
    }

    /// Load this instance's active runs and reconcile their admitted members
    /// against the sessions that survived the restart. Idempotent: a run
    /// already in the book is left as it is, and an admitted member whose
    /// session is still alive stays admitted — never re-spawned. A loaded run
    /// idle past [`STALE_RUN_AGE`] has its waiting members cancelled rather
    /// than admitted.
    pub(crate) async fn boot(&self) -> Result<(), String> {
        let mut book = self.book.lock().await;
        if book.booted {
            return Ok(());
        }
        let loaded = match self.store.load_active_runs(&self.owner_instance).await {
            Ok(loaded) => loaded,
            Err(e) => {
                self.set_ledger_state(LedgerState::Failed(e.clone()));
                return Err(e);
            }
        };
        let now = self.now();
        let mut fresh = Vec::new();
        for (run, members) in loaded {
            if let std::collections::btree_map::Entry::Vacant(slot) = book.runs.entry(run.id) {
                fresh.push(run.id);
                slot.insert(RunEntry::new(run, members));
            }
        }
        let mut changed = Vec::new();
        let ids: Vec<Uuid> = book.runs.keys().copied().collect();
        for id in ids {
            let Some(entry) = book.runs.get_mut(&id) else {
                continue;
            };
            let mut touched = false;
            if fresh.contains(&id) && self.is_stale_at_boot(entry, now) {
                touched |= self.cancel_stale(entry, now).await;
            }
            touched |= self
                .reconcile_admitted(entry, reason::RUNNER_RESTARTED, now)
                .await;
            if touched {
                changed.push(self.settle_run_state(entry).await);
            }
        }
        book.booted = true;
        self.publish(&book);
        // Only after the views are published, so a reader that sees `Loaded`
        // never reads the empty pre-boot list.
        self.set_ledger_state(LedgerState::Loaded);
        drop(book);
        for view in &changed {
            self.events.changed(view);
        }
        info!(
            reconciled_runs = changed.len(),
            "fanout: active runs loaded and reconciled"
        );
        Ok(())
    }

    /// Whether a run just loaded at boot is stale (see [`STALE_RUN_AGE`]):
    /// idle past the bound AND not demonstrably in use or deliberately paused.
    fn is_stale_at_boot(&self, entry: &RunEntry, now: DateTime<Utc>) -> bool {
        if now.signed_duration_since(entry.last_activity()) <= STALE_RUN_AGE {
            return false;
        }
        let drain_paused = entry.members.iter().any(|m| {
            matches!(m.state, MemberState::Queued | MemberState::Refused)
                && m.reason.as_deref() == Some(reason::RUNNER_DRAINING)
        });
        if drain_paused || self.host.drain_deferral().is_some() {
            info!(run_id = %entry.run.id,
                "fanout: an idle run was loaded while drain-paused — resumed, not cancelled");
            return false;
        }
        let in_use = entry.members.iter().any(|m| {
            m.state == MemberState::Admitted
                && m.claude_session_id.as_deref().is_some_and(|csid| {
                    matches!(
                        self.host.liveness(csid, m.terminal_id.as_deref()),
                        Liveness::Live { .. } | Liveness::Unconfirmed
                    )
                })
        });
        if in_use {
            info!(run_id = %entry.run.id,
                "fanout: an idle run was loaded with a live member — resumed, not cancelled");
            return false;
        }
        true
    }

    /// Cancel a stale run's waiting members (see [`STALE_RUN_AGE`]).
    async fn cancel_stale(&self, entry: &mut RunEntry, now: DateTime<Utc>) -> bool {
        let run_id = entry.run.id;
        let mut changed = false;
        for m in entry
            .members
            .iter_mut()
            .filter(|m| matches!(m.state, MemberState::Queued | MemberState::Refused))
        {
            m.state = MemberState::Cancelled;
            m.reason = Some(reason::STALE_AFTER_RESTART.to_string());
            self.persist_member(run_id, m, now).await;
            changed = true;
        }
        if changed {
            warn!(run_id = %run_id, created_at = %entry.run.created_at,
                "fanout: a stale run was loaded at start — its waiting members are cancelled, \
                 not admitted");
        }
        changed
    }

    /// One admission pass: release slots whose sessions ended, return refused
    /// members whose backoff elapsed to the queue, then admit the lowest
    /// queued index of each run while
    /// `admitted < min(max_concurrent, parallel_fanout bound)`.
    ///
    /// The admissions are recorded under the book lock; the spawns run after
    /// it is released ([`Self::spawn_admitted`]).
    pub(crate) async fn tick(&self) {
        // A spawn that answered after its loop stopped waiting is recorded
        // before this tick reads liveness, so a late `Spawned` re-adopts its
        // member rather than racing a second release.
        self.record_late_settles().await;
        // Read before taking the book: the bound may resolve the registry
        // over the network.
        let bound = self.host.fanout_bound().await;
        let drain = self.host.drain_deferral();
        let now = self.now();
        let mut pending = Vec::new();
        let mut changed = Vec::new();
        {
            let mut book = self.book.lock().await;
            if !book.booted {
                return;
            }
            let mut halt = false;
            let ids: Vec<Uuid> = book
                .runs
                .iter()
                .filter(|(_, e)| e.run.state == RunState::Active)
                .map(|(id, _)| *id)
                .collect();
            for id in ids {
                let Some(entry) = book.runs.get_mut(&id) else {
                    continue;
                };
                let before = entry.view();
                self.reconcile_admitted(entry, reason::TERMINAL_EXIT, now)
                    .await;
                self.requeue_refused(entry, now).await;
                self.mark_drain(entry, drain.as_deref(), now).await;
                if !halt && drain.is_none() {
                    halt = self.admit(entry, bound, now, &mut pending).await;
                }
                let after = self.settle_run_state(entry).await;
                if after != before {
                    changed.push(after);
                }
            }
            book.runs.retain(|_, e| {
                e.completed_at
                    .is_none_or(|at| now.signed_duration_since(at) < COMPLETED_RETENTION)
            });
            self.publish(&book);
        }
        for view in &changed {
            self.events.changed(view);
        }
        self.spawn_admitted(pending).await;
    }

    /// Spawn the admissions a tick recorded, one at a time and with the book
    /// UNLOCKED, recording each outcome under the lock as it lands. A drain or
    /// an occupied bound stops the rest of the batch; a refusal stops the rest
    /// of that run. A member not attempted goes back to the queue.
    async fn spawn_admitted(&self, pending: Vec<PendingSpawn>) {
        let mut halt_all = false;
        let mut halted_runs: HashSet<Uuid> = HashSet::new();
        for PendingSpawn { req } in pending {
            let settle = if halt_all || halted_runs.contains(&req.run_id) {
                SpawnSettle::NotAttempted
            } else {
                // A task of its own, so a panicking spawn settles rather than
                // stranding the member in flight. A panic says nothing about
                // whether the terminal was created before it — so it is
                // UNKNOWN, never a refusal (which would re-queue the member
                // under a fresh session id and could run its prompt twice).
                self.await_spawn(req.clone()).await
            };
            match &settle {
                SpawnSettle::Outcome(
                    SpawnOutcome::DeferredByDrain { .. } | SpawnOutcome::BoundOccupied { .. },
                ) => {
                    halt_all = true;
                }
                SpawnSettle::Outcome(SpawnOutcome::Refused { .. }) | SpawnSettle::Unknown(_) => {
                    halted_runs.insert(req.run_id);
                }
                _ => {}
            }
            self.record_spawn(&req, settle).await;
        }
    }

    /// Run one spawn on a task of its own and wait for it, at most
    /// `spawn_call_timeout` ([`SPAWN_CALL_TIMEOUT`]).
    ///
    /// The task delivers its outcome over a oneshot; when nobody is listening
    /// any more (this wait timed out, or the loop running it was aborted) it
    /// queues the outcome as a [`LateSettle`] instead, so no answer is ever
    /// dropped. A timeout is outcome-UNKNOWN, exactly like a panic: the member
    /// stays admitted under its pinned session id for liveness to decide, and
    /// the late answer, when it comes, is recorded by the next tick.
    async fn await_spawn(&self, req: MemberSpawnRequest) -> SpawnSettle {
        let (tx, mut rx) = tokio::sync::oneshot::channel::<SpawnOutcome>();
        let host = self.host.clone();
        let late = self.late_settles.clone();
        let task = tokio::spawn(async move {
            let outcome = host.spawn_fanout_member(req.clone()).await;
            if let Err(outcome) = tx.send(outcome) {
                warn!(run_id = %req.run_id, index = req.index, outcome = ?outcome,
                    "fanout: a spawn answered after its caller stopped waiting — \
                     recorded on the next tick");
                let settle = LateSettle { req, outcome };
                match late.lock() {
                    Ok(mut g) => g.push(settle),
                    Err(p) => p.into_inner().push(settle),
                }
            }
        });
        match tokio::time::timeout(self.spawn_call_timeout, &mut rx).await {
            Ok(Ok(outcome)) => SpawnSettle::Outcome(outcome),
            // The sender was dropped unsent: the task panicked (or was
            // cancelled) inside the spawn.
            Ok(Err(_)) => match task.await {
                Err(e) => SpawnSettle::Unknown(format!("spawn task failed: {e}")),
                Ok(()) => SpawnSettle::Unknown("spawn task ended without an answer".to_string()),
            },
            Err(_) => {
                // Close first, then look once more: an outcome sent in the gap
                // between the timeout and the close is taken here, and any
                // later one fails its send and goes to the late queue — never
                // both, never neither.
                rx.close();
                match rx.try_recv() {
                    Ok(outcome) => SpawnSettle::Outcome(outcome),
                    Err(_) => SpawnSettle::Unknown(format!(
                        "spawn did not answer within {}s — still running; its outcome is \
                         recorded when it lands",
                        self.spawn_call_timeout.as_secs()
                    )),
                }
            }
        }
    }

    /// Record what one spawn came to.
    async fn record_spawn(&self, req: &MemberSpawnRequest, settle: SpawnSettle) {
        let now = self.now();
        let view = {
            let mut book = self.book.lock().await;
            let Some(entry) = book.runs.get_mut(&req.run_id) else {
                warn!(run_id = %req.run_id, index = req.index,
                    "fanout: a spawn settled for a run the book no longer holds");
                return;
            };
            // The member this spawn was for: same index, same pinned session.
            let Some(pos) = entry.members.iter().position(|m| {
                m.index == req.index
                    && m.claude_session_id.as_deref() == Some(req.claude_session_id.as_str())
            }) else {
                warn!(run_id = %req.run_id, index = req.index,
                    "fanout: a spawn settled for a member no longer holding its session");
                return;
            };
            // Only this admission's settle may clear its in-flight marker.
            entry.spawning.remove(&req.index);
            let run_id = entry.run.id;
            if entry.members[pos].state != MemberState::Admitted {
                if !readopt_late_spawn(&mut entry.members[pos], &settle, now) {
                    warn!(run_id = %run_id, index = req.index,
                        state = ?entry.members[pos].state, outcome = ?settle,
                        "fanout: a spawn settled for a member no longer admitted — ignored");
                    return;
                }
                // A running `claude` the liveness reconcile released before
                // its spawn answered (it outlived SPAWN_CALL_TIMEOUT and its
                // record was not written yet). It counts toward the cap again:
                // the run may sit one over its cap until a member ends, which
                // is the lesser harm than a session nothing tracks.
                warn!(run_id = %run_id, index = req.index,
                    session = %req.claude_session_id,
                    terminal_id = ?entry.members[pos].terminal_id,
                    "fanout: a late spawn landed for a member released spawn_unconfirmed — \
                     re-adopted as admitted; the run may briefly exceed its cap");
                entry.unconfirmed.remove(&req.index);
                entry.retry.remove(&req.index);
                self.persist_member(run_id, &mut entry.members[pos], now)
                    .await;
                if entry.run.state == RunState::Completed {
                    entry.completed_at = None;
                }
                let view = self.settle_run_state(entry).await;
                self.publish(&book);
                drop(book);
                self.events.changed(&view);
                return;
            }
            let cancel_requested = entry.cancel_requested;
            let member = &mut entry.members[pos];
            match settle {
                SpawnSettle::Unknown(why) => {
                    // Outcome-UNKNOWN: keep the admission and its pinned
                    // session id; the in-flight marker is gone, so the next
                    // tick's reconcile reads liveness — live/unconfirmed keeps
                    // the slot, no record at all releases `spawn_unconfirmed`.
                    // A cancel never applies: the member may be running.
                    //
                    // A terminal already pinned to the session (the create
                    // registered it, then the task panicked before the
                    // lifecycle record existed) is recorded now, so the
                    // member is never read as `Unrecorded` with no terminal.
                    let found = match self.host.liveness(&req.claude_session_id, None) {
                        Liveness::Live { terminal_id } => Some(terminal_id),
                        _ => None,
                    };
                    warn!(run_id = %run_id, index = member.index, error = %why,
                        terminal_id = ?found,
                        "fanout: a spawn's outcome was lost — member kept admitted under its \
                         session id for the liveness reconcile to decide");
                    member.terminal_id = found;
                    entry.unconfirmed.remove(&req.index);
                }
                SpawnSettle::Outcome(SpawnOutcome::Spawned { terminal_id }) => {
                    info!(run_id = %run_id, index = member.index, terminal_id = %terminal_id,
                        "fanout: member admitted");
                    member.terminal_id = Some(terminal_id);
                    entry.retry.remove(&req.index);
                }
                not_spawned if cancel_requested => {
                    info!(run_id = %run_id, index = member.index, outcome = ?not_spawned,
                        "fanout: spawn did not happen after a cancel — member cancelled");
                    unadmit(
                        member,
                        MemberState::Cancelled,
                        Some(reason::CANCELLED.to_string()),
                    );
                    entry.retry.remove(&req.index);
                }
                SpawnSettle::Outcome(SpawnOutcome::DeferredByDrain { reason: why }) => {
                    info!(run_id = %run_id, index = member.index, drain = %why,
                        "fanout: admission deferred by the device drain");
                    unadmit(
                        member,
                        MemberState::Queued,
                        Some(reason::RUNNER_DRAINING.to_string()),
                    );
                }
                SpawnSettle::Outcome(SpawnOutcome::BoundOccupied { detail }) => {
                    info!(run_id = %run_id, index = member.index, detail = %detail,
                        "fanout: parallel_fanout bound occupied — retrying next tick");
                    unadmit(
                        member,
                        MemberState::Queued,
                        Some(reason::FANOUT_BOUND_OCCUPIED.to_string()),
                    );
                }
                SpawnSettle::Outcome(SpawnOutcome::Refused { reason: why }) => {
                    let class = reason_class(&why);
                    let attempts = match entry.retry.get(&req.index) {
                        Some(r) if r.class == class => r.attempts.saturating_add(1),
                        _ => 1,
                    };
                    let next_at = now + retry_delay(attempts);
                    warn!(run_id = %run_id, index = member.index, reason = %why, attempts,
                        next_retry_at = %next_at, "fanout: member refused — retried after a backoff");
                    unadmit(member, MemberState::Refused, Some(why));
                    entry.retry.insert(
                        req.index,
                        RetryState {
                            class,
                            attempts,
                            next_at,
                        },
                    );
                }
                SpawnSettle::NotAttempted => unadmit(member, MemberState::Queued, None),
            }
            self.persist_member(run_id, &mut entry.members[pos], now)
                .await;
            let view = self.settle_run_state(entry).await;
            self.publish(&book);
            view
        };
        self.events.changed(&view);
    }

    /// Release every admitted member whose session ended (the finished marker,
    /// or its terminal exited), recording `exit_reason` for an exit. A live
    /// member that moved terminals is re-pointed. A member whose spawn is in
    /// flight is skipped. Returns whether anything changed.
    async fn reconcile_admitted(
        &self,
        entry: &mut RunEntry,
        exit_reason: &str,
        now: DateTime<Utc>,
    ) -> bool {
        let run_id = entry.run.id;
        let mut changed = false;
        for m in entry
            .members
            .iter_mut()
            .filter(|m| m.state == MemberState::Admitted)
        {
            if let Some(at) = entry.spawning.get(&m.index).copied() {
                if now.signed_duration_since(at) < SPAWN_SETTLE_BOUND {
                    continue;
                }
                // The settle was lost (the loop driving the spawn panicked or
                // was aborted): outcome-UNKNOWN, decided by liveness below.
                warn!(run_id = %run_id, index = m.index, admitted_at = %at,
                    "fanout: a spawn never settled — its outcome is read from liveness");
                entry.spawning.remove(&m.index);
            }
            let Some(csid) = m.claude_session_id.clone() else {
                // Admitted with no session id cannot occur through `admit`;
                // a hand-edited row is released rather than holding a slot
                // nothing can ever free.
                mark_released(m, exit_reason, now);
                self.persist_member(run_id, m, now).await;
                changed = true;
                continue;
            };
            let liveness = self.host.liveness(&csid, m.terminal_id.as_deref());
            if !matches!(liveness, Liveness::Unconfirmed | Liveness::Unknown) {
                entry.unconfirmed.remove(&m.index);
            }
            let release = match liveness {
                Liveness::Live { terminal_id } => {
                    if m.terminal_id.as_deref() != Some(terminal_id.as_str()) {
                        m.terminal_id = Some(terminal_id);
                        self.persist_member(run_id, m, now).await;
                        changed = true;
                    }
                    None
                }
                Liveness::Finished => Some(reason::FINISHED.to_string()),
                Liveness::Exited => Some(exit_reason.to_string()),
                // No record and no terminal: a member whose spawn was never
                // recorded may never have run — not the same as one that did.
                Liveness::Unrecorded if m.terminal_id.is_none() => {
                    Some(reason::SPAWN_UNCONFIRMED.to_string())
                }
                Liveness::Unrecorded => Some(exit_reason.to_string()),
                Liveness::Unconfirmed => {
                    let (since, why) = entry
                        .unconfirmed
                        .entry(m.index)
                        .or_insert_with(|| (now, exit_reason.to_string()))
                        .clone();
                    (now.signed_duration_since(since) >= LIVENESS_GRACE).then_some(why)
                }
                Liveness::Unknown => None,
            };
            if let Some(why) = release {
                entry.unconfirmed.remove(&m.index);
                mark_released(m, &why, now);
                self.persist_member(run_id, m, now).await;
                changed = true;
            }
        }
        changed
    }

    /// `refused → queued` once the member's backoff has elapsed, keeping the
    /// reason visible.
    async fn requeue_refused(&self, entry: &mut RunEntry, now: DateTime<Utc>) {
        let run_id = entry.run.id;
        for m in entry
            .members
            .iter_mut()
            .filter(|m| m.state == MemberState::Refused)
        {
            if entry.retry.get(&m.index).is_some_and(|r| r.next_at > now) {
                continue;
            }
            m.state = MemberState::Queued;
            self.persist_member(run_id, m, now).await;
        }
    }

    /// While drained, every queued member says so; once the drain lifts the
    /// stale word is cleared.
    async fn mark_drain(&self, entry: &mut RunEntry, drain: Option<&str>, now: DateTime<Utc>) {
        let run_id = entry.run.id;
        for m in entry
            .members
            .iter_mut()
            .filter(|m| m.state == MemberState::Queued)
        {
            let draining = m.reason.as_deref() == Some(reason::RUNNER_DRAINING);
            let next = match (drain.is_some(), draining) {
                (true, false) => Some(reason::RUNNER_DRAINING.to_string()),
                (false, true) => None,
                _ => continue,
            };
            m.reason = next;
            self.persist_member(run_id, m, now).await;
        }
        if let Some(why) = drain {
            if entry.members.iter().any(|m| m.state == MemberState::Queued) {
                tracing::debug!(run_id = %run_id, drain = %why, "fanout: admission deferred by the device drain");
            }
        }
    }

    /// Admit queued members of one run up to its cap, recording each admission
    /// and queueing its spawn on `pending`. Returns `true` when the whole tick
    /// must stop admitting (the ledger could not record an admission).
    async fn admit(
        &self,
        entry: &mut RunEntry,
        bound: u32,
        now: DateTime<Utc>,
        pending: &mut Vec<PendingSpawn>,
    ) -> bool {
        let cap = entry.run.max_concurrent.min(bound).max(1) as usize;
        let run_id = entry.run.id;
        loop {
            // In-flight spawns are `admitted`, so they count here.
            let admitted = entry
                .members
                .iter()
                .filter(|m| m.state == MemberState::Admitted)
                .count();
            if admitted >= cap {
                return false;
            }
            let Some(pos) = entry
                .members
                .iter()
                .enumerate()
                .filter(|(_, m)| m.state == MemberState::Queued)
                .min_by_key(|(_, m)| m.index)
                .map(|(pos, _)| pos)
            else {
                return false;
            };

            // Record the admission BEFORE spawning, under a freshly minted
            // session id: a crash between the two then reads, on restart, as an
            // admitted member with no session (released `spawn_unconfirmed`),
            // never as a queued one that would be spawned a second time.
            let csid = Uuid::new_v4().to_string();
            let mut admitted_member = entry.members[pos].clone();
            admitted_member.state = MemberState::Admitted;
            admitted_member.claude_session_id = Some(csid.clone());
            admitted_member.terminal_id = None;
            admitted_member.reason = None;
            admitted_member.admitted_at = Some(now);
            admitted_member.released_at = None;
            admitted_member.updated_at = Some(now);
            if let Err(e) = self.store.update_member(run_id, &admitted_member).await {
                warn!(run_id = %run_id, index = admitted_member.index, error = %e,
                    "fanout: could not record an admission — not spawning");
                entry.members[pos].reason = Some(reason::STORE_UNAVAILABLE.to_string());
                return true;
            }
            let index = admitted_member.index;
            entry.members[pos] = admitted_member;
            entry.spawning.insert(index, now);
            entry.unconfirmed.remove(&index);
            pending.push(PendingSpawn {
                req: MemberSpawnRequest {
                    run_id,
                    index,
                    title: entry.members[pos].title.clone(),
                    prompt: entry.members[pos].prompt.clone(),
                    working_dir: entry.run.working_dir.clone(),
                    tenant_id: entry.run.tenant_id,
                    config_dir_policy: entry.run.config_dir_policy.clone(),
                    claude_session_id: csid,
                },
            });
        }
    }

    /// Recompute the run's state from its members; persist a change.
    async fn settle_run_state(&self, entry: &mut RunEntry) -> RunView {
        let state = derived_run_state(&entry.members);
        if state != entry.run.state {
            entry.run.state = state;
            entry.run.updated_at = Some(self.now());
            if state == RunState::Completed {
                entry.completed_at = Some(self.now());
                info!(run_id = %entry.run.id, "fanout: run completed");
            }
            if let Err(e) = self.store.update_run(&entry.run).await {
                warn!(run_id = %entry.run.id, error = %e,
                    "fanout: could not persist the run's state — the book keeps it");
            }
        }
        entry.view()
    }

    /// Write-through for a transition the tick has already decided. A failed
    /// write is logged and the book keeps the transition: every tick
    /// transition is either re-derived from liveness on restart or is a
    /// return to the queue, so a stale row can never cause a second spawn.
    async fn persist_member(&self, run_id: Uuid, member: &mut FanoutMember, now: DateTime<Utc>) {
        member.updated_at = Some(now);
        if let Err(e) = self.store.update_member(run_id, member).await {
            warn!(run_id = %run_id, index = member.index, error = %e,
                "fanout: could not persist a member transition — the book keeps it");
        }
    }

    fn publish(&self, book: &Book) {
        let mut views: Vec<RunView> = book.runs.values().map(RunEntry::view).collect();
        views.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
        let views = Arc::new(views);
        match self.published.write() {
            Ok(mut g) => *g = views,
            Err(p) => *p.into_inner() = views,
        }
    }
}

fn mark_released(m: &mut FanoutMember, why: &str, now: DateTime<Utc>) {
    m.state = MemberState::Released;
    m.reason = Some(why.to_string());
    m.released_at = Some(now);
}

/// Re-admit a member whose spawn answered `Spawned` after the liveness
/// reconcile had released it `spawn_unconfirmed` — the only release a late
/// spawn can contradict. Any other state (an operator release, an exit, a
/// cancel) or any other outcome is left alone. Returns whether it re-adopted.
fn readopt_late_spawn(m: &mut FanoutMember, settle: &SpawnSettle, now: DateTime<Utc>) -> bool {
    let SpawnSettle::Outcome(SpawnOutcome::Spawned { terminal_id }) = settle else {
        return false;
    };
    if m.state != MemberState::Released || m.reason.as_deref() != Some(reason::SPAWN_UNCONFIRMED) {
        return false;
    }
    m.state = MemberState::Admitted;
    m.terminal_id = Some(terminal_id.clone());
    m.reason = None;
    m.released_at = None;
    if m.admitted_at.is_none() {
        m.admitted_at = Some(now);
    }
    true
}

/// Undo an admission that did not produce a session.
fn unadmit(m: &mut FanoutMember, state: MemberState, why: Option<String>) {
    m.state = state;
    m.claude_session_id = None;
    m.terminal_id = None;
    m.admitted_at = None;
    m.reason = why;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fanout::model::{build_members, MemberInput};
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryStore {
        rows: Mutex<HashMap<Uuid, (FanoutRun, Vec<FanoutMember>)>>,
        fail_member_writes: std::sync::atomic::AtomicBool,
        fail_loads: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl FanoutStore for MemoryStore {
        async fn insert_run(
            &self,
            run: &FanoutRun,
            members: &[FanoutMember],
        ) -> Result<(), String> {
            self.rows
                .lock()
                .unwrap()
                .insert(run.id, (run.clone(), members.to_vec()));
            Ok(())
        }
        async fn update_run(&self, run: &FanoutRun) -> Result<(), String> {
            let mut rows = self.rows.lock().unwrap();
            let row = rows.get_mut(&run.id).ok_or("no run")?;
            row.0 = run.clone();
            Ok(())
        }
        async fn update_member(&self, run_id: Uuid, member: &FanoutMember) -> Result<(), String> {
            if self.fail_member_writes.load(Ordering::SeqCst) {
                return Err("store down".to_string());
            }
            let mut rows = self.rows.lock().unwrap();
            let row = rows.get_mut(&run_id).ok_or("no run")?;
            let slot = row
                .1
                .iter_mut()
                .find(|m| m.index == member.index)
                .ok_or("no member")?;
            *slot = member.clone();
            Ok(())
        }
        async fn load_active_runs(
            &self,
            owner_instance: &str,
        ) -> Result<Vec<(FanoutRun, Vec<FanoutMember>)>, String> {
            if self.fail_loads.load(Ordering::SeqCst) {
                return Err("connection refused".to_string());
            }
            Ok(self
                .rows
                .lock()
                .unwrap()
                .values()
                .filter(|(r, _)| r.state == RunState::Active && r.owner_instance == owner_instance)
                .cloned()
                .collect())
        }
    }

    /// A recording spawner. A session is "alive" while its id is in `live`;
    /// a test ends one by removing it (a terminal exit) or adding it to
    /// `finished`. Every spawn asserts the cap it was handed was respected.
    #[derive(Default)]
    struct MockHost {
        bound: AtomicU32,
        drained: Mutex<Option<String>>,
        live: Mutex<HashMap<String, String>>,
        finished: Mutex<HashSet<String>>,
        scripted: Mutex<VecDeque<SpawnOutcome>>,
        spawns: Mutex<Vec<MemberSpawnRequest>>,
        max_live: AtomicU32,
        cap_limit: AtomicU32,
        no_terminal_manager: std::sync::atomic::AtomicBool,
        /// Sessions whose record is open but no terminal answers.
        unconfirmed: Mutex<HashSet<String>>,
        /// Sessions with no record and no terminal.
        unrecorded: Mutex<HashSet<String>>,
        /// When set, every spawn waits for a permit — a spawn held in flight.
        gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
        /// Spawns that have STARTED (before the gate).
        started: AtomicU32,
        /// Make the next spawn panic: `Some(true)` after its terminal is
        /// created (the session runs), `Some(false)` before (nothing ran).
        panic_next: Mutex<Option<bool>>,
    }

    impl MockHost {
        fn with_bound(bound: u32) -> Arc<Self> {
            let h = Self::default();
            h.bound.store(bound, Ordering::SeqCst);
            h.cap_limit.store(u32::MAX, Ordering::SeqCst);
            Arc::new(h)
        }
        fn exit(&self, csid: &str) {
            self.live.lock().unwrap().remove(csid);
        }
        fn spawn_count(&self) -> usize {
            self.spawns.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl FanoutHost for MockHost {
        fn drain_deferral(&self) -> Option<String> {
            self.drained.lock().unwrap().clone()
        }
        async fn fanout_bound(&self) -> u32 {
            self.bound.load(Ordering::SeqCst)
        }
        async fn spawn_fanout_member(&self, req: MemberSpawnRequest) -> SpawnOutcome {
            self.started.fetch_add(1, Ordering::SeqCst);
            let gate = self.gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.acquire().await.unwrap().forget();
            }
            // Yield so concurrent releases interleave with the spawn.
            tokio::task::yield_now().await;
            self.spawns.lock().unwrap().push(req.clone());
            let panic_after_create = self.panic_next.lock().unwrap().take();
            match panic_after_create {
                Some(true) => {
                    let terminal_id = format!("term-{}-{}", req.run_id.simple(), req.index);
                    self.live
                        .lock()
                        .unwrap()
                        .insert(req.claude_session_id.clone(), terminal_id);
                    panic!("spawn panicked after creating its terminal");
                }
                Some(false) => {
                    self.unrecorded
                        .lock()
                        .unwrap()
                        .insert(req.claude_session_id.clone());
                    panic!("spawn panicked before creating anything");
                }
                None => {}
            }
            if let Some(o) = self.scripted.lock().unwrap().pop_front() {
                if !matches!(o, SpawnOutcome::Spawned { .. }) {
                    return o;
                }
            }
            let terminal_id = format!("term-{}-{}", req.run_id.simple(), req.index);
            // A session a test marked finished while its spawn was in flight
            // finished at once: it never holds a live terminal.
            if self
                .finished
                .lock()
                .unwrap()
                .contains(&req.claude_session_id)
            {
                return SpawnOutcome::Spawned { terminal_id };
            }
            let mut live = self.live.lock().unwrap();
            live.insert(req.claude_session_id.clone(), terminal_id.clone());
            let n = live.len() as u32;
            assert!(
                n <= self.cap_limit.load(Ordering::SeqCst),
                "spawned a session past the cap: {n} live"
            );
            self.max_live.fetch_max(n, Ordering::SeqCst);
            SpawnOutcome::Spawned { terminal_id }
        }
        fn liveness(&self, claude_session_id: &str, _terminal_id: Option<&str>) -> Liveness {
            if self.no_terminal_manager.load(Ordering::SeqCst) {
                return Liveness::Unknown;
            }
            if self.finished.lock().unwrap().contains(claude_session_id) {
                return Liveness::Finished;
            }
            if self.unconfirmed.lock().unwrap().contains(claude_session_id) {
                return Liveness::Unconfirmed;
            }
            if self.unrecorded.lock().unwrap().contains(claude_session_id) {
                return Liveness::Unrecorded;
            }
            match self.live.lock().unwrap().get(claude_session_id) {
                Some(t) => Liveness::Live {
                    terminal_id: t.clone(),
                },
                None => Liveness::Exited,
            }
        }
    }

    #[derive(Default)]
    struct RecordingEvents(Mutex<Vec<RunView>>);
    impl FanoutEvents for RecordingEvents {
        fn changed(&self, run: &RunView) {
            self.0.lock().unwrap().push(run.clone());
        }
    }

    fn members(n: usize) -> Vec<FanoutMember> {
        let inputs: Vec<MemberInput> = (0..n)
            .map(|i| MemberInput {
                title: format!("member {i}"),
                prompt: format!("do task {i}"),
                preview_index: None,
            })
            .collect();
        build_members(&inputs).unwrap()
    }

    fn new_run(n: usize, cap: u32, tenant: Option<Uuid>) -> NewRun {
        NewRun {
            tenant_id: tenant,
            template_slug: Some("seed".to_string()),
            template_version: Some(2),
            requested_max_concurrent: cap,
            config_dir_policy: ConfigDirPolicy::BestHeadroom,
            working_dir: "/work/repo".to_string(),
            members: members(n),
        }
    }

    struct Fixture {
        store: Arc<MemoryStore>,
        host: Arc<MockHost>,
        events: Arc<RecordingEvents>,
        d: Arc<FanoutDispatcher>,
    }

    async fn fixture(bound: u32) -> Fixture {
        fixture_with_spawn_timeout(bound, SPAWN_CALL_TIMEOUT).await
    }

    async fn fixture_with_spawn_timeout(bound: u32, timeout: std::time::Duration) -> Fixture {
        let store = Arc::new(MemoryStore::default());
        let host = MockHost::with_bound(bound);
        let events = Arc::new(RecordingEvents::default());
        let mut d = FanoutDispatcher::new(
            "primary".to_string(),
            store.clone(),
            host.clone(),
            events.clone(),
        );
        d.spawn_call_timeout = timeout;
        let d = Arc::new(d);
        d.boot().await.unwrap();
        Fixture {
            store,
            host,
            events,
            d,
        }
    }

    /// Move the dispatcher's clock forward without sleeping.
    fn advance(d: &FanoutDispatcher, secs: i64) {
        d.clock_skew_secs.fetch_add(secs, Ordering::SeqCst);
    }

    fn states(view: &RunView) -> Vec<MemberState> {
        view.members.iter().map(|m| m.state).collect()
    }

    fn csid(view: &RunView, index: usize) -> String {
        view.members[index].claude_session_id.clone().unwrap()
    }

    #[tokio::test]
    async fn fanout_admits_lowest_index_up_to_the_cap() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(5, 2, None)).await.unwrap().run.id;
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        use MemberState::*;
        assert_eq!(states(&v), vec![Admitted, Admitted, Queued, Queued, Queued]);
        // A second tick with nothing released admits nothing more.
        f.d.tick().await;
        assert_eq!(f.host.spawn_count(), 2);
        // A terminal exit frees exactly one slot, taken by the next index.
        f.host.exit(&csid(&v, 0));
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(
            states(&v),
            vec![Released, Admitted, Admitted, Queued, Queued]
        );
        assert_eq!(v.members[0].reason.as_deref(), Some(reason::TERMINAL_EXIT));
        assert!(v.members[0].released_at.is_some());
    }

    #[tokio::test]
    async fn fanout_every_spawn_gets_a_fresh_session_id_and_the_prompt() {
        let f = fixture(15).await;
        f.d.create(new_run(3, 3, None)).await.unwrap();
        f.d.tick().await;
        let spawns = f.host.spawns.lock().unwrap().clone();
        let ids: HashSet<&str> = spawns
            .iter()
            .map(|s| s.claude_session_id.as_str())
            .collect();
        assert_eq!(ids.len(), 3, "session ids must be distinct");
        for (i, s) in spawns.iter().enumerate() {
            assert!(Uuid::parse_str(&s.claude_session_id).is_ok());
            assert_eq!(s.prompt, format!("do task {i}"));
            assert_eq!(s.working_dir, "/work/repo");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fanout_cap_is_never_exceeded_under_concurrent_releases() {
        let f = fixture(15).await;
        f.host.cap_limit.store(3, Ordering::SeqCst);
        let id = f.d.create(new_run(40, 3, None)).await.unwrap().run.id;
        let mut handles = Vec::new();
        // Tickers.
        for _ in 0..3 {
            let d = f.d.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..60 {
                    d.tick().await;
                    let v = d.get(id).unwrap().unwrap();
                    assert!(v.counts.admitted <= 3, "{} admitted", v.counts.admitted);
                    tokio::task::yield_now().await;
                }
            }));
        }
        // Terminal exits.
        {
            let d = f.d.clone();
            let host = f.host.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..200 {
                    let v = d.get(id).unwrap().unwrap();
                    if let Some(m) = v.members.iter().find(|m| m.state == MemberState::Admitted) {
                        host.exit(m.claude_session_id.as_deref().unwrap());
                    }
                    tokio::task::yield_now().await;
                }
            }));
        }
        // Finished markers, racing the exits.
        {
            let d = f.d.clone();
            let host = f.host.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..200 {
                    let v = d.get(id).unwrap().unwrap();
                    if let Some(m) = v
                        .members
                        .iter()
                        .rev()
                        .find(|m| m.state == MemberState::Admitted)
                    {
                        let csid = m.claude_session_id.clone().unwrap();
                        // A finished session's terminal is gone from the cap's
                        // point of view the moment the slot is released.
                        host.finished.lock().unwrap().insert(csid.clone());
                        host.live.lock().unwrap().remove(&csid);
                    }
                    tokio::task::yield_now().await;
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        // Drain the queue to the end.
        for _ in 0..200 {
            let v = f.d.get(id).unwrap().unwrap();
            for m in v
                .members
                .iter()
                .filter(|m| m.state == MemberState::Admitted)
            {
                f.host.exit(m.claude_session_id.as_deref().unwrap());
            }
            f.d.tick().await;
            if f.d.get(id).unwrap().unwrap().state == RunState::Completed {
                break;
            }
        }
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(v.state, RunState::Completed, "{:?}", v.counts);
        assert_eq!(v.counts.released, 40);
        assert_eq!(
            f.host.spawn_count(),
            40,
            "every member spawned exactly once"
        );
        assert!(f.host.max_live.load(Ordering::SeqCst) <= 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fanout_operator_releases_racing_ticks_never_overfill_the_book() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(30, 4, None)).await.unwrap().run.id;
        let mut handles = Vec::new();
        for _ in 0..3 {
            let d = f.d.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..50 {
                    d.tick().await;
                    tokio::task::yield_now().await;
                }
            }));
        }
        for worker in 0..3u32 {
            let d = f.d.clone();
            handles.push(tokio::spawn(async move {
                for round in 0..80u32 {
                    let index = (round * 3 + worker) % 30;
                    // Conflict (not admitted) and Ok are both fine; the cap is
                    // what is under test.
                    let _ = d.release(id, index).await;
                    let v = d.get(id).unwrap().unwrap();
                    assert!(v.counts.admitted <= 4, "{} admitted", v.counts.admitted);
                    tokio::task::yield_now().await;
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert!(f.d.get(id).unwrap().unwrap().counts.admitted <= 4);
    }

    #[tokio::test]
    async fn fanout_cap_is_clamped_to_the_parallel_fanout_bound() {
        let f = fixture(2).await;
        let out = f.d.create(new_run(6, 5, None)).await.unwrap();
        assert_eq!(out.run.max_concurrent, 2);
        assert_eq!(out.fanout_bound, 2);
        assert_eq!(out.clamped_from, Some(5));
        let id = out.run.id;
        let out = f.d.set_max_concurrent(id, 10).await.unwrap();
        assert_eq!((out.run.max_concurrent, out.clamped_from), (2, Some(10)));
        let out = f.d.set_max_concurrent(id, 1).await.unwrap();
        assert_eq!((out.run.max_concurrent, out.clamped_from), (1, None));
        let out = f.d.set_max_concurrent(id, 2).await.unwrap();
        assert_eq!(out.run.max_concurrent, 2);
        // The bound is re-checked per admission: it dropping after create
        // shrinks the effective cap without a PATCH.
        f.host.bound.store(1, Ordering::SeqCst);
        f.d.tick().await;
        assert_eq!(f.d.get(id).unwrap().unwrap().counts.admitted, 1);
        // And the persisted run carries the clamped value.
        let rows = f.store.rows.lock().unwrap();
        assert_eq!(rows.get(&id).unwrap().0.max_concurrent, 2);
    }

    #[tokio::test]
    async fn fanout_refusal_keeps_the_member_queued_with_its_reason() {
        let f = fixture(15).await;
        f.host
            .scripted
            .lock()
            .unwrap()
            .push_back(SpawnOutcome::Refused {
                reason: "resource_guard:critical: commit charge at 97%".to_string(),
            });
        let id = f.d.create(new_run(3, 2, None)).await.unwrap().run.id;
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        use MemberState::*;
        // The refusal stops this run's admission for the tick.
        assert_eq!(states(&v), vec![Refused, Queued, Queued]);
        assert!(v.members[0]
            .reason
            .as_deref()
            .unwrap()
            .starts_with("resource_guard:critical:"));
        assert!(v.members[0].claude_session_id.is_none());
        assert_eq!(v.counts.admitted, 0);
        assert_eq!(v.members[0].refusals, 1);
        assert!(v.members[0].next_retry_at.is_some());
        // Once the first backoff has elapsed: back to the queue, then admitted.
        advance(&f.d, 5);
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(states(&v), vec![Admitted, Admitted, Queued]);
        assert!(v.members[0].reason.is_none());
    }

    #[tokio::test]
    async fn fanout_drained_runner_leaves_members_queued() {
        let f = fixture(15).await;
        *f.host.drained.lock().unwrap() = Some("coord drained this device".to_string());
        let id = f.d.create(new_run(3, 2, None)).await.unwrap().run.id;
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(f.host.spawn_count(), 0);
        assert!(v.members.iter().all(|m| m.state == MemberState::Queued));
        assert!(v
            .members
            .iter()
            .all(|m| m.reason.as_deref() == Some(reason::RUNNER_DRAINING)));
        // The spawn's own gate deferring (a drain that landed between the
        // pre-check and the spawn) is the same: queued, never refused.
        *f.host.drained.lock().unwrap() = None;
        f.host
            .scripted
            .lock()
            .unwrap()
            .push_back(SpawnOutcome::DeferredByDrain {
                reason: "drained".to_string(),
            });
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert!(v.members.iter().all(|m| m.state == MemberState::Queued));
        assert_eq!(
            v.members[0].reason.as_deref(),
            Some(reason::RUNNER_DRAINING)
        );
        // Undrained: admitted, and the stale word is gone from the rest.
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        use MemberState::*;
        assert_eq!(states(&v), vec![Admitted, Admitted, Queued]);
        assert!(v.members.iter().all(|m| m.reason.is_none()));
    }

    #[tokio::test]
    async fn fanout_cancel_leaves_admitted_members_alone() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(4, 1, None)).await.unwrap().run.id;
        f.d.tick().await;
        let v = f.d.cancel(id).await.unwrap();
        use MemberState::*;
        assert_eq!(states(&v), vec![Admitted, Cancelled, Cancelled, Cancelled]);
        assert_eq!(v.state, RunState::Active);
        let live = csid(&v, 0);
        assert!(
            f.host.live.lock().unwrap().contains_key(&live),
            "session untouched"
        );
        // Nothing cancelled is ever spawned.
        f.host.exit(&live);
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(states(&v), vec![Released, Cancelled, Cancelled, Cancelled]);
        assert_eq!(v.state, RunState::Completed);
        assert_eq!(f.host.spawn_count(), 1);
        let rows = f.store.rows.lock().unwrap();
        assert_eq!(rows.get(&id).unwrap().0.state, RunState::Completed);
    }

    #[tokio::test]
    async fn fanout_release_route_frees_a_slot_without_touching_the_session() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(2, 1, None)).await.unwrap().run.id;
        f.d.tick().await;
        assert!(matches!(
            f.d.release(id, 1).await,
            Err(OpError::Conflict(_))
        ));
        assert!(matches!(
            f.d.release(id, 9).await,
            Err(OpError::NotFound(_))
        ));
        let v = f.d.release(id, 0).await.unwrap();
        assert_eq!(
            v.members[0].reason.as_deref(),
            Some(reason::OPERATOR_RELEASE)
        );
        assert!(f.host.live.lock().unwrap().contains_key(&csid(&v, 0)));
        f.d.tick().await;
        assert_eq!(
            f.d.get(id).unwrap().unwrap().members[1].state,
            MemberState::Admitted
        );
    }

    #[tokio::test]
    async fn fanout_finished_marker_releases_the_slot() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(2, 1, None)).await.unwrap().run.id;
        f.d.tick().await;
        let first = csid(&f.d.get(id).unwrap().unwrap(), 0);
        f.host.finished.lock().unwrap().insert(first);
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(v.members[0].state, MemberState::Released);
        assert_eq!(v.members[0].reason.as_deref(), Some(reason::FINISHED));
        assert_eq!(v.members[1].state, MemberState::Admitted);
    }

    #[tokio::test]
    async fn fanout_unknown_liveness_never_releases() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(2, 1, None)).await.unwrap().run.id;
        f.d.tick().await;
        f.host.no_terminal_manager.store(true, Ordering::SeqCst);
        f.host.live.lock().unwrap().clear();
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(v.members[0].state, MemberState::Admitted);
        assert_eq!(v.members[1].state, MemberState::Queued);
    }

    #[tokio::test]
    async fn fanout_restart_reconcile_is_idempotent_and_keeps_survivors() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(4, 3, None)).await.unwrap().run.id;
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        let (survivor, lost) = (csid(&v, 0), csid(&v, 1));
        let survivor_terminal = v.members[0].terminal_id.clone().unwrap();
        // The runner dies: member 1's terminal does not come back, member 0's
        // does (pty-holder), and member 2's comes back under a NEW terminal id.
        f.host.exit(&lost);
        let rebound = csid(&v, 2);
        f.host
            .live
            .lock()
            .unwrap()
            .insert(rebound.clone(), "term-restored".to_string());
        let spawned_before = f.host.spawn_count();

        let events = Arc::new(RecordingEvents::default());
        let restarted = FanoutDispatcher::new(
            "primary".to_string(),
            f.store.clone(),
            f.host.clone(),
            events.clone(),
        );
        restarted.boot().await.unwrap();
        let v = restarted.get(id).unwrap().unwrap();
        use MemberState::*;
        assert_eq!(states(&v), vec![Admitted, Released, Admitted, Queued]);
        assert_eq!(
            v.members[0].terminal_id.as_deref(),
            Some(survivor_terminal.as_str())
        );
        assert_eq!(
            v.members[1].reason.as_deref(),
            Some(reason::RUNNER_RESTARTED)
        );
        assert_eq!(v.members[2].terminal_id.as_deref(), Some("term-restored"));
        assert_eq!(
            f.host.spawn_count(),
            spawned_before,
            "reconcile spawns nothing"
        );
        assert!(!events.0.lock().unwrap().is_empty());

        // Idempotent: a second boot (a supervised respawn of the loop) and a
        // whole second restart both land on the same book.
        restarted.boot().await.unwrap();
        assert_eq!(restarted.get(id).unwrap().unwrap(), v);
        let again = FanoutDispatcher::new(
            "primary".to_string(),
            f.store.clone(),
            f.host.clone(),
            Arc::new(RecordingEvents::default()),
        );
        again.boot().await.unwrap();
        let w = again.get(id).unwrap().unwrap();
        assert_eq!(states(&w), states(&v));
        assert_eq!(f.host.spawn_count(), spawned_before);
        assert_eq!(survivor, csid(&w, 0));

        // The freed slot goes to the next queued member, and only it.
        again.tick().await;
        let w = again.get(id).unwrap().unwrap();
        assert_eq!(states(&w), vec![Admitted, Released, Admitted, Admitted]);
        assert_eq!(f.host.spawn_count(), spawned_before + 1);
    }

    #[tokio::test]
    async fn fanout_restart_ignores_another_instances_runs() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(2, 1, None)).await.unwrap().run.id;
        let temp = FanoutDispatcher::new(
            "temp-1".to_string(),
            f.store.clone(),
            f.host.clone(),
            Arc::new(RecordingEvents::default()),
        );
        temp.boot().await.unwrap();
        assert!(temp.get(id).unwrap().is_none());
        temp.tick().await;
        assert_eq!(f.host.spawn_count(), 0);
    }

    #[tokio::test]
    async fn fanout_the_runs_tenant_reaches_every_member_spawn() {
        let f = fixture(15).await;
        let tenant = Uuid::from_u128(0xA1);
        let id =
            f.d.create(new_run(4, 2, Some(tenant)))
                .await
                .unwrap()
                .run
                .id;
        f.d.tick().await;
        for m in
            f.d.get(id)
                .unwrap()
                .unwrap()
                .members
                .iter()
                .filter(|m| m.state == MemberState::Admitted)
        {
            f.host.exit(m.claude_session_id.as_deref().unwrap());
        }
        f.d.tick().await;
        let spawns = f.host.spawns.lock().unwrap().clone();
        assert_eq!(spawns.len(), 4);
        assert!(spawns.iter().all(|s| s.tenant_id == Some(tenant)));
        // …including after a restart reloads the run from the ledger.
        let restarted = FanoutDispatcher::new(
            "primary".to_string(),
            f.store.clone(),
            f.host.clone(),
            Arc::new(RecordingEvents::default()),
        );
        let id2 =
            f.d.create(new_run(1, 1, Some(tenant)))
                .await
                .unwrap()
                .run
                .id;
        restarted.boot().await.unwrap();
        restarted.tick().await;
        let last = f.host.spawns.lock().unwrap().last().cloned().unwrap();
        assert_eq!((last.run_id, last.tenant_id), (id2, Some(tenant)));
    }

    #[tokio::test]
    async fn fanout_a_failed_admission_write_spawns_nothing() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(2, 2, None)).await.unwrap().run.id;
        f.store.fail_member_writes.store(true, Ordering::SeqCst);
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(f.host.spawn_count(), 0);
        assert_eq!(v.members[0].state, MemberState::Queued);
        assert_eq!(
            v.members[0].reason.as_deref(),
            Some(reason::STORE_UNAVAILABLE)
        );
        f.store.fail_member_writes.store(false, Ordering::SeqCst);
        f.d.tick().await;
        assert_eq!(f.d.get(id).unwrap().unwrap().counts.admitted, 2);
    }

    #[tokio::test]
    async fn fanout_operator_ops_refuse_on_a_ledger_failure_and_change_nothing() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(2, 1, None)).await.unwrap().run.id;
        f.store.fail_member_writes.store(true, Ordering::SeqCst);
        assert!(matches!(f.d.cancel(id).await, Err(OpError::Store(_))));
        assert!(f
            .d
            .get(id)
            .unwrap()
            .unwrap()
            .members
            .iter()
            .all(|m| m.state == MemberState::Queued));
    }

    #[tokio::test]
    async fn fanout_every_state_change_is_announced() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(2, 1, None)).await.unwrap().run.id;
        let n0 = f.events.0.lock().unwrap().len();
        assert_eq!(n0, 1, "create announces");
        f.d.tick().await;
        f.d.tick().await; // no change → no event
        let evs = f.events.0.lock().unwrap().clone();
        // The admission (recorded before the spawn), then the spawn's outcome.
        assert_eq!(evs.len(), 3);
        assert!(evs.iter().all(|e| e.id == id));
        assert_eq!(evs[1].counts.admitted, 1);
        assert!(
            evs[1].members[0].terminal_id.is_none(),
            "announced in flight"
        );
        assert!(
            evs[2].members[0].terminal_id.is_some(),
            "then with its terminal"
        );
    }

    fn unbooted(store: Arc<MemoryStore>, host: Arc<MockHost>) -> FanoutDispatcher {
        FanoutDispatcher::new(
            "primary".to_string(),
            store,
            host,
            Arc::new(RecordingEvents::default()),
        )
    }

    #[tokio::test]
    async fn fanout_an_unloaded_ledger_is_unknown_never_an_empty_list() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(2, 1, None)).await.unwrap().run.id;
        // A restarted runner inside its boot settle: the ledger HAS a run.
        let d = unbooted(f.store.clone(), f.host.clone());
        assert_eq!(d.ledger_state(), LedgerState::Pending);
        let err = d.list().unwrap_err();
        assert!(err.reason.contains("not been loaded yet"), "{err:?}");
        assert!(!err.load_failed, "the boot settle is not a failed load");
        assert!(d.get(id).unwrap_err().reason.contains("UNKNOWN"));
        // Every operator op refuses with the same reason and changes nothing.
        assert!(matches!(d.cancel(id).await, Err(OpError::NotLoaded(_))));
        assert!(matches!(d.release(id, 0).await, Err(OpError::NotLoaded(_))));
        assert!(matches!(
            d.set_max_concurrent(id, 2).await,
            Err(OpError::NotLoaded(_))
        ));
        let rows_before = f.store.rows.lock().unwrap().len();
        assert!(matches!(
            d.create(new_run(1, 1, None)).await,
            Err(OpError::NotLoaded(_))
        ));
        assert_eq!(
            f.store.rows.lock().unwrap().len(),
            rows_before,
            "nothing inserted"
        );
        // The tick admits nothing before boot.
        d.tick().await;
        assert_eq!(f.host.spawn_count(), 0);
        // Loaded: the run is there.
        d.boot().await.unwrap();
        assert_eq!(d.ledger_state(), LedgerState::Loaded);
        assert_eq!(d.list().unwrap().len(), 1);
        assert_eq!(d.get(id).unwrap().unwrap().id, id);
    }

    #[tokio::test]
    async fn fanout_a_failed_load_is_unknown_with_the_store_error_until_one_succeeds() {
        let store = Arc::new(MemoryStore::default());
        let host = MockHost::with_bound(15);
        let d = unbooted(store.clone(), host.clone());
        store.fail_loads.store(true, Ordering::SeqCst);
        assert!(d.boot().await.is_err());
        assert_eq!(
            d.ledger_state(),
            LedgerState::Failed("connection refused".to_string())
        );
        let err = d.list().unwrap_err();
        assert!(err.load_failed);
        assert!(
            err.reason.contains("could not be loaded") && err.reason.contains("connection refused"),
            "{err:?}"
        );
        match d.create(new_run(1, 1, None)).await {
            Err(OpError::NotLoaded(why)) => {
                assert!(why.reason.contains("connection refused"), "{why:?}")
            }
            other => panic!("expected NotLoaded, got {other:?}"),
        }
        // The retry succeeds: an empty list is now a real answer.
        store.fail_loads.store(false, Ordering::SeqCst);
        d.boot().await.unwrap();
        assert!(d.list().unwrap().is_empty());
        assert!(d.create(new_run(1, 1, None)).await.is_ok());
    }

    fn refuse_next(host: &MockHost, why: &str) {
        host.scripted
            .lock()
            .unwrap()
            .push_back(SpawnOutcome::Refused {
                reason: why.to_string(),
            });
    }

    /// A persistent refusal is retried on a doubling backoff keyed on the
    /// refusal's class — never at tick rate — and a different class restarts it.
    #[tokio::test]
    async fn fanout_a_persistent_refusal_backs_off_instead_of_retrying_every_tick() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(1, 1, None)).await.unwrap().run.id;
        for _ in 0..3 {
            refuse_next(&f.host, "no authenticated Claude account on this runner");
        }
        f.d.tick().await; // attempt 1 → refused, retry in 5 s
        assert_eq!(f.host.spawn_count(), 1);
        for _ in 0..5 {
            f.d.tick().await; // inside the backoff: no attempt
        }
        assert_eq!(
            f.host.spawn_count(),
            1,
            "a refusal must not retry at tick rate"
        );
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(v.members[0].state, MemberState::Refused);
        assert_eq!(v.members[0].refusals, 1);
        advance(&f.d, 5);
        f.d.tick().await; // attempt 2 → refused, retry in 10 s
        assert_eq!(f.host.spawn_count(), 2);
        assert_eq!(f.d.get(id).unwrap().unwrap().members[0].refusals, 2);
        advance(&f.d, 5);
        f.d.tick().await;
        assert_eq!(
            f.host.spawn_count(),
            2,
            "the second delay is twice the first"
        );
        advance(&f.d, 5);
        f.d.tick().await; // attempt 3 → refused, retry in 20 s
        assert_eq!(f.host.spawn_count(), 3);
        assert_eq!(f.d.get(id).unwrap().unwrap().members[0].refusals, 3);
        // A different kind of refusal restarts the backoff at its base.
        refuse_next(&f.host, "resource_guard:critical: commit charge at 97%");
        advance(&f.d, 20);
        f.d.tick().await;
        assert_eq!(f.host.spawn_count(), 4);
        assert_eq!(f.d.get(id).unwrap().unwrap().members[0].refusals, 1);
        // …and two of the same class with different detail share it.
        refuse_next(&f.host, "resource_guard:critical: commit charge at 98%");
        advance(&f.d, 5);
        f.d.tick().await;
        assert_eq!(f.d.get(id).unwrap().unwrap().members[0].refusals, 2);
        // A successful spawn clears it.
        advance(&f.d, 10);
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(v.members[0].state, MemberState::Admitted);
        assert_eq!(
            (v.members[0].refusals, v.members[0].next_retry_at.clone()),
            (0, None)
        );
    }

    #[test]
    fn fanout_retry_delay_doubles_and_caps() {
        let secs: Vec<i64> = (1..=9).map(|a| retry_delay(a).num_seconds()).collect();
        assert_eq!(secs, vec![5, 10, 20, 40, 80, 160, 300, 300, 300]);
        assert_eq!(retry_delay(u32::MAX).num_seconds(), 300);
        assert_eq!(
            reason_class("resource_guard:critical: x"),
            "resource_guard".to_string()
        );
    }

    /// The spawn runs with the book UNLOCKED: while one is held in flight,
    /// reads, operator ops and a second tick all complete — and the in-flight
    /// member still counts toward the cap, is never spawned twice, and cannot
    /// be released out from under its spawn.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fanout_a_spawn_in_flight_does_not_hold_the_book() {
        let f = fixture(15).await;
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *f.host.gate.lock().unwrap() = Some(gate.clone());
        let id = f.d.create(new_run(4, 2, None)).await.unwrap().run.id;
        let ticker = {
            let d = f.d.clone();
            tokio::spawn(async move { d.tick().await })
        };
        // Wait until the first spawn is in flight.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while f.host.started.load(Ordering::SeqCst) == 0 {
            assert!(std::time::Instant::now() < deadline, "spawn never started");
            tokio::task::yield_now().await;
        }
        let quick = std::time::Duration::from_secs(2);
        // Both cap slots are admitted (in flight), visible to a reader.
        let v = f.d.get(id).unwrap().unwrap();
        use MemberState::*;
        assert_eq!(states(&v), vec![Admitted, Admitted, Queued, Queued]);
        // A second tick completes and admits nothing: the cap is full.
        tokio::time::timeout(quick, f.d.tick())
            .await
            .expect("tick blocked on a spawn");
        assert_eq!(f.host.started.load(Ordering::SeqCst), 1);
        // Releasing an in-flight member is refused, not raced.
        let r = tokio::time::timeout(quick, f.d.release(id, 0))
            .await
            .expect("release blocked on a spawn");
        assert!(
            matches!(r, Err(OpError::Conflict(ref m)) if m.contains("being spawned")),
            "{r:?}"
        );
        // A PATCH and a cancel complete too.
        tokio::time::timeout(quick, f.d.set_max_concurrent(id, 2))
            .await
            .expect("PATCH blocked on a spawn")
            .unwrap();
        let v = tokio::time::timeout(quick, f.d.cancel(id))
            .await
            .expect("cancel blocked on a spawn")
            .unwrap();
        assert_eq!(states(&v), vec![Admitted, Admitted, Cancelled, Cancelled]);
        // Let both spawns land.
        gate.add_permits(2);
        ticker.await.unwrap();
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(states(&v), vec![Admitted, Admitted, Cancelled, Cancelled]);
        assert!(v.members.iter().take(2).all(|m| m.terminal_id.is_some()));
        assert_eq!(f.host.spawn_count(), 2, "nothing spawned twice");
    }

    /// A cancel that lands while a member's spawn is in flight wins if that
    /// spawn does not happen: the member is cancelled, never re-queued.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fanout_a_cancel_during_a_spawn_that_fails_cancels_the_member() {
        let f = fixture(15).await;
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *f.host.gate.lock().unwrap() = Some(gate.clone());
        refuse_next(&f.host, "resource_guard:critical: low memory");
        let id = f.d.create(new_run(2, 1, None)).await.unwrap().run.id;
        let ticker = {
            let d = f.d.clone();
            tokio::spawn(async move { d.tick().await })
        };
        while f.host.started.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        f.d.cancel(id).await.unwrap();
        gate.add_permits(1);
        ticker.await.unwrap();
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(
            states(&v),
            vec![MemberState::Cancelled, MemberState::Cancelled]
        );
        assert_eq!(v.state, RunState::Completed);
        advance(&f.d, 600);
        f.d.tick().await;
        assert_eq!(f.host.started.load(Ordering::SeqCst), 1, "never retried");
    }

    /// An open record with no terminal answering (mid-respawn, not yet
    /// rebound) holds its slot for the grace window, then releases.
    #[tokio::test]
    async fn fanout_unconfirmed_liveness_holds_the_slot_for_a_grace_window() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(2, 1, None)).await.unwrap().run.id;
        f.d.tick().await;
        let first = csid(&f.d.get(id).unwrap().unwrap(), 0);
        f.host.exit(&first);
        f.host.unconfirmed.lock().unwrap().insert(first.clone());
        f.d.tick().await;
        advance(&f.d, LIVENESS_GRACE.num_seconds() - 1);
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(
            v.members[0].state,
            MemberState::Admitted,
            "inside the grace window"
        );
        assert_eq!(v.members[1].state, MemberState::Queued);
        // Rebinding inside the window clears it…
        f.host.unconfirmed.lock().unwrap().remove(&first);
        f.host
            .live
            .lock()
            .unwrap()
            .insert(first.clone(), "term-rebound".to_string());
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(v.members[0].terminal_id.as_deref(), Some("term-rebound"));
        // …and a fresh unconfirmed spell starts a fresh window, past which the
        // slot is released.
        f.host.exit(&first);
        f.host.unconfirmed.lock().unwrap().insert(first.clone());
        f.d.tick().await;
        advance(&f.d, LIVENESS_GRACE.num_seconds());
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(v.members[0].state, MemberState::Released);
        assert_eq!(v.members[0].reason.as_deref(), Some(reason::TERMINAL_EXIT));
        assert_eq!(v.members[1].state, MemberState::Admitted);
    }

    /// A member admitted but never recorded as spawned (a crash between the
    /// admission and the spawn), with no session record, is released as
    /// `spawn_unconfirmed` — not as `runner_restarted`, which says it ran.
    #[tokio::test]
    async fn fanout_a_member_lost_before_its_spawn_is_spawn_unconfirmed() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(2, 2, None)).await.unwrap().run.id;
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        // Member 0's row says admitted with no terminal; nothing recorded it.
        let lost = csid(&v, 0);
        {
            let mut rows = f.store.rows.lock().unwrap();
            rows.get_mut(&id).unwrap().1[0].terminal_id = None;
        }
        f.host.exit(&lost);
        f.host.unrecorded.lock().unwrap().insert(lost);
        // Member 1 ran and its terminal died with the runner.
        f.host.exit(&csid(&v, 1));
        let restarted = unbooted(f.store.clone(), f.host.clone());
        restarted.boot().await.unwrap();
        let w = restarted.get(id).unwrap().unwrap();
        assert_eq!(
            w.members[0].reason.as_deref(),
            Some(reason::SPAWN_UNCONFIRMED)
        );
        assert_eq!(
            w.members[1].reason.as_deref(),
            Some(reason::RUNNER_RESTARTED)
        );
    }

    /// A run idle past the staleness bound is not resumed at runner start:
    /// its waiting members are cancelled with `stale_after_restart`, nothing
    /// is spawned, and a fresh run in the same ledger is admitted as usual.
    #[tokio::test]
    async fn fanout_a_stale_run_is_not_admitted_after_a_restart() {
        let f = fixture(15).await;
        let stale = f.d.create(new_run(3, 1, None)).await.unwrap().run.id;
        {
            let mut rows = f.store.rows.lock().unwrap();
            rows.get_mut(&stale).unwrap().0.created_at =
                Utc::now() - STALE_RUN_AGE - chrono::Duration::hours(1);
        }
        let fresh = f.d.create(new_run(1, 1, None)).await.unwrap().run.id;
        let spawned_before = f.host.spawn_count();
        // A later runner reusing the owner name.
        let later = unbooted(f.store.clone(), f.host.clone());
        later.boot().await.unwrap();
        let v = later.get(stale).unwrap().unwrap();
        assert!(v.members.iter().all(|m| m.state == MemberState::Cancelled
            && m.reason.as_deref() == Some(reason::STALE_AFTER_RESTART)));
        assert_eq!(v.state, RunState::Completed);
        later.tick().await;
        assert_eq!(f.host.spawn_count(), spawned_before + 1);
        assert_eq!(
            later.get(fresh).unwrap().unwrap().members[0].state,
            MemberState::Admitted
        );
        // A run whose last admission is recent is not stale, however old.
        let busy = f.d.create(new_run(2, 1, None)).await.unwrap().run.id;
        {
            let mut rows = f.store.rows.lock().unwrap();
            let row = rows.get_mut(&busy).unwrap();
            row.0.created_at = Utc::now() - chrono::Duration::days(3);
            row.1[0].state = MemberState::Released;
            row.1[0].released_at = Some(Utc::now() - chrono::Duration::hours(1));
        }
        let again = unbooted(f.store.clone(), f.host.clone());
        again.boot().await.unwrap();
        assert_eq!(
            again.get(busy).unwrap().unwrap().members[1].state,
            MemberState::Queued
        );
    }

    /// A spawn task that panics AFTER its terminal was created is outcome-
    /// UNKNOWN, not a refusal: the member stays admitted under the SAME session
    /// id, the next tick's reconcile finds the session live and records its
    /// terminal, and the prompt is never spawned a second time.
    #[tokio::test]
    async fn fanout_a_panicked_spawn_that_ran_is_kept_never_respawned() {
        let f = fixture(15).await;
        *f.host.panic_next.lock().unwrap() = Some(true);
        let id = f.d.create(new_run(2, 1, None)).await.unwrap().run.id;
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        let pinned = csid(&v, 0);
        assert_eq!(v.members[0].state, MemberState::Admitted);
        assert!(
            v.members[0].terminal_id.is_some(),
            "the terminal pinned to the session is recorded at the UNKNOWN settle"
        );
        assert_eq!(v.members[1].state, MemberState::Queued, "the cap is held");
        // The marker is cleared, so an operator release is no longer refused…
        // but the next tick decides first.
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(v.members[0].state, MemberState::Admitted);
        assert_eq!(csid(&v, 0), pinned, "the pinned session id is kept");
        assert!(
            v.members[0].terminal_id.is_some(),
            "liveness recorded the terminal"
        );
        assert_eq!(v.members[1].state, MemberState::Queued);
        assert_eq!(f.host.spawn_count(), 1, "the prompt ran once");
        // The persisted row agrees: admitted, same session.
        let row = f.store.rows.lock().unwrap()[&id].1[0].clone();
        assert_eq!(row.state, MemberState::Admitted);
        assert_eq!(row.claude_session_id.as_deref(), Some(pinned.as_str()));
    }

    /// A spawn task that panics BEFORE anything was created is read by the
    /// next tick as no record and no terminal: released `spawn_unconfirmed`,
    /// and the slot goes to the next member.
    #[tokio::test]
    async fn fanout_a_panicked_spawn_that_never_ran_is_spawn_unconfirmed() {
        let f = fixture(15).await;
        *f.host.panic_next.lock().unwrap() = Some(false);
        let id = f.d.create(new_run(2, 1, None)).await.unwrap().run.id;
        f.d.tick().await;
        assert_eq!(
            f.d.get(id).unwrap().unwrap().members[0].state,
            MemberState::Admitted
        );
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(v.members[0].state, MemberState::Released);
        assert_eq!(
            v.members[0].reason.as_deref(),
            Some(reason::SPAWN_UNCONFIRMED)
        );
        assert_eq!(v.members[1].state, MemberState::Admitted);
        assert_eq!(f.host.spawn_count(), 2, "member 0 was never re-spawned");
    }

    /// Wait (bounded) until `cond` holds.
    async fn until(what: &str, cond: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !cond() {
            assert!(std::time::Instant::now() < deadline, "{what}");
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// The loop driving a spawn is aborted mid-flight and the spawn has not
    /// answered. The in-flight marker expires after SPAWN_SETTLE_BOUND and the
    /// member is handed to the liveness reconcile, like a panicked spawn —
    /// never pinned forever, never re-spawned. When the orphaned spawn finally
    /// answers `Spawned`, the next tick RE-ADOPTS the member it released
    /// `spawn_unconfirmed`: a running claude is never left untracked.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fanout_a_lost_spawn_settle_expires_into_the_liveness_reconcile() {
        // Short enough that member 1's held spawn (below) settles UNKNOWN
        // promptly; the aborted loop never reaches its own wait's end.
        let f = fixture_with_spawn_timeout(15, std::time::Duration::from_millis(500)).await;
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *f.host.gate.lock().unwrap() = Some(gate.clone());
        let id = f.d.create(new_run(2, 1, None)).await.unwrap().run.id;
        let ticker = {
            let d = f.d.clone();
            tokio::spawn(async move { d.tick().await })
        };
        until("spawn never started", || {
            f.host.started.load(Ordering::SeqCst) > 0
        })
        .await;
        // The supervised loop dies mid-`spawn_admitted`; the detached spawn
        // task is still waiting, and its session does not exist yet.
        ticker.abort();
        let _ = ticker.await;
        let lost = csid(&f.d.get(id).unwrap().unwrap(), 0);
        f.host.unrecorded.lock().unwrap().insert(lost.clone());
        // Inside the bound the member is still in flight: not reconciled, and
        // an operator release is refused.
        f.d.tick().await;
        assert!(matches!(
            f.d.release(id, 0).await,
            Err(OpError::Conflict(ref m)) if m.contains("being spawned")
        ));
        assert_eq!(f.d.get(id).unwrap().unwrap().members[0].terminal_id, None);
        // Past it, liveness decides: no record and no terminal.
        advance(&f.d, SPAWN_SETTLE_BOUND.num_seconds() + 1);
        f.host.cap_limit.store(2, Ordering::SeqCst);
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(v.members[0].state, MemberState::Released);
        assert_eq!(
            v.members[0].reason.as_deref(),
            Some(reason::SPAWN_UNCONFIRMED)
        );
        assert_eq!(v.members[1].state, MemberState::Admitted, "slot reused");
        // Both held spawns now land late: member 0's orphan (its loop died)
        // and member 1's (its wait timed out, settled UNKNOWN and kept).
        f.host.unrecorded.lock().unwrap().remove(&lost);
        gate.add_permits(2);
        until("the late spawns never landed", || {
            f.d.late_settles.lock().unwrap().len() == 2
        })
        .await;
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(v.members[0].state, MemberState::Admitted, "re-adopted");
        assert_eq!(csid(&v, 0), lost, "under its own pinned session");
        assert!(v.members[0].terminal_id.is_some());
        assert_eq!(v.members[0].reason, None);
        assert_eq!(v.members[1].state, MemberState::Admitted);
        assert_eq!(f.host.spawn_count(), 2, "member 0 was never re-spawned");
        let row = f.store.rows.lock().unwrap()[&id].1[0].clone();
        assert_eq!(
            row.state,
            MemberState::Admitted,
            "the re-adoption is persisted"
        );
    }

    /// The spawn wait is shorter than the settle bound, so the marker only
    /// ever expires for a loop that died.
    #[test]
    fn fanout_the_spawn_wait_is_shorter_than_the_settle_bound() {
        assert!(
            chrono::Duration::from_std(SPAWN_CALL_TIMEOUT).unwrap() < SPAWN_SETTLE_BOUND,
            "SPAWN_CALL_TIMEOUT must settle a slow spawn before its marker expires"
        );
    }

    /// A spawn that outlives SPAWN_CALL_TIMEOUT settles outcome-UNKNOWN (kept
    /// admitted, never refused or re-queued); the liveness reconcile releases
    /// it `spawn_unconfirmed` while no record exists; and when the spawn
    /// finally answers `Spawned`, the member is RE-ADOPTED as admitted under
    /// its own session id with its terminal — counting toward the cap again,
    /// so the run may sit one over its cap until a member ends.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fanout_a_spawn_that_outlives_its_wait_is_readopted_when_it_lands() {
        let f = fixture_with_spawn_timeout(15, std::time::Duration::from_millis(50)).await;
        f.host.cap_limit.store(2, Ordering::SeqCst);
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *f.host.gate.lock().unwrap() = Some(gate.clone());
        let id = f.d.create(new_run(2, 1, None)).await.unwrap().run.id;
        // The tick returns once the wait times out; the spawn runs on.
        tokio::time::timeout(std::time::Duration::from_secs(5), f.d.tick())
            .await
            .expect("the tick waited past the spawn timeout");
        let v = f.d.get(id).unwrap().unwrap();
        let slow = csid(&v, 0);
        assert_eq!(v.members[0].state, MemberState::Admitted, "UNKNOWN, kept");
        assert_eq!(v.members[0].terminal_id, None);
        assert_eq!(v.members[1].state, MemberState::Queued);
        // No record and no terminal yet: released `spawn_unconfirmed`, and the
        // slot goes to member 1 (whose own spawn is held, and kept, too).
        f.host.unrecorded.lock().unwrap().insert(slow.clone());
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(
            v.members[0].reason.as_deref(),
            Some(reason::SPAWN_UNCONFIRMED)
        );
        assert_eq!(v.members[1].state, MemberState::Admitted);
        let held = csid(&v, 1);
        f.host.unconfirmed.lock().unwrap().insert(held);
        // Member 0's spawn lands late.
        f.host.unrecorded.lock().unwrap().remove(&slow);
        gate.add_permits(1);
        until("the late spawn never landed", || {
            f.d.late_settles.lock().unwrap().len() == 1
        })
        .await;
        f.d.tick().await;
        let v = f.d.get(id).unwrap().unwrap();
        assert_eq!(v.members[0].state, MemberState::Admitted, "re-adopted");
        assert_eq!(csid(&v, 0), slow);
        assert_eq!(
            v.members[0].terminal_id.as_deref(),
            Some(format!("term-{}-0", id.simple()).as_str())
        );
        assert_eq!(v.members[0].released_at, None);
        assert_eq!(
            states(&v),
            vec![MemberState::Admitted, MemberState::Admitted],
            "transiently over the cap of 1 rather than an untracked claude"
        );
        assert_eq!(v.state, RunState::Active);
        assert_eq!(f.host.spawn_count(), 1, "nothing was spawned twice");
        // Over the cap, nothing more is admitted; the re-adopted member is
        // reconciled like any other (it reads live and keeps its terminal).
        f.d.tick().await;
        assert_eq!(
            f.d.get(id).unwrap().unwrap().members[0].state,
            MemberState::Admitted
        );
    }

    /// A late answer never resurrects a member released for any reason other
    /// than `spawn_unconfirmed` (an operator release, an exit), and a late
    /// non-`Spawned` answer never re-admits anything.
    #[test]
    fn fanout_only_a_late_spawned_contradicts_only_spawn_unconfirmed() {
        let now = Utc::now();
        let spawned = SpawnSettle::Outcome(SpawnOutcome::Spawned {
            terminal_id: "t1".to_string(),
        });
        let mut m = members(1).remove(0);
        mark_released(&mut m, reason::SPAWN_UNCONFIRMED, now);
        let refused = SpawnSettle::Outcome(SpawnOutcome::Refused {
            reason: "x".to_string(),
        });
        assert!(!readopt_late_spawn(&mut m, &refused, now));
        assert_eq!(m.state, MemberState::Released);
        assert!(readopt_late_spawn(&mut m, &spawned, now));
        assert_eq!(m.state, MemberState::Admitted);
        assert_eq!(m.terminal_id.as_deref(), Some("t1"));
        for other in [reason::TERMINAL_EXIT, reason::FINISHED] {
            let mut m = members(1).remove(0);
            mark_released(&mut m, other, now);
            assert!(!readopt_late_spawn(&mut m, &spawned, now), "{other}");
            assert_eq!(m.state, MemberState::Released);
        }
    }

    /// Every write stamps `updated_at`, refusals and drain deferrals included,
    /// so a run whose only activity was being refused is not "idle" — and the
    /// stale rule never cancels a run that is drain-paused or has a live
    /// member, however long ago its last write was.
    #[tokio::test]
    async fn fanout_the_stale_rule_spares_active_drained_and_refused_runs() {
        let f = fixture(15).await;
        let old = Utc::now() - STALE_RUN_AGE - chrono::Duration::hours(1);
        let age = |store: &MemoryStore, id: Uuid| {
            let mut rows = store.rows.lock().unwrap();
            let row = rows.get_mut(&id).unwrap();
            row.0.created_at = old;
            row.0.updated_at = None;
            for m in row.1.iter_mut() {
                m.updated_at = None;
                m.admitted_at = m.admitted_at.map(|_| old);
            }
        };
        // (a) A run with a live admitted member and a long-admitted session.
        let live = f.d.create(new_run(2, 1, None)).await.unwrap().run.id;
        f.d.tick().await;
        // (c) A run refused an hour ago: its refusal write is activity.
        refuse_next(&f.host, "resource_guard:critical: low memory");
        let refused = f.d.create(new_run(1, 1, None)).await.unwrap().run.id;
        f.d.tick().await;
        // (b) A run deferred by the drain (the documented quiesce path). The
        // drain lifts while the runner is down, so only the member's own
        // reason says it was paused.
        *f.host.drained.lock().unwrap() = Some("device drained".to_string());
        let drained = f.d.create(new_run(1, 1, None)).await.unwrap().run.id;
        f.d.tick().await;
        *f.host.drained.lock().unwrap() = None;
        // (d) A genuinely idle run, as the control (created after the drained
        // tick, so nothing marked it).
        let idle = f.d.create(new_run(1, 1, None)).await.unwrap().run.id;
        age(&f.store, live);
        age(&f.store, drained);
        age(&f.store, idle);
        {
            let mut rows = f.store.rows.lock().unwrap();
            // Isolate (a) to the liveness arm: the drained tick marked its
            // queued member too.
            rows.get_mut(&live).unwrap().1[1].reason = None;
            assert_eq!(
                rows[&drained].1[0].reason.as_deref(),
                Some(reason::RUNNER_DRAINING)
            );
            let row = rows.get_mut(&refused).unwrap();
            assert_eq!(row.1[0].state, MemberState::Refused);
            assert!(row.1[0].admitted_at.is_none(), "unadmit cleared it");
            assert!(row.1[0].updated_at.is_some(), "the refusal was stamped");
            row.0.created_at = old;
            row.1[0].updated_at = Some(Utc::now() - chrono::Duration::hours(1));
        }
        let later = unbooted(f.store.clone(), f.host.clone());
        later.boot().await.unwrap();
        let st = |id: Uuid| states(&later.get(id).unwrap().unwrap());
        assert_eq!(
            st(live),
            vec![MemberState::Admitted, MemberState::Queued],
            "a run with a live member is resumed"
        );
        assert_eq!(st(drained), vec![MemberState::Queued], "a drain is a pause");
        assert_eq!(st(refused), vec![MemberState::Refused]);
        assert_eq!(
            st(idle),
            vec![MemberState::Cancelled],
            "the control is stale"
        );
    }
}
