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

/// A row's state as the notice loop last read it from the census (Phase 4).
///
/// Derived each tick from observed evidence, never set by intent: `paused`
/// means the row was told AND its own process reads idle at a prompt with no
/// live children; a `hasLiveChildren` that is null, or an activity that could
/// not be classified, is `unknown` — never `paused`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseState {
    /// Not yet told to save and pause (still running its own work, or idle
    /// but the notice could not be delivered yet — `pause_reason` says which).
    Working,
    /// Told, and not yet idle with no live children.
    Notified,
    /// Told, and now idle at a prompt with no live children.
    Paused,
    /// Declared finished on the work axis; still running until wind-down.
    Finished,
    /// Its session is no longer running here: closed (a finished row closed by
    /// the drain's wind-down executor, or a session closed since the prepare).
    Closed,
    /// Its account ran out of usage during the prepare (the freeze kept it).
    LimitHit,
    /// An input the state needs could not be read.
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
    /// The operator set [`Self::held`] by hand (the panel's held toggle,
    /// [`super::restore::set_held_on`]). A re-capture never recomputes an
    /// operator's choice from a newly read effort.
    #[serde(default)]
    pub held_by_operator: bool,
    /// Unix millis the save-and-pause notice was delivered (Phase 4).
    pub notified_at: Option<i64>,
    /// Unix millis the account's usage window resets, when a confirmed
    /// usage-limit hit during the prepare had one (Phase 3).
    pub limit_reset_at: Option<i64>,
    /// The work axis at capture: `Some(true)` when the session declared itself
    /// finished. `None` when the lifecycle record could not be read.
    pub finished: Option<bool>,
    /// The lifecycle record's provider (`claude` for a Claude Code session);
    /// `None` when the record could not be read.
    #[serde(default)]
    pub provider: Option<String>,
    /// Phase 4: the row's state at the notice loop's last read. `None` before
    /// the first tick.
    #[serde(default)]
    pub pause_state: Option<PauseState>,
    /// What that state was read from, or which input could not be read.
    #[serde(default)]
    pub pause_reason: Option<String>,
    /// The row's activity classification at the last read: idle `Some(true)`,
    /// working or stale `Some(false)`, unclassified `None`. Live children do
    /// not enter it.
    #[serde(default)]
    pub idle_now: Option<bool>,
    /// The census children hint at the last read, INFORMATION ONLY: `true`
    /// when the row's `claude` has a child process attached (its MCP servers,
    /// a background shell), which a restart ends. `None` when uncomputable or
    /// unread. Never blocks the verdict.
    #[serde(default)]
    pub has_live_children: Option<bool>,
    /// Why the notice loop does not reach this row (no live terminal, not a
    /// Claude session). `None` while it can be reached.
    #[serde(default)]
    pub notice_skipped: Option<String>,
    /// Why the last delivery attempt did not paste; it is retried next tick.
    #[serde(default)]
    pub notice_deferred: Option<String>,
    /// The input arm the notice went through: `idle`, `busy_prompt`, or
    /// `shared_terminal` (the same pane and process were told through another
    /// record).
    #[serde(default)]
    pub notice_arm: Option<String>,
    /// The `claude` pid the census attributed to this session when it was
    /// told; `None` when unread. The re-send guard matches a pane's told
    /// process by it.
    #[serde(default)]
    pub notified_pid: Option<u32>,
    /// Unix millis the "restart cancelled — resume your work" notice was
    /// delivered, for a row that had been told to pause and whose prepare was
    /// then cancelled.
    #[serde(default)]
    pub resume_notified_at: Option<i64>,
    /// Unix millis the session's lifecycle record was first seen NOT open by a
    /// re-capture (closed by wind-down, or by its own exit). Cleared when it is
    /// open again.
    #[serde(default)]
    pub left_capture_at: Option<i64>,
    /// Phase 6: the last restore of this row by the rebuilt runner — its
    /// resume verdict and, when an effort was re-applied, whether the prompt
    /// line read back confirmed it. `None` until a restore was attempted.
    #[serde(default)]
    pub restore: Option<super::restore::RowRestore>,
}

impl CohortRow {
    /// The name the panel and the verdict show: the session name, else the
    /// tab title.
    pub fn display_name(&self) -> Option<String> {
        self.session
            .session_name
            .clone()
            .or_else(|| self.session.title.clone())
    }
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
    /// The lifecycle record's provider.
    pub provider: Option<String>,
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
    if crate::terminal::graceful_exit::shows_busy_marker(screen) {
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
        held_by_operator: false,
        notified_at: None,
        limit_reset_at: None,
        finished: facts.finished,
        provider: facts.provider,
        pause_state: None,
        pause_reason: None,
        idle_now: None,
        has_live_children: None,
        notice_skipped: None,
        notice_deferred: None,
        notice_arm: None,
        notified_pid: None,
        resume_notified_at: None,
        left_capture_at: None,
        restore: None,
    }
}

/// Fold a fresh capture into the pinned cohort (Phase 4: "re-taken on every
/// prepare state change", D4) without losing what the prepare recorded.
///
/// * A session in both keeps its prepare-time row — effort, baseline, model,
///   `held`, `state_at_prepare` (a `limit_hit` mark included), the notice and
///   usage-limit stamps — and takes the fresh ledger entry (a tab may have
///   moved) and work axis. An effort that was unread at prepare and is read
///   now is taken from the fresh row; a read one is never overwritten. The
///   fresh row's `held` comes with it unless the operator set `held` by hand.
/// * A session only in the fresh capture (opened during the prepare) joins.
/// * A session only in the pinned cohort is kept — every session is recorded —
///   and stamped `left_capture_at` the first time it is missing.
///
/// Rows stay in ledger order (by Claude session id). The capture time and
/// reason are the fresh capture's.
pub fn merge_recapture(pinned: &Cohort, fresh: Cohort, now_ms: i64) -> Cohort {
    use std::collections::BTreeMap;
    let mut by_id: BTreeMap<String, CohortRow> = pinned
        .rows
        .iter()
        .map(|r| (r.session.claude_session_id.clone(), r.clone()))
        .collect();
    let mut seen = std::collections::HashSet::new();
    for fresh_row in fresh.rows {
        let id = fresh_row.session.claude_session_id.clone();
        seen.insert(id.clone());
        match by_id.get_mut(&id) {
            Some(row) => {
                row.session = fresh_row.session;
                if fresh_row.finished.is_some() {
                    row.finished = fresh_row.finished;
                }
                if fresh_row.provider.is_some() {
                    row.provider = fresh_row.provider;
                }
                if row.model.is_none() {
                    row.model = fresh_row.model;
                }
                if row.effort.source == effort::EffortSource::Unknown
                    && fresh_row.effort.source != effort::EffortSource::Unknown
                {
                    row.effort = fresh_row.effort;
                    row.baseline = fresh_row.baseline;
                    if !row.held_by_operator {
                        row.held = fresh_row.held;
                    }
                }
                row.left_capture_at = None;
            }
            None => {
                by_id.insert(id, fresh_row);
            }
        }
    }
    for (id, row) in by_id.iter_mut() {
        if !seen.contains(id) && row.left_capture_at.is_none() {
            row.left_capture_at = Some(now_ms);
        }
    }
    Cohort {
        captured_at_ms: fresh.captured_at_ms,
        captured_at: fresh.captured_at,
        reason: fresh.reason,
        rows: by_id.into_values().collect(),
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
        let record = self.store.get(&entry.claude_session_id);
        let finished = record.as_ref().map(|r| r.finished_at.is_some());
        let provider = record.map(|r| r.provider);
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
            provider,
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
            name_source: None,
            resume_config_dir: None,
            resume_command: None,
            provider: None,
            last_seen_at: None,
            finished: false,
        }
    }

    fn ledger(ids: &[&str]) -> SessionLedger {
        SessionLedger {
            ledger_version: session_ledger::LEDGER_VERSION,
            captured_at_ms: 1_000,
            captured_at: "2026-10-21T00:00:00Z".to_string(),
            reason: "restart-prep".to_string(),
            boot_at_ms: None,
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
                    provider: None,
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

    #[test]
    fn a_recapture_that_first_reads_the_effort_keeps_the_operators_held_choice() {
        let unread = RowFacts {
            finished: Some(false),
            ..Default::default()
        };
        let read_max = || RowFacts {
            screen: Some(idle_at("◉ max · /effort")),
            finished: Some(false),
            ..Default::default()
        };
        let pinned = build_cohort(
            ledger(&["a", "b"]),
            &fixed(vec![("a", unread.clone()), ("b", unread)]),
        );
        assert!(
            !pinned.rows[0].held && !pinned.rows[1].held,
            "unknown effort: not held"
        );
        // The operator released `a` by hand (held stays false, but it is now
        // THEIR false); `b` is untouched.
        let mut pinned = pinned;
        pinned.rows[0].held_by_operator = true;
        let fresh = build_cohort(
            ledger(&["a", "b"]),
            &fixed(vec![("a", read_max()), ("b", read_max())]),
        );
        let merged = merge_recapture(&pinned, fresh, 1_000);
        assert_eq!(
            merged.rows[0].effort.level.as_deref(),
            Some("max"),
            "effort taken"
        );
        assert!(
            !merged.rows[0].held,
            "the operator's choice survives the first read"
        );
        assert!(merged.rows[0].held_by_operator);
        assert!(merged.rows[1].held, "an untouched row takes the fresh held");
    }

    #[test]
    fn a_recapture_keeps_the_prepare_stamps_adds_new_sessions_and_marks_closed_ones() {
        let unread = || RowFacts {
            finished: Some(false),
            ..Default::default()
        };
        let mut pinned = build_cohort(
            ledger(&["a", "b"]),
            &fixed(vec![
                (
                    "a",
                    RowFacts {
                        screen: Some(idle_at("◉ max · /effort")),
                        finished: Some(false),
                        ..Default::default()
                    },
                ),
                ("b", unread()),
            ]),
        );
        pinned.rows[0].notified_at = Some(7);
        pinned.rows[0].state_at_prepare = StateAtPrepare::LimitHit;
        pinned.rows[0].limit_reset_at = Some(9);
        // The re-capture: `a` moved to zone 5 and now reads `low`; `b` is
        // gone; `c` opened during the prepare.
        let mut moved = ledger(&["a", "c"]);
        moved.sessions[0].zone_index = 5;
        moved.captured_at_ms = 2_000;
        let fresh = build_cohort(
            moved,
            &fixed(vec![
                (
                    "a",
                    RowFacts {
                        screen: Some(idle_at("○ low · /effort")),
                        finished: Some(false),
                        ..Default::default()
                    },
                ),
                ("c", unread()),
            ]),
        );
        let merged = merge_recapture(&pinned, fresh, 3_000);
        assert_eq!(merged.captured_at_ms, 2_000);
        let ids: Vec<&str> = merged
            .rows
            .iter()
            .map(|r| r.session.claude_session_id.as_str())
            .collect();
        assert_eq!(ids, ["a", "b", "c"], "every session is still recorded");
        let a = &merged.rows[0];
        assert_eq!(a.session.zone_index, 5, "the fresh ledger entry");
        assert_eq!(a.notified_at, Some(7), "the notice stamp survives");
        assert_eq!(a.state_at_prepare, StateAtPrepare::LimitHit);
        assert_eq!(a.limit_reset_at, Some(9));
        assert_eq!(
            a.effort.level.as_deref(),
            Some("max"),
            "a read effort is kept"
        );
        assert!(a.held);
        assert_eq!(a.left_capture_at, None);
        assert_eq!(merged.rows[1].left_capture_at, Some(3_000), "b closed");
        assert_eq!(merged.rows[2].left_capture_at, None, "c joined");

        // A later re-capture keeps b's first-missing time; b reappearing clears it.
        let again = merge_recapture(&merged, build_cohort(ledger(&["a"]), &fixed(vec![])), 4_000);
        assert_eq!(again.rows[1].left_capture_at, Some(3_000));
        let back = merge_recapture(
            &again,
            build_cohort(ledger(&["a", "b", "c"]), &fixed(vec![])),
            5_000,
        );
        assert_eq!(back.rows[1].left_capture_at, None);
        // An effort unread at prepare is taken once it is read.
        assert_eq!(back.rows[1].effort.source, EffortSource::Unknown);
        let read = merge_recapture(
            &back,
            build_cohort(
                ledger(&["b"]),
                &fixed(vec![(
                    "b",
                    RowFacts {
                        screen: Some(idle_at("● high · /effort")),
                        ..Default::default()
                    },
                )]),
            ),
            6_000,
        );
        assert_eq!(read.rows[1].effort.level.as_deref(), Some("high"));
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
            awaiting_account_since: None,
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
