//! The synchronous steps of a session spawn, run on the blocking pool rather
//! than on an `app-rt` async worker.
//!
//! ## Why this module exists
//!
//! On 2026-10-27 a burst of gate continuations overloaded the runner and the
//! box was hard-restarted (plan
//! `2026-10-28-runner-spawn-bursts-overwhelm-the-box-beyond-the-continuation-cap`,
//! D2 / Phase 3). The spawn paths are `async`, but they made synchronous calls
//! that can block for seconds or minutes. Each such call holds the worker it
//! runs on for its whole duration:
//!
//! - **The PTY create** (`create_tracked_terminal_session_backend`, its wrapper
//!   `spawn_resumed_pane`, and `TerminalManager::create` on the HTTP and relay
//!   doors). One create was measured at 38 s (20:22:41 -> 20:23:19). Completions
//!   of other tasks then landed in a batch within 80 ms of that create returning.
//! - **The fleet commands / skills provisioning**
//!   (`provision_fleet_commands_for_session`,
//!   `provision_fleet_skills_for_session`). Each pass fetches the registry on a
//!   joined OS thread and polls a `git ls-files` child with
//!   `std::thread::sleep` for up to 5 s. Phase 1 found 20 of these passes
//!   running strictly one after another from 19:55:18 to 20:22:39, before the
//!   first PTY create.
//!
//! The app runtime has at most 16 workers (`APP_RUNTIME_WORKER_THREADS_MAX`), and
//! the API runtime the HTTP doors run on has fewer. A burst of spawns can
//! therefore hold all of a runtime's workers. Then it polls nothing else: on the
//! app runtime, not the claim heartbeats or the coord calls of the other
//! continuations; on the API runtime, not `/livez`. A timer cannot fire either,
//! because the time driver only advances when a worker reaches the scheduler
//! (see [`crate::off_runtime`]).
//!
//! ## What it provides
//!
//! Two seams, so that every async call site uses the same mechanism:
//!
//! - [`run_terminal_create_off_worker`] for the PTY create;
//! - [`run_provisioning_off_worker`] for session-cwd provisioning.
//!
//! And [`run_detached`], for a request handler that awaits a create: without it,
//! a client disconnect could abandon the handler after the PTY opened.
//!
//! The tests below pin the async call sites they list to these seams (see
//! `tests`), so moving a listed site back inline fails the build's tests. The
//! list is kept by hand: a NEW spawn path has to be added to it. The worktree
//! `git fetch` + `git worktree add` step has its own seam,
//! `agent_worktree::materialize_serial`, because it must also hold the
//! per-checkout lock.

use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;
use tracing::warn;

/// Run a synchronous terminal-session create on the blocking pool and await
/// it.
///
/// `create` is the call exactly as the site would have made it inline. It
/// returns the create's own `Ok` / `Err`, so the site's result handling is
/// unchanged.
///
/// If the create does not complete (it panicked, or the runtime is shutting
/// down), this returns an `Err` that says so. The site's existing failure arm
/// then runs, for example `report_spawn_failed`, rather than the whole spawn
/// task unwinding with nothing reported. In that case it is UNKNOWN whether a
/// PTY opened before the panic. The message says only that the task did not
/// complete; it does not claim that nothing was created.
///
/// Dropping the returned future does NOT cancel `create`, because a blocking
/// task cannot be cancelled. Every caller today runs on its own detached spawn
/// task and awaits this to completion.
pub(crate) async fn run_terminal_create_off_worker<T, F>(create: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    spawn_blocking_tracked(create)
        .await
        .unwrap_or_else(|e| Err(format!("terminal create task did not complete: {e}")))
}

/// Run synchronous session-cwd provisioning on the blocking pool and await it.
/// This covers the fleet commands and skills, `.mcp.json` where it is part of
/// the same pass, and the agent definitions.
///
/// Provisioning is fail-soft by contract: the provisioners log and swallow
/// their own errors, and a failure must never abort a spawn that could
/// otherwise launch. A pass that does not complete (it panicked) is handled
/// the same way. It is logged with `workdir` and returns `None`, and each
/// caller decides what `None` means for the value it needed.
pub(crate) async fn run_provisioning_off_worker<R, F>(workdir: &str, work: F) -> Option<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    match spawn_blocking_tracked(work).await {
        Ok(out) => Some(out),
        Err(e) => {
            warn!(
                workdir = %workdir,
                error = %e,
                "session provisioning did not complete (continuing the spawn; the session may \
                 lack its fleet commands, skills or agent definitions)"
            );
            None
        }
    }
}

/// Run a request handler's body detached from the request, and await it.
///
/// A create that ran inline could not be interrupted. Once a handler awaits it
/// off-worker, that await is a cancel point: a client that disconnects drops the
/// handler, while the blocking task goes on to open the PTY, and whatever the
/// handler does next (recording the session, typing its initial command, parking
/// its isolated context) never runs. Detached, the body always runs to its end.
/// A client that disconnects earlier therefore still gets its terminal.
///
/// A panic in the body is still a caught panic, as it was inline: its crash dump
/// is downgraded (see [`run_detached_with`]) and the panic is re-raised on this
/// task, so the API's `CatchPanicLayer` answers its 500. The request's tracing
/// span goes with the body.
pub(crate) async fn run_detached<F>(body: F) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    run_detached_with(body, || {
        crate::logging::retract_last_crash_dump("detached request body panic");
    })
    .await
}

/// [`run_detached`], with what to do on the thread that panicked injected.
///
/// The panic hook leaves the crash dump's path in a thread-local, and
/// `resume_unwind` runs no hook, so `CatchPanicLayer` could only find that path
/// when the request's task happened to run on the same worker. So the panic is
/// caught inside the detached task, where `catch_unwind` resumes on the thread
/// that panicked, and `on_panic` runs there.
async fn run_detached_with<F>(body: F, on_panic: impl FnOnce() + Send + 'static) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    use futures::FutureExt;
    let caught =
        std::panic::AssertUnwindSafe(tracing::Instrument::in_current_span(body)).catch_unwind();
    let joined = tokio::spawn(async move {
        let out = caught.await;
        if out.is_err() {
            on_panic();
        }
        out
    })
    .await;
    match joined {
        Ok(Ok(out)) => out,
        Ok(Err(payload)) => std::panic::resume_unwind(payload),
        Err(e) => panic!("detached request body did not complete: {e}"),
    }
}

/// Test support shared by every off-worker seam's tests: the latency harness
/// and the source pin.
#[cfg(test)]
pub(crate) mod test_support {
    use std::future::Future;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    /// Worker threads of the harness runtime. Two, so that the harness can
    /// hold every worker with only a few blocked tasks.
    pub(crate) const WORKERS: usize = 2;
    /// The unrelated timer task's own sleep.
    pub(crate) const SLEEPER: Duration = Duration::from_millis(50);
    /// The most the blocked spawn steps may add to [`SLEEPER`].
    pub(crate) const MAX_EXTRA_DELAY: Duration = Duration::from_millis(500);

    /// Start `tasks` futures from `make(i, entered)` on a fresh
    /// [`WORKERS`]-worker runtime. Each future's blocking body must increment
    /// `entered` when it starts. Wait until [`WORKERS`] bodies have started,
    /// then time an unrelated `tokio::time::sleep(SLEEPER)` task.
    ///
    /// Returns how much LONGER than [`SLEEPER`] that task took, and the
    /// futures' outputs in spawn order.
    ///
    /// The timing is measured on this test thread with std channels, outside
    /// the runtime. A blocked runtime cannot run its own timer, so a
    /// measurement taken inside it would also stall (see
    /// [`crate::off_runtime`]). The harness waits for [`WORKERS`] bodies, not
    /// one, because that is how many it takes to hold every worker when the
    /// bodies run inline. With fewer, one worker stays free, the timer task runs
    /// on it, and the test could not fail.
    pub(crate) fn timer_delay_while_blocked<T, Fut>(
        tasks: usize,
        make: impl Fn(usize, Arc<AtomicUsize>) -> Fut,
    ) -> (Duration, Vec<T>)
    where
        Fut: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        assert!(
            tasks >= WORKERS,
            "fewer tasks than workers cannot hold every worker"
        );
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKERS)
            .enable_all()
            .build()
            .expect("runtime");
        let entered = Arc::new(AtomicUsize::new(0));
        let (done_tx, done_rx) = mpsc::channel::<(usize, T)>();
        for i in 0..tasks {
            let fut = make(i, entered.clone());
            let done_tx = done_tx.clone();
            rt.spawn(async move {
                let out = fut.await;
                let _ = done_tx.send((i, out));
            });
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while entered.load(Ordering::SeqCst) < WORKERS {
            assert!(
                Instant::now() < deadline,
                "the blocking bodies never started"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        let started = Instant::now();
        let (tick_tx, tick_rx) = mpsc::channel::<Instant>();
        rt.spawn(async move {
            tokio::time::sleep(SLEEPER).await;
            let _ = tick_tx.send(Instant::now());
        });
        let ticked = tick_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the timer task never ran");
        let extra = ticked.duration_since(started).saturating_sub(SLEEPER);

        let mut outs: Vec<(usize, T)> = (0..tasks)
            .map(|_| {
                done_rx
                    .recv_timeout(Duration::from_secs(60))
                    .expect("a spawned task never finished")
            })
            .collect();
        outs.sort_by_key(|(i, _)| *i);
        (extra, outs.into_iter().map(|(_, out)| out).collect())
    }

    /// The body of the top-level `fn name` in `src`: its declaration line up to
    /// the first `}` in column 0 after it. Comment lines are dropped. `None`
    /// when no such fn exists.
    pub(crate) fn fn_body(src: &str, name: &str) -> Option<String> {
        let decl = regex::Regex::new(&format!(
            r"^(?:pub(?:\([a-z]+\))?\s+)?(?:async\s+)?fn\s+{}\b",
            regex::escape(name)
        ))
        .expect("fn decl regex");
        let lines: Vec<&str> = src.lines().collect();
        let start = lines.iter().position(|l| decl.is_match(l))?;
        let end = (start + 1..lines.len()).find(|&i| lines[i].starts_with('}'))?;
        Some(
            lines[start..=end]
                .iter()
                .filter(|l| !l.trim_start().starts_with("//"))
                .copied()
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    /// Byte ranges of the argument list of every `seam(` call in `body`, from
    /// its `(` to the matching `)`. String and char literals are skipped while
    /// matching, so a paren inside one cannot unbalance the count.
    fn seam_spans(body: &str, seam: &str) -> Vec<(usize, usize)> {
        let re = regex::Regex::new(&format!(r"\b{}\s*\(", regex::escape(seam))).expect("seam re");
        let bytes = body.as_bytes();
        let mut spans = Vec::new();
        for m in re.find_iter(body) {
            let open = m.end() - 1;
            let mut depth = 0usize;
            let mut i = open;
            while i < bytes.len() {
                match bytes[i] {
                    b'"' => {
                        i += 1;
                        while i < bytes.len() && bytes[i] != b'"' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                    }
                    b'\'' if i + 2 < bytes.len() && bytes[i + 2] == b'\'' => i += 2,
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            spans.push((open, i));
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
        }
        spans
    }

    /// `call(` as a regex. The `\b` before the name and the `\s*\(` after it
    /// mean `provision_session_cwd(` does not match
    /// `provision_session_cwd_off_worker(`.
    fn call_re(call: &str) -> regex::Regex {
        regex::Regex::new(&format!(r"\b{}\s*\(", regex::escape(call))).expect("call re")
    }

    /// Every `call(` in `body` as `(inside, outside)`: `inside` counts the calls
    /// that sit within the argument list of a `seam(` call, and `outside`
    /// counts the bare ones.
    pub(crate) fn calls_inside_and_outside(body: &str, seam: &str, call: &str) -> (usize, usize) {
        let spans = seam_spans(body, seam);
        call_re(call)
            .find_iter(body)
            .fold((0, 0), |(inside, outside), m| {
                if spans.iter().any(|&(s, e)| m.start() > s && m.start() < e) {
                    (inside + 1, outside)
                } else {
                    (inside, outside + 1)
                }
            })
    }

    /// How many times `body` calls `call(`.
    pub(crate) fn count_calls(body: &str, call: &str) -> usize {
        call_re(call).find_iter(body).count()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{
        calls_inside_and_outside, count_calls, fn_body, timer_delay_while_blocked, MAX_EXTRA_DELAY,
    };
    use super::*;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    const AGENT_RUNTIME: &str = include_str!("agent_runtime.rs");
    const LOOPING: &str = include_str!("looping_agent_supervisor.rs");
    const RESPAWN: &str = include_str!("session/respawn.rs");
    const RESUME_DOOR: &str = include_str!("session/resume_door.rs");
    const SCHEDULER_REMOTE: &str = include_str!("scheduler_remote_agent.rs");
    const ISOLATED_EDIT: &str = include_str!("agent_worktree/isolated_edit.rs");
    const AI_SESSION_EXECUTOR: &str = include_str!("orchestration_loop/ai_session_executor.rs");
    const TERMINALS: &str = include_str!("mcp/terminals.rs");
    const STEWARD: &str = include_str!("mcp/steward.rs");
    const TAURI_PROXY: &str = include_str!("mcp/tauri_proxy.rs");
    const BACKEND_RELAY: &str = include_str!("mcp/backend_relay.rs");

    /// Every async fn that creates an unattended terminal session:
    /// `(file, source, fn, seam, create call)`. Each create call must sit
    /// inside the seam's argument list. The `create_tracked_…` sites, respawn's
    /// `spawn_resumed_pane` (a wrapper around the same create) and the HTTP and
    /// relay doors' `TerminalManager::create` use
    /// [`run_terminal_create_off_worker`]. Boot resume already ran its create
    /// in `tokio::task::spawn_blocking` before this plan; it is pinned so that
    /// it stays there.
    const TERMINAL_CREATE_SITES: &[(&str, &str, &str, &str, &str)] = &[
        (
            "agent_runtime.rs",
            AGENT_RUNTIME,
            "run_continuation_terminal",
            "run_terminal_create_off_worker",
            "create_tracked_terminal_session_backend",
        ),
        (
            "agent_runtime.rs",
            AGENT_RUNTIME,
            "run_condition_check_terminal",
            "run_terminal_create_off_worker",
            "create_tracked_terminal_session_backend",
        ),
        (
            "looping_agent_supervisor.rs",
            LOOPING,
            "spawn_looping_agent_terminal",
            "run_terminal_create_off_worker",
            "create_tracked_terminal_session_backend",
        ),
        (
            "session/respawn.rs",
            RESPAWN,
            "materialize",
            "run_terminal_create_off_worker",
            "spawn_resumed_pane",
        ),
        (
            "session/resume_door.rs",
            RESUME_DOOR,
            "resume_one",
            "spawn_blocking",
            "spawn_resumed_pane",
        ),
        (
            "mcp/terminals.rs",
            TERMINALS,
            "create_terminal_handler",
            "run_terminal_create_off_worker",
            ".create",
        ),
        (
            "mcp/steward.rs",
            STEWARD,
            "start_steward",
            "run_terminal_create_off_worker",
            ".create",
        ),
        (
            "mcp/tauri_proxy.rs",
            TAURI_PROXY,
            "dispatch",
            "run_terminal_create_off_worker",
            ".create",
        ),
        (
            "mcp/backend_relay.rs",
            BACKEND_RELAY,
            "handle_terminal_create",
            "run_terminal_create_off_worker",
            ".create",
        ),
    ];

    /// Every async fn that provisions a session cwd, with the provisioning calls
    /// it makes. Each such call must sit inside a `run_provisioning_off_worker(`
    /// call. Phase 1 of the plan named the fleet pair. `provision_session_cwd`
    /// is the guarded operator-cwd pass that runs the same pair, and
    /// `provision_agent_definitions_recorded` sits in the same block of
    /// `run_agent_subprocess`.
    const PROVISIONING_SITES: &[(&str, &str, &str, &[&str])] = &[
        (
            "agent_runtime.rs",
            AGENT_RUNTIME,
            "run_continuation_terminal",
            &[
                "provision_fleet_commands_for_session",
                "provision_fleet_skills_for_session",
            ],
        ),
        (
            "agent_runtime.rs",
            AGENT_RUNTIME,
            "run_agent_subprocess",
            &[
                "provision_agent_definitions_recorded",
                "provision_fleet_commands_for_session",
                "provision_fleet_skills_for_session",
            ],
        ),
        (
            "looping_agent_supervisor.rs",
            LOOPING,
            "spawn_looping_agent_terminal",
            &[
                "provision_fleet_commands_for_session",
                "provision_fleet_skills_for_session",
            ],
        ),
        (
            "scheduler_remote_agent.rs",
            SCHEDULER_REMOTE,
            "launch",
            &[
                "provision_fleet_commands_for_session",
                "provision_fleet_skills_for_session",
            ],
        ),
        (
            "agent_worktree/isolated_edit.rs",
            ISOLATED_EDIT,
            "provision_session_cwd_off_worker",
            &["provision_session_cwd"],
        ),
    ];

    /// Async fns that once called the synchronous `provision_session_cwd`. They
    /// must now reach it only through `provision_session_cwd_off_worker`.
    const PROVISION_SESSION_CWD_CALLERS: &[(&str, &str, &str)] = &[
        (
            "agent_worktree/isolated_edit.rs",
            ISOLATED_EDIT,
            "acquire_for_terminal",
        ),
        (
            "agent_worktree/isolated_edit.rs",
            ISOLATED_EDIT,
            "acquire_for_worker",
        ),
        (
            "orchestration_loop/ai_session_executor.rs",
            AI_SESSION_EXECUTOR,
            "dispatch_subtask",
        ),
    ];

    /// The PTY-create pin: every unattended terminal-create site makes its
    /// create call inside its seam, and makes it at least once. This is the
    /// test that enters through each real call site. The latency of the seam
    /// itself is `agent_runtime::tests::continuation_terminal_create_runs_off_the_async_workers`.
    ///
    /// Mutation check: replace a site's `run_terminal_create_off_worker(move ||
    /// …).await` with the bare create call, and this test names that site.
    #[test]
    fn continuation_terminal_create_sites_all_run_off_the_async_workers() {
        for (file, src, func, seam, call) in TERMINAL_CREATE_SITES {
            let body = fn_body(src, func).unwrap_or_else(|| panic!("{file}: no fn {func}"));
            let (inside, outside) = calls_inside_and_outside(&body, seam, call);
            assert_eq!(
                outside, 0,
                "{file}::{func} calls {call}( on the async worker — wrap it in {seam}"
            );
            assert!(inside >= 1, "{file}::{func} no longer calls {call}(");
        }
    }

    /// Each HTTP door runs its whole body through [`run_detached`], because
    /// awaiting the create gives it a cancel point it did not have inline. The
    /// relay's own detachment (`runs_off_read_loop`) predates this plan.
    #[test]
    fn the_http_create_doors_run_detached_from_their_request() {
        for (file, src, func, body_fn) in [
            (
                "mcp/terminals.rs",
                TERMINALS,
                "create_terminal_detached",
                "create_terminal_handler",
            ),
            (
                "mcp/steward.rs",
                STEWARD,
                "start_steward_detached",
                "start_steward",
            ),
            (
                "mcp/tauri_proxy.rs",
                TAURI_PROXY,
                "tauri_invoke_handler",
                "dispatch",
            ),
        ] {
            let body = fn_body(src, func).unwrap_or_else(|| panic!("{file}: no fn {func}"));
            assert_eq!(
                calls_inside_and_outside(&body, "run_detached", body_fn),
                (1, 0),
                "{file}::{func} must run {body_fn} inside run_detached"
            );
        }
    }

    /// The body outlives the request: this is what a client disconnect is.
    ///
    /// Mutation check: make [`run_detached`] just `body.await`. Aborting the
    /// request then drops the body, and the channel below closes unsent.
    #[tokio::test]
    async fn a_detached_body_runs_to_its_end_when_the_request_is_dropped() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let request = tokio::spawn(run_detached(async move {
            let _ = started_tx.send(());
            let _ = go_rx.await;
            let _ = done_tx.send(());
        }));
        started_rx.await.expect("the body never started");
        request.abort();
        let _ = request.await;
        let _ = go_tx.send(());
        done_rx
            .await
            .expect("dropping the request dropped the detached body");
    }

    /// The crash dump's path is only readable on the thread that panicked, so
    /// that is where `on_panic` must run, whichever worker the request's task is
    /// on.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_detached_panic_is_retracted_on_the_thread_that_panicked() {
        use std::sync::{Arc, Mutex};
        let panicked_on = Arc::new(Mutex::new(None));
        let retracted_on = Arc::new(Mutex::new(None));
        let (p, r) = (panicked_on.clone(), retracted_on.clone());
        let joined = tokio::spawn(run_detached_with(
            async move {
                *p.lock().unwrap() = Some(std::thread::current().id());
                panic!("stand-in body panic");
            },
            move || *r.lock().unwrap() = Some(std::thread::current().id()),
        ))
        .await;
        assert!(joined.unwrap_err().is_panic());
        let panicked = panicked_on.lock().unwrap().expect("the body never ran");
        assert_eq!(*retracted_on.lock().unwrap(), Some(panicked));
    }

    /// A panic comes back as the same panic, on the request's own task, so the
    /// API's `CatchPanicLayer` answers its 500. A `JoinError` turned into a
    /// normal response would skip that.
    #[tokio::test]
    async fn a_detached_body_panic_is_re_raised_on_the_request_task() {
        let joined = tokio::spawn(run_detached(async { panic!("stand-in body panic") })).await;
        let payload = joined.unwrap_err().into_panic();
        assert_eq!(
            payload.downcast_ref::<&str>().copied(),
            Some("stand-in body panic")
        );
    }

    /// D2, the provisioning half: a burst of provisioning passes must not hold
    /// the async workers. The stand-in sleeps 2 s, roughly a `git ls-files`
    /// probe at its 5 s bound under a slow `CreateProcess`. Four such passes on
    /// a 2-worker runtime must not delay an unrelated 50 ms timer by more than
    /// 500 ms.
    ///
    /// Mutation check: make `run_provisioning_off_worker` call `work()` inline
    /// instead of through `spawn_blocking_tracked`. Then both workers stay
    /// blocked for 2 s and this test fails.
    #[test]
    fn provisioning_off_worker_does_not_delay_the_async_workers() {
        let (extra, outs) = timer_delay_while_blocked(4, |i, entered| async move {
            run_provisioning_off_worker("D:/stand-in/workdir", move || {
                entered.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_secs(2));
                i * 10
            })
            .await
        });
        assert!(
            extra <= MAX_EXTRA_DELAY,
            "four 2 s provisioning passes delayed a 50 ms timer by {extra:?} (bound 500ms) — \
             they ran on the async workers"
        );
        assert_eq!(outs, vec![Some(0), Some(10), Some(20), Some(30)]);
    }

    /// A pass that panics is the fail-soft `None`, never a panic in the spawn
    /// task.
    #[tokio::test]
    async fn provisioning_off_worker_answers_none_for_a_pass_that_panicked() {
        let out: Option<u8> = run_provisioning_off_worker("D:/stand-in/workdir", || {
            panic!("stand-in provisioning panic")
        })
        .await;
        assert_eq!(out, None);
    }

    /// The real pair, run through the seam into a scratch cwd, writes the
    /// bundle. This shows that the provisioners work on a blocking-pool thread
    /// (their registry fetch builds its own runtime on its own thread, which
    /// must not panic there). `vet-plan.md` is one of the commands
    /// `fleet_commands` bundles.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn provisioning_off_worker_writes_the_fleet_bundle_into_the_session_cwd() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let wd = dir.path().to_string_lossy().to_string();
        let work_wd = wd.clone();
        let out = run_provisioning_off_worker(&wd, move || {
            crate::fleet_commands::provision_fleet_commands_for_session(&work_wd);
            crate::fleet_skills::provision_fleet_skills_for_session(&work_wd);
        })
        .await;
        assert_eq!(out, Some(()));
        assert!(
            dir.path()
                .join(".claude")
                .join("commands")
                .join("vet-plan.md")
                .is_file(),
            "the fleet commands were not written into the session cwd"
        );
        assert!(
            dir.path().join(".claude").join("skills").is_dir(),
            "the fleet skills were not written into the session cwd"
        );
    }

    /// The pin: every listed async fn makes each of its provisioning calls
    /// inside `run_provisioning_off_worker(`, and makes it at least once. A fn
    /// that is renamed or loses its call fails here instead of passing
    /// vacuously.
    #[test]
    fn provisioning_off_worker_covers_every_async_call_site() {
        for (file, src, func, calls) in PROVISIONING_SITES {
            let body = fn_body(src, func).unwrap_or_else(|| panic!("{file}: no fn {func}"));
            for call in *calls {
                let (inside, outside) =
                    calls_inside_and_outside(&body, "run_provisioning_off_worker", call);
                assert_eq!(
                    outside, 0,
                    "{file}::{func} calls {call}( on the async worker — wrap it in \
                     run_provisioning_off_worker"
                );
                assert!(inside >= 1, "{file}::{func} no longer calls {call}(");
            }
        }
        for (file, src, func) in PROVISION_SESSION_CWD_CALLERS {
            let body = fn_body(src, func).unwrap_or_else(|| panic!("{file}: no fn {func}"));
            assert_eq!(
                count_calls(&body, "provision_session_cwd"),
                0,
                "{file}::{func} calls the synchronous provision_session_cwd( — use \
                 provision_session_cwd_off_worker"
            );
            assert!(
                count_calls(&body, "provision_session_cwd_off_worker") >= 1,
                "{file}::{func} no longer provisions its cwd"
            );
        }
    }

    /// The pin's mutation twin: a bare call is counted outside the seam, a call
    /// wrapped in a multi-statement closure is counted inside it, and a paren
    /// in a string literal does not end the span early.
    #[test]
    fn provisioning_off_worker_pin_tells_a_bare_call_from_a_wrapped_one() {
        let bare = "async fn f() {\n    provision_fleet_skills_for_session(&wd);\n}";
        assert_eq!(
            calls_inside_and_outside(
                bare,
                "run_provisioning_off_worker",
                "provision_fleet_skills_for_session"
            ),
            (0, 1)
        );
        let wrapped = "async fn f() {\n    run_provisioning_off_worker(&wd, move || {\n        \
                       warn!(\"a ) in a string\");\n        provision_fleet_skills_for_session(&w);\n    \
                       })\n    .await;\n}";
        assert_eq!(
            calls_inside_and_outside(
                wrapped,
                "run_provisioning_off_worker",
                "provision_fleet_skills_for_session"
            ),
            (1, 0)
        );
    }
}
