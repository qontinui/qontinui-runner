//! A bounded child run whose deadline COVERS PROCESS CREATION.
//!
//! Plan `2026-10-19-served-corpus-probe-budget-excludes-process-creation`,
//! Phase 1.
//!
//! [`crate::process_helpers::run_with_timeout_detailed`] starts its clock
//! AFTER `Command::spawn` returns. On Windows, Rust std holds one process-wide
//! lock across `CreateProcessW`, so every `std::process` (and tokio) spawn in
//! the runner queues behind every other one, and when process creation costs
//! seconds a caller can sit inside `spawn()` for minutes before its timeout
//! even begins. A caller that promises a wall-clock bound — the served-corpus
//! probe on the terminal-create path is the motivating one — cannot keep that
//! promise through that door.
//!
//! [`run_with_deadline_detailed`] takes an ABSOLUTE deadline and honours it
//! whatever process creation costs:
//!
//! - **Deadline already past** → [`NotStarted::DeadlinePassed`], and nothing is
//!   spawned.
//! - **The spawn runs on a named helper thread** (`deadline-spawn`), never on
//!   the calling thread. The caller waits on a condvar until the deadline; if
//!   the spawn has not delivered by then the caller returns
//!   [`NotStarted::SpawnPending`] at once.
//! - **A child that starts late is killed and parked, never waited on.** When
//!   the helper's spawn returns into a slot its caller has abandoned, the
//!   helper kills the child, fires its tree guard, and parks the `Child` for
//!   [`crate::process_helpers::reap_detached_children`].
//! - **On expiry a started child is killed**, `try_wait` is polled for at most
//!   [`KILLED_DRAIN_GRACE`], and a child that has not exited by then is parked
//!   rather than waited on — the unbounded post-kill `wait()` of the older door
//!   is not here.
//!
//! The call therefore returns no later than `deadline + KILLED_DRAIN_GRACE`
//! plus one poll, on every path.
//!
//! **Invariant: at no instant is a spawned child owned by neither side.** The
//! helper owns the `Child` from the moment `spawn` returns until it either
//! moves it into the slot (under the slot lock, while the slot is still
//! `Pending`) or kills and parks it itself (having seen `Abandoned` under that
//! lock, after which the caller never looks at the slot again). The caller
//! marks the slot `Abandoned` only under that lock and only while it is still
//! `Pending`, so exactly one side ends up responsible.
//!
//! **Opt-in, and only for read-only children.** Killing a child milliseconds
//! after a late start is harmless for a read (`git status` with
//! `GIT_OPTIONAL_LOCKS=0`) and NOT harmless for a mutating command (a
//! `git commit` killed mid-write can leave `index.lock` behind). That is why
//! this is a separate primitive rather than a change to `run_bounded`, whose
//! callers include mutating ones.

use std::io;
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::process_helpers::{
    park_detached, program_label, reap_detached_children, truncation_of, wait_for_drain,
    ChildTreeGuard, PipeDrain, Truncation, COMPLETED_DRAIN_GRACE, KILLED_DRAIN_GRACE,
};

/// How far past its deadline a run can still return on the kill path: the
/// killed child's exit and the reader drain share this one grace. A caller
/// that waits for a run from another thread must wait at least this long past
/// the deadline, or it discards results the run was about to hand back.
pub const KILL_GRACE: Duration = KILLED_DRAIN_GRACE;

/// How a child is created. Production passes [`Command::spawn`]; a test passes
/// a spawner that sleeps first, which is how "process creation is slow" is
/// reproduced without a slow machine.
pub type Spawner = fn(&mut Command) -> io::Result<Child>;

/// Why a run never got a child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotStarted {
    /// The deadline had already passed when the call was made; the spawner
    /// was never called.
    DeadlinePassed,
    /// The spawner was called, but had not returned by the deadline. If it
    /// returns later, the child it produces is killed and parked.
    SpawnPending,
}

/// What happened to the child.
#[derive(Debug)]
pub enum DeadlineOutcome {
    /// The child exited on its own before the deadline.
    Completed(Output),
    /// The child was still running at the deadline and was killed.
    TimedOut {
        /// The killed child's OS pid, for the caller's log line.
        pid: u32,
        /// Whether the killed child was seen to exit inside
        /// [`KILLED_DRAIN_GRACE`]. `false` means it was parked for the
        /// detached-child reaper instead of waited on.
        reaped: bool,
    },
    /// No child was ever handed to the caller.
    NotStarted(NotStarted),
}

/// [`run_with_deadline_detailed`]'s result.
#[derive(Debug)]
pub struct DeadlineRun {
    /// What happened.
    pub outcome: DeadlineOutcome,
    /// `Some` iff a [`DeadlineOutcome::Completed`] run's captured output is
    /// missing bytes — the same meaning as
    /// [`crate::process_helpers::TimedRun::truncation`]. Always `None` on the
    /// other outcomes.
    pub truncation: Option<Truncation>,
    /// How long process creation (spawn + tree-guard attach + both pipe-reader
    /// spawns) took, when it finished before the deadline. `None` when it did
    /// not, or never ran.
    pub spawn_elapsed: Option<Duration>,
}

/// Run `cmd` to completion, but never past `deadline` — process creation
/// INCLUDED. See the module docs for the guarantees.
///
/// `Err` only when the spawn itself failed, `try_wait` failed, or the
/// `deadline-spawn` helper thread could not be created (the process is at its
/// thread ceiling). It never falls back to spawning on the calling thread:
/// that is the unbounded wait this primitive exists to remove.
pub fn run_with_deadline_detailed(cmd: Command, deadline: Instant) -> io::Result<DeadlineRun> {
    run_with_deadline_spawner(cmd, deadline, Command::spawn)
}

/// [`run_with_deadline_detailed`] with the [`Spawner`] as a parameter.
pub fn run_with_deadline_spawner(
    mut cmd: Command,
    deadline: Instant,
    spawner: Spawner,
) -> io::Result<DeadlineRun> {
    reap_detached_children();

    if Instant::now() >= deadline {
        return Ok(not_started(NotStarted::DeadlinePassed));
    }

    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Pre-spawn half of the tree reaper (Unix process group; no-op on Windows).
    ChildTreeGuard::arm(&mut cmd);
    let label = program_label(&cmd);

    let slot = Arc::new(Slot::default());
    let helper_slot = Arc::clone(&slot);
    let helper_label = label.clone();
    std::thread::Builder::new()
        .name("deadline-spawn".to_string())
        .spawn(move || spawn_into_slot(cmd, spawner, &helper_slot, &helper_label))
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("could not start the deadline-spawn helper thread for {label}: {e}"),
            )
        })?;

    let started = match slot.wait_until(deadline) {
        Waited::Delivered(Ok(started)) => started,
        Waited::Delivered(Err(e)) => {
            qontinui_runner_lib::util::resource_exhaustion::note_fd_exhaustion(&e);
            return Err(e);
        }
        Waited::Abandoned => {
            tracing::debug!(
                program = %label,
                "process creation did not finish before the deadline; \
                 the child, if it starts, will be killed and parked"
            );
            return Ok(not_started(NotStarted::SpawnPending));
        }
    };
    qontinui_runner_lib::util::resource_exhaustion::note_spawn_succeeded();
    run_started(started, deadline, &label)
}

fn not_started(why: NotStarted) -> DeadlineRun {
    DeadlineRun {
        outcome: DeadlineOutcome::NotStarted(why),
        truncation: None,
        spawn_elapsed: None,
    }
}

/// Everything the helper thread built, handed over in one piece.
struct Started {
    child: Child,
    tree: ChildTreeGuard,
    stdout: PipeDrain,
    stderr: PipeDrain,
    spawn_elapsed: Duration,
}

enum SlotState {
    /// The helper has not returned from the spawn yet.
    Pending,
    /// The helper finished first: the spawn's result is here.
    Delivered(io::Result<Started>),
    /// The caller gave up first; whatever the helper produces is its to end.
    Abandoned,
    /// The caller took the delivered result.
    Taken,
}

struct Slot {
    state: Mutex<SlotState>,
    ready: Condvar,
}

impl Default for Slot {
    fn default() -> Self {
        Slot {
            state: Mutex::new(SlotState::Pending),
            ready: Condvar::new(),
        }
    }
}

enum Waited {
    Delivered(io::Result<Started>),
    Abandoned,
}

impl Slot {
    /// The caller's half: wait for a delivery until `deadline`, and on expiry
    /// mark the slot abandoned — both under the lock, so the helper sees one or
    /// the other, never neither.
    fn wait_until(&self, deadline: Instant) -> Waited {
        let mut g = self.state.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if matches!(*g, SlotState::Delivered(_)) {
                let SlotState::Delivered(result) = std::mem::replace(&mut *g, SlotState::Taken)
                else {
                    unreachable!("matched Delivered under the same lock");
                };
                return Waited::Delivered(result);
            }
            let now = Instant::now();
            if now >= deadline {
                *g = SlotState::Abandoned;
                return Waited::Abandoned;
            }
            g = self
                .ready
                .wait_timeout(g, deadline - now)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

/// The helper thread's whole job: create the child, guard it, start draining
/// it, then hand it over — or, if the caller has already gone, end it.
fn spawn_into_slot(mut cmd: Command, spawner: Spawner, slot: &Slot, label: &str) {
    let began = Instant::now();
    let result = spawner(&mut cmd).map(|mut child| {
        let tree = ChildTreeGuard::attach_armed(&child);
        let (stdout, stderr) = PipeDrain::for_child(&mut child);
        Started {
            child,
            tree,
            stdout,
            stderr,
            spawn_elapsed: began.elapsed(),
        }
    });
    let mut g = slot.state.lock().unwrap_or_else(|p| p.into_inner());
    match *g {
        SlotState::Pending => {
            *g = SlotState::Delivered(result);
            slot.ready.notify_all();
        }
        _ => {
            // Abandoned: the caller returned `SpawnPending` and will never
            // look at the slot again, so the child is this thread's alone from
            // here. End it — and never wait on it. The resource-exhaustion
            // stamps still see the spawn's outcome: the caller never will.
            drop(g);
            match &result {
                Ok(_) => qontinui_runner_lib::util::resource_exhaustion::note_spawn_succeeded(),
                Err(e) => {
                    let _ = qontinui_runner_lib::util::resource_exhaustion::note_fd_exhaustion(e);
                }
            }
            if let Ok(mut late) = result {
                let pid = late.child.id();
                let _ = late.child.kill();
                drop(late.tree);
                drop(late.stdout);
                drop(late.stderr);
                tracing::debug!(
                    child_pid = pid,
                    program = %label,
                    spawn_ms = u64::try_from(late.spawn_elapsed.as_millis()).unwrap_or(u64::MAX),
                    "a child started after its caller's deadline; killed and parked"
                );
                park_detached(late.child);
            }
        }
    }
}

/// Drive a delivered child against the deadline.
fn run_started(started: Started, deadline: Instant, label: &str) -> io::Result<DeadlineRun> {
    let Started {
        mut child,
        tree,
        stdout,
        stderr,
        spawn_elapsed,
    } = started;
    let pid = child.id();
    let mut poll = Duration::from_millis(2);
    let max_poll = Duration::from_millis(50);
    loop {
        if let Some(status) = child.try_wait()? {
            // Bounded drain, never a join. Capped by what is left of the
            // deadline, with KILLED_DRAIN_GRACE as the floor so a child that
            // exits right at the deadline still has its output read.
            let grace = COMPLETED_DRAIN_GRACE.min(
                deadline
                    .saturating_duration_since(Instant::now())
                    .max(KILLED_DRAIN_GRACE),
            );
            let drained = wait_for_drain(&stdout, &stderr, grace);
            let out_bytes = stdout.snapshot();
            let err_bytes = stderr.snapshot();
            let truncation = truncation_of(&stdout, &stderr, drained);
            if let Some(reason) = truncation {
                tracing::warn!(
                    child_pid = pid,
                    program = %label,
                    ?reason,
                    stdout_bytes = out_bytes.len(),
                    stderr_bytes = err_bytes.len(),
                    "child exited but its output could not be read in full; \
                     the captured stdout/stderr is INCOMPLETE"
                );
            }
            // Release the tree WITHOUT killing it, as the older door does.
            tree.disarm();
            return Ok(DeadlineRun {
                outcome: DeadlineOutcome::Completed(Output {
                    status,
                    stdout: out_bytes,
                    stderr: err_bytes,
                }),
                truncation,
                spawn_elapsed: Some(spawn_elapsed),
            });
        }
        let now = Instant::now();
        if now >= deadline {
            let _ = child.kill();
            // ONE grace for the exit and the drain together, so the call ends
            // by `deadline + KILLED_DRAIN_GRACE`.
            let grace_end = Instant::now() + KILLED_DRAIN_GRACE;
            let reaped = wait_exit_until(&mut child, grace_end);
            // Fire the reaper: any grandchild the child left behind, including
            // one holding our pipe write ends.
            drop(tree);
            let _ = wait_for_drain(
                &stdout,
                &stderr,
                grace_end.saturating_duration_since(Instant::now()),
            );
            if !reaped {
                park_detached(child);
            }
            return Ok(DeadlineRun {
                outcome: DeadlineOutcome::TimedOut { pid, reaped },
                truncation: None,
                spawn_elapsed: Some(spawn_elapsed),
            });
        }
        std::thread::sleep(poll.min(deadline - now));
        poll = (poll * 2).min(max_poll);
    }
}

/// Poll `try_wait` until `end`. `true` when the child was seen to exit (and is
/// therefore reaped).
fn wait_exit_until(child: &mut Child, end: Instant) -> bool {
    let mut poll = Duration::from_millis(1);
    loop {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return true;
        }
        let now = Instant::now();
        if now >= end {
            return false;
        }
        std::thread::sleep(poll.min(end - now));
        poll = (poll * 2).min(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Slack for scheduler noise on a loaded CI box.
    const EPS: Duration = Duration::from_millis(400);

    /// Every test here that spawns holds this: they park and reap children in
    /// the process-global detached-child ledger and start pipe readers, which
    /// `process_helpers`' gauge tests count.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        crate::process_helpers::GAUGE_TESTS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// A child that would run ~30 s if nobody killed it.
    fn long_child() -> Command {
        #[cfg(windows)]
        {
            let mut c = crate::process_helpers::no_window("ping");
            c.args(["-n", "31", "127.0.0.1"]);
            c
        }
        #[cfg(not(windows))]
        {
            let mut c = Command::new("sleep");
            c.arg("30");
            c
        }
    }

    /// One spawned child's identity, taken while it is certainly alive: on
    /// Windows a process HANDLE (so a reused pid can never be mistaken for
    /// it), elsewhere the pid (Unix pid reuse is far slower than these tests).
    struct Watched {
        pid: u32,
        #[cfg(windows)]
        handle: usize,
    }

    impl Watched {
        fn of(child: &Child) -> Watched {
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawHandle;
                use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS};
                use windows_sys::Win32::System::Threading::GetCurrentProcess;
                let mut out: windows_sys::Win32::Foundation::HANDLE = std::ptr::null_mut();
                // SAFETY: `child` is alive and owns its handle for this call;
                // `out` is a valid local that receives a handle we close in
                // `Drop`.
                let ok = unsafe {
                    let me = GetCurrentProcess();
                    DuplicateHandle(
                        me,
                        child.as_raw_handle() as _,
                        me,
                        &mut out,
                        0,
                        0,
                        DUPLICATE_SAME_ACCESS,
                    )
                };
                assert!(ok != 0 && !out.is_null(), "DuplicateHandle failed");
                Watched {
                    pid: child.id(),
                    handle: out as usize,
                }
            }
            #[cfg(not(windows))]
            {
                Watched { pid: child.id() }
            }
        }

        fn alive(&self) -> bool {
            #[cfg(windows)]
            {
                use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
                use windows_sys::Win32::System::Threading::WaitForSingleObject;
                // SAFETY: a process handle this value owns, with SYNCHRONIZE
                // (DUPLICATE_SAME_ACCESS of std's PROCESS_ALL_ACCESS handle).
                unsafe { WaitForSingleObject(self.handle as _, 0) == WAIT_TIMEOUT }
            }
            #[cfg(not(windows))]
            {
                // Reap parked children first, so an exited one is not
                // mistaken for alive (a Unix zombie).
                reap_detached_children();
                // SAFETY: signal 0 delivers nothing; it only probes existence.
                unsafe { libc::kill(self.pid as i32, 0) == 0 }
            }
        }

        fn wait_gone(&self, within: Duration) {
            let end = Instant::now() + within;
            while self.alive() {
                assert!(Instant::now() < end, "pid {} survived", self.pid);
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    #[cfg(windows)]
    impl Drop for Watched {
        fn drop(&mut self) {
            // SAFETY: closing the handle `of` duplicated, exactly once.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle as _) };
        }
    }

    #[test]
    fn process_deadline_a_fast_child_completes_with_its_output() {
        let _serial = serial();
        let mut cmd = Command::new("git");
        cmd.arg("--version");
        let run = run_with_deadline_detailed(cmd, Instant::now() + Duration::from_secs(20))
            .expect("git runs");
        match run.outcome {
            DeadlineOutcome::Completed(out) => {
                assert!(out.status.success());
                assert!(
                    String::from_utf8_lossy(&out.stdout).starts_with("git version"),
                    "{out:?}"
                );
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        assert_eq!(run.truncation, None);
        assert!(run.spawn_elapsed.is_some());
    }

    static PAST_CALLS: AtomicUsize = AtomicUsize::new(0);
    fn counting_spawner(cmd: &mut Command) -> io::Result<Child> {
        PAST_CALLS.fetch_add(1, Ordering::SeqCst);
        cmd.spawn()
    }

    #[test]
    fn process_deadline_a_passed_deadline_never_calls_the_spawner() {
        let past = Instant::now();
        std::thread::sleep(Duration::from_millis(2));
        let run = run_with_deadline_spawner(long_child(), past, counting_spawner).unwrap();
        assert!(matches!(
            run.outcome,
            DeadlineOutcome::NotStarted(NotStarted::DeadlinePassed)
        ));
        assert_eq!(PAST_CALLS.load(Ordering::SeqCst), 0);
    }

    static SLOW_CHILD: Mutex<Option<(Watched, Instant)>> = Mutex::new(None);
    const SLOW_BUDGET: Duration = Duration::from_millis(400);
    fn slow_spawner(cmd: &mut Command) -> io::Result<Child> {
        std::thread::sleep(SLOW_BUDGET * 3);
        let child = cmd.spawn()?;
        *SLOW_CHILD.lock().unwrap() = Some((Watched::of(&child), Instant::now()));
        Ok(child)
    }

    /// THE defect: process creation that takes 3x the budget. The call
    /// returns at the deadline, not after the spawn, and the child that
    /// started late is killed rather than left running.
    #[test]
    fn process_deadline_a_slow_spawn_returns_at_the_deadline_and_the_late_child_dies() {
        let _serial = serial();
        let started = Instant::now();
        let run =
            run_with_deadline_spawner(long_child(), started + SLOW_BUDGET, slow_spawner).unwrap();
        let elapsed = started.elapsed();
        assert!(
            matches!(
                run.outcome,
                DeadlineOutcome::NotStarted(NotStarted::SpawnPending)
            ),
            "{run:?}"
        );
        assert!(
            elapsed < SLOW_BUDGET + Duration::from_millis(250),
            "returned after {elapsed:?}; the deadline must cover process creation"
        );
        // The late child: wait for the spawner to return, then it must be gone
        // within KILLED_DRAIN_GRACE + 1 s.
        let end = Instant::now() + SLOW_BUDGET * 3 + Duration::from_secs(10);
        let (watched, spawned_at) = loop {
            if let Some(v) = SLOW_CHILD.lock().unwrap().take() {
                break v;
            }
            assert!(Instant::now() < end, "the slow spawner never returned");
            std::thread::sleep(Duration::from_millis(20));
        };
        let budget_left = (spawned_at + KILLED_DRAIN_GRACE + Duration::from_secs(1))
            .saturating_duration_since(Instant::now());
        watched.wait_gone(budget_left.max(Duration::from_millis(1)));
    }

    static HUNG_CHILD: Mutex<Option<Watched>> = Mutex::new(None);
    fn recording_spawner(cmd: &mut Command) -> io::Result<Child> {
        let child = cmd.spawn()?;
        *HUNG_CHILD.lock().unwrap() = Some(Watched::of(&child));
        Ok(child)
    }

    /// A child that is still running at the deadline is killed there. The
    /// budget is generous (3 s) so a slow spawn on a loaded box still delivers
    /// the child before the deadline; the bound under test is the kill.
    #[test]
    fn process_deadline_a_hung_child_is_killed_at_the_deadline() {
        let _serial = serial();
        let budget = Duration::from_secs(3);
        let started = Instant::now();
        let run =
            run_with_deadline_spawner(long_child(), started + budget, recording_spawner).unwrap();
        let elapsed = started.elapsed();
        assert!(
            matches!(run.outcome, DeadlineOutcome::TimedOut { .. }),
            "expected TimedOut, got {run:?}"
        );
        assert!(
            elapsed < budget + KILLED_DRAIN_GRACE + EPS,
            "took {elapsed:?}"
        );
        let watched = HUNG_CHILD.lock().unwrap().take().expect("the spawner ran");
        watched.wait_gone(Duration::from_secs(5));
    }

    fn failing_spawner(_cmd: &mut Command) -> io::Result<Child> {
        Err(io::Error::new(io::ErrorKind::NotFound, "no such program"))
    }

    #[test]
    fn process_deadline_a_spawn_error_is_an_err() {
        let r = run_with_deadline_spawner(
            long_child(),
            Instant::now() + Duration::from_secs(5),
            failing_spawner,
        );
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn process_deadline_a_missing_program_is_an_err() {
        let cmd = Command::new("qontinui-no-such-program-for-deadline-test");
        let r = run_with_deadline_detailed(cmd, Instant::now() + Duration::from_secs(5));
        assert!(r.is_err(), "{r:?}");
    }

    static RACE_CHILDREN: Mutex<Vec<Watched>> = Mutex::new(Vec::new());
    static RACE_TICK: AtomicUsize = AtomicUsize::new(0);
    const RACE_BUDGET: Duration = Duration::from_millis(100);
    fn racing_spawner(cmd: &mut Command) -> io::Result<Child> {
        // Every third spawn is immediate (delivered: the caller kills it at
        // the deadline), every third lands well past the deadline (abandoned:
        // the helper kills it), and the rest jitter across the deadline
        // itself (70..=130 ms against 100 ms), where the two sides race.
        let tick = RACE_TICK.fetch_add(1, Ordering::SeqCst);
        let sleep_ms = match tick % 3 {
            0 => 0,
            1 => 250,
            _ => 70 + (tick * 7 % 61) as u64,
        };
        std::thread::sleep(Duration::from_millis(sleep_ms));
        let child = cmd.spawn()?;
        RACE_CHILDREN.lock().unwrap().push(Watched::of(&child));
        Ok(child)
    }

    /// The race at the deadline, 200 times: every child is either handed to
    /// the caller (and killed at the deadline) or killed by the helper; none
    /// survives, whichever side won — and both sides are seen to win.
    #[test]
    fn process_deadline_no_child_survives_a_race_at_the_deadline() {
        let _serial = serial();
        let mut outcomes = [0usize; 2];
        for _ in 0..200 {
            let run = run_with_deadline_spawner(
                long_child(),
                Instant::now() + RACE_BUDGET,
                racing_spawner,
            )
            .unwrap();
            match run.outcome {
                DeadlineOutcome::TimedOut { .. } => outcomes[0] += 1,
                DeadlineOutcome::NotStarted(NotStarted::SpawnPending) => outcomes[1] += 1,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(outcomes[0] + outcomes[1], 200, "{outcomes:?}");
        assert!(
            outcomes[0] > 0 && outcomes[1] > 0,
            "both sides of the race must be exercised: {outcomes:?}"
        );
        // Let every abandoned helper's spawn return.
        let end = Instant::now() + Duration::from_secs(30);
        while RACE_CHILDREN.lock().unwrap().len() < 200 {
            assert!(Instant::now() < end, "a helper never returned");
            std::thread::sleep(Duration::from_millis(20));
        }
        let children = std::mem::take(&mut *RACE_CHILDREN.lock().unwrap());
        for child in &children {
            child.wait_gone(Duration::from_secs(10));
        }
        reap_detached_children();
    }
}
