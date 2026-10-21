//! Phase 6 — the backend restore of a prepared cohort (plan
//! `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, D4–D7).
//!
//! A runner rebuilt over a prepare record moves it to `restoring` (the boot
//! step in [`super::prepare`]). From then until the cohort is dismissed, the
//! cohort is restored by hand, one row at a time, through the backend resume
//! core (`session::resume_door`) — never auto-resumed by the boot restore.
//! This module is what that core and the boot side read:
//!
//! * [`RestoringCohort`] — the rows of a `restoring` record, each with the
//!   effort it is resumed at. The resume core admits an `open` row with no live
//!   terminal only when it is one of these (D7), and passes
//!   [`RestoreTarget::effort`] as `--effort <level>` (D5).
//! * [`effort_to_apply`] — the re-apply rule: the level the session RAN at,
//!   when it was read and differs from the account's baseline.
//! * [`read_back_effort`] — after a verified resume, read the pane's status
//!   row and say whether the effort came back: [`EFFORT_RESTORED`] or
//!   `effort_not_restored(<reason>)`. Never a guess: an indicator that was not
//!   seen is a failure to confirm, not a success.
//! * [`note_restore_on`] — the per-row restore verdict, recorded on the cohort
//!   row itself so the panel and the placeholders read one record.
//! * [`split_placeholders`] — the boot side: while a cohort is `restoring`,
//!   its rows are returned to the webview as placeholders, NOT in the
//!   auto-restore set (D6). A crash boot with no record is unchanged.
//! * [`unfinish_for_restore`] — Restore on a finished cohort row first lifts
//!   the finish mark, because the resume core refuses finished rows (D4).
//!
//! The withheld-boot-restore shield's `in_active_cohort` arm
//! (`session_lifecycle_store::withheld_boot_restore_candidate`) reads
//! [`restoring_cohort_ids`] once per lifecycle poll tick, so a placeholder is
//! never orphan-closed while it waits for its Restore click.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::warn;

use super::cohort::CohortRow;
use super::effort::{self, EffortSource};
use super::prepare::RestartPrep;
use super::{rfc3339, PrepState, PrepareRecord};
use crate::session::session_lifecycle_store::SessionLifecycleStore;
use crate::terminal::graceful_exit::ScreenText;

/// The read-back verdict when the pane's status row shows the re-applied level.
pub const EFFORT_RESTORED: &str = "effort_restored";

/// How long the read-back waits for the resumed CLI to paint its status row.
pub const EFFORT_READ_BACK_DEADLINE: Duration = Duration::from_secs(30);
/// How often the read-back looks.
pub const EFFORT_READ_BACK_POLL: Duration = Duration::from_millis(1000);

/// `effort_not_restored(<reason>)`.
pub fn effort_not_restored(reason: &str) -> String {
    format!("effort_not_restored({reason})")
}

/// One restore of a cohort row, as the resume core answered it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowRestore {
    pub at_ms: i64,
    pub at: String,
    /// The resume core's verdict, verbatim: `resumed`, `failed(<reason>)` or
    /// `skipped(<reason>)` — `skipped(resource_guard:<lane>)` when the box was
    /// too busy to spawn, which is a retry, not a failure.
    pub verdict: String,
    /// The level passed as `--effort`, when one was.
    pub effort_applied: Option<String>,
    /// `effort_restored` | `effort_not_restored(<reason>)`; `None` when no
    /// effort was re-applied or the resume did not land.
    pub effort: Option<String>,
}

impl RowRestore {
    /// The resume landed (verified by a process read).
    pub fn is_resumed(&self) -> bool {
        self.verdict == "resumed"
    }
}

/// Whether `row` was restored by this cohort's restore already.
fn restored(row: &CohortRow) -> bool {
    row.restore.as_ref().is_some_and(RowRestore::is_resumed)
}

/// What one cohort row is resumed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreTarget {
    /// The level to pass as `--effort`, or `None` to let the account's own
    /// default apply ([`effort_to_apply`]).
    pub effort: Option<String>,
}

/// The level a row is resumed at: the effort it RAN at, when that was read
/// (off its pane or its account's `settings.json`) and differs from the
/// baseline a fresh session on that account gets anyway. `None` — no flag —
/// when the effort was not read (never a guessed level) or equals the
/// baseline. A baseline that is itself unknown cannot be equal, so a read
/// effort is re-applied rather than dropped.
///
/// Only a canonical level ever comes out ([`effort::rank`]): the value lands
/// in an argv, and a record edited on disk must not be able to put anything
/// else there.
pub fn effort_to_apply(row: &CohortRow) -> Option<String> {
    if row.effort.source == EffortSource::Unknown {
        return None;
    }
    let level = row.effort.level.as_deref()?;
    let canonical = effort::EFFORT_LEVELS.get(effort::rank(level)?)?;
    let same_as_baseline = row
        .baseline
        .baseline
        .as_deref()
        .and_then(effort::rank)
        .is_some_and(|b| effort::EFFORT_LEVELS.get(b) == Some(canonical));
    (!same_as_baseline).then(|| (*canonical).to_string())
}

/// The rows of a `restoring` record still waiting to be restored, keyed by
/// Claude session id. A row whose restore already landed is no longer one of
/// them: it is a live session again, and a later crash must auto-restore it
/// like any other rather than turn it back into a placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoringCohort {
    pub prepare_id: String,
    targets: BTreeMap<String, RestoreTarget>,
}

impl RestoringCohort {
    /// The restoring cohort of `rec`; `None` unless `rec` is `restoring`. A
    /// prepare still in its window (the runner was not rebuilt), a cancelled
    /// one and a finished one have no cohort to restore.
    pub fn of(rec: &PrepareRecord) -> Option<Self> {
        (rec.state == PrepState::Restoring).then(|| Self {
            prepare_id: rec.prepare_id.clone(),
            targets: rec
                .cohort
                .rows
                .iter()
                .filter(|row| !restored(row))
                .map(|row| {
                    (
                        row.session.claude_session_id.clone(),
                        RestoreTarget {
                            effort: effort_to_apply(row),
                        },
                    )
                })
                .collect(),
        })
    }

    pub fn contains(&self, claude_session_id: &str) -> bool {
        self.targets.contains_key(claude_session_id)
    }

    pub fn target(&self, claude_session_id: &str) -> Option<&RestoreTarget> {
        self.targets.get(claude_session_id)
    }

    pub fn ids(&self) -> HashSet<String> {
        self.targets.keys().cloned().collect()
    }
}

/// The live record's restoring cohort. `None` with no controller (every unit
/// test), no record, or a record that is not `restoring` — the crash-boot and
/// never-prepared paths, which are unchanged.
pub fn restoring_cohort() -> Option<RestoringCohort> {
    let rec = super::prepare::install_live()?.record()?;
    RestoringCohort::of(&rec)
}

/// The ids of [`restoring_cohort`], empty when there is none. Read once per
/// lifecycle poll tick by the withheld-boot-restore shield.
pub fn restoring_cohort_ids() -> HashSet<String> {
    restoring_cohort().map(|c| c.ids()).unwrap_or_default()
}

/// Record a restore verdict on the cohort row `claude_session_id` of prepare
/// `prepare_id`, while that record is `restoring`. `Ok(false)` when the record
/// is no longer restoring or carries no such row (nothing written). Once every
/// row is restored the record moves to `done`; a row that is never restored
/// (held, Finished) keeps it `restoring` until the cohort is dismissed.
pub fn note_restore_on(
    ctl: &RestartPrep,
    prepare_id: &str,
    claude_session_id: &str,
    restore: RowRestore,
) -> Result<bool, String> {
    let applies = ctl.record().is_some_and(|r| {
        r.prepare_id == prepare_id
            && r.state == PrepState::Restoring
            && r.cohort
                .rows
                .iter()
                .any(|row| row.session.claude_session_id == claude_session_id)
    });
    if !applies {
        return Ok(false);
    }
    ctl.update(prepare_id, |r| {
        if r.state != PrepState::Restoring {
            return;
        }
        if let Some(row) = r
            .cohort
            .rows
            .iter_mut()
            .find(|row| row.session.claude_session_id == claude_session_id)
        {
            row.restore = Some(restore);
        }
        if r.cohort.rows.iter().all(restored) {
            r.state = PrepState::Done;
        }
    })
    .map(|_| true)
}

/// [`note_restore_on`] on the live controller. A failed write is logged: the
/// verdict is also in the resume core's reply and its log line.
pub fn note_restore(
    prepare_id: &str,
    claude_session_id: &str,
    verdict: &str,
    effort_applied: Option<String>,
    effort: Option<String>,
) {
    let Some(ctl) = super::prepare::install_live() else {
        return;
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    let restore = RowRestore {
        at_ms: now_ms,
        at: rfc3339(now_ms),
        verdict: verdict.to_string(),
        effort_applied,
        effort,
    };
    if let Err(e) = note_restore_on(ctl, prepare_id, claude_session_id, restore) {
        warn!(
            session = claude_session_id,
            error = %e,
            "restart_prep: restore verdict not recorded on the cohort row"
        );
    }
}

/// PURE: what one read of the pane says about the re-applied `wanted` level.
/// `Ok(())` only when the status row shows exactly `wanted`; `Err(None)` when
/// no indicator is on screen yet (keep looking); `Err(Some(seen))` when it
/// shows another level.
pub fn judge_effort(wanted: &str, screen: &ScreenText) -> Result<(), Option<&'static str>> {
    match effort::parse_screen(&screen.lines) {
        Some(seen) if effort::rank(seen) == effort::rank(wanted) => Ok(()),
        Some(seen) => Err(Some(seen)),
        None => Err(None),
    }
}

/// Read the resumed pane's status row until it confirms `wanted` or
/// `deadline` passes. `read` returns the pane's grid, or `None` when the pane
/// is gone. The answer is [`EFFORT_RESTORED`] or
/// `effort_not_restored(<reason>)`, where the reason is `pane_vanished`,
/// `shows_<level>` (the last level seen, when it was another one), or
/// `indicator_not_seen`.
pub async fn read_back_effort(
    wanted: &str,
    mut read: impl FnMut() -> Option<ScreenText>,
    deadline: Duration,
    poll: Duration,
) -> String {
    let until = tokio::time::Instant::now() + deadline;
    let mut last_other: Option<&'static str> = None;
    loop {
        let Some(screen) = read() else {
            return effort_not_restored("pane_vanished");
        };
        match judge_effort(wanted, &screen) {
            Ok(()) => return EFFORT_RESTORED.to_string(),
            Err(Some(other)) => last_other = Some(other),
            Err(None) => {}
        }
        if tokio::time::Instant::now() >= until {
            return match last_other {
                Some(other) => effort_not_restored(&format!("shows_{other}")),
                None => effort_not_restored("indicator_not_seen"),
            };
        }
        tokio::time::sleep(poll).await;
    }
}

/// The boot side (D6): split the auto-restore set into the rows the webview
/// restores now and the cohort rows it shows as placeholders instead. With an
/// empty `cohort` — no record, or a record that is not `restoring` —
/// everything is restored exactly as before.
pub fn split_placeholders<T>(
    rows: Vec<T>,
    id_of: impl Fn(&T) -> &str,
    cohort: &HashSet<String>,
) -> (Vec<T>, Vec<T>) {
    rows.into_iter().partition(|r| !cohort.contains(id_of(r)))
}

/// Restore on a FINISHED cohort row lifts the finish mark first, because the
/// resume core refuses a finished row (D4). Only rows of the restoring
/// `cohort` are touched — an operator Restore of any other finished session
/// still reads `skipped(finished)`. Returns the ids un-finished.
pub fn unfinish_for_restore(
    store: &SessionLifecycleStore,
    ids: &[String],
    cohort: Option<&RestoringCohort>,
) -> Vec<String> {
    let Some(cohort) = cohort else {
        return Vec::new();
    };
    ids.iter()
        .filter(|id| cohort.contains(id))
        .filter(|id| store.get(id).is_some_and(|r| r.finished_at.is_some()))
        .filter_map(|id| match store.set_finished(id, false, None) {
            Ok(_) => Some(id.clone()),
            Err(e) => {
                warn!(
                    session = %id,
                    error = ?e,
                    "restart_prep: could not lift the finish mark before restore — the resume \
                     core will refuse the row as finished"
                );
                None
            }
        })
        .collect()
}

/// A `restoring` cohort over `ids` (prepare `prep-test`), each row captured at
/// `max` effort off its pane on the CLI-default `high` baseline — so each is
/// re-applied as `--effort max` — for the tests of the resume core and the
/// boot side.
#[cfg(test)]
pub(crate) fn restoring_cohort_for_test(ids: &[&str]) -> RestoringCohort {
    use super::effort::Effort;
    let ctl_dir = tempfile::tempdir().expect("tempdir");
    let mut rec = super::prepare::active_controller_for_test(ctl_dir.path(), ids, 0)
        .record()
        .expect("seeded record");
    for row in &mut rec.cohort.rows {
        row.effort = Effort {
            level: Some("max".to_string()),
            source: EffortSource::Screen,
        };
    }
    rec.state = PrepState::Restoring;
    RestoringCohort::of(&rec).expect("a restoring record has a cohort")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::restart_prep::cohort::{build_row, Cohort, RowFacts};
    use crate::restart_prep::effort::{Baseline, DefaultSource, Effort};
    use crate::restart_prep::prepare::active_controller_for_test;
    use crate::session::session_ledger::LedgerEntry;

    fn entry(id: &str) -> LedgerEntry {
        LedgerEntry {
            claude_session_id: id.to_string(),
            terminal_id: format!("term-{id}"),
            page_id: "page-a".to_string(),
            zone_index: 1,
            title: None,
            session_name: None,
            account_label: None,
            config_dir: None,
            working_dir: None,
            restorable: None,
            worktree_path: None,
            plan_slug: None,
            work_unit_id: None,
            wip_state: None,
            wip_ref: None,
            custody_session_mismatch: false,
        }
    }

    fn row_with(level: Option<&str>, source: EffortSource, baseline: Option<&str>) -> CohortRow {
        let mut row = build_row(entry("s"), RowFacts::default());
        row.effort = Effort {
            level: level.map(str::to_string),
            source,
        };
        row.baseline = Baseline {
            baseline: baseline.map(str::to_string),
            default_source: if baseline.is_some() {
                DefaultSource::CliDefault
            } else {
                DefaultSource::Unknown
            },
        };
        row
    }

    fn screen(lines: &[&str]) -> ScreenText {
        ScreenText {
            lines: lines.iter().map(|l| l.to_string()).collect(),
            cursor_row: u16::try_from(lines.len().saturating_sub(1)).unwrap_or(0),
            cursor_col: 2,
        }
    }

    #[test]
    fn effort_is_re_applied_only_when_read_and_different_from_the_baseline() {
        use EffortSource::*;
        let cases: &[(Option<&str>, EffortSource, Option<&str>, Option<&str>)] = &[
            // Above the baseline: re-applied.
            (Some("max"), Screen, Some("high"), Some("max")),
            // Below it too — the session comes back as it ran.
            (Some("low"), Screen, Some("high"), Some("low")),
            // Equal to the baseline: the account default brings it back.
            (Some("high"), Screen, Some("high"), None),
            (Some("HIGH"), Screen, Some("high"), None),
            // The account's own default, read from settings: equal by definition.
            (Some("medium"), AccountDefault, Some("medium"), None),
            // An unknown baseline cannot be equal: the read level is re-applied.
            (Some("xhigh"), Screen, None, Some("xhigh")),
            // An unread effort is never guessed.
            (None, Unknown, Some("high"), None),
            (Some("max"), Unknown, Some("high"), None),
            // Not a level at all: nothing reaches the argv.
            (
                Some("--dangerously-skip-permissions"),
                Screen,
                Some("high"),
                None,
            ),
        ];
        for (level, source, baseline, want) in cases {
            let row = row_with(*level, *source, *baseline);
            assert_eq!(
                effort_to_apply(&row).as_deref(),
                *want,
                "{level:?} from {source:?} over baseline {baseline:?}"
            );
        }
    }

    fn record(state: PrepState, ids: &[&str]) -> PrepareRecord {
        let rows = ids
            .iter()
            .map(|id| {
                let mut r = build_row(entry(id), RowFacts::default());
                r.effort = Effort {
                    level: Some("max".to_string()),
                    source: EffortSource::Screen,
                };
                r
            })
            .collect();
        let mut rec = PrepareRecord::new(
            "prep-r".to_string(),
            1_000,
            Cohort {
                captured_at_ms: 1_000,
                captured_at: String::new(),
                reason: "restart-prep".to_string(),
                rows,
            },
        );
        rec.state = state;
        rec
    }

    #[test]
    fn only_a_restoring_record_has_a_cohort_to_restore() {
        for state in [
            PrepState::Preparing,
            PrepState::Ready,
            PrepState::Done,
            PrepState::Cancelled,
        ] {
            assert_eq!(
                RestoringCohort::of(&record(state, &["a"])),
                None,
                "{state:?}"
            );
        }
        let c = RestoringCohort::of(&record(PrepState::Restoring, &["a", "b"])).unwrap();
        assert!(c.contains("a") && c.contains("b") && !c.contains("z"));
        assert_eq!(c.ids().len(), 2);
        assert_eq!(c.target("a").unwrap().effort.as_deref(), Some("max"));
        // Zero rows is a cohort with nothing in it, not an absent one.
        let empty = RestoringCohort::of(&record(PrepState::Restoring, &[])).unwrap();
        assert!(empty.ids().is_empty());
    }

    #[test]
    fn with_no_live_controller_there_is_no_restoring_cohort() {
        // The crash-boot / never-prepared path: a unit-test build installs no
        // controller, exactly like a runner that has no record.
        assert_eq!(restoring_cohort(), None);
        assert!(restoring_cohort_ids().is_empty());
    }

    #[test]
    fn judge_effort_reads_the_status_row() {
        let shows = |l: &str| screen(&["transcript", l, "────", "❯\u{a0}"]);
        assert_eq!(judge_effort("max", &shows("◉ max · /effort")), Ok(()));
        assert_eq!(
            judge_effort("max", &shows("● high · /effort")),
            Err(Some("high"))
        );
        assert_eq!(judge_effort("max", &shows("● max · /eff")), Err(None));
        assert_eq!(judge_effort("max", &screen(&["❯ "])), Err(None));
    }

    #[tokio::test]
    async fn the_read_back_confirms_the_level_once_it_is_painted() {
        let mut reads = vec![
            screen(&["starting…"]),
            screen(&["● high · /effort"]),
            screen(&["◉ max · /effort"]),
        ]
        .into_iter();
        let got = read_back_effort(
            "max",
            move || reads.next(),
            Duration::from_secs(5),
            Duration::from_millis(1),
        )
        .await;
        assert_eq!(got, EFFORT_RESTORED);
    }

    #[tokio::test]
    async fn the_read_back_names_why_it_could_not_confirm() {
        // Another level stays on screen until the deadline.
        let other = read_back_effort(
            "max",
            || Some(screen(&["● high · /effort"])),
            Duration::from_millis(20),
            Duration::from_millis(1),
        )
        .await;
        assert_eq!(other, "effort_not_restored(shows_high)");
        // No indicator ever painted.
        let unseen = read_back_effort(
            "max",
            || Some(screen(&["❯ "])),
            Duration::from_millis(20),
            Duration::from_millis(1),
        )
        .await;
        assert_eq!(unseen, "effort_not_restored(indicator_not_seen)");
        // The pane went away.
        let gone = read_back_effort(
            "max",
            || None,
            Duration::from_secs(5),
            Duration::from_millis(1),
        )
        .await;
        assert_eq!(gone, "effort_not_restored(pane_vanished)");
    }

    #[test]
    fn cohort_rows_become_placeholders_and_a_crash_boot_restores_as_before() {
        let rows = vec!["a", "b", "c"];
        // A crash boot / no record: nothing is a placeholder.
        let (restore, placeholders) = split_placeholders(rows.clone(), |r| r, &HashSet::new());
        assert_eq!(restore, ["a", "b", "c"]);
        assert!(placeholders.is_empty());
        // A restoring cohort holding b and c (and z, which is not open here).
        let cohort: HashSet<String> = ["b", "c", "z"].iter().map(|s| s.to_string()).collect();
        let (restore, placeholders) = split_placeholders(rows, |r| r, &cohort);
        assert_eq!(restore, ["a"]);
        assert_eq!(placeholders, ["b", "c"]);
    }

    #[test]
    fn a_restore_verdict_lands_on_the_row_only_while_restoring() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = active_controller_for_test(dir.path(), &["s1", "s2"], 1_000);
        let prepare_id = ctl.record().unwrap().prepare_id;
        let verdict = |v: &str| RowRestore {
            at_ms: 5,
            at: rfc3339(5),
            verdict: v.to_string(),
            effort_applied: Some("max".to_string()),
            effort: Some(EFFORT_RESTORED.to_string()),
        };
        // Still preparing: nothing is restored yet.
        assert_eq!(
            note_restore_on(&ctl, &prepare_id, "s1", verdict("resumed")),
            Ok(false)
        );
        ctl.update(&prepare_id, |r| r.state = PrepState::Restoring)
            .unwrap();
        assert_eq!(
            note_restore_on(&ctl, &prepare_id, "s1", verdict("resumed")),
            Ok(true)
        );
        assert_eq!(
            note_restore_on(&ctl, &prepare_id, "not-a-row", verdict("resumed")),
            Ok(false)
        );
        assert_eq!(
            note_restore_on(&ctl, "another-prepare", "s1", verdict("resumed")),
            Ok(false)
        );
        let rec = ctl.record().unwrap();
        assert_eq!(
            rec.cohort.rows[0]
                .restore
                .as_ref()
                .map(|r| r.verdict.as_str()),
            Some("resumed")
        );
        assert_eq!(rec.cohort.rows[1].restore, None);
        // Persisted: the record on disk carries it.
        let on_disk = crate::restart_prep::load_from(&dir.path().join("restart-prep.json"))
            .expect("record on disk");
        assert_eq!(on_disk.cohort.rows[0].restore, rec.cohort.rows[0].restore);
        // A restored row leaves the cohort still waiting; the other stays.
        let waiting = RestoringCohort::of(&rec).expect("still restoring");
        assert!(!waiting.contains("s1") && waiting.contains("s2"));
        // A skip is not a restore: the record keeps restoring.
        note_restore_on(
            &ctl,
            &prepare_id,
            "s2",
            verdict("skipped(resource_guard:thread_count)"),
        )
        .unwrap();
        assert_eq!(ctl.record().unwrap().state, PrepState::Restoring);
        assert!(RestoringCohort::of(&ctl.record().unwrap())
            .unwrap()
            .contains("s2"));
        // The last row restored: the cohort is done.
        note_restore_on(&ctl, &prepare_id, "s2", verdict("resumed")).unwrap();
        assert_eq!(ctl.record().unwrap().state, PrepState::Done);
        assert_eq!(RestoringCohort::of(&ctl.record().unwrap()), None);
    }

    #[test]
    fn a_finished_cohort_row_is_un_finished_and_then_admitted_and_nothing_else_is() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        for id in ["in-cohort", "outside"] {
            store.record_open(crate::session::session_lifecycle_store::test_open_record(
                id,
            ));
            store
                .set_finished(id, true, Some("done".to_string()))
                .unwrap();
        }
        let cohort = RestoringCohort::of(&record(PrepState::Restoring, &["in-cohort"])).unwrap();
        let ids = vec!["in-cohort".to_string(), "outside".to_string()];
        // No restoring cohort: nothing is touched.
        assert!(unfinish_for_restore(&store, &ids, None).is_empty());
        assert!(store.get("in-cohort").unwrap().finished_at.is_some());

        assert_eq!(
            unfinish_for_restore(&store, &ids, Some(&cohort)),
            ["in-cohort"]
        );
        assert!(store.get("in-cohort").unwrap().finished_at.is_none());
        assert!(
            store.get("outside").unwrap().finished_at.is_some(),
            "a finished session outside the cohort keeps its mark"
        );
        // Idempotent: a second Restore un-finishes nothing more.
        assert!(unfinish_for_restore(&store, &ids, Some(&cohort)).is_empty());
    }
}
