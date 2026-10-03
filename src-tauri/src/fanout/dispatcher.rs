//! The fan-out dispatcher: a queue, a cap and a spawn.
//!
//! Every mutation — create, cancel, PATCH, release and the admission tick —
//! runs under ONE async mutex over the in-memory book, so the cap is exact by
//! construction: no two admissions can each read "one slot free" and both
//! spawn. Every transition is written through to the store; the book is
//! reloaded from it at runner start and its `admitted` members reconciled
//! against the terminals that survived ([`FanoutDispatcher::boot`]).
//!
//! The three seams — [`FanoutStore`], [`FanoutHost`], [`FanoutEvents`] — are
//! traits so the admission rules are tested against an in-memory ledger and a
//! recording spawner (see `tests` below); production wires PostgreSQL
//! (`database::pg::fanout`) and the Tauri host (`fanout::host`).

use std::collections::BTreeMap;
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
    /// `refused` with this reason and is retried on the next tick.
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
    /// Its terminal exited or is gone.
    Exited,
    /// The host cannot tell (no terminal manager in this process). Never
    /// releases a slot: an unknown is not a death.
    Unknown,
}

/// The process the members run in.
#[async_trait]
pub(crate) trait FanoutHost: Send + Sync {
    /// `Some(reason)` while autonomous spawns are deferred by coord's device
    /// drain. A cheap pre-check: the spawn itself re-checks as the authority.
    fn drain_deferral(&self) -> Option<String>;
    /// The tenant's current `parallel_fanout` bound.
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
    /// The `parallel_fanout` bound the cap was clamped against.
    pub fanout_bound: u32,
    /// What the caller asked for, when the clamp changed it.
    pub clamped_from: Option<u32>,
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
}

/// How long a completed run stays listed after it completes.
const COMPLETED_RETENTION: chrono::Duration = chrono::Duration::hours(24);

struct RunEntry {
    run: FanoutRun,
    members: Vec<FanoutMember>,
    /// When the run became `completed` in this process, for retention.
    completed_at: Option<DateTime<Utc>>,
}

#[derive(Default)]
struct Book {
    runs: BTreeMap<Uuid, RunEntry>,
    /// The store's active runs are loaded and reconciled.
    booted: bool,
}

pub(crate) struct FanoutDispatcher {
    owner_instance: String,
    store: Arc<dyn FanoutStore>,
    host: Arc<dyn FanoutHost>,
    events: Arc<dyn FanoutEvents>,
    book: tokio::sync::Mutex<Book>,
    /// The views last published, newest run first — what the read routes
    /// serve, so a read never waits behind a spawn holding the book.
    published: RwLock<Arc<Vec<RunView>>>,
    wake: tokio::sync::Notify,
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
            wake: tokio::sync::Notify::new(),
        }
    }

    /// Ask the admission loop to tick now rather than at its next interval.
    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    /// Resolves on the next [`Self::wake`].
    pub(crate) async fn woken(&self) {
        self.wake.notified().await;
    }

    /// Every run this runner holds, newest first.
    pub(crate) fn list(&self) -> Arc<Vec<RunView>> {
        match self.published.read() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }

    /// One run.
    pub(crate) fn get(&self, id: Uuid) -> Option<RunView> {
        self.list().iter().find(|r| r.id == id).cloned()
    }

    /// Create a run. Persisted before it is admitted to the book, so a run
    /// the ledger could not record is never queued at all.
    pub(crate) async fn create(&self, new: NewRun) -> Result<CapOutcome, OpError> {
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
            created_at: Utc::now(),
            state: RunState::Active,
            owner_instance: self.owner_instance.clone(),
        };
        self.store
            .insert_run(&run, &new.members)
            .await
            .map_err(OpError::Store)?;
        let view = RunView::of(&run, &new.members);
        {
            let mut book = self.book.lock().await;
            book.runs.insert(
                run.id,
                RunEntry {
                    run,
                    members: new.members,
                    completed_at: None,
                },
            );
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
    /// member's session is never touched.
    pub(crate) async fn cancel(&self, id: Uuid) -> Result<RunView, OpError> {
        let mut book = self.book.lock().await;
        let entry = book
            .runs
            .get_mut(&id)
            .ok_or_else(|| OpError::NotFound(format!("no fan-out run {id}")))?;
        let mut next = entry.members.clone();
        let mut changed = Vec::new();
        for (pos, m) in next.iter_mut().enumerate() {
            if matches!(m.state, MemberState::Queued | MemberState::Refused) {
                m.state = MemberState::Cancelled;
                m.reason = Some(reason::CANCELLED.to_string());
                changed.push(pos);
            }
        }
        for &pos in &changed {
            self.store
                .update_member(id, &next[pos])
                .await
                .map_err(OpError::Store)?;
            entry.members[pos] = next[pos].clone();
        }
        let view = self.settle_run_state(entry).await;
        self.publish(&book);
        drop(book);
        self.events.changed(&view);
        Ok(view)
    }

    /// Set `max_concurrent`, clamped to the current `parallel_fanout` bound.
    pub(crate) async fn set_max_concurrent(
        &self,
        id: Uuid,
        requested: u32,
    ) -> Result<CapOutcome, OpError> {
        let bound = self.host.fanout_bound().await;
        let max_concurrent = clamp_max_concurrent(requested, bound);
        let mut book = self.book.lock().await;
        let entry = book
            .runs
            .get_mut(&id)
            .ok_or_else(|| OpError::NotFound(format!("no fan-out run {id}")))?;
        let mut next = entry.run.clone();
        next.max_concurrent = max_concurrent;
        self.store.update_run(&next).await.map_err(OpError::Store)?;
        entry.run = next;
        let view = RunView::of(&entry.run, &entry.members);
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
    /// claim on the cap ends.
    pub(crate) async fn release(&self, id: Uuid, index: u32) -> Result<RunView, OpError> {
        let mut book = self.book.lock().await;
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
        let mut next = current.clone();
        mark_released(&mut next, reason::OPERATOR_RELEASE);
        self.store
            .update_member(id, &next)
            .await
            .map_err(OpError::Store)?;
        entry.members[pos] = next;
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
    /// session is still alive stays admitted — never re-spawned.
    pub(crate) async fn boot(&self) -> Result<(), String> {
        let mut book = self.book.lock().await;
        if book.booted {
            return Ok(());
        }
        let loaded = self.store.load_active_runs(&self.owner_instance).await?;
        for (run, members) in loaded {
            book.runs.entry(run.id).or_insert(RunEntry {
                run,
                members,
                completed_at: None,
            });
        }
        let mut changed = Vec::new();
        let ids: Vec<Uuid> = book.runs.keys().copied().collect();
        for id in ids {
            let Some(entry) = book.runs.get_mut(&id) else {
                continue;
            };
            if self.reconcile_admitted(entry, reason::RUNNER_RESTARTED).await {
                changed.push(self.settle_run_state(entry).await);
            }
        }
        book.booted = true;
        self.publish(&book);
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

    /// One admission pass: release slots whose sessions ended, return refused
    /// members to the queue, then admit the lowest queued index of each run
    /// while `admitted < min(max_concurrent, parallel_fanout bound)`.
    pub(crate) async fn tick(&self) {
        let mut book = self.book.lock().await;
        if !book.booted {
            return;
        }
        let bound = self.host.fanout_bound().await;
        let drain = self.host.drain_deferral();
        let mut changed = Vec::new();
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
            let before = RunView::of(&entry.run, &entry.members);
            self.reconcile_admitted(entry, reason::TERMINAL_EXIT).await;
            self.requeue_refused(entry).await;
            self.mark_drain(entry, drain.as_deref()).await;
            if !halt && drain.is_none() {
                halt = self.admit(entry, bound).await;
            }
            let after = self.settle_run_state(entry).await;
            if after != before {
                changed.push(after);
            }
        }
        let now = Utc::now();
        book.runs.retain(|_, e| {
            e.completed_at
                .is_none_or(|at| now.signed_duration_since(at) < COMPLETED_RETENTION)
        });
        self.publish(&book);
        drop(book);
        for view in &changed {
            self.events.changed(view);
        }
    }

    /// Release every admitted member whose session ended (the finished marker,
    /// or its terminal exited), recording `exit_reason` for an exit. A live
    /// member that moved terminals is re-pointed. Returns whether anything
    /// changed.
    async fn reconcile_admitted(&self, entry: &mut RunEntry, exit_reason: &str) -> bool {
        let run_id = entry.run.id;
        let mut changed = false;
        for m in entry
            .members
            .iter_mut()
            .filter(|m| m.state == MemberState::Admitted)
        {
            let Some(csid) = m.claude_session_id.clone() else {
                // Admitted with no session id cannot occur through `admit`;
                // a hand-edited row is released rather than holding a slot
                // nothing can ever free.
                mark_released(m, exit_reason);
                self.persist_member(run_id, m).await;
                changed = true;
                continue;
            };
            match self.host.liveness(&csid, m.terminal_id.as_deref()) {
                Liveness::Live { terminal_id } => {
                    if m.terminal_id.as_deref() != Some(terminal_id.as_str()) {
                        m.terminal_id = Some(terminal_id);
                        self.persist_member(run_id, m).await;
                        changed = true;
                    }
                }
                Liveness::Finished => {
                    mark_released(m, reason::FINISHED);
                    self.persist_member(run_id, m).await;
                    changed = true;
                }
                Liveness::Exited => {
                    mark_released(m, exit_reason);
                    self.persist_member(run_id, m).await;
                    changed = true;
                }
                Liveness::Unknown => {}
            }
        }
        changed
    }

    /// `refused → queued`, keeping the reason visible.
    async fn requeue_refused(&self, entry: &mut RunEntry) {
        let run_id = entry.run.id;
        for m in entry
            .members
            .iter_mut()
            .filter(|m| m.state == MemberState::Refused)
        {
            m.state = MemberState::Queued;
            self.persist_member(run_id, m).await;
        }
    }

    /// While drained, every queued member says so; once the drain lifts the
    /// stale word is cleared.
    async fn mark_drain(&self, entry: &mut RunEntry, drain: Option<&str>) {
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
            self.persist_member(run_id, m).await;
        }
        if let Some(why) = drain {
            if entry
                .members
                .iter()
                .any(|m| m.state == MemberState::Queued)
            {
                tracing::debug!(run_id = %run_id, drain = %why, "fanout: admission deferred by the device drain");
            }
        }
    }

    /// Admit queued members of one run up to its cap. Returns `true` when the
    /// whole tick must stop admitting (the drain or the bound said no, or the
    /// ledger failed) — a run-local refusal only stops this run.
    async fn admit(&self, entry: &mut RunEntry, bound: u32) -> bool {
        let cap = entry.run.max_concurrent.min(bound).max(1) as usize;
        let run_id = entry.run.id;
        loop {
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
            // admitted member with no live session (released), never as a
            // queued one that would be spawned a second time.
            let csid = Uuid::new_v4().to_string();
            let mut admitted_member = entry.members[pos].clone();
            admitted_member.state = MemberState::Admitted;
            admitted_member.claude_session_id = Some(csid.clone());
            admitted_member.terminal_id = None;
            admitted_member.reason = None;
            admitted_member.admitted_at = Some(Utc::now());
            admitted_member.released_at = None;
            if let Err(e) = self.store.update_member(run_id, &admitted_member).await {
                warn!(run_id = %run_id, index = admitted_member.index, error = %e,
                    "fanout: could not record an admission — not spawning");
                entry.members[pos].reason = Some(reason::STORE_UNAVAILABLE.to_string());
                return true;
            }
            entry.members[pos] = admitted_member;

            let req = MemberSpawnRequest {
                run_id,
                index: entry.members[pos].index,
                title: entry.members[pos].title.clone(),
                prompt: entry.members[pos].prompt.clone(),
                working_dir: entry.run.working_dir.clone(),
                tenant_id: entry.run.tenant_id,
                config_dir_policy: entry.run.config_dir_policy.clone(),
                claude_session_id: csid,
            };
            let outcome = self.host.spawn_fanout_member(req).await;
            let member = &mut entry.members[pos];
            let (halt_tick, halt_run) = match outcome {
                SpawnOutcome::Spawned { terminal_id } => {
                    info!(run_id = %run_id, index = member.index, terminal_id = %terminal_id,
                        "fanout: member admitted");
                    member.terminal_id = Some(terminal_id);
                    (false, false)
                }
                SpawnOutcome::DeferredByDrain { reason: why } => {
                    info!(run_id = %run_id, index = member.index, drain = %why,
                        "fanout: admission deferred by the device drain");
                    unadmit(member, MemberState::Queued, reason::RUNNER_DRAINING.to_string());
                    (true, true)
                }
                SpawnOutcome::BoundOccupied { detail } => {
                    info!(run_id = %run_id, index = member.index, detail = %detail,
                        "fanout: parallel_fanout bound occupied — retrying next tick");
                    unadmit(
                        member,
                        MemberState::Queued,
                        reason::FANOUT_BOUND_OCCUPIED.to_string(),
                    );
                    (true, true)
                }
                SpawnOutcome::Refused { reason: why } => {
                    warn!(run_id = %run_id, index = member.index, reason = %why,
                        "fanout: member refused — it returns to the queue next tick");
                    unadmit(member, MemberState::Refused, why);
                    (false, true)
                }
            };
            self.persist_member(run_id, member).await;
            if halt_run {
                return halt_tick;
            }
        }
    }

    /// Recompute the run's state from its members; persist a change.
    async fn settle_run_state(&self, entry: &mut RunEntry) -> RunView {
        let state = derived_run_state(&entry.members);
        if state != entry.run.state {
            entry.run.state = state;
            if state == RunState::Completed {
                entry.completed_at = Some(Utc::now());
                info!(run_id = %entry.run.id, "fanout: run completed");
            }
            if let Err(e) = self.store.update_run(&entry.run).await {
                warn!(run_id = %entry.run.id, error = %e,
                    "fanout: could not persist the run's state — the book keeps it");
            }
        }
        RunView::of(&entry.run, &entry.members)
    }

    /// Write-through for a transition the tick has already decided. A failed
    /// write is logged and the book keeps the transition: every tick
    /// transition is either re-derived from liveness on restart or is a
    /// return to the queue, so a stale row can never cause a second spawn.
    async fn persist_member(&self, run_id: Uuid, member: &FanoutMember) {
        if let Err(e) = self.store.update_member(run_id, member).await {
            warn!(run_id = %run_id, index = member.index, error = %e,
                "fanout: could not persist a member transition — the book keeps it");
        }
    }

    fn publish(&self, book: &Book) {
        let mut views: Vec<RunView> = book
            .runs
            .values()
            .map(|e| RunView::of(&e.run, &e.members))
            .collect();
        views.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
        let views = Arc::new(views);
        match self.published.write() {
            Ok(mut g) => *g = views,
            Err(p) => *p.into_inner() = views,
        }
    }
}

fn mark_released(m: &mut FanoutMember, why: &str) {
    m.state = MemberState::Released;
    m.reason = Some(why.to_string());
    m.released_at = Some(Utc::now());
}

/// Undo an admission that did not produce a session.
fn unadmit(m: &mut FanoutMember, state: MemberState, why: String) {
    m.state = state;
    m.claude_session_id = None;
    m.terminal_id = None;
    m.admitted_at = None;
    m.reason = Some(why);
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
            // Yield so concurrent releases interleave with the spawn.
            tokio::task::yield_now().await;
            self.spawns.lock().unwrap().push(req.clone());
            if let Some(o) = self.scripted.lock().unwrap().pop_front() {
                if !matches!(o, SpawnOutcome::Spawned { .. }) {
                    return o;
                }
            }
            let mut live = self.live.lock().unwrap();
            let terminal_id = format!("term-{}-{}", req.run_id.simple(), req.index);
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
        let store = Arc::new(MemoryStore::default());
        let host = MockHost::with_bound(bound);
        let events = Arc::new(RecordingEvents::default());
        let d = Arc::new(FanoutDispatcher::new(
            "primary".to_string(),
            store.clone(),
            host.clone(),
            events.clone(),
        ));
        d.boot().await.unwrap();
        Fixture {
            store,
            host,
            events,
            d,
        }
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
        let v = f.d.get(id).unwrap();
        use MemberState::*;
        assert_eq!(states(&v), vec![Admitted, Admitted, Queued, Queued, Queued]);
        // A second tick with nothing released admits nothing more.
        f.d.tick().await;
        assert_eq!(f.host.spawn_count(), 2);
        // A terminal exit frees exactly one slot, taken by the next index.
        f.host.exit(&csid(&v, 0));
        f.d.tick().await;
        let v = f.d.get(id).unwrap();
        assert_eq!(states(&v), vec![Released, Admitted, Admitted, Queued, Queued]);
        assert_eq!(v.members[0].reason.as_deref(), Some(reason::TERMINAL_EXIT));
        assert!(v.members[0].released_at.is_some());
    }

    #[tokio::test]
    async fn fanout_every_spawn_gets_a_fresh_session_id_and_the_prompt() {
        let f = fixture(15).await;
        f.d.create(new_run(3, 3, None)).await.unwrap();
        f.d.tick().await;
        let spawns = f.host.spawns.lock().unwrap().clone();
        let ids: HashSet<&str> = spawns.iter().map(|s| s.claude_session_id.as_str()).collect();
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
                    let v = d.get(id).unwrap();
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
                    let v = d.get(id).unwrap();
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
                    let v = d.get(id).unwrap();
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
            let v = f.d.get(id).unwrap();
            for m in v.members.iter().filter(|m| m.state == MemberState::Admitted) {
                f.host.exit(m.claude_session_id.as_deref().unwrap());
            }
            f.d.tick().await;
            if f.d.get(id).unwrap().state == RunState::Completed {
                break;
            }
        }
        let v = f.d.get(id).unwrap();
        assert_eq!(v.state, RunState::Completed, "{:?}", v.counts);
        assert_eq!(v.counts.released, 40);
        assert_eq!(f.host.spawn_count(), 40, "every member spawned exactly once");
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
                    let v = d.get(id).unwrap();
                    assert!(v.counts.admitted <= 4, "{} admitted", v.counts.admitted);
                    tokio::task::yield_now().await;
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert!(f.d.get(id).unwrap().counts.admitted <= 4);
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
        assert_eq!(f.d.get(id).unwrap().counts.admitted, 1);
        // And the persisted run carries the clamped value.
        let rows = f.store.rows.lock().unwrap();
        assert_eq!(rows.get(&id).unwrap().0.max_concurrent, 2);
    }

    #[tokio::test]
    async fn fanout_refusal_keeps_the_member_queued_with_its_reason() {
        let f = fixture(15).await;
        f.host.scripted.lock().unwrap().push_back(SpawnOutcome::Refused {
            reason: "resource_guard:critical: commit charge at 97%".to_string(),
        });
        let id = f.d.create(new_run(3, 2, None)).await.unwrap().run.id;
        f.d.tick().await;
        let v = f.d.get(id).unwrap();
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
        // Next tick: back to the queue, then admitted.
        f.d.tick().await;
        let v = f.d.get(id).unwrap();
        assert_eq!(states(&v), vec![Admitted, Admitted, Queued]);
        assert!(v.members[0].reason.is_none());
    }

    #[tokio::test]
    async fn fanout_drained_runner_leaves_members_queued() {
        let f = fixture(15).await;
        *f.host.drained.lock().unwrap() = Some("coord drained this device".to_string());
        let id = f.d.create(new_run(3, 2, None)).await.unwrap().run.id;
        f.d.tick().await;
        let v = f.d.get(id).unwrap();
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
        let v = f.d.get(id).unwrap();
        assert!(v.members.iter().all(|m| m.state == MemberState::Queued));
        assert_eq!(v.members[0].reason.as_deref(), Some(reason::RUNNER_DRAINING));
        // Undrained: admitted, and the stale word is gone from the rest.
        f.d.tick().await;
        let v = f.d.get(id).unwrap();
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
        assert!(f.host.live.lock().unwrap().contains_key(&live), "session untouched");
        // Nothing cancelled is ever spawned.
        f.host.exit(&live);
        f.d.tick().await;
        let v = f.d.get(id).unwrap();
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
        assert_eq!(v.members[0].reason.as_deref(), Some(reason::OPERATOR_RELEASE));
        assert!(f.host.live.lock().unwrap().contains_key(&csid(&v, 0)));
        f.d.tick().await;
        assert_eq!(f.d.get(id).unwrap().members[1].state, MemberState::Admitted);
    }

    #[tokio::test]
    async fn fanout_finished_marker_releases_the_slot() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(2, 1, None)).await.unwrap().run.id;
        f.d.tick().await;
        let first = csid(&f.d.get(id).unwrap(), 0);
        f.host.finished.lock().unwrap().insert(first);
        f.d.tick().await;
        let v = f.d.get(id).unwrap();
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
        let v = f.d.get(id).unwrap();
        assert_eq!(v.members[0].state, MemberState::Admitted);
        assert_eq!(v.members[1].state, MemberState::Queued);
    }

    #[tokio::test]
    async fn fanout_restart_reconcile_is_idempotent_and_keeps_survivors() {
        let f = fixture(15).await;
        let id = f.d.create(new_run(4, 3, None)).await.unwrap().run.id;
        f.d.tick().await;
        let v = f.d.get(id).unwrap();
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
        let v = restarted.get(id).unwrap();
        use MemberState::*;
        assert_eq!(states(&v), vec![Admitted, Released, Admitted, Queued]);
        assert_eq!(v.members[0].terminal_id.as_deref(), Some(survivor_terminal.as_str()));
        assert_eq!(v.members[1].reason.as_deref(), Some(reason::RUNNER_RESTARTED));
        assert_eq!(v.members[2].terminal_id.as_deref(), Some("term-restored"));
        assert_eq!(f.host.spawn_count(), spawned_before, "reconcile spawns nothing");
        assert!(!events.0.lock().unwrap().is_empty());

        // Idempotent: a second boot (a supervised respawn of the loop) and a
        // whole second restart both land on the same book.
        restarted.boot().await.unwrap();
        assert_eq!(restarted.get(id).unwrap(), v);
        let again = FanoutDispatcher::new(
            "primary".to_string(),
            f.store.clone(),
            f.host.clone(),
            Arc::new(RecordingEvents::default()),
        );
        again.boot().await.unwrap();
        let w = again.get(id).unwrap();
        assert_eq!(states(&w), states(&v));
        assert_eq!(f.host.spawn_count(), spawned_before);
        assert_eq!(survivor, csid(&w, 0));

        // The freed slot goes to the next queued member, and only it.
        again.tick().await;
        let w = again.get(id).unwrap();
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
        assert!(temp.get(id).is_none());
        temp.tick().await;
        assert_eq!(f.host.spawn_count(), 0);
    }

    #[tokio::test]
    async fn fanout_the_runs_tenant_reaches_every_member_spawn() {
        let f = fixture(15).await;
        let tenant = Uuid::from_u128(0xA1);
        let id = f.d.create(new_run(4, 2, Some(tenant))).await.unwrap().run.id;
        f.d.tick().await;
        for m in f.d.get(id).unwrap().members.iter().filter(|m| m.state == MemberState::Admitted) {
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
        let id2 = f.d.create(new_run(1, 1, Some(tenant))).await.unwrap().run.id;
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
        let v = f.d.get(id).unwrap();
        assert_eq!(f.host.spawn_count(), 0);
        assert_eq!(v.members[0].state, MemberState::Queued);
        assert_eq!(v.members[0].reason.as_deref(), Some(reason::STORE_UNAVAILABLE));
        f.store.fail_member_writes.store(false, Ordering::SeqCst);
        f.d.tick().await;
        assert_eq!(f.d.get(id).unwrap().counts.admitted, 2);
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
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[1].id, id);
        assert_eq!(evs[1].counts.admitted, 1);
    }
}
