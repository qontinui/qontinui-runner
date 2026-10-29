//! Worktree materialisation: off the async workers, and serialised per shared
//! checkout.
//!
//! [`super::allocate_and_materialize_with_claim`] materialises each planned row
//! with a locality `git fetch` and a `git worktree add`. Each git call is
//! bounded at 300 s. Both are synchronous. Concurrent sessions of one repo cut
//! their worktrees from the same shared checkout, so an in-process lock per
//! checkout serialises them rather than letting them race on that checkout's
//! ref and worktree locks.
//!
//! Before plan
//! `2026-10-28-runner-spawn-bursts-overwhelm-the-box-beyond-the-continuation-cap`
//! (D2), the git work ran inline on the async worker while that lock was held.
//! A burst of isolated continuations for one repo then held a worker for every
//! fetch in turn. Now the git work runs on the blocking pool. The lock is still
//! a `tokio::sync::Mutex`, so the sessions waiting for it park instead of
//! holding workers.
//!
//! The lock guard moves INTO the blocking task. It is therefore held for
//! exactly as long as the git work runs, even when the awaiting future is
//! dropped. A blocking task cannot be cancelled, and releasing the lock when
//! the future is dropped would let the next materialisation of the same
//! checkout start while the first one's `git worktree add` is still running.

use std::path::Path;
use std::sync::Arc;

use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

use super::{
    materialize_one_worktree, worktree_resource_key, AllocateError, MaterializedWorktree,
    PlannedWorktreeRow, TargetMode,
};

/// The in-process lock that serialises worktree materialisation against one
/// shared checkout. Keyed by [`worktree_resource_key`], so two spellings of one
/// path share a lock.
fn checkout_materialize_lock(canonical: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: once_cell::sync::Lazy<
        std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    > = once_cell::sync::Lazy::new(Default::default);
    let mut locks = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    locks
        .entry(worktree_resource_key(canonical))
        .or_default()
        .clone()
}

/// Materialise one planned row (locality fetch, `git worktree add`, push ref)
/// on the blocking pool, serialised against every other materialisation of the
/// same checkout. This is what the `worktree` arm of
/// [`super::allocate_and_materialize_with_claim`] calls for each row.
pub(super) async fn materialize_row(
    row: PlannedWorktreeRow,
    target_mode: TargetMode,
) -> Result<MaterializedWorktree, AllocateError> {
    let canonical = row.canonical.clone();
    materialize_serialized(&canonical, move || {
        materialize_one_worktree(row, target_mode)
    })
    .await
}

/// Take `canonical`'s materialise lock, then run `work` on the blocking pool
/// with the guard moved into it. See the module doc for why the guard travels
/// with the work.
///
/// A `work` that does not complete (it panicked, or the runtime is shutting
/// down) is an [`AllocateError::Other`] naming the checkout. The caller's
/// existing `Err` arm then releases the claims it holds. In that case it is
/// UNKNOWN how far the git work got, and the message says so rather than
/// claiming that nothing was created.
async fn materialize_serialized<R, F>(canonical: &Path, work: F) -> Result<R, AllocateError>
where
    F: FnOnce() -> Result<R, AllocateError> + Send + 'static,
    R: Send + 'static,
{
    let serialized = checkout_materialize_lock(canonical).lock_owned().await;
    spawn_blocking_tracked(move || {
        let _serialized = serialized;
        work()
    })
    .await
    .unwrap_or_else(|e| {
        Err(AllocateError::Other(format!(
            "worktree materialisation from {} did not complete (how far its git work got is \
             unknown): {e}",
            canonical.display()
        )))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn_off_worker::test_support::{timer_delay_while_blocked, MAX_EXTRA_DELAY};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// A checkout path no other test (and no other run of this one) shares, so
    /// the process-global lock table cannot couple tests.
    fn unique_checkout(tag: &str) -> PathBuf {
        PathBuf::from(format!(
            "D:/materialize-serial-test/{tag}-{}",
            uuid::Uuid::new_v4()
        ))
    }

    /// Marks one materialisation in flight for as long as it lives, and records
    /// the most that were ever in flight at once.
    struct InFlight {
        now: Arc<AtomicUsize>,
    }

    impl InFlight {
        fn enter(now: &Arc<AtomicUsize>, max: &Arc<AtomicUsize>) -> Self {
            let n = now.fetch_add(1, Ordering::SeqCst) + 1;
            max.fetch_max(n, Ordering::SeqCst);
            Self { now: now.clone() }
        }
    }

    impl Drop for InFlight {
        fn drop(&mut self) {
            self.now.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// D2: a materialisation burst must not hold the async workers. Eight
    /// materialisations of eight different checkouts each sleep 2 s, standing
    /// in for a `git fetch` + `git worktree add`, on a 2-worker runtime. An
    /// unrelated 50 ms timer must still finish within 50 + 500 ms.
    ///
    /// The checkouts are distinct on purpose. Eight materialisations of ONE
    /// checkout would queue on its lock, and a queued waiter parks rather than
    /// holding a worker. Only one worker would then be blocked, and this test
    /// could not fail even with the git work inline.
    ///
    /// Mutation check: call `work()` directly in `materialize_serialized`
    /// instead of through `spawn_blocking_tracked`. Then both workers stay
    /// blocked and the timer is about 2 s late, so this test fails.
    #[test]
    fn materialize_off_worker_does_not_delay_the_async_workers() {
        let (extra, outs) = timer_delay_while_blocked(8, |i, entered| async move {
            let checkout = unique_checkout(&format!("burst-{i}"));
            materialize_serialized(&checkout, move || {
                entered.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_secs(2));
                Ok::<_, AllocateError>(format!("worktree-{i}"))
            })
            .await
            .map_err(|e| e.to_string())
        });
        assert!(
            extra <= MAX_EXTRA_DELAY,
            "eight 2 s materialisations delayed a 50 ms timer by {extra:?} (bound 500ms) — \
             the git work ran on the async workers"
        );
        let expected: Vec<Result<String, String>> =
            (0..8).map(|i| Ok(format!("worktree-{i}"))).collect();
        assert_eq!(outs, expected);
    }

    /// Repetition: the per-checkout lock still serialises. Two materialisations
    /// of ONE checkout never overlap, so together they take at least 2 x 300 ms.
    /// Two of DIFFERENT checkouts do overlap, which shows the measurement can
    /// see an overlap.
    ///
    /// Mutation check: take no lock in `materialize_serialized`. Then the
    /// same-checkout pair reaches 2 in flight, and this test fails.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_checkout_lock_still_serialises_two_materialisations_of_one_checkout() {
        async fn pair(a: PathBuf, b: PathBuf) -> (usize, Duration) {
            let (now, max) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
            let run = |checkout: PathBuf| {
                let (now, max) = (now.clone(), max.clone());
                async move {
                    materialize_serialized(&checkout, move || {
                        let _in_flight = InFlight::enter(&now, &max);
                        std::thread::sleep(Duration::from_millis(300));
                        Ok::<_, AllocateError>(())
                    })
                    .await
                }
            };
            let started = Instant::now();
            let (ra, rb) = tokio::join!(run(a), run(b));
            assert!(ra.is_ok() && rb.is_ok());
            (max.load(Ordering::SeqCst), started.elapsed())
        }

        let one = unique_checkout("same");
        // A second spelling of the same checkout: `worktree_resource_key`
        // folds the case and the trailing slash, so it shares the lock.
        let one_again = PathBuf::from(format!("{}/", one.display()).to_uppercase());
        let (same_max, same_elapsed) = pair(one, one_again).await;
        assert_eq!(
            same_max, 1,
            "two materialisations of one checkout overlapped"
        );
        assert!(
            same_elapsed >= Duration::from_millis(600),
            "serialised pair finished in {same_elapsed:?}, under 2 x 300ms"
        );

        let (diff_max, _) = pair(unique_checkout("a"), unique_checkout("b")).await;
        assert_eq!(
            diff_max, 2,
            "two different checkouts should materialise side by side"
        );
    }

    /// The guard travels with the git work. If the awaiting future is dropped
    /// while that work runs, the next materialisation of the same checkout
    /// must still wait for the work to end.
    ///
    /// Mutation check: hold the guard in `materialize_serialized`'s own frame
    /// instead of moving it into the closure. Then aborting the first task
    /// releases the lock early, the second task's work starts while the
    /// first's is still in flight, and this test fails.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dropped_materialisation_keeps_its_checkout_locked_until_its_git_work_ends() {
        let checkout = unique_checkout("dropped");
        let (now, max) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let entered = Arc::new(AtomicUsize::new(0));

        let first = {
            let (checkout, now, max, entered) =
                (checkout.clone(), now.clone(), max.clone(), entered.clone());
            tokio::spawn(async move {
                materialize_serialized(&checkout, move || {
                    let _in_flight = InFlight::enter(&now, &max);
                    entered.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(600));
                    Ok::<_, AllocateError>(())
                })
                .await
            })
        };
        while entered.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        first.abort();
        let _ = first.await;

        let second = materialize_serialized(&checkout, {
            let (now, max) = (now.clone(), max.clone());
            move || {
                let _in_flight = InFlight::enter(&now, &max);
                Ok::<_, AllocateError>(())
            }
        })
        .await;
        assert!(second.is_ok());
        assert_eq!(
            max.load(Ordering::SeqCst),
            1,
            "the second materialisation ran while the dropped one's git work was still in flight"
        );
    }

    /// A materialisation that panics comes back as an `Err` naming the
    /// checkout. It is not a panic in the allocate task, which would skip the
    /// claim release.
    #[tokio::test]
    async fn a_materialisation_that_panics_is_an_error_naming_its_checkout() {
        let checkout = unique_checkout("panics");
        let out: Result<(), AllocateError> =
            materialize_serialized(&checkout, || panic!("stand-in git panic")).await;
        let msg = match out {
            Err(AllocateError::Other(msg)) => msg,
            other => panic!("expected AllocateError::Other, got {other:?}"),
        };
        assert!(
            msg.starts_with(&format!(
                "worktree materialisation from {} did not complete",
                checkout.display()
            )),
            "{msg}"
        );
    }

    /// The pin on the production call sites: the allocate reaches git only
    /// through a blocking-pool seam, in both of its arms. The tests above drive
    /// the seam directly, so without this an inline call at the site would leave
    /// them all green.
    #[test]
    fn the_allocate_runs_its_git_work_off_the_async_workers() {
        use crate::spawn_off_worker::test_support::{
            calls_inside_and_outside, count_calls, fn_body,
        };
        let body = fn_body(
            include_str!("mod.rs"),
            "allocate_and_materialize_with_claim",
        )
        .expect("no fn allocate_and_materialize_with_claim");
        assert_eq!(
            count_calls(&body, "materialize_one_worktree"),
            0,
            "the allocate calls materialize_one_worktree( on the async worker"
        );
        assert!(
            count_calls(&body, "materialize_row") >= 1,
            "the allocate no longer materialises through materialize_row"
        );
        assert_eq!(
            calls_inside_and_outside(&body, "spawn_blocking_tracked", "checkout_shared_branches"),
            (1, 0),
            "the shared-branch checkout must run inside spawn_blocking_tracked"
        );
    }

    /// The real row materialiser, entered through the real seam, on a 2-worker
    /// runtime: a `git worktree add` from a scratch checkout lands at the
    /// planned target on the planned branch. The latency and lock tests above
    /// use stand-ins; this one shows that the production call runs correctly
    /// on a blocking-pool thread.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn materialize_row_cuts_the_planned_worktree_off_the_async_workers() {
        use std::process::Command;
        let src = tempfile::tempdir().unwrap();
        let git = |dir: &Path, args: &[&str]| -> String {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(src.path(), &["init", "-q"]);
        git(src.path(), &["config", "user.email", "t@example.com"]);
        git(src.path(), &["config", "user.name", "t"]);
        std::fs::write(src.path().join("f"), "x").unwrap();
        git(src.path(), &["add", "f"]);
        git(src.path(), &["commit", "-q", "-m", "init"]);
        let sha = git(src.path(), &["rev-parse", "HEAD"]);

        let out = tempfile::tempdir().unwrap();
        let target = out.path().join("agent-x").join("repo-x");
        let row = PlannedWorktreeRow {
            row: super::super::CoordAllocatedWorktree {
                repo: "repo-x".to_string(),
                branch: "agent/phase-3-off-worker".to_string(),
                parent_sha: sha.clone(),
                worktree_path: String::new(),
                status: "allocated".to_string(),
                push_ref: String::new(),
                origin: Some("requested".to_string()),
                sibling_store_sha: None,
                parent_sha_provenance: super::super::ParentShaProvenance::default(),
            },
            canonical: src.path().to_path_buf(),
            target: target.clone(),
        };

        let materialized = materialize_row(row, TargetMode::Junctioned)
            .await
            .expect("materialise");
        assert_eq!(materialized.repo, "repo-x");
        assert_eq!(materialized.branch, "agent/phase-3-off-worker");
        assert_eq!(materialized.worktree_path, target);
        assert_eq!(git(&target, &["rev-parse", "HEAD"]), sha);
        assert_eq!(
            git(&target, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "agent/phase-3-off-worker"
        );
    }
}
