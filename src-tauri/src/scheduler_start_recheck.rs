//! The tick's last check before it starts a due task: is the row it read
//! still the row?
//!
//! Plan `2026-11-27-devops-runner-scheduler-relaunches-a-cron-task-from-a-stale-tick-snapshot`.
//! A tick reads every task once, then walks the due ones, and the walk awaits
//! for seconds between that snapshot and a later candidate's start (an inline
//! probe grace, a history read, a sync task run to completion). Anything that
//! changes the row in that window — a run that finalized and wrote the next
//! `next_run`, a "Run now", an edit, a disable — is invisible to the stale
//! copy. On 2026-11-27 `dev-ops-steward` started a second time for its 05:27Z
//! slot, 6 s after the 05:27 run ended, from such a copy.
//!
//! # Why the re-read happens under the mark
//!
//! Every site that releases a `running_tasks` mark after a run ATTEMPTS the
//! `next_run` write first. So once [`SchedulerService::try_mark_running`]
//! succeeds, any earlier run's `next_run` write has already been attempted
//! (and, unless it failed, has landed), and no other starter
//! can begin the task while the mark is held. A re-read taken after the mark
//! therefore sees every change the snapshot missed; one taken before it does
//! not. The residue: a finalize whose `next_run` write FAILED (PG down)
//! leaves the served slot in the row, which this check reads as unchanged.
//!
//! # What is compared
//!
//! The SLOT (`next_run`, as a parsed instant), not dueness: a `Condition`
//! task's finalize sets `next_run` to the finalize instant, which reads as due
//! at once. Every schedule kind moves `next_run` at finalize (to `None` for
//! `Once`). `modified_at` defers an edit to the next tick, as the condition
//! Fire branch already does. An unreadable row does not start the task:
//! UNKNOWN is not "due", and the next tick is 60 s away.

use crate::scheduler::ScheduledTask;
use crate::scheduler_service::SchedulerService;
use std::future::Future;
use tracing::info;

/// What the fresh row says about starting the task the tick read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SlotRecheck {
    /// The row is still the one the tick read: start it.
    Start,
    /// `next_run` moved: the slot was served (or rescheduled) since the read.
    Consumed {
        read: Option<String>,
        now: Option<String>,
    },
    /// `modified_at` moved: the task was edited since the read.
    Edited,
    /// The task is disabled now.
    Disabled,
    /// The task no longer exists.
    Gone,
    /// The row could not be re-read.
    Unreadable(String),
}

impl SlotRecheck {
    /// The clause for `not starting task '<name>' — <reason> since this tick
    /// read it`. `Disabled` / `Gone` share `relaunch_hold`'s wording.
    pub(crate) fn reason(&self) -> String {
        match self {
            SlotRecheck::Start => "nothing changed".to_string(),
            SlotRecheck::Consumed { read, now } => format!(
                "its slot was consumed: next_run is {} (it was {})",
                now.as_deref().unwrap_or("unset"),
                read.as_deref().unwrap_or("unset")
            ),
            SlotRecheck::Edited => "the task was edited".to_string(),
            SlotRecheck::Disabled => "the task was disabled".to_string(),
            SlotRecheck::Gone => "the task was deleted".to_string(),
            SlotRecheck::Unreadable(e) => format!("its row could not be re-read ({e})"),
        }
    }
}

/// Two RFC 3339 timestamps name the same instant. Text that does not parse
/// is compared as text, so it is equal only to itself.
fn same_instant(a: &str, b: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn same_optional_instant(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => same_instant(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// Decide whether the tick may start `snapshot`, given the row re-read under
/// its mark. Pure.
pub(crate) fn recheck_slot(
    snapshot: &ScheduledTask,
    fresh: Result<Option<ScheduledTask>, String>,
) -> SlotRecheck {
    let fresh = match fresh {
        Err(e) => return SlotRecheck::Unreadable(e),
        Ok(None) => return SlotRecheck::Gone,
        Ok(Some(fresh)) => fresh,
    };
    if !fresh.enabled {
        return SlotRecheck::Disabled;
    }
    if !same_instant(&snapshot.modified_at, &fresh.modified_at) {
        return SlotRecheck::Edited;
    }
    if !same_optional_instant(snapshot.next_run.as_deref(), fresh.next_run.as_deref()) {
        return SlotRecheck::Consumed {
            read: snapshot.next_run.clone(),
            now: fresh.next_run,
        };
    }
    SlotRecheck::Start
}

impl SchedulerService {
    /// Mark `snapshot` running, then re-read its row (`reread`) and decide
    /// with [`recheck_slot`].
    ///
    /// - `None`: the mark was refused (the task is already running); nothing
    ///   was read and the existing mark is left alone.
    /// - `Some(Start)`: the mark is HELD; the caller must run the task (which
    ///   releases it).
    /// - any other verdict: the mark was released and the reason logged.
    pub(crate) async fn mark_and_recheck_slot<R, F>(
        &self,
        snapshot: &ScheduledTask,
        reread: R,
    ) -> Option<SlotRecheck>
    where
        R: FnOnce() -> F,
        F: Future<Output = Result<Option<ScheduledTask>, String>>,
    {
        if !self.try_mark_running(&snapshot.id).await {
            return None;
        }
        let verdict = recheck_slot(snapshot, reread().await);
        if verdict != SlotRecheck::Start {
            self.release_unstarted_mark(&snapshot.id).await;
            info!(
                "Scheduler: not starting task '{}' — {} since this tick read it",
                snapshot.name,
                verdict.reason()
            );
        }
        Some(verdict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::{
        schedule_expression_default, scheduled_task_type_default, ScheduledTaskExt,
    };
    use crate::scheduler_service::start_due_tasks;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const STEWARD_ID: &str = "f1505e22-3cdd-4a91-901a-2d0573cf4e76";
    const MODIFIED: &str = "2026-11-23T13:09:27.430182+00:00";

    fn task(id: &str, name: &str, next_run: Option<&str>, modified_at: &str) -> ScheduledTask {
        let mut t = ScheduledTask::new(
            name.to_string(),
            None,
            schedule_expression_default(),
            scheduled_task_type_default(),
        );
        t.id = id.to_string();
        t.enabled = true;
        t.next_run = next_run.map(str::to_string);
        t.modified_at = modified_at.to_string();
        t
    }

    fn steward(next_run: &str) -> ScheduledTask {
        task(STEWARD_ID, "dev-ops-steward", Some(next_run), MODIFIED)
    }

    /// Run `snapshot` through the helper against `fresh`. The re-read
    /// closure records whether the mark was held when it was called (and
    /// asserts it). Returns (verdict, mark held at re-read, mark held after).
    async fn through_helper(
        snapshot: &ScheduledTask,
        fresh: Result<Option<ScheduledTask>, String>,
    ) -> (Option<SlotRecheck>, bool, bool) {
        let svc = SchedulerService::new(None);
        let held_at_read = AtomicBool::new(false);
        let verdict = svc
            .mark_and_recheck_slot(snapshot, || async {
                let held = svc.is_task_running(&snapshot.id).await;
                held_at_read.store(held, Ordering::SeqCst);
                assert!(held, "the row must be re-read only while holding the mark");
                fresh
            })
            .await;
        let held_after = svc.is_task_running(&snapshot.id).await;
        (verdict, held_at_read.load(Ordering::SeqCst), held_after)
    }

    // 1
    #[tokio::test]
    async fn a_slot_finalized_after_the_snapshot_is_not_started_again() {
        let snapshot = steward("2026-11-27T05:27:00+00:00");
        let fresh = steward("2026-11-27T06:27:00+00:00");
        let (verdict, _, held_after) = through_helper(&snapshot, Ok(Some(fresh))).await;
        assert_eq!(
            verdict,
            Some(SlotRecheck::Consumed {
                read: Some("2026-11-27T05:27:00+00:00".to_string()),
                now: Some("2026-11-27T06:27:00+00:00".to_string()),
            })
        );
        assert!(!held_after, "a refused start releases the mark");
    }

    // 2 — positive control for 1.
    #[tokio::test]
    async fn an_unchanged_slot_starts() {
        let snapshot = steward("2026-11-27T05:27:00+00:00");
        let fresh = steward("2026-11-27T05:27:00+00:00");
        let (verdict, _, held_after) = through_helper(&snapshot, Ok(Some(fresh))).await;
        assert_eq!(verdict, Some(SlotRecheck::Start));
        assert!(held_after, "a start keeps the mark for the run to release");
    }

    // 3
    #[tokio::test]
    async fn a_row_edited_disabled_or_deleted_since_the_snapshot_is_not_started() {
        let snapshot = steward("2026-11-27T05:27:00+00:00");

        let mut edited = steward("2026-11-27T05:27:00+00:00");
        edited.modified_at = "2026-11-27T05:37:00+00:00".to_string();
        let (verdict, _, held_after) = through_helper(&snapshot, Ok(Some(edited))).await;
        assert_eq!(verdict, Some(SlotRecheck::Edited));
        assert!(!held_after);

        let mut disabled = steward("2026-11-27T05:27:00+00:00");
        disabled.enabled = false;
        let (verdict, _, held_after) = through_helper(&snapshot, Ok(Some(disabled))).await;
        assert_eq!(verdict, Some(SlotRecheck::Disabled));
        assert!(!held_after);

        let (verdict, _, held_after) = through_helper(&snapshot, Ok(None)).await;
        assert_eq!(verdict, Some(SlotRecheck::Gone));
        assert!(!held_after);

        assert_eq!(SlotRecheck::Disabled.reason(), "the task was disabled");
        assert_eq!(SlotRecheck::Gone.reason(), "the task was deleted");
    }

    // 4
    #[tokio::test]
    async fn an_unreadable_row_does_not_start() {
        let snapshot = steward("2026-11-27T05:27:00+00:00");
        let (verdict, _, held_after) =
            through_helper(&snapshot, Err("PG pool error: timed out".to_string())).await;
        assert_eq!(
            verdict,
            Some(SlotRecheck::Unreadable(
                "PG pool error: timed out".to_string()
            ))
        );
        assert!(!held_after, "the mark is released");
    }

    // 5
    #[tokio::test]
    async fn next_run_is_compared_as_an_instant() {
        let snapshot = steward("2026-11-27T05:27:00Z");
        let fresh = steward("2026-11-27T05:27:00+00:00");
        let (verdict, _, _) = through_helper(&snapshot, Ok(Some(fresh))).await;
        assert_eq!(verdict, Some(SlotRecheck::Start));
    }

    /// The 2026-11-27 05:37 tick: `return-to-main` (a waiter, first in the
    /// walk) holds the tick while the 05:27 run finalizes; the walk then
    /// reaches the stale `dev-ops-steward` copy.
    async fn replay_incident() -> (usize, Vec<String>, bool) {
        let svc = Arc::new(SchedulerService::new(None));
        // The 05:27 run holds the mark.
        assert!(svc.try_mark_running(STEWARD_ID).await);
        let store = Arc::new(Mutex::new(Some(steward("2026-11-27T05:27:00+00:00"))));
        let launches = Arc::new(AtomicUsize::new(0));
        let all_reads_held = Arc::new(AtomicBool::new(true));

        let waiter = task(
            "a6f0bf11-0000-0000-0000-000000000000",
            "return-to-main",
            Some("2026-11-27T05:20:00+00:00"),
            MODIFIED,
        );
        let snapshot = steward("2026-11-27T05:27:00+00:00");

        start_due_tasks(vec![waiter, snapshot], 2, |candidate: ScheduledTask| {
            let svc = svc.clone();
            let store = store.clone();
            let launches = launches.clone();
            let all_reads_held = all_reads_held.clone();
            async move {
                if candidate.name == "return-to-main" {
                    // While it waits on its probe, the 05:27 run finalizes:
                    // next_run first, then the mark.
                    *store.lock().unwrap() = Some(steward("2026-11-27T06:27:00+00:00"));
                    svc.release_unstarted_mark(STEWARD_ID).await;
                    return false;
                }
                let id = candidate.id.clone();
                let verdict = svc
                    .mark_and_recheck_slot(&candidate, || async {
                        let held = svc.is_task_running(&id).await;
                        all_reads_held.fetch_and(held, Ordering::SeqCst);
                        assert!(held, "the row must be re-read only while holding the mark");
                        Ok(store.lock().unwrap().clone())
                    })
                    .await;
                if verdict != Some(SlotRecheck::Start) {
                    return false;
                }
                launches.fetch_add(1, Ordering::SeqCst);
                true
            }
        })
        .await;

        (
            launches.load(Ordering::SeqCst),
            svc.get_running_tasks().await,
            all_reads_held.load(Ordering::SeqCst),
        )
    }

    // 6
    #[tokio::test]
    async fn the_incident_replays_without_a_second_start() {
        let (launches, running, _) = replay_incident().await;
        assert_eq!(launches, 0, "the stale copy must not launch");
        assert!(running.is_empty(), "no mark is left behind");
    }

    // 6, relaunch-shaped: an account-limit relaunch loops inside the settle
    // while HOLDING the original mark, so a tick candidate is refused at the
    // mark, reads nothing, and leaves that mark alone.
    #[tokio::test]
    async fn a_task_still_marked_is_refused_at_the_mark_and_keeps_it() {
        let svc = SchedulerService::new(None);
        assert!(svc.try_mark_running(STEWARD_ID).await);
        let reads = AtomicUsize::new(0);
        let verdict = svc
            .mark_and_recheck_slot(&steward("2026-11-27T01:27:00+00:00"), || async {
                reads.fetch_add(1, Ordering::SeqCst);
                Ok(Some(steward("2026-11-27T01:27:00+00:00")))
            })
            .await;
        assert_eq!(verdict, None);
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert_eq!(svc.get_running_tasks().await, vec![STEWARD_ID.to_string()]);
    }

    // 8 — the behaviour check for "re-read after the mark": every re-read in
    // tests 1-6 found the mark held.
    #[tokio::test]
    async fn the_slot_recheck_reads_the_row_only_while_holding_the_mark() {
        let snapshot = steward("2026-11-27T05:27:00+00:00");
        let mut edited = steward("2026-11-27T05:27:00+00:00");
        edited.modified_at = "2026-11-27T05:37:00+00:00".to_string();
        let cases: Vec<Result<Option<ScheduledTask>, String>> = vec![
            Ok(Some(steward("2026-11-27T05:27:00+00:00"))),
            Ok(Some(steward("2026-11-27T06:27:00+00:00"))),
            Ok(Some(edited)),
            Ok(None),
            Err("PG pool error: timed out".to_string()),
        ];
        for fresh in cases {
            let (verdict, held_at_read, _) = through_helper(&snapshot, fresh).await;
            assert!(verdict.is_some());
            assert!(held_at_read);
        }
        let (_, _, all_reads_held) = replay_incident().await;
        assert!(all_reads_held);
    }
}
