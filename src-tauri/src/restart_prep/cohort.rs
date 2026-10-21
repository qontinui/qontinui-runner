//! The cohort: every session that was open when a restart was prepared (plan
//! `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, D4).
//!
//! **Not a second capture routine.** The cohort is
//! [`crate::session::session_ledger::capture`] — the ledger the boot latch and
//! the 45 s liveness poll already write — taken at prepare time and pinned
//! under a name, so a later tick's rewrite of `session-ledger.json` cannot
//! overwrite the set the operator prepared. Each ledger entry is carried
//! verbatim as [`CohortRow::session`]; this module only adds the fields the
//! restart needs and the ledger does not hold: effort and model (D5), the
//! session's state at prepare, whether it is held for a hand restore (D6), and
//! the notice / usage-limit stamps later phases write.
//!
//! What a row knows about its pane, transcript and account comes through
//! [`CohortProbe`], so the builder is pure over its inputs and the live reads
//! ([`LiveCohortProbe`]) are one small adapter.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::effort::{self, Baseline, Effort};
use crate::session::session_ledger::{self, LedgerEntry, SessionLedger};
use crate::session::session_lifecycle_store::SessionLifecycleStore;
use crate::session::shutdown_marker::BootClassification;
use crate::session::snapshot_history::TranscriptProbe;
use crate::terminal::graceful_exit::ScreenText;
use crate::terminal::TerminalManager;

/// A session's state when the prepare captured it.
///
/// Phase 1 derives `working`, `idle`, `finished` and `unknown` from what it
/// can read; `paused` and `limit_hit` are written by the notice loop and the
/// account freeze of later phases. An unread pane is `unknown`, never `idle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateAtPrepare {
    /// Mid-turn: the busy marker is on screen.
    Working,
    /// At an idle prompt.
    Idle,
    /// Told to save and pause, and now idle with no live children.
    Paused,
    /// Its account ran out of usage during the prepare.
    LimitHit,
    /// Declared finished on the work axis.
    Finished,
    /// No input that could say which.
    Unknown,
}

/// One cohort session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortRow {
    /// The ledger entry, verbatim (its own camelCase wire shape): page, zone,
    /// title, name, account, config dir, worktree, custody.
    pub session: LedgerEntry,
    /// The effort the session ran at, with where that was read.
    pub effort: Effort,
    /// The effort a session on this account gets anyway.
    pub baseline: Baseline,
    /// The transcript's last real assistant model; `None` when unreadable.
    pub model: Option<String>,
    pub state_at_prepare: StateAtPrepare,
    /// Held for a hand restore: `effort > baseline`, `false` when either side
    /// is unknown. Bulk restore skips a held row; the operator can release it.
    pub held: bool,
    /// Unix millis the save-and-pause notice was delivered (Phase 4).
    pub notified_at: Option<i64>,
    /// Unix millis the account's usage window resets, when a confirmed
    /// usage-limit hit during the prepare had one (Phase 3).
    pub limit_reset_at: Option<i64>,
    /// The work axis at capture: `Some(true)` when the session declared itself
    /// finished. `None` when the lifecycle record could not be read.
    pub finished: Option<bool>,
}

/// The pinned capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cohort {
    /// The ledger's own capture time and reason.
    pub captured_at_ms: i64,
    pub captured_at: String,
    pub reason: String,
    pub rows: Vec<CohortRow>,
}

/// What one ledger entry's pane, transcript and account say. Every field is
/// optional: an unread input stays unread.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowFacts {
    /// The pane's rendered grid; `None` when the session has no live terminal.
    pub screen: Option<ScreenText>,
    /// `Some(true)` when the lifecycle record carries a finish mark.
    pub finished: Option<bool>,
    /// The transcript's last real assistant model.
    pub model: Option<String>,
    /// The account's `settings.json` `effortLevel`.
    pub settings_effort: Option<String>,
}

/// The per-row reads the builder needs.
pub trait CohortProbe {
    fn facts(&self, entry: &LedgerEntry) -> RowFacts;
}

/// The state a row's facts support.
fn state_of(facts: &RowFacts) -> StateAtPrepare {
    if facts.finished == Some(true) {
        return StateAtPrepare::Finished;
    }
    let Some(screen) = &facts.screen else {
        return StateAtPrepare::Unknown;
    };
    let normalized = crate::terminal::output_scan::normalize(&screen.lines.join("\n"));
    if normalized.contains(crate::terminal::account_migration::BUSY_MARKER) {
        return StateAtPrepare::Working;
    }
    if qontinui_runner_lib::looping_agent::idle::snapshot_looks_idle(
        &screen.lines,
        screen.cursor_row,
    ) {
        return StateAtPrepare::Idle;
    }
    StateAtPrepare::Unknown
}

/// One row from a ledger entry and its facts.
pub fn build_row(session: LedgerEntry, facts: RowFacts) -> CohortRow {
    let screen_level = facts
        .screen
        .as_ref()
        .and_then(|s| effort::parse_screen(&s.lines));
    let settings = facts.settings_effort.as_deref();
    let effort = effort::effort_from(screen_level, settings);
    let baseline = effort::baseline_from(settings, effort::CLI_DEFAULT_EFFORT);
    let held = effort::is_above(&effort, &baseline);
    CohortRow {
        state_at_prepare: state_of(&facts),
        session,
        effort,
        baseline,
        model: facts.model,
        held,
        notified_at: None,
        limit_reset_at: None,
        finished: facts.finished,
    }
}

/// The cohort over an already-taken ledger capture. Row order is the ledger's
/// (sorted by Claude session id).
pub fn build_cohort(ledger: SessionLedger, probe: &dyn CohortProbe) -> Cohort {
    let rows = ledger
        .sessions
        .into_iter()
        .map(|entry| {
            let facts = probe.facts(&entry);
            build_row(entry, facts)
        })
        .collect();
    Cohort {
        captured_at_ms: ledger.captured_at_ms,
        captured_at: ledger.captured_at,
        reason: ledger.reason,
        rows,
    }
}

/// Take the ledger capture now and build the cohort over it.
pub fn capture_cohort(
    store: &SessionLifecycleStore,
    transcripts: &dyn TranscriptProbe,
    probe: &dyn CohortProbe,
    reason: &str,
    now_ms: i64,
    boot: Option<BootClassification>,
) -> Cohort {
    build_cohort(
        session_ledger::capture(store, transcripts, reason, now_ms, boot),
        probe,
    )
}

/// The live reads: the pane's grid from the terminal manager, the finish mark
/// from the lifecycle store, the model from the transcript under the account
/// the session ran on, and the account's `settings.json`.
pub struct LiveCohortProbe {
    pub store: Arc<SessionLifecycleStore>,
    pub terminals: Option<Arc<TerminalManager>>,
}

impl CohortProbe for LiveCohortProbe {
    fn facts(&self, entry: &LedgerEntry) -> RowFacts {
        let screen = self
            .terminals
            .as_ref()
            .and_then(|tm| tm.get(&entry.terminal_id))
            .map(|session| session.grid_screen());
        let finished = self
            .store
            .get(&entry.claude_session_id)
            .map(|r| r.finished_at.is_some());
        let config_dir = entry
            .config_dir
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty());
        // Read exactly as the migration's respawn reads it: the transcript
        // under the account the session RAN on.
        let model = match (config_dir, entry.working_dir.as_deref()) {
            (Some(cfg), Some(wd)) if !wd.trim().is_empty() => {
                crate::terminal::account_migration::transcript_last_model(
                    &crate::terminal::transcript::session_transcript_path(
                        Path::new(cfg),
                        wd,
                        &entry.claude_session_id,
                    ),
                )
            }
            _ => None,
        };
        let settings_effort = config_dir.and_then(|c| effort::settings_effort_level(Path::new(c)));
        RowFacts {
            screen,
            finished,
            model,
            settings_effort,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::restart_prep::effort::{DefaultSource, EffortSource};

    fn entry(id: &str) -> LedgerEntry {
        LedgerEntry {
            claude_session_id: id.to_string(),
            terminal_id: format!("term-{id}"),
            page_id: "page-a".to_string(),
            zone_index: 2,
            title: Some(format!("tab-{id}")),
            session_name: Some(format!("name-{id}")),
            account_label: Some("iris".to_string()),
            config_dir: Some("/home/u/.claude-iris".to_string()),
            working_dir: Some("/home/u/repo".to_string()),
            restorable: Some(true),
            worktree_path: None,
            plan_slug: None,
            work_unit_id: None,
            wip_state: None,
            wip_ref: None,
            custody_session_mismatch: false,
        }
    }

    fn ledger(ids: &[&str]) -> SessionLedger {
        SessionLedger {
            ledger_version: session_ledger::LEDGER_VERSION,
            captured_at_ms: 1_000,
            captured_at: "2026-10-21T00:00:00Z".to_string(),
            reason: "restart-prep".to_string(),
            shutdown_at: None,
            clean_shutdown: None,
            sessions: ids.iter().map(|id| entry(id)).collect(),
        }
    }

    fn screen(rows: &[&str], cursor_row: u16, cursor_col: u16) -> ScreenText {
        ScreenText {
            lines: rows.iter().map(|s| s.to_string()).collect(),
            cursor_row,
            cursor_col,
        }
    }

    fn idle_at(effort_row: &str) -> ScreenText {
        screen(&[effort_row, "────", "❯\u{a0}", "────"], 2, 2)
    }

    fn busy_at(effort_row: &str) -> ScreenText {
        screen(
            &[
                "✻ Churning… (3s · esc to interrupt)",
                effort_row,
                "────",
                "❯\u{a0}",
                "────",
            ],
            3,
            2,
        )
    }

    /// Facts keyed by session id; an id with no entry gets the default (every
    /// input unread).
    struct Fixed(HashMap<String, RowFacts>);

    impl CohortProbe for Fixed {
        fn facts(&self, entry: &LedgerEntry) -> RowFacts {
            self.0
                .get(&entry.claude_session_id)
                .cloned()
                .unwrap_or_default()
        }
    }

    fn fixed(pairs: Vec<(&str, RowFacts)>) -> Fixed {
        Fixed(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    #[test]
    fn an_empty_ledger_is_an_empty_cohort() {
        let c = build_cohort(ledger(&[]), &fixed(vec![]));
        assert!(c.rows.is_empty());
        assert_eq!(c.captured_at_ms, 1_000);
        assert_eq!(c.reason, "restart-prep");
    }

    #[test]
    fn one_session_carries_its_ledger_entry_and_its_effort() {
        let c = build_cohort(
            ledger(&["s1"]),
            &fixed(vec![(
                "s1",
                RowFacts {
                    screen: Some(idle_at("● high · /effort")),
                    finished: Some(false),
                    model: Some("claude-opus-5-5".to_string()),
                    settings_effort: None,
                },
            )]),
        );
        assert_eq!(c.rows.len(), 1);
        let r = &c.rows[0];
        assert_eq!(
            r.session,
            entry("s1"),
            "the ledger entry is carried verbatim"
        );
        assert_eq!(r.session.page_id, "page-a");
        assert_eq!(r.session.zone_index, 2);
        assert_eq!(r.effort.level.as_deref(), Some("high"));
        assert_eq!(r.effort.source, EffortSource::Screen);
        assert_eq!(r.baseline.default_source, DefaultSource::CliDefault);
        assert_eq!(r.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(r.state_at_prepare, StateAtPrepare::Idle);
        assert!(!r.held, "high is the CLI default, so not held");
        assert_eq!(r.finished, Some(false));
        assert_eq!(r.notified_at, None);
        assert_eq!(r.limit_reset_at, None);
    }

    #[test]
    fn many_sessions_each_get_their_own_row_and_state() {
        let c = build_cohort(
            ledger(&["a", "b", "c", "d", "e"]),
            &fixed(vec![
                (
                    "a",
                    RowFacts {
                        screen: Some(busy_at("◉ max · /effort")),
                        finished: Some(false),
                        ..Default::default()
                    },
                ),
                (
                    "b",
                    RowFacts {
                        screen: Some(idle_at("○ low · /effort")),
                        finished: Some(false),
                        ..Default::default()
                    },
                ),
                (
                    "c",
                    RowFacts {
                        screen: Some(idle_at("● high · /effort")),
                        finished: Some(true),
                        ..Default::default()
                    },
                ),
                (
                    "d",
                    RowFacts {
                        // Neither busy nor idle: the cursor is up in output.
                        screen: Some(screen(&["streaming…", "❯ "], 0, 3)),
                        finished: Some(false),
                        settings_effort: Some("xhigh".to_string()),
                        ..Default::default()
                    },
                ),
                // "e": no facts at all.
            ]),
        );
        let ids: Vec<&str> = c
            .rows
            .iter()
            .map(|r| r.session.claude_session_id.as_str())
            .collect();
        assert_eq!(ids, ["a", "b", "c", "d", "e"]);
        let states: Vec<StateAtPrepare> = c.rows.iter().map(|r| r.state_at_prepare).collect();
        assert_eq!(
            states,
            [
                StateAtPrepare::Working,
                StateAtPrepare::Idle,
                StateAtPrepare::Finished,
                StateAtPrepare::Unknown,
                StateAtPrepare::Unknown,
            ]
        );
        let held: Vec<bool> = c.rows.iter().map(|r| r.held).collect();
        // a: max > high (CLI default). b: low. c: high = high.
        // d: xhigh from settings, against settings xhigh. e: unknown.
        assert_eq!(held, [true, false, false, false, false]);
        assert_eq!(c.rows[3].effort.source, EffortSource::AccountDefault);
        assert_eq!(c.rows[3].baseline.default_source, DefaultSource::Settings);
    }

    #[test]
    fn a_finished_row_is_recorded_with_finished_true() {
        let c = build_cohort(
            ledger(&["done"]),
            &fixed(vec![(
                "done",
                RowFacts {
                    screen: Some(idle_at("● high · /effort")),
                    finished: Some(true),
                    ..Default::default()
                },
            )]),
        );
        assert_eq!(c.rows.len(), 1, "a finished session is still recorded");
        assert_eq!(c.rows[0].finished, Some(true));
        assert_eq!(c.rows[0].state_at_prepare, StateAtPrepare::Finished);
    }

    #[test]
    fn an_unknown_effort_is_not_held() {
        // A max-effort-looking session whose indicator is unreadable, on an
        // account with no `effortLevel`: unknown, and so not held.
        let c = build_cohort(
            ledger(&["u"]),
            &fixed(vec![(
                "u",
                RowFacts {
                    screen: Some(idle_at("◉ max · /eff")),
                    finished: Some(false),
                    ..Default::default()
                },
            )]),
        );
        let r = &c.rows[0];
        assert_eq!(r.effort.source, EffortSource::Unknown);
        assert_eq!(r.effort.level, None);
        assert!(!r.held);
        // No pane at all reads the same.
        let r = build_row(entry("x"), RowFacts::default());
        assert_eq!(r.effort.source, EffortSource::Unknown);
        assert!(!r.held);
        assert_eq!(r.state_at_prepare, StateAtPrepare::Unknown);
        assert_eq!(r.finished, None);
    }

    fn open_record(
        id: &str,
        zone: i32,
    ) -> crate::session::session_lifecycle_store::TerminalSessionRecord {
        crate::session::session_lifecycle_store::TerminalSessionRecord {
            claude_session_id: id.to_string(),
            config_dir: Some("/nonexistent/cfg".to_string()),
            working_dir: Some("/nonexistent/work".to_string()),
            page_id: "page-b".to_string(),
            zone_index: zone,
            title: Some(format!("tab-{id}")),
            terminal_id: format!("term-{id}"),
            opened_at: 1,
            last_seen_at: 1,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: crate::session::session_lifecycle_store::DEFAULT_PROVIDER.to_string(),
            origin: None,
            restore_pending_at: None,
            confirmed_at: Some(1),
            handle: None,
            account_label: Some("iris".to_string()),
            account_wrapper: None,
            session_name: None,
            name_source: None,
            tenant_id: None,
            task_run_id: None,
            bypass_permissions: None,
            restored_from_boot_at: None,
            restore_tier: None,
            finished_at: None,
            wind_down_outcome: None,
            wind_down_at: None,
            finish_reason: None,
            finish_synced: false,
            spawn_device_default: None,
            adopted_from: None,
        }
    }

    #[derive(Debug)]
    struct NoTranscripts;
    impl TranscriptProbe for NoTranscripts {
        fn transcript_exists(&self, _: &str, _: Option<&str>) -> bool {
            false
        }
    }

    #[test]
    fn capture_cohort_is_the_ledgers_own_capture() {
        // Through the real `session_ledger::capture` over a real store: the
        // cohort's rows are exactly the store's open records, and the live
        // probe with no terminal manager reads every pane as unread.
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap(),
        );
        let live = LiveCohortProbe {
            store: store.clone(),
            terminals: None,
        };
        let empty = capture_cohort(&store, &NoTranscripts, &live, "t", 5, None);
        assert!(empty.rows.is_empty());
        assert_eq!(empty.captured_at_ms, 5);

        store.record_open(open_record("bbbb", 3));
        store.record_open(open_record("aaaa", 1));
        let c = capture_cohort(&store, &NoTranscripts, &live, "t", 6, None);
        let got: Vec<(&str, &str, i32)> = c
            .rows
            .iter()
            .map(|r| {
                (
                    r.session.claude_session_id.as_str(),
                    r.session.page_id.as_str(),
                    r.session.zone_index,
                )
            })
            .collect();
        assert_eq!(got, [("aaaa", "page-b", 1), ("bbbb", "page-b", 3)]);
        for r in &c.rows {
            assert_eq!(r.finished, Some(false), "the store answered the work axis");
            assert_eq!(r.state_at_prepare, StateAtPrepare::Unknown, "no pane read");
            assert_eq!(r.effort.source, EffortSource::Unknown);
            assert_eq!(r.model, None, "no transcript on disk");
            assert_eq!(r.session.account_label.as_deref(), Some("iris"));
        }
    }
}
