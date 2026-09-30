//! Attach-deadline reaper for REMOTE-created terminals (TARGET role).
//!
//! Plan `2026-09-23-remote-create-residuals-after-coord-registration-confirm`,
//! Phase 1.
//!
//! A remote create enqueues its `terminal_created` reply on the relay writer.
//! `deliver_create_reply` (`backend_relay.rs`) closes the terminal when that
//! enqueue fails, but an enqueue that returned `Ok(())` can still be discarded
//! when relay teardown aborts the writer task afterwards. The source then never
//! learns the terminal's id, its create grant is spent, and the PTY lives and
//! heartbeats in coord forever with no one ever attaching.
//!
//! This module closes that gap: every admitted remote create is ARMED with a
//! deadline; an attach grant bound to the terminal records it as EVER attached;
//! a terminal still never-attached at the deadline is closed through the same
//! `TerminalManager::close` the create path's own cleanup uses (whose exit hook
//! closes the confirmed coord session).
//!
//! "Ever attached", not "currently attached": a source that attached and then
//! detached (window closed, to reattach later) still owns the terminal, so the
//! current-binding view (`RemoteAttachGrants::grants_bound_to`) is the wrong
//! predicate and is deliberately not consulted.
//!
//! Only an attach-GRANT bind counts as ownership. A remote-created terminal is
//! created for the remote source; someone typing into it in this runner's own
//! UI does not bind a grant and does not keep it alive past the deadline.
//!
//! Time is MONOTONIC (`tokio::time::Instant`) throughout, so a wall-clock step
//! can neither reap early nor make the single scheduled sweep miss its row.
//! The decision is the pure [`decide`]; the table and the sweep are thin
//! wrappers so the whole rule is testable without a `TerminalManager`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use tokio::time::Instant;
use tracing::{debug, info, warn};

/// How long a remote-created terminal may go without ANY attach before it is
/// reaped.
///
/// Sized against the one bound coord fixes fleet-wide — the create grant's TTL
/// (`CREATE_GRANT_TTL_SECS` = 900 s, qontinui-coord
/// `crates/coord/src/jwt.rs:113`) — plus margin, rather than against the
/// source-side windows (`SESSION_VISIBILITY_ATTEMPTS` × retry and
/// `GRANT_LEARN_WINDOW` in `commands/remote_attach.rs`), which on the SOURCE
/// are another machine's build and so cannot be trusted from here. A 20-minute
/// orphan PTY costs far less than reaping a legitimate slow attach.
pub const REMOTE_CREATE_ATTACH_DEADLINE: Duration = Duration::from_secs(20 * 60);

/// Coord's create-grant TTL, mirrored for the sizing test only. The authority
/// is qontinui-coord `crates/coord/src/jwt.rs:113` `CREATE_GRANT_TTL_SECS`.
#[cfg(test)]
const COORD_CREATE_GRANT_TTL_SECS: u64 = 900;

/// Why a reap happened — the `reason` field of its log line.
const REAP_REASON: &str = "no_attach_before_deadline";

/// What the reaper does with one armed terminal at time `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapDecision {
    /// A source attached at some point — the terminal is owned; disarm it.
    Keep,
    /// Never attached and the deadline has not passed yet.
    Wait,
    /// Never attached and the deadline has passed — close it.
    Reap,
}

/// The whole rule, pure.
pub fn decide(
    created_at: Instant,
    ever_attached: bool,
    now: Instant,
    deadline: Duration,
) -> ReapDecision {
    if ever_attached {
        ReapDecision::Keep
    } else if now.saturating_duration_since(created_at) >= deadline {
        ReapDecision::Reap
    } else {
        ReapDecision::Wait
    }
}

/// What a close attempt found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseOutcome {
    /// The terminal was live and is now closed.
    Closed,
    /// The terminal was already gone (it exited, or the create path closed an
    /// undeliverable reply's terminal) — nothing to reap.
    AlreadyGone,
}

/// One row of the table: an armed remote create, or an attach that arrived
/// before its terminal was armed.
#[derive(Debug, Clone)]
struct Armed {
    /// When the create was armed — or, for a not-yet-armed attach marker, when
    /// the attach was seen (used only to age the marker out).
    created_at: Instant,
    coord_session_id: Option<String>,
    ever_attached: bool,
    /// `false` for an attach marker whose `arm` has not arrived yet. Only
    /// armed rows are ever reaped.
    armed: bool,
}

/// One terminal the sweep decided to close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaped {
    pub terminal_id: String,
    pub coord_session_id: Option<String>,
}

/// The armed-terminal table.
#[derive(Debug)]
pub struct RemoteCreateReaper {
    deadline: Duration,
    armed: Mutex<HashMap<String, Armed>>,
}

impl RemoteCreateReaper {
    pub fn new(deadline: Duration) -> Self {
        Self {
            deadline,
            armed: Mutex::new(HashMap::new()),
        }
    }

    /// Plain data with no cross-row invariant a panic could break, so a
    /// poisoned lock is recovered rather than silently disabling the reaper.
    fn table(&self) -> MutexGuard<'_, HashMap<String, Armed>> {
        self.armed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record a freshly admitted remote create.
    ///
    /// `terminal_create` runs off the relay's read loop and arms only after
    /// coord confirms the session, so an attach can bind in between. An
    /// attach already recorded for this id (see [`Self::mark_attached`]) is
    /// PRESERVED — a terminal that was attached before it was armed is owned,
    /// and must not be reaped.
    pub fn arm(&self, terminal_id: &str, coord_session_id: Option<String>, now: Instant) {
        let mut table = self.table();
        let attached_already = table.get(terminal_id).is_some_and(|row| row.ever_attached);
        table.insert(
            terminal_id.to_string(),
            Armed {
                created_at: now,
                coord_session_id,
                ever_attached: attached_already,
                armed: true,
            },
        );
    }

    /// Record that an attach grant was bound to `terminal_id`. Sticky: a later
    /// detach does not clear it.
    ///
    /// For an id not armed yet, a sticky attached MARKER is recorded, which a
    /// later [`Self::arm`] honors (the attach-before-arm ordering). Every
    /// attach of a terminal that is never armed — a local or web-created one —
    /// also leaves a marker, so markers older than the deadline are pruned
    /// here: an arm that races its own attach arrives within seconds, not
    /// minutes.
    pub fn mark_attached(&self, terminal_id: &str, now: Instant) {
        let deadline = self.deadline;
        let mut table = self.table();
        table
            .retain(|_, row| row.armed || now.saturating_duration_since(row.created_at) < deadline);
        table
            .entry(terminal_id.to_string())
            .and_modify(|row| row.ever_attached = true)
            .or_insert(Armed {
                created_at: now,
                coord_session_id: None,
                ever_attached: true,
                armed: false,
            });
    }

    /// Apply [`decide`] to every armed terminal: `Reap` rows are removed and
    /// returned, `Keep` rows are removed (owned — nothing left to watch),
    /// `Wait` rows stay. Unarmed attach markers are kept until they age out.
    pub fn take_due(&self, now: Instant) -> Vec<Reaped> {
        let mut due = Vec::new();
        let deadline = self.deadline;
        self.table().retain(|terminal_id, row| {
            if !row.armed {
                return now.saturating_duration_since(row.created_at) < deadline;
            }
            match decide(row.created_at, row.ever_attached, now, deadline) {
                ReapDecision::Wait => true,
                ReapDecision::Keep => false,
                ReapDecision::Reap => {
                    due.push(Reaped {
                        terminal_id: terminal_id.clone(),
                        coord_session_id: row.coord_session_id.clone(),
                    });
                    false
                }
            }
        });
        due
    }

    #[cfg(test)]
    fn is_armed(&self, terminal_id: &str) -> bool {
        self.table().get(terminal_id).is_some_and(|row| row.armed)
    }

    #[cfg(test)]
    fn has_row(&self, terminal_id: &str) -> bool {
        self.table().contains_key(terminal_id)
    }
}

/// Close every terminal that is due at `now`, logging each reap. Returns what
/// was due. A close that fails is logged and not retried — the row is gone
/// either way.
pub async fn sweep<F, Fut>(reaper: &RemoteCreateReaper, now: Instant, close: F) -> Vec<Reaped>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<CloseOutcome, String>>,
{
    let due = reaper.take_due(now);
    let deadline_secs = reaper.deadline.as_secs();
    for reaped in &due {
        let coord_session = reaped.coord_session_id.as_deref().unwrap_or("-");
        match close(reaped.terminal_id.clone()).await {
            Ok(CloseOutcome::Closed) => info!(
                terminal_id = %reaped.terminal_id,
                coord_session = %coord_session,
                deadline_secs,
                reason = REAP_REASON,
                "remote create: reaped a remote-created terminal no source ever attached \
                 (its terminal_created reply was likely lost at relay teardown)"
            ),
            Ok(CloseOutcome::AlreadyGone) => debug!(
                terminal_id = %reaped.terminal_id,
                coord_session = %coord_session,
                deadline_secs,
                reason = "already_closed",
                "remote create: attach deadline passed for a terminal that had already closed"
            ),
            Err(e) => warn!(
                terminal_id = %reaped.terminal_id,
                coord_session = %coord_session,
                deadline_secs,
                reason = REAP_REASON,
                error = %e,
                "remote create: reaping a never-attached remote-created terminal failed"
            ),
        }
    }
    due
}

/// Arm `terminal_id` on `reaper` now and schedule the sweep that runs once its
/// deadline has passed.
pub fn arm_and_schedule<F, Fut>(
    reaper: Arc<RemoteCreateReaper>,
    terminal_id: &str,
    coord_session_id: Option<String>,
    close: F,
) -> tokio::task::JoinHandle<Vec<Reaped>>
where
    F: Fn(String) -> Fut + Send + 'static,
    Fut: Future<Output = Result<CloseOutcome, String>> + Send,
{
    let armed_at = Instant::now();
    reaper.arm(terminal_id, coord_session_id, armed_at);
    let wake_at = armed_at + reaper.deadline;
    tokio::spawn(async move {
        tokio::time::sleep_until(wake_at).await;
        sweep(&reaper, Instant::now(), close).await
    })
}

static REAPER: OnceLock<Arc<RemoteCreateReaper>> = OnceLock::new();

/// The process-wide reaper.
pub fn reaper() -> Arc<RemoteCreateReaper> {
    REAPER
        .get_or_init(|| Arc::new(RemoteCreateReaper::new(REMOTE_CREATE_ATTACH_DEADLINE)))
        .clone()
}

/// What the reaper closes a terminal through. The production implementor is
/// [`crate::terminal::TerminalManager`]; the trait is the seam that lets the
/// production close path be tested without a Tauri `AppHandle` or a PTY.
pub trait ReapTarget: Send + Sync + 'static {
    /// Close `terminal_id`. `Ok(AlreadyGone)` when there was no such terminal —
    /// it exited, or another close got there first.
    fn close_terminal(&self, terminal_id: &str) -> Result<CloseOutcome, String>;
}

/// The prefix of `TerminalManager::close`'s error for an id it does not hold
/// (`terminal/manager.rs`). Pinned by
/// `a_real_manager_maps_an_unknown_terminal_to_already_gone`.
const TERMINAL_NOT_FOUND: &str = "Terminal session not found";

impl ReapTarget for crate::terminal::TerminalManager {
    /// No `get`-then-`close` check: the close itself answers whether the
    /// terminal was there, so a terminal that exits between a check and the
    /// close cannot be misreported as a failed reap.
    fn close_terminal(&self, terminal_id: &str) -> Result<CloseOutcome, String> {
        match self.close(terminal_id) {
            Ok(()) => Ok(CloseOutcome::Closed),
            Err(e) if e.starts_with(TERMINAL_NOT_FOUND) => Ok(CloseOutcome::AlreadyGone),
            Err(e) => Err(e),
        }
    }
}

/// Close `terminal_id` through `target` on the blocking pool — the same
/// `TerminalManager::close` on the blocking pool that `deliver_create_reply`'s
/// caller runs for an undeliverable reply. Killing the PTY child is what fires
/// the terminal's exit hook, which closes its confirmed coord session.
pub async fn close_through<T: ReapTarget + ?Sized>(
    target: Arc<T>,
    terminal_id: String,
) -> Result<CloseOutcome, String> {
    qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        target.close_terminal(&terminal_id)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

/// Arm `terminal_id` on `reaper` with `target` as the close. What
/// [`arm_remote_created`] runs, with the reaper and target injectable.
pub fn arm_with_target<T: ReapTarget + ?Sized>(
    reaper: Arc<RemoteCreateReaper>,
    target: Arc<T>,
    terminal_id: &str,
    coord_session_id: Option<String>,
) -> tokio::task::JoinHandle<Vec<Reaped>> {
    arm_and_schedule(reaper, terminal_id, coord_session_id, move |id: String| {
        close_through(target.clone(), id)
    })
}

/// Production arm: the process-wide reaper, closing through the
/// `TerminalManager`.
pub fn arm_remote_created(
    tm: Arc<crate::terminal::TerminalManager>,
    terminal_id: &str,
    coord_session_id: Option<String>,
) {
    arm_with_target(reaper(), tm, terminal_id, coord_session_id);
}

/// Record that an attach grant was bound to `terminal_id` (process-wide).
pub fn mark_attached(terminal_id: &str) {
    reaper().mark_attached(terminal_id, Instant::now());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    const N: Duration = REMOTE_CREATE_ATTACH_DEADLINE;
    const SEC: Duration = Duration::from_secs(1);

    type Close = Box<dyn Fn(String) -> std::future::Ready<Result<CloseOutcome, String>> + Send>;

    fn recorder() -> (Arc<Mutex<Vec<String>>>, Close) {
        let closed = Arc::new(Mutex::new(Vec::new()));
        let sink = closed.clone();
        let close: Close = Box::new(move |id: String| {
            sink.lock().unwrap().push(id);
            std::future::ready(Ok(CloseOutcome::Closed))
        });
        (closed, close)
    }

    #[test]
    fn deadline_exceeds_coord_create_grant_ttl_by_at_least_five_minutes() {
        // `>=`, not `>`: the plan resolves N to exactly 20 min = 900 s + 5 min,
        // i.e. a full five-minute margin past the grant's expiry.
        assert!(
            REMOTE_CREATE_ATTACH_DEADLINE.as_secs() >= COORD_CREATE_GRANT_TTL_SECS + 5 * 60,
            "the reaper must never race a create grant that is still live at coord"
        );
        assert_eq!(REMOTE_CREATE_ATTACH_DEADLINE, Duration::from_secs(20 * 60));
    }

    /// Retention guard: the reap arm itself. Deleting it (`decide` never
    /// answering `Reap`) must fail here as well as in the sweep tests.
    #[test]
    fn decide_reaps_a_never_attached_terminal_at_the_deadline() {
        let t0 = Instant::now();
        assert_eq!(decide(t0, false, t0 + N, N), ReapDecision::Reap);
        assert_eq!(decide(t0, false, t0 + N + SEC, N), ReapDecision::Reap);
        assert_eq!(decide(t0, false, t0 + N - SEC, N), ReapDecision::Wait);
        assert_eq!(decide(t0, false, t0, N), ReapDecision::Wait);
    }

    #[test]
    fn decide_keeps_an_ever_attached_terminal_forever() {
        let t0 = Instant::now();
        assert_eq!(decide(t0, true, t0, N), ReapDecision::Keep);
        assert_eq!(decide(t0, true, t0 + 10 * N, N), ReapDecision::Keep);
    }

    #[tokio::test]
    async fn never_attached_terminal_is_closed_past_the_deadline() {
        let t0 = Instant::now();
        let reaper = RemoteCreateReaper::new(N);
        reaper.arm("term-orphan", Some("coord-1".into()), t0);
        let (closed, close) = recorder();

        // Before the deadline: nothing closes, the row stays armed.
        assert!(sweep(&reaper, t0 + N - SEC, &close).await.is_empty());
        assert!(closed.lock().unwrap().is_empty());
        assert!(reaper.is_armed("term-orphan"));

        // Past it: closed, with its coord session carried for the log.
        let reaped = sweep(&reaper, t0 + N + SEC, &close).await;
        assert_eq!(
            reaped,
            vec![Reaped {
                terminal_id: "term-orphan".into(),
                coord_session_id: Some("coord-1".into()),
            }]
        );
        assert_eq!(*closed.lock().unwrap(), vec!["term-orphan".to_string()]);
        assert!(!reaper.is_armed("term-orphan"));

        // Reaped once: a later sweep does not close it again.
        assert!(sweep(&reaper, t0 + 3 * N, &close).await.is_empty());
        assert_eq!(closed.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn attached_before_the_deadline_survives_past_it() {
        let t0 = Instant::now();
        let reaper = RemoteCreateReaper::new(N);
        reaper.arm("term-owned", Some("coord-2".into()), t0);
        reaper.mark_attached("term-owned", t0);
        let (closed, close) = recorder();

        assert!(sweep(&reaper, t0 + N + SEC, &close).await.is_empty());
        assert!(sweep(&reaper, t0 + 5 * N, &close).await.is_empty());
        assert!(closed.lock().unwrap().is_empty());
        // Owned terminals are disarmed rather than watched forever.
        assert!(!reaper.is_armed("term-owned"));
    }

    #[tokio::test]
    async fn only_the_never_attached_terminal_is_reaped() {
        let t0 = Instant::now();
        let reaper = RemoteCreateReaper::new(N);
        reaper.arm("orphan", None, t0);
        reaper.arm("owned", None, t0);
        reaper.arm("young", None, t0 + N);
        reaper.mark_attached("owned", t0);
        // Marking a terminal the reaper never armed is a no-op.
        reaper.mark_attached("local-terminal", t0);
        let (closed, close) = recorder();

        let reaped = sweep(&reaper, t0 + N + SEC, &close).await;
        assert_eq!(reaped.len(), 1);
        assert_eq!(*closed.lock().unwrap(), vec!["orphan".to_string()]);
        assert!(reaper.is_armed("young"));
        assert!(!reaper.is_armed("local-terminal"));
    }

    #[tokio::test]
    async fn a_failed_or_already_gone_close_is_not_retried() {
        let t0 = Instant::now();
        let reaper = RemoteCreateReaper::new(N);
        reaper.arm("errs", None, t0);
        reaper.arm("gone", None, t0);
        let calls = Arc::new(AtomicU64::new(0));
        let c = calls.clone();
        let close = move |id: String| {
            c.fetch_add(1, Ordering::SeqCst);
            std::future::ready(if id == "gone" {
                Ok(CloseOutcome::AlreadyGone)
            } else {
                Err("pty close failed".to_string())
            })
        };
        assert_eq!(sweep(&reaper, t0 + N, &close).await.len(), 2);
        assert!(sweep(&reaper, t0 + 2 * N, &close).await.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// The scheduled path end to end on a paused tokio clock: arming schedules
    /// a sweep that fires at `N` and closes the never-attached terminal.
    #[tokio::test(start_paused = true)]
    async fn scheduled_sweep_closes_a_never_attached_terminal() {
        let reaper = Arc::new(RemoteCreateReaper::new(N));
        let (closed, close) = recorder();
        let start = Instant::now();
        let handle = arm_and_schedule(reaper.clone(), "term-lost-reply", None, close);
        let reaped = handle.await.unwrap();
        assert_eq!(reaped.len(), 1);
        assert_eq!(*closed.lock().unwrap(), vec!["term-lost-reply".to_string()]);
        assert!(start.elapsed() >= N);
    }

    #[tokio::test(start_paused = true)]
    async fn scheduled_sweep_spares_a_terminal_attached_in_time() {
        let reaper = Arc::new(RemoteCreateReaper::new(N));
        let (closed, close) = recorder();
        let handle = arm_and_schedule(reaper.clone(), "term-attached", None, close);
        tokio::time::sleep(Duration::from_secs(250)).await;
        reaper.mark_attached("term-attached", Instant::now());
        assert!(handle.await.unwrap().is_empty());
        assert!(closed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_attach_that_lands_before_the_arm_is_honored() {
        let t0 = Instant::now();
        let reaper = RemoteCreateReaper::new(N);
        // The attach binds while `terminal_create` is still awaiting coord's
        // confirmation, i.e. before the row exists.
        reaper.mark_attached("term-raced", t0);
        assert!(!reaper.is_armed("term-raced"));
        reaper.arm("term-raced", Some("coord-3".into()), t0 + SEC);
        assert!(reaper.is_armed("term-raced"));
        let (closed, close) = recorder();

        assert!(sweep(&reaper, t0 + 2 * N, &close).await.is_empty());
        assert!(
            closed.lock().unwrap().is_empty(),
            "an owned terminal was reaped"
        );
    }

    #[tokio::test]
    async fn re_arming_an_attached_terminal_keeps_it_attached() {
        let t0 = Instant::now();
        let reaper = RemoteCreateReaper::new(N);
        reaper.arm("term-a", None, t0);
        reaper.mark_attached("term-a", t0);
        reaper.arm("term-a", None, t0);
        let (closed, close) = recorder();
        assert!(sweep(&reaper, t0 + 2 * N, &close).await.is_empty());
        assert!(closed.lock().unwrap().is_empty());
    }

    #[test]
    fn unarmed_attach_markers_age_out() {
        let t0 = Instant::now();
        let reaper = RemoteCreateReaper::new(N);
        reaper.mark_attached("local-1", t0);
        // A later attach prunes markers older than the deadline.
        reaper.mark_attached("local-2", t0 + N + SEC);
        assert!(!reaper.has_row("local-1"));
        assert!(reaper.has_row("local-2"));
        // So does a sweep.
        assert!(reaper.take_due(t0 + 3 * N).is_empty());
        assert!(!reaper.has_row("local-2"));
    }

    /// A test `ReapTarget` that behaves like a manager whose terminal carries
    /// an exit hook: closing a live terminal fires the hook with its coord id.
    struct HookedTarget {
        live: Mutex<HashMap<String, String>>,
        exit_hook_fired: Mutex<Vec<String>>,
        fail: bool,
    }

    impl ReapTarget for HookedTarget {
        fn close_terminal(&self, terminal_id: &str) -> Result<CloseOutcome, String> {
            if self.fail {
                return Err("pty close failed".into());
            }
            match self.live.lock().unwrap().remove(terminal_id) {
                Some(coord) => {
                    self.exit_hook_fired.lock().unwrap().push(coord);
                    Ok(CloseOutcome::Closed)
                }
                None => Ok(CloseOutcome::AlreadyGone),
            }
        }
    }

    /// The production arm path (`arm_with_target` → `close_through` on the
    /// blocking pool) closes through its target at the deadline, and the
    /// target's close — here standing in for the exit hook — fires once.
    #[tokio::test(start_paused = true)]
    async fn the_production_arm_path_closes_through_its_target() {
        let target = Arc::new(HookedTarget {
            live: Mutex::new(HashMap::from([(
                "term-p".to_string(),
                "coord-p".to_string(),
            )])),
            exit_hook_fired: Mutex::new(Vec::new()),
            fail: false,
        });
        let reaper = Arc::new(RemoteCreateReaper::new(N));
        let handle = arm_with_target(reaper, target.clone(), "term-p", Some("coord-p".into()));
        let reaped = handle.await.unwrap();
        assert_eq!(reaped.len(), 1);
        assert!(target.live.lock().unwrap().is_empty());
        assert_eq!(
            *target.exit_hook_fired.lock().unwrap(),
            vec!["coord-p".to_string()]
        );
    }

    #[tokio::test]
    async fn close_through_passes_the_target_outcome_and_error_up() {
        let target = Arc::new(HookedTarget {
            live: Mutex::new(HashMap::new()),
            exit_hook_fired: Mutex::new(Vec::new()),
            fail: true,
        });
        assert_eq!(
            close_through(target, "t".into()).await,
            Err("pty close failed".to_string())
        );
    }

    /// The REAL manager: an id it does not hold — a terminal that exited, or
    /// closed between any check and the reap — is `AlreadyGone`, not a failed
    /// reap. This also pins `TERMINAL_NOT_FOUND` to the manager's wording.
    #[tokio::test]
    async fn a_real_manager_maps_an_unknown_terminal_to_already_gone() {
        let tm = Arc::new(crate::terminal::TerminalManager::new());
        assert_eq!(
            close_through(tm, "no-such-terminal".into()).await,
            Ok(CloseOutcome::AlreadyGone)
        );
    }

    /// The REAL manager holding a (PTY-less) session: the reap removes it
    /// through `TerminalManager::close`, and a second close is `AlreadyGone`.
    #[tokio::test]
    async fn a_real_manager_closes_a_live_terminal_on_reap() {
        let tm = Arc::new(crate::terminal::TerminalManager::new());
        let session =
            crate::terminal::session::tests::make_test_session(Arc::new(Mutex::new(Vec::new())));
        tm.insert_for_test("term-real", Arc::new(session));
        assert!(tm.get("term-real").is_some());

        let reaper = RemoteCreateReaper::new(N);
        let t0 = Instant::now();
        reaper.arm("term-real", None, t0);
        let target = tm.clone();
        let reaped = sweep(&reaper, t0 + N, move |id| close_through(target.clone(), id)).await;
        assert_eq!(reaped.len(), 1);
        assert!(
            tm.get("term-real").is_none(),
            "the reap must close the terminal"
        );
        assert_eq!(
            close_through(tm, "term-real".into()).await,
            Ok(CloseOutcome::AlreadyGone)
        );
    }

    /// Wiring guard: the reaper does nothing unless `backend_relay.rs` ARMS an
    /// admitted remote create after its coord registration is settled (and
    /// before the `terminal_created` reply is built), and MARKS a terminal
    /// attached once `admit_terminal_attach` has bound a grant to it. Deleting
    /// either call leaves every unit test above green, so pin them by shape.
    #[test]
    fn backend_relay_arms_creates_and_marks_attaches() {
        let src = include_str!("backend_relay.rs");

        let (_, create) = src
            .split_once("async fn handle_terminal_create(")
            .expect("handle_terminal_create");
        let (create, _) = create
            .split_once("\nfn handle_terminal_input(")
            .expect("end of handle_terminal_create");
        let settle = create
            .find("settle_remote_registration(")
            .expect("registration settle");
        let arm = create
            .find("remote_create_reaper::arm_remote_created(")
            .expect("handle_terminal_create must arm the attach-deadline reaper");
        let reply = create
            .find("\"type\": \"terminal_created\",")
            .expect("created reply");
        assert!(
            settle < arm && arm < reply,
            "arm after settle, before the reply"
        );

        let (_, attach) = src
            .split_once("async fn handle_terminal_attach(")
            .expect("handle_terminal_attach");
        let gate = attach.find("match gate()").expect("attach gate");
        let mark = attach
            .find("remote_create_reaper::mark_attached(&terminal_id)")
            .expect("handle_terminal_attach must mark the bound terminal attached");
        let flow = attach
            .find("flow_gates().remove(&block.grant_jti)")
            .expect("flow gate reset");
        assert!(
            gate < mark && mark < flow,
            "mark after the bind, in the same handler"
        );
    }
}
