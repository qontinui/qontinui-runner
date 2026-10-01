//! "Why did my session stop?" — one typed answer per session, assembled from
//! what the lifecycle registry already records.
//!
//! Plan `2026-09-20-the-published-product-works-without-knowing-a-development-environment-exists`,
//! Phase C5. Served as `GET /sessions/{id}/stop-reason` (`mcp::sessions`).
//!
//! # The recorded inputs, enumerated
//!
//! Everything here is read off one [`TerminalSessionRecord`]; nothing is
//! inferred from process state, logs or coord. Three recorded axes decide the
//! answer, in this precedence:
//!
//! 1. **`wind_down_outcome`** — a drained runner asked a FINISHED session to
//!    `/exit`. The three outcomes in which the agent left
//!    ([`WIND_DOWN_CLOSED`], [`WIND_DOWN_CLOSE_REFUSED`],
//!    [`WIND_DOWN_CLOSE_UNKNOWN`]) are the stop, whatever `close_reason` the
//!    pane's later close wrote. The two in which it did not
//!    ([`WIND_DOWN_EXIT_STUCK`], [`WIND_DOWN_NOT_ATTEMPTED`]) are not a stop and
//!    fall through.
//! 2. **`state`** — `"open"` is not stopped: [`StopCode::Running`], or
//!    [`StopCode::Finished`] when the work axis says the work is done.
//! 3. **`close_reason`** on a `"closed"` record — the nine `CLOSE_REASON_*`
//!    values in `session_lifecycle_store`, one code each.
//!
//! # Unknown is first-class — never a default reason
//!
//! A `close_reason` outside that set, a closed record with no reason at all,
//! and a `state` that is neither `open` nor `closed` all render
//! `code: "unknown"` with the raw value in `detail`. The next action for them
//! is `report_defect`: the runner recorded something its own vocabulary does
//! not name, which is a defect in the runner, not in the session.
//!
//! # The next action reads the record, not a guess
//!
//! Where the conversation can be continued, the action is to resume it — but
//! only when the record shows a provider actually STARTED there
//! (`confirmed_at`), its transcript is on disk (the caller probes it with
//! `past_sessions::resolve_transcript_path`, the same test the previous-sessions
//! listing uses for "restorable"), and the work axis does not already say it is
//! finished (`finished_at`). A finished session's stop needs nothing
//! (`none_terminal`); an unconfirmed one, or one without a transcript, has no
//! conversation to resume. The resume command prefers the launcher recorded at
//! spawn (`account_wrapper`) and otherwise falls back to the previous-sessions
//! derivation from the config dir.
//!
//! # What this does NOT cover
//!
//! One session plane: TERMINAL-hosted sessions in the lifecycle registry.
//! Stream-json sessions spawned by `POST /sessions/spawn` are task runs, write
//! no lifecycle record, and record their stop elsewhere (the task-run row, the
//! agent runtime's stop reasons); an id from that plane answers `404` with a
//! message naming the plane rather than a guess. Covering it is a follow-up.

use serde::Serialize;

use qontinui_types::glossary::GlossaryTerm;
use qontinui_types::refusal::{NextAction, NextActionKind};

use crate::session::past_sessions::account_from_config_dir;
use crate::session::session_lifecycle_store::{
    TerminalSessionRecord, CLOSE_REASON_EXPLICIT, CLOSE_REASON_NEVER_STARTED,
    CLOSE_REASON_NO_TERMINAL, CLOSE_REASON_POLL_DEAD, CLOSE_REASON_PTY_EXIT,
    CLOSE_REASON_SUPERSEDED, CLOSE_REASON_SUPERSEDED_TERMINAL_REUSE,
    CLOSE_REASON_TERMINAL_ONLY_IDLE, CLOSE_REASON_WORKER_BIND_FAILED, DEFAULT_PROVIDER,
    WIND_DOWN_CLOSED, WIND_DOWN_CLOSE_REFUSED, WIND_DOWN_CLOSE_UNKNOWN,
};

/// Where the answer was read from. One value today; named so a second source
/// (e.g. the 14-day snapshot history) can never be mistaken for this one.
pub const SOURCE_LIFECYCLE_REGISTRY: &str = "runner_lifecycle_registry";

/// The page that lists every recent session with its resume command, named as
/// the runner UI names it. Used as the target when a resume is possible but
/// this module has no command line to offer for the provider.
pub const PREVIOUS_SESSIONS_PAGE: &str = "Previous sessions";

/// Why a session stopped (or that it has not). Closed set; the wire value is
/// [`StopCode::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopCode {
    /// Open, and its work is not marked finished. Nothing has stopped.
    Running,
    /// Open, and its work is marked finished. The agent may still be running;
    /// nothing is owed.
    Finished,
    /// A drained runner asked this finished session to exit, and it did.
    WoundDown,
    /// [`CLOSE_REASON_EXPLICIT`] — the operator closed the tab.
    ClosedByOperator,
    /// [`CLOSE_REASON_PTY_EXIT`] — the session's process exited.
    ProcessExited,
    /// [`CLOSE_REASON_POLL_DEAD`] — no agent was seen in the terminal for the
    /// liveness debounce.
    LivenessLost,
    /// [`CLOSE_REASON_TERMINAL_ONLY_IDLE`] — a tile restored without its
    /// conversation idled out.
    RestoredWithoutConversation,
    /// [`CLOSE_REASON_NEVER_STARTED`] — no agent ever started here.
    NeverStarted,
    /// [`CLOSE_REASON_NO_TERMINAL`] — the terminal hosting it is gone.
    TerminalGone,
    /// [`CLOSE_REASON_SUPERSEDED`] — a provisional record replaced by the
    /// session that actually started in its terminal.
    SupersededProvisional,
    /// [`CLOSE_REASON_SUPERSEDED_TERMINAL_REUSE`] — its terminal now hosts a
    /// newer session.
    SupersededByNewSession,
    /// [`CLOSE_REASON_WORKER_BIND_FAILED`] — an orchestration worker the loop
    /// could not bind was torn down.
    WorkerBindFailed,
    /// The recorded reason is not in the enumerated set; the raw value is in
    /// `detail`.
    Unknown,
}

impl StopCode {
    /// Every code, in declaration order.
    pub const ALL: &'static [StopCode] = &[
        StopCode::Running,
        StopCode::Finished,
        StopCode::WoundDown,
        StopCode::ClosedByOperator,
        StopCode::ProcessExited,
        StopCode::LivenessLost,
        StopCode::RestoredWithoutConversation,
        StopCode::NeverStarted,
        StopCode::TerminalGone,
        StopCode::SupersededProvisional,
        StopCode::SupersededByNewSession,
        StopCode::WorkerBindFailed,
        StopCode::Unknown,
    ];

    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            StopCode::Running => "running",
            StopCode::Finished => "finished",
            StopCode::WoundDown => "wound_down",
            StopCode::ClosedByOperator => "closed_by_operator",
            StopCode::ProcessExited => "process_exited",
            StopCode::LivenessLost => "liveness_lost",
            StopCode::RestoredWithoutConversation => "restored_without_conversation",
            StopCode::NeverStarted => "never_started",
            StopCode::TerminalGone => "terminal_gone",
            StopCode::SupersededProvisional => "superseded_provisional",
            StopCode::SupersededByNewSession => "superseded_by_new_session",
            StopCode::WorkerBindFailed => "worker_bind_failed",
            StopCode::Unknown => "unknown",
        }
    }

    /// The sentence a reader sees for this code.
    pub fn headline(self) -> &'static str {
        match self {
            StopCode::Running => "The session has not stopped; it is still open",
            StopCode::Finished => "The session's work is marked finished; nothing further is owed",
            StopCode::WoundDown => {
                "The session had finished its work and was closed so this runner could drain"
            }
            StopCode::ClosedByOperator => "The session's tab was closed",
            StopCode::ProcessExited => "The session's process exited",
            StopCode::LivenessLost => {
                "No agent was found running in the session's terminal, so it was marked closed"
            }
            StopCode::RestoredWithoutConversation => {
                "The session was restored without its conversation and sat idle, so it was closed"
            }
            StopCode::NeverStarted => "No agent ever started in this terminal",
            StopCode::TerminalGone => "The terminal hosting the session no longer exists",
            StopCode::SupersededProvisional => {
                "This record was a placeholder replaced by the session that actually started"
            }
            StopCode::SupersededByNewSession => {
                "The session's terminal was reused by a newer session"
            }
            StopCode::WorkerBindFailed => {
                "An automated worker session could not be attached to its run and was removed"
            }
            StopCode::Unknown => "The session stopped for a reason this version does not recognise",
        }
    }

    /// The glossary terms a reader needs to understand this code.
    pub fn glossary_terms(self) -> Vec<GlossaryTerm> {
        use GlossaryTerm::{AgentSession, Drain, RestartReadiness, SessionStatus};
        match self {
            StopCode::Running | StopCode::Finished => vec![SessionStatus, AgentSession],
            StopCode::WoundDown => vec![Drain, SessionStatus, RestartReadiness],
            StopCode::ClosedByOperator
            | StopCode::ProcessExited
            | StopCode::LivenessLost
            | StopCode::RestoredWithoutConversation
            | StopCode::TerminalGone
            | StopCode::SupersededByNewSession => vec![AgentSession, SessionStatus],
            StopCode::NeverStarted
            | StopCode::SupersededProvisional
            | StopCode::WorkerBindFailed => vec![AgentSession],
            StopCode::Unknown => vec![AgentSession, GlossaryTerm::Unknown],
        }
    }
}

/// The raw recorded fields the answer was derived from, echoed so a reader can
/// check the mapping rather than trust it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordedInputs {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_down_outcome: Option<String>,
    pub finished: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    pub confirmed: bool,
}

/// The body of `GET /sessions/{id}/stop-reason`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StopReason {
    pub session_id: String,
    pub code: StopCode,
    /// [`StopCode::headline`] for `code`.
    pub summary: &'static str,
    /// The raw value behind an `unknown` code, or supporting text (a finish
    /// reason) for a known one. Absent when there is nothing to add.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub glossary_terms: Vec<GlossaryTerm>,
    pub next_action: NextAction,
    /// [`NextAction::render`] — the imperative sentence for `next_action`.
    pub next_action_text: String,
    /// ISO 8601 time the deciding fact was recorded. `None` only when the
    /// record carries no timestamp for it (a closed record with no
    /// `closed_at`), which is stated rather than filled with "now".
    pub observed_at: Option<String>,
    /// [`SOURCE_LIFECYCLE_REGISTRY`].
    pub source: &'static str,
    pub recorded: RecordedInputs,
}

fn iso(ms: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).map(|t| t.to_rfc3339())
}

/// Resume the conversation, when the record shows there is one to resume (a
/// confirmed provider start and a transcript on disk) and its work is not
/// already finished; otherwise nothing is owed.
fn resume_or_nothing(rec: &TerminalSessionRecord, transcript_exists: bool) -> NextAction {
    if rec.finished_at.is_some() || rec.confirmed_at.is_none() || !transcript_exists {
        return NextAction::new(NextActionKind::NoneTerminal);
    }
    if rec.provider == DEFAULT_PROVIDER {
        let launcher = rec
            .account_wrapper
            .clone()
            .unwrap_or_else(|| account_from_config_dir(rec.config_dir.as_deref()).wrapper);
        NextAction::new(NextActionKind::RunCommand)
            .with_target(format!("{launcher} --resume {}", rec.claude_session_id))
    } else {
        NextAction::new(NextActionKind::OpenPage).with_target(PREVIOUS_SESSIONS_PAGE)
    }
}

/// Map a recorded `close_reason` to its code, or `None` when this version does
/// not enumerate it.
pub fn code_for_close_reason(reason: &str) -> Option<StopCode> {
    Some(match reason {
        CLOSE_REASON_EXPLICIT => StopCode::ClosedByOperator,
        CLOSE_REASON_PTY_EXIT => StopCode::ProcessExited,
        CLOSE_REASON_POLL_DEAD => StopCode::LivenessLost,
        CLOSE_REASON_TERMINAL_ONLY_IDLE => StopCode::RestoredWithoutConversation,
        CLOSE_REASON_NEVER_STARTED => StopCode::NeverStarted,
        CLOSE_REASON_NO_TERMINAL => StopCode::TerminalGone,
        CLOSE_REASON_SUPERSEDED => StopCode::SupersededProvisional,
        CLOSE_REASON_SUPERSEDED_TERMINAL_REUSE => StopCode::SupersededByNewSession,
        CLOSE_REASON_WORKER_BIND_FAILED => StopCode::WorkerBindFailed,
        _ => return None,
    })
}

/// Derive the stop reason for one record. Pure: `transcript_exists` is the
/// caller's probe of the session's transcript on disk.
pub fn stop_reason(rec: &TerminalSessionRecord, transcript_exists: bool) -> StopReason {
    // `wind_down_outcome` is cleared when a closed record re-opens
    // (`record_open`) but survives an un-finish, so it is the stop only while
    // it is still CURRENT: the record is closed and its work is still marked
    // finished. An un-finished session falls through to `state`.
    let wound_down = rec.state == "closed"
        && rec.finished_at.is_some()
        && matches!(
            rec.wind_down_outcome.as_deref(),
            Some(WIND_DOWN_CLOSED | WIND_DOWN_CLOSE_REFUSED | WIND_DOWN_CLOSE_UNKNOWN)
        );
    // WIND_DOWN_EXIT_STUCK / WIND_DOWN_NOT_ATTEMPTED are recorded but are not
    // a stop — the agent is still there — so they fall through to `state`.

    let finish_detail = rec.finish_reason.clone();
    let (code, detail, next_action, observed_ms) = if wound_down {
        (
            StopCode::WoundDown,
            finish_detail,
            NextAction::new(NextActionKind::NoneTerminal),
            rec.wind_down_at.or(rec.closed_at),
        )
    } else {
        match rec.state.as_str() {
            "open" if rec.finished_at.is_some() => (
                StopCode::Finished,
                finish_detail,
                NextAction::new(NextActionKind::NoneTerminal),
                rec.finished_at,
            ),
            // Nothing has stopped: asking again later is the only action that
            // can produce a different answer.
            "open" => (
                StopCode::Running,
                None,
                NextAction::retry_later(None),
                Some(rec.last_seen_at),
            ),
            "closed" => match rec.close_reason.as_deref() {
                Some(reason) => match code_for_close_reason(reason) {
                    Some(code) => {
                        // `no-terminal` is non-restorable as a TILE (main.rs),
                        // but the conversation may still be resumable, which is
                        // what this action is about.
                        let action = match code {
                            StopCode::ClosedByOperator
                            | StopCode::ProcessExited
                            | StopCode::LivenessLost
                            | StopCode::TerminalGone
                            | StopCode::SupersededByNewSession => {
                                resume_or_nothing(rec, transcript_exists)
                            }
                            StopCode::WorkerBindFailed => {
                                NextAction::new(NextActionKind::ReportDefect)
                            }
                            _ => NextAction::new(NextActionKind::NoneTerminal),
                        };
                        (code, finish_detail, action, rec.closed_at)
                    }
                    None => (
                        StopCode::Unknown,
                        Some(reason.to_string()),
                        NextAction::new(NextActionKind::ReportDefect),
                        rec.closed_at,
                    ),
                },
                None => (
                    StopCode::Unknown,
                    Some("the record is closed but carries no close reason".to_string()),
                    NextAction::new(NextActionKind::ReportDefect),
                    rec.closed_at,
                ),
            },
            other => (
                StopCode::Unknown,
                Some(format!("unrecognised record state `{other}`")),
                NextAction::new(NextActionKind::ReportDefect),
                rec.closed_at,
            ),
        }
    };

    StopReason {
        session_id: rec.claude_session_id.clone(),
        code,
        summary: code.headline(),
        detail,
        glossary_terms: code.glossary_terms(),
        next_action_text: next_action.render(),
        next_action,
        observed_at: observed_ms.and_then(iso),
        source: SOURCE_LIFECYCLE_REGISTRY,
        recorded: RecordedInputs {
            state: rec.state.clone(),
            close_reason: rec.close_reason.clone(),
            wind_down_outcome: rec.wind_down_outcome.clone(),
            finished: rec.finished_at.is_some(),
            finish_reason: rec.finish_reason.clone(),
            confirmed: rec.confirmed_at.is_some(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::session_lifecycle_store::{WIND_DOWN_EXIT_STUCK, WIND_DOWN_NOT_ATTEMPTED};

    const CLOSED_AT: i64 = 1_790_000_000_000;

    fn rec(state: &str, close_reason: Option<&str>) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: "sess-1".to_string(),
            config_dir: None,
            working_dir: Some("/repo".to_string()),
            page_id: "default".to_string(),
            zone_index: 0,
            title: None,
            terminal_id: "term-1".to_string(),
            opened_at: CLOSED_AT - 60_000,
            last_seen_at: CLOSED_AT - 1_000,
            state: state.to_string(),
            closed_at: (state == "closed").then_some(CLOSED_AT),
            close_reason: close_reason.map(str::to_string),
            provider: DEFAULT_PROVIDER.to_string(),
            origin: None,
            restore_pending_at: None,
            confirmed_at: Some(CLOSED_AT - 50_000),
            handle: None,
            account_label: None,
            account_wrapper: Some("clg".to_string()),
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
        }
    }

    /// The common case: a transcript is on disk.
    fn sr(r: &TerminalSessionRecord) -> StopReason {
        stop_reason(r, true)
    }

    fn resume() -> NextAction {
        NextAction::new(NextActionKind::RunCommand).with_target("clg --resume sess-1")
    }

    fn none() -> NextAction {
        NextAction::new(NextActionKind::NoneTerminal)
    }

    /// Assert one closed-reason mapping end to end.
    fn assert_closed(reason: &str, code: StopCode, action: NextAction, terms: &[GlossaryTerm]) {
        let r = sr(&rec("closed", Some(reason)));
        assert_eq!(r.code, code, "{reason}");
        assert_eq!(r.next_action, action, "{reason}");
        assert_eq!(r.glossary_terms, terms, "{reason}");
        assert_eq!(r.observed_at, iso(CLOSED_AT), "{reason}");
        assert_eq!(r.recorded.close_reason.as_deref(), Some(reason));
        assert_eq!(r.source, SOURCE_LIFECYCLE_REGISTRY);
        assert!(!r.next_action_text.is_empty());
    }

    use GlossaryTerm::{AgentSession, Drain, RestartReadiness, SessionStatus};

    #[test]
    fn explicit_close_offers_resume() {
        assert_closed(
            CLOSE_REASON_EXPLICIT,
            StopCode::ClosedByOperator,
            resume(),
            &[AgentSession, SessionStatus],
        );
    }

    #[test]
    fn pty_exit_offers_resume() {
        assert_closed(
            CLOSE_REASON_PTY_EXIT,
            StopCode::ProcessExited,
            resume(),
            &[AgentSession, SessionStatus],
        );
    }

    #[test]
    fn poll_dead_offers_resume() {
        assert_closed(
            CLOSE_REASON_POLL_DEAD,
            StopCode::LivenessLost,
            resume(),
            &[AgentSession, SessionStatus],
        );
    }

    #[test]
    fn terminal_only_idle_is_terminal() {
        assert_closed(
            CLOSE_REASON_TERMINAL_ONLY_IDLE,
            StopCode::RestoredWithoutConversation,
            none(),
            &[AgentSession, SessionStatus],
        );
    }

    #[test]
    fn never_started_is_terminal() {
        assert_closed(
            CLOSE_REASON_NEVER_STARTED,
            StopCode::NeverStarted,
            none(),
            &[AgentSession],
        );
    }

    #[test]
    fn no_terminal_offers_resume() {
        assert_closed(
            CLOSE_REASON_NO_TERMINAL,
            StopCode::TerminalGone,
            resume(),
            &[AgentSession, SessionStatus],
        );
    }

    #[test]
    fn superseded_provisional_is_terminal() {
        assert_closed(
            CLOSE_REASON_SUPERSEDED,
            StopCode::SupersededProvisional,
            none(),
            &[AgentSession],
        );
    }

    #[test]
    fn superseded_by_terminal_reuse_offers_resume() {
        assert_closed(
            CLOSE_REASON_SUPERSEDED_TERMINAL_REUSE,
            StopCode::SupersededByNewSession,
            resume(),
            &[AgentSession, SessionStatus],
        );
    }

    #[test]
    fn worker_bind_failure_is_a_defect() {
        assert_closed(
            CLOSE_REASON_WORKER_BIND_FAILED,
            StopCode::WorkerBindFailed,
            NextAction::new(NextActionKind::ReportDefect),
            &[AgentSession],
        );
    }

    /// Every enumerated reason maps, and to a distinct code.
    #[test]
    fn the_enumeration_is_total_and_injective() {
        let reasons = [
            CLOSE_REASON_EXPLICIT,
            CLOSE_REASON_PTY_EXIT,
            CLOSE_REASON_POLL_DEAD,
            CLOSE_REASON_TERMINAL_ONLY_IDLE,
            CLOSE_REASON_NEVER_STARTED,
            CLOSE_REASON_NO_TERMINAL,
            CLOSE_REASON_SUPERSEDED,
            CLOSE_REASON_SUPERSEDED_TERMINAL_REUSE,
            CLOSE_REASON_WORKER_BIND_FAILED,
        ];
        let codes: std::collections::HashSet<StopCode> = reasons
            .iter()
            .map(|r| code_for_close_reason(r).unwrap_or_else(|| panic!("{r} unmapped")))
            .collect();
        assert_eq!(codes.len(), reasons.len());
        assert!(!codes.contains(&StopCode::Unknown));
    }

    /// The frontend's close-reason union and the backend's enumeration name
    /// the same two strings — a reason added to the TS union without a
    /// backend mapping would otherwise surface as `unknown`.
    #[test]
    fn frontend_close_reasons_are_enumerated() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../src/components/terminal/sessionRecordArgs.ts");
        let src = std::fs::read_to_string(&path).expect("sessionRecordArgs.ts");
        let decl = src
            .lines()
            .find(|l| l.contains("export type FrontendSessionCloseReason"))
            .expect("FrontendSessionCloseReason declaration");
        let members: Vec<&str> = decl.split('"').skip(1).step_by(2).collect();
        assert_eq!(members, [CLOSE_REASON_EXPLICIT, CLOSE_REASON_PTY_EXIT]);
        for m in members {
            assert!(code_for_close_reason(m).is_some(), "{m}");
        }
    }

    #[test]
    fn an_unenumerated_reason_is_unknown_with_the_raw_value() {
        let r = sr(&rec("closed", Some("user-closed")));
        assert_eq!(r.code, StopCode::Unknown);
        assert_eq!(r.detail.as_deref(), Some("user-closed"));
        assert_eq!(r.next_action.kind, NextActionKind::ReportDefect);
        assert_eq!(r.observed_at, iso(CLOSED_AT));
        assert_eq!(r.glossary_terms, [AgentSession, GlossaryTerm::Unknown]);
    }

    #[test]
    fn a_closed_record_without_a_reason_is_unknown_not_a_default() {
        let r = sr(&rec("closed", None));
        assert_eq!(r.code, StopCode::Unknown);
        assert!(r.detail.unwrap().contains("no close reason"));
        assert_eq!(r.next_action.kind, NextActionKind::ReportDefect);
    }

    #[test]
    fn an_unrecognised_state_is_unknown() {
        let r = sr(&rec("zombie", None));
        assert_eq!(r.code, StopCode::Unknown);
        assert!(r.detail.unwrap().contains("zombie"));
    }

    #[test]
    fn an_open_session_has_not_stopped() {
        let r = sr(&rec("open", None));
        assert_eq!(r.code, StopCode::Running);
        assert_eq!(r.next_action, NextAction::retry_later(None));
        assert_eq!(r.observed_at, iso(CLOSED_AT - 1_000));
        assert_eq!(r.glossary_terms, [SessionStatus, AgentSession]);
    }

    /// Finished is not closed: an open, finished session reports `finished`
    /// with its finish reason, and nothing is owed.
    #[test]
    fn an_open_finished_session_is_finished() {
        let mut r = rec("open", None);
        r.finished_at = Some(CLOSED_AT - 5_000);
        r.finish_reason = Some("unattended: all landed".to_string());
        let s = sr(&r);
        assert_eq!(s.code, StopCode::Finished);
        assert_eq!(s.next_action, none());
        assert_eq!(s.detail.as_deref(), Some("unattended: all landed"));
        assert_eq!(s.observed_at, iso(CLOSED_AT - 5_000));
    }

    /// A closed session whose work was finished owes nothing, whichever way
    /// it closed.
    #[test]
    fn a_finished_session_is_never_offered_a_resume() {
        let mut r = rec("closed", Some(CLOSE_REASON_PTY_EXIT));
        r.finished_at = Some(CLOSED_AT - 5_000);
        assert_eq!(sr(&r).next_action, none());
    }

    /// No provider ever confirmed a conversation there, so there is none to
    /// resume.
    #[test]
    fn an_unconfirmed_session_is_never_offered_a_resume() {
        let mut r = rec("closed", Some(CLOSE_REASON_POLL_DEAD));
        r.confirmed_at = None;
        assert_eq!(sr(&r).next_action, none());
    }

    /// No transcript on disk: nothing to resume, as the previous-sessions
    /// listing would also say.
    #[test]
    fn a_session_without_a_transcript_is_never_offered_a_resume() {
        let r = rec("closed", Some(CLOSE_REASON_PTY_EXIT));
        assert_eq!(stop_reason(&r, false).next_action, none());
    }

    /// The wind-down marker is sticky; once the session is open again (resumed
    /// under the same id) it is not the stop.
    #[test]
    fn a_resumed_wound_down_session_is_running() {
        let mut r = rec("open", None);
        r.finished_at = None;
        r.wind_down_outcome = Some(WIND_DOWN_CLOSED.to_string());
        r.wind_down_at = Some(CLOSED_AT - 2_000);
        assert_eq!(sr(&r).code, StopCode::Running);
        // Closed again, but un-finished: the close reason decides.
        let mut r = rec("closed", Some(CLOSE_REASON_PTY_EXIT));
        r.wind_down_outcome = Some(WIND_DOWN_CLOSED.to_string());
        assert_eq!(sr(&r).code, StopCode::ProcessExited);
    }

    #[test]
    fn resume_falls_back_to_the_config_dir_launcher() {
        let mut r = rec("closed", Some(CLOSE_REASON_EXPLICIT));
        r.account_wrapper = None;
        r.config_dir = None;
        assert_eq!(
            sr(&r).next_action.target.as_deref(),
            Some("claude --resume sess-1")
        );
    }

    #[test]
    fn a_non_claude_provider_is_pointed_at_the_previous_sessions_page() {
        let mut r = rec("closed", Some(CLOSE_REASON_PTY_EXIT));
        r.provider = "gemini".to_string();
        assert_eq!(
            sr(&r).next_action,
            NextAction::new(NextActionKind::OpenPage).with_target(PREVIOUS_SESSIONS_PAGE)
        );
    }

    /// A wind-down in which the agent left is the stop, whatever the pane's
    /// later close recorded; one in which it did not leave is not a stop.
    #[test]
    fn wind_down_outcomes() {
        for outcome in [
            WIND_DOWN_CLOSED,
            WIND_DOWN_CLOSE_REFUSED,
            WIND_DOWN_CLOSE_UNKNOWN,
        ] {
            let mut r = rec("closed", Some(CLOSE_REASON_EXPLICIT));
            r.finished_at = Some(CLOSED_AT - 9_000);
            r.wind_down_outcome = Some(outcome.to_string());
            r.wind_down_at = Some(CLOSED_AT - 2_000);
            let s = sr(&r);
            assert_eq!(s.code, StopCode::WoundDown, "{outcome}");
            assert_eq!(s.next_action, none());
            assert_eq!(s.glossary_terms, [Drain, SessionStatus, RestartReadiness]);
            assert_eq!(s.observed_at, iso(CLOSED_AT - 2_000));
        }
        for outcome in [WIND_DOWN_EXIT_STUCK, WIND_DOWN_NOT_ATTEMPTED] {
            let mut r = rec("open", None);
            r.finished_at = Some(CLOSED_AT - 9_000);
            r.wind_down_outcome = Some(outcome.to_string());
            assert_eq!(sr(&r).code, StopCode::Finished, "{outcome}");
        }
    }

    #[test]
    fn a_closed_record_without_closed_at_states_no_time() {
        let mut r = rec("closed", Some(CLOSE_REASON_PTY_EXIT));
        r.closed_at = None;
        assert_eq!(sr(&r).observed_at, None);
    }

    #[test]
    fn wire_shape() {
        let v = serde_json::to_value(sr(&rec("closed", Some(CLOSE_REASON_POLL_DEAD)))).unwrap();
        assert_eq!(v["code"], "liveness_lost");
        assert_eq!(
            v["glossary_terms"],
            serde_json::json!(["agent_session", "session_status"])
        );
        assert_eq!(v["next_action"]["kind"], "run_command");
        assert_eq!(v["next_action"]["target"], "clg --resume sess-1");
        assert_eq!(v["source"], SOURCE_LIFECYCLE_REGISTRY);
        assert_eq!(v["recorded"]["state"], "closed");
        assert!(v["observed_at"].as_str().unwrap().starts_with("2026-"));
        for code in StopCode::ALL {
            assert_eq!(serde_json::to_value(code).unwrap(), code.as_str());
            assert!(!code.headline().is_empty());
            assert!(!code.glossary_terms().is_empty());
        }
    }
}
