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
//! 1. **`wind_down_outcome`** — the wind-down executor asked a FINISHED
//!    session to `/exit`, on one of two arms recorded in `wind_down_arm`: the
//!    DRAIN arm ([`WIND_DOWN_ARM_DRAIN`], and every outcome recorded before
//!    the arm field existed) or the `finished_close` arm
//!    ([`WIND_DOWN_ARM_FINISHED_CLOSE`]). The three outcomes in which the agent
//!    left ([`WIND_DOWN_CLOSED`], [`WIND_DOWN_CLOSE_REFUSED`],
//!    [`WIND_DOWN_CLOSE_UNKNOWN`]) are the stop, whatever `close_reason` the
//!    pane's later close wrote; an arm this version does not name is
//!    `unknown`. The three in which it did not ([`WIND_DOWN_EXIT_STUCK`],
//!    [`WIND_DOWN_NOT_ATTEMPTED`], [`WIND_DOWN_NO_CLOSE_AUTHORITY`]) are not a
//!    stop and fall through.
//! 2. **`state`** — `"open"` is not stopped: [`StopCode::Running`], or
//!    [`StopCode::Finished`] when the work axis says the work is done.
//! 3. **`close_reason`** on a `"closed"` record — the ten `CLOSE_REASON_*`
//!    values in `session_lifecycle_store`, one code each. The census test
//!    below pins every production site that writes one.
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
//! (`confirmed_at`), its transcript is on disk ([`probe_resume`] uses
//! `past_sessions::resolve_transcript_path`, the same test the previous-sessions
//! listing uses for "restorable"), and the work axis does not already say it is
//! finished (`finished_at`). A finished session's stop needs nothing
//! (`none_terminal`); an unconfirmed one, or one without a transcript, has no
//! conversation to resume.
//!
//! The command line is never built here. It is
//! `session_ledger::resume_command_for` — the one resume-line builder, shared
//! with the ledger report and Previous sessions — fed the resolved account and
//! resume directory exactly as Previous sessions feeds it
//! (`past_sessions::resume_command_for_record`). That builder yields NO line
//! for an unknown account (a `--resume` under the wrong account finds nothing)
//! or an unsafe token; the answer is then the Previous sessions page, never a
//! guessed line.
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

use crate::session::session_lifecycle_store::{
    TerminalSessionRecord, CLOSE_REASON_EXPLICIT, CLOSE_REASON_LAUNCH_FAILED,
    CLOSE_REASON_NEVER_STARTED, CLOSE_REASON_NO_TERMINAL, CLOSE_REASON_POLL_DEAD,
    CLOSE_REASON_PTY_EXIT, CLOSE_REASON_SUPERSEDED, CLOSE_REASON_SUPERSEDED_TERMINAL_REUSE,
    CLOSE_REASON_TERMINAL_ONLY_IDLE, CLOSE_REASON_WORKER_BIND_FAILED, DEFAULT_PROVIDER,
    WIND_DOWN_ARM_DRAIN, WIND_DOWN_ARM_FINISHED_CLOSE, WIND_DOWN_CLOSED, WIND_DOWN_CLOSE_REFUSED,
    WIND_DOWN_CLOSE_UNKNOWN,
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
    /// The `finished_close` arm closed this finished, idle session's window —
    /// no drain involved.
    ClosedAfterFinishing,
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
    /// [`CLOSE_REASON_LAUNCH_FAILED`] — the session's terminal died and the
    /// runner holds positive evidence its Claude CLI never launched there.
    LaunchFailed,
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
        StopCode::ClosedAfterFinishing,
        StopCode::ClosedByOperator,
        StopCode::ProcessExited,
        StopCode::LivenessLost,
        StopCode::RestoredWithoutConversation,
        StopCode::NeverStarted,
        StopCode::TerminalGone,
        StopCode::SupersededProvisional,
        StopCode::SupersededByNewSession,
        StopCode::WorkerBindFailed,
        StopCode::LaunchFailed,
        StopCode::Unknown,
    ];

    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            StopCode::Running => "running",
            StopCode::Finished => "finished",
            StopCode::WoundDown => "wound_down",
            StopCode::ClosedAfterFinishing => "closed_after_finishing",
            StopCode::ClosedByOperator => "closed_by_operator",
            StopCode::ProcessExited => "process_exited",
            StopCode::LivenessLost => "liveness_lost",
            StopCode::RestoredWithoutConversation => "restored_without_conversation",
            StopCode::NeverStarted => "never_started",
            StopCode::TerminalGone => "terminal_gone",
            StopCode::SupersededProvisional => "superseded_provisional",
            StopCode::SupersededByNewSession => "superseded_by_new_session",
            StopCode::WorkerBindFailed => "worker_bind_failed",
            StopCode::LaunchFailed => "launch_failed",
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
            StopCode::ClosedAfterFinishing => {
                "The session had finished its work and sat idle, so its window was closed"
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
            StopCode::LaunchFailed => {
                "The session's terminal ended without the Claude CLI ever launching in it"
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
            StopCode::ClosedAfterFinishing => vec![SessionStatus, AgentSession],
            StopCode::ClosedByOperator
            | StopCode::ProcessExited
            | StopCode::LivenessLost
            | StopCode::RestoredWithoutConversation
            | StopCode::TerminalGone
            | StopCode::SupersededByNewSession
            | StopCode::LaunchFailed => vec![AgentSession, SessionStatus],
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_down_arm: Option<String>,
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

/// What the disk says about resuming one record: is its transcript there, and
/// the copy-able resume line `session_ledger::resume_command_for` built for it
/// (`None` when that builder refuses — an unknown account, an unsafe token —
/// or for a provider whose layout it does not know).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResumeProbe {
    pub transcript_exists: bool,
    pub resume_command: Option<String>,
}

/// Probe the disk for [`stop_reason`]'s resume offer. Only a closed,
/// confirmed, unfinished record can be offered one, so nothing else touches
/// the filesystem. A provider other than Claude is reported as present with
/// no command, so the answer points at the Previous sessions page (which
/// judges restorability itself) instead of claiming nothing is owed.
pub fn probe_resume(rec: &TerminalSessionRecord) -> ResumeProbe {
    if rec.state != "closed" || rec.confirmed_at.is_none() || rec.finished_at.is_some() {
        return ResumeProbe::default();
    }
    if rec.provider != DEFAULT_PROVIDER {
        return ResumeProbe {
            transcript_exists: true,
            resume_command: None,
        };
    }
    let transcript_exists = crate::session::past_sessions::resolve_transcript_path(
        rec.config_dir.as_deref(),
        rec.working_dir.as_deref(),
        &rec.claude_session_id,
    )
    .is_some();
    if !transcript_exists {
        return ResumeProbe::default();
    }
    ResumeProbe {
        transcript_exists,
        resume_command: crate::session::past_sessions::resume_command_for_record(
            rec.config_dir.as_deref(),
            rec.working_dir.as_deref(),
            &rec.claude_session_id,
        ),
    }
}

/// Resume the conversation, when the record shows there is one to resume (a
/// confirmed provider start and a transcript on disk) and its work is not
/// already finished; otherwise nothing is owed. The command is the probe's
/// `resume_command_for` line; with none, the Previous sessions page — never a
/// line built here.
fn resume_or_nothing(rec: &TerminalSessionRecord, probe: &ResumeProbe) -> NextAction {
    if rec.finished_at.is_some() || rec.confirmed_at.is_none() || !probe.transcript_exists {
        return NextAction::new(NextActionKind::NoneTerminal);
    }
    match (&probe.resume_command, rec.provider == DEFAULT_PROVIDER) {
        (Some(line), true) => NextAction::new(NextActionKind::RunCommand).with_target(line.clone()),
        _ => NextAction::new(NextActionKind::OpenPage).with_target(PREVIOUS_SESSIONS_PAGE),
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
        CLOSE_REASON_LAUNCH_FAILED => StopCode::LaunchFailed,
        _ => return None,
    })
}

/// Derive the stop reason for one record. Pure: `probe` is the caller's
/// [`probe_resume`] of the record.
pub fn stop_reason(rec: &TerminalSessionRecord, probe: &ResumeProbe) -> StopReason {
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
    // WIND_DOWN_EXIT_STUCK / WIND_DOWN_NOT_ATTEMPTED /
    // WIND_DOWN_NO_CLOSE_AUTHORITY are recorded but are not a stop — the agent
    // is still there — so they fall through to `state`.

    let finish_detail = rec.finish_reason.clone();
    let wound_down_at = rec.wind_down_at.or(rec.closed_at);
    let (code, detail, next_action, observed_ms) = if wound_down {
        match rec.wind_down_arm.as_deref() {
            // `None`: every outcome recorded before the arm field existed was
            // the drain arm's (`TerminalSessionRecord::wind_down_arm`).
            None | Some(WIND_DOWN_ARM_DRAIN) => (
                StopCode::WoundDown,
                finish_detail,
                NextAction::new(NextActionKind::NoneTerminal),
                wound_down_at,
            ),
            Some(WIND_DOWN_ARM_FINISHED_CLOSE) => (
                StopCode::ClosedAfterFinishing,
                finish_detail,
                NextAction::new(NextActionKind::NoneTerminal),
                wound_down_at,
            ),
            Some(other) => (
                StopCode::Unknown,
                Some(format!("unrecognised wind-down arm `{other}`")),
                NextAction::new(NextActionKind::ReportDefect),
                wound_down_at,
            ),
        }
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
                            | StopCode::SupersededByNewSession
                            // Nothing ran, so the conversation is exactly as
                            // it was; resuming is also how a reader meets the
                            // launch failure in their own shell.
                            | StopCode::LaunchFailed => resume_or_nothing(rec, probe),
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
            wind_down_arm: rec.wind_down_arm.clone(),
            finished: rec.finished_at.is_some(),
            finish_reason: rec.finish_reason.clone(),
            confirmed: rec.confirmed_at.is_some(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::session_lifecycle_store::{
        WIND_DOWN_EXIT_STUCK, WIND_DOWN_NOT_ATTEMPTED, WIND_DOWN_NO_CLOSE_AUTHORITY,
    };

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
            awaiting_account_since: None,
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
            wind_down_arm: None,
            finish_reason: None,
            finish_synced: false,
            finish_coord_session_id: None,
            finish_coord_readdressed: false,
            spawn_device_default: None,
            adopted_from: None,
            last_session_start_at: None,
            gate_id: None,
            gate_consuming_device_id: None,
            gate_bound_boot_ms: None,
        }
    }

    /// The common case: a transcript is on disk.
    fn sr(r: &TerminalSessionRecord) -> StopReason {
        stop_reason(r, &probe())
    }

    /// A line as `session_ledger::resume_command_for` renders one.
    const RESUME_LINE: &str = "cd \"/repo\" && CLAUDE_CONFIG_DIR=\"/cfg\" claude --resume sess-1";

    fn probe() -> ResumeProbe {
        ResumeProbe {
            transcript_exists: true,
            resume_command: Some(RESUME_LINE.to_string()),
        }
    }

    fn resume() -> NextAction {
        NextAction::new(NextActionKind::RunCommand).with_target(RESUME_LINE)
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

    #[test]
    fn launch_failure_offers_resume() {
        assert_closed(
            CLOSE_REASON_LAUNCH_FAILED,
            StopCode::LaunchFailed,
            resume(),
            &[AgentSession, SessionStatus],
        );
        // The launch-failure module records this very constant.
        assert_eq!(
            crate::terminal::launch_failure::LAUNCH_FAILED,
            CLOSE_REASON_LAUNCH_FAILED
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
            CLOSE_REASON_LAUNCH_FAILED,
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
        // From the declaration to its terminating `;`, so a union a formatter
        // wraps over several lines is read whole.
        let (_, tail) = src
            .split_once("export type FrontendSessionCloseReason")
            .expect("FrontendSessionCloseReason declaration");
        let (decl, _) = tail.split_once(';').expect("terminating `;`");
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
        assert_eq!(stop_reason(&r, &ResumeProbe::default()).next_action, none());
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

    /// When `resume_command_for` yields no line (an unknown account, an
    /// unsafe token), the answer is the Previous sessions page — never a line
    /// assembled here, and never the account's CLI wrapper.
    #[test]
    fn no_resume_line_points_at_the_previous_sessions_page() {
        let mut r = rec("closed", Some(CLOSE_REASON_EXPLICIT));
        r.account_wrapper = Some("clg".to_string());
        let s = stop_reason(
            &r,
            &ResumeProbe {
                transcript_exists: true,
                resume_command: None,
            },
        );
        assert_eq!(
            s.next_action,
            NextAction::new(NextActionKind::OpenPage).with_target(PREVIOUS_SESSIONS_PAGE)
        );
        assert!(!s.next_action_text.contains("--resume"));
    }

    /// The probe answers without touching the filesystem for a record that
    /// can never be offered a resume, defers to the Previous sessions page for
    /// a provider whose transcript layout it does not know, and for an
    /// unknown account (no recorded config dir, no transcript found) offers
    /// nothing rather than a guessed line.
    #[test]
    fn the_resume_probe_is_scoped_and_never_guesses() {
        // The transcript scan reads the account roster; keep it off the
        // operator's real config.
        let _amb = crate::test_env::isolated_ambient();
        let mut r = rec("closed", Some(CLOSE_REASON_PTY_EXIT));
        r.provider = "gemini".to_string();
        assert_eq!(
            probe_resume(&r),
            ResumeProbe {
                transcript_exists: true,
                resume_command: None
            }
        );
        r.state = "open".to_string();
        assert_eq!(probe_resume(&r), ResumeProbe::default());
        r.state = "closed".to_string();
        r.finished_at = Some(1);
        assert_eq!(probe_resume(&r), ResumeProbe::default());
        r.finished_at = None;
        r.confirmed_at = None;
        assert_eq!(probe_resume(&r), ResumeProbe::default());
        // A Claude session whose transcript is nowhere on disk.
        let mut r = rec("closed", Some(CLOSE_REASON_PTY_EXIT));
        r.claude_session_id = "c5-no-such-session-0b0e9a40-6b1f-4c3e".to_string();
        r.config_dir = None;
        assert_eq!(probe_resume(&r), ResumeProbe::default());
        // The shared builder itself: no account → no line.
        assert_eq!(
            crate::session::past_sessions::resume_command_for_record(
                None,
                Some("/repo"),
                "c5-no-such-session-0b0e9a40-6b1f-4c3e",
            ),
            None
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
        for outcome in [
            WIND_DOWN_EXIT_STUCK,
            WIND_DOWN_NOT_ATTEMPTED,
            WIND_DOWN_NO_CLOSE_AUTHORITY,
        ] {
            let mut r = rec("open", None);
            r.finished_at = Some(CLOSED_AT - 9_000);
            r.wind_down_outcome = Some(outcome.to_string());
            assert_eq!(sr(&r).code, StopCode::Finished, "{outcome}");
        }
    }

    /// The arm decides which code a wind-down stop reads as: the drain arm
    /// (and an outcome recorded before the arm field existed) is
    /// `wound_down`, the `finished_close` arm is `closed_after_finishing`, and
    /// an arm this version does not name is `unknown` with the raw value.
    #[test]
    fn the_wind_down_arm_decides_the_code() {
        for (arm, code) in [
            (None, StopCode::WoundDown),
            (Some(WIND_DOWN_ARM_DRAIN), StopCode::WoundDown),
            (
                Some(WIND_DOWN_ARM_FINISHED_CLOSE),
                StopCode::ClosedAfterFinishing,
            ),
        ] {
            let mut r = rec("closed", Some(CLOSE_REASON_PTY_EXIT));
            r.finished_at = Some(CLOSED_AT - 9_000);
            r.wind_down_outcome = Some(WIND_DOWN_CLOSED.to_string());
            r.wind_down_arm = arm.map(str::to_string);
            r.wind_down_at = Some(CLOSED_AT - 2_000);
            let s = sr(&r);
            assert_eq!(s.code, code, "{arm:?}");
            assert_eq!(s.next_action, none(), "{arm:?}");
            assert_eq!(s.observed_at, iso(CLOSED_AT - 2_000));
            assert_eq!(s.recorded.wind_down_arm.as_deref(), arm);
        }
        let mut r = rec("closed", Some(CLOSE_REASON_PTY_EXIT));
        r.finished_at = Some(CLOSED_AT - 9_000);
        r.wind_down_outcome = Some(WIND_DOWN_CLOSED.to_string());
        r.wind_down_arm = Some("some_future_arm".to_string());
        let s = sr(&r);
        assert_eq!(s.code, StopCode::Unknown);
        assert!(s.detail.unwrap().contains("some_future_arm"));
        assert_eq!(s.next_action.kind, NextActionKind::ReportDefect);
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
        assert_eq!(v["next_action"]["target"], RESUME_LINE);
        assert_eq!(v["source"], SOURCE_LIFECYCLE_REGISTRY);
        assert_eq!(v["recorded"]["state"], "closed");
        assert!(v["observed_at"].as_str().unwrap().starts_with("2026-"));
        for code in StopCode::ALL {
            assert_eq!(serde_json::to_value(code).unwrap(), code.as_str());
            assert!(!code.headline().is_empty());
            assert!(!code.glossary_terms().is_empty());
        }
    }

    // -----------------------------------------------------------------------
    // Census: every close reason a production writer records maps to a code.
    // -----------------------------------------------------------------------

    /// What one production close-reason write site records.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Recorded {
        /// A string literal, e.g. `record_close(id, "poll-dead")`.
        Literal(String),
        /// A `CLOSE_REASON_*` constant, by name.
        Const(String),
        /// Anything else — a variable or a call — rendered for the forwarder
        /// table.
        Indirect(String),
        /// A copy of another record's `close_reason` field, or `None`: no new
        /// value.
        NotAMint,
        /// A macro whose tokens name `record_close` / `record_close_checked`
        /// but do not parse as expressions — the census cannot see inside it.
        UnparsedMacro,
    }

    /// One write site: the file, the enclosing fn, and what it records.
    #[derive(Debug, Clone)]
    struct Site {
        file: String,
        func: String,
        recorded: Recorded,
    }

    fn render(e: &syn::Expr) -> String {
        match e {
            syn::Expr::Path(p) => p
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>()
                .join("::"),
            syn::Expr::Field(f) => {
                let member = match &f.member {
                    syn::Member::Named(i) => i.to_string(),
                    syn::Member::Unnamed(i) => i.index.to_string(),
                };
                format!("{}.{member}", render(&f.base))
            }
            syn::Expr::MethodCall(m) => format!(
                "{}.{}({})",
                render(&m.receiver),
                m.method,
                m.args.iter().map(render).collect::<Vec<_>>().join(", ")
            ),
            syn::Expr::Call(c) => format!(
                "{}({})",
                render(&c.func),
                c.args.iter().map(render).collect::<Vec<_>>().join(", ")
            ),
            syn::Expr::Reference(r) => render(&r.expr),
            syn::Expr::Paren(p) => render(&p.expr),
            syn::Expr::Macro(m) => format!(
                "{}!(..)",
                m.mac
                    .path
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect::<Vec<_>>()
                    .join("::")
            ),
            _ => "<expr>".to_string(),
        }
    }

    fn is_close_reason_field(e: &syn::Expr) -> bool {
        matches!(e, syn::Expr::Field(f)
            if matches!(&f.member, syn::Member::Named(n) if n == "close_reason"))
    }

    /// A COPY of another record's value: exactly a `<expr>.close_reason`
    /// field read, optionally behind `&`, parens, `.clone()`, `.as_deref()`
    /// or `.as_ref()`. A local merely NAMED `close_reason`, or a call whose
    /// name contains it, is not a copy — it is [`Recorded::Indirect`]. Only
    /// trusted where a value moves between records (an assignment or a struct
    /// field); as a `record_close` argument it is a NEW close — see
    /// [`classify_close_arg`].
    fn is_close_reason_copy(e: &syn::Expr) -> bool {
        match e {
            syn::Expr::Reference(r) => is_close_reason_copy(&r.expr),
            syn::Expr::Paren(p) => is_close_reason_copy(&p.expr),
            syn::Expr::MethodCall(m)
                if m.args.is_empty()
                    && ["clone", "as_deref", "as_ref"].contains(&m.method.to_string().as_str()) =>
            {
                is_close_reason_copy(&m.receiver)
            }
            other => is_close_reason_field(other),
        }
    }

    /// Peel `Some(..)`, `&..`, `.to_string()`, `.into()`, `.to_owned()`,
    /// `String::from(..)` and a closure (`get_or_insert_with(|| ..)`) down to
    /// the value actually recorded.
    fn classify(e: &syn::Expr) -> Recorded {
        if is_close_reason_copy(e) {
            return Recorded::NotAMint;
        }
        match e {
            syn::Expr::Reference(r) => classify(&r.expr),
            syn::Expr::Paren(p) => classify(&p.expr),
            syn::Expr::Closure(c) => classify(&c.body),
            syn::Expr::MethodCall(m)
                if m.args.is_empty()
                    && ["to_string", "into", "to_owned"]
                        .contains(&m.method.to_string().as_str()) =>
            {
                classify(&m.receiver)
            }
            syn::Expr::Call(c) if c.args.len() == 1 => {
                let f = render(&c.func);
                if f == "Some" || f == "String::from" {
                    classify(&c.args[0])
                } else {
                    Recorded::Indirect(render(e))
                }
            }
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => Recorded::Literal(s.value()),
            syn::Expr::MethodCall(m)
                if m.args.is_empty()
                    && m.method == "as_str"
                    && typed_close_reason(&m.receiver).is_some() =>
            {
                classify(&m.receiver)
            }
            syn::Expr::Path(_) if typed_close_reason(e).is_some() => {
                Recorded::Const(typed_close_reason(e).unwrap_or_default())
            }
            syn::Expr::Path(p) => {
                let name = p
                    .path
                    .segments
                    .last()
                    .map(|s| s.ident.to_string())
                    .unwrap_or_default();
                if name == "None" {
                    Recorded::NotAMint
                } else if name.starts_with("CLOSE_REASON_") {
                    Recorded::Const(name)
                } else {
                    Recorded::Indirect(render(e))
                }
            }
            _ => Recorded::Indirect(render(e)),
        }
    }

    /// `CloseReason::<Variant>` (any path prefix) rendered as
    /// `"CloseReason::<Variant>"` — a variant of the typed set every
    /// production close writes (`crate::session::close_reason`), or `None`
    /// for any other expression.
    fn typed_close_reason(e: &syn::Expr) -> Option<String> {
        let syn::Expr::Path(p) = e else { return None };
        let segs: Vec<String> = p
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();
        match segs.as_slice() {
            [.., ty, variant] if ty == "CloseReason" => Some(format!("CloseReason::{variant}")),
            _ => None,
        }
    }

    /// The wire string a `"CloseReason::<Variant>"` rendering names, or
    /// `None` for a variant the typed set does not have.
    fn typed_close_reason_wire(rendered: &str) -> Option<&'static str> {
        let variant = rendered.strip_prefix("CloseReason::")?;
        crate::session::close_reason::CloseReason::ALL
            .into_iter()
            .find(|r| format!("{r:?}") == variant)
            .map(|r| r.as_str())
    }

    /// The reason argument of `record_close` / `record_close_checked`. A
    /// field read there is never a copy — whatever struct it came off (a
    /// deserialized request, say), passing it to `record_close` records a new
    /// close — so it is [`Recorded::Indirect`] and must be a named forwarder.
    fn classify_close_arg(e: &syn::Expr) -> Recorded {
        if is_close_reason_copy(e) {
            Recorded::Indirect(render(e))
        } else {
            classify(e)
        }
    }

    /// `cfg(test)` or `cfg(all(.., test, ..))` — and nothing wider: a
    /// `cfg(any(test, feature = ".."))` item also compiles into production.
    fn cfg_is_test_only(meta: &syn::Meta) -> bool {
        use syn::punctuated::Punctuated;
        match meta {
            syn::Meta::Path(p) => p.is_ident("test"),
            syn::Meta::List(l) if l.path.is_ident("all") => l
                .parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
                .map(|ms| ms.iter().any(cfg_is_test_only))
                .unwrap_or(false),
            _ => false,
        }
    }

    fn is_test_only(attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|a| {
            let path = a.path();
            if path.segments.last().is_some_and(|s| s.ident == "test") {
                return true;
            }
            path.is_ident("cfg")
                && a.parse_args::<syn::Meta>()
                    .is_ok_and(|m| cfg_is_test_only(&m))
        })
    }

    /// Does `tokens` contain one of `names` as an IDENT (not inside a string
    /// literal, so a log line saying "record_close" does not count)?
    fn tokens_name(tokens: &proc_macro2::TokenStream, names: &[&str]) -> bool {
        tokens.clone().into_iter().any(|t| match t {
            proc_macro2::TokenTree::Ident(i) => names.iter().any(|n| i == n),
            proc_macro2::TokenTree::Group(g) => tokens_name(&g.stream(), names),
            _ => false,
        })
    }

    /// Every fn that closes a lifecycle record with a caller-chosen reason, with
    /// the reason's position counted from the END of the argument list — so
    /// the method form and its UFCS form (receiver as the first argument) read
    /// the same argument:
    /// - `record_close(id, reason)`;
    /// - `record_close_checked(id, terminal, reason)`;
    /// - `record_close_gated(shutting_down, id, terminal, reason, created_at)`
    ///   — the shutdown-latched, launch-failure-aware door;
    /// - `commands::terminal::record_close_for(tm, store, id, terminal,
    ///   reason)` — the Tauri close command's body.
    const RECORD_CLOSE_FNS: [(&str, usize); 4] = [
        ("record_close", 0),
        ("record_close_checked", 0),
        ("record_close_gated", 1),
        ("record_close_for", 0),
    ];
    const RECORD_CLOSE_FN_NAMES: [&str; 4] = [
        RECORD_CLOSE_FNS[0].0,
        RECORD_CLOSE_FNS[1].0,
        RECORD_CLOSE_FNS[2].0,
        RECORD_CLOSE_FNS[3].0,
    ];

    /// The reason argument of a [`RECORD_CLOSE_FNS`] call named `name`, or
    /// `None` when `name` is not one (or the call has too few arguments).
    fn close_reason_arg<'a>(
        name: &str,
        args: &'a syn::punctuated::Punctuated<syn::Expr, syn::Token![,]>,
    ) -> Option<&'a syn::Expr> {
        let (_, from_end) = RECORD_CLOSE_FNS.iter().find(|(n, _)| *n == name)?;
        let i = args.len().checked_sub(1 + from_end)?;
        args.iter().nth(i)
    }
    /// `Option<String>` methods that can store a new value in `close_reason`.
    const OPTION_MUTATORS: [&str; 4] = ["replace", "insert", "get_or_insert", "get_or_insert_with"];

    struct CloseWriteCensus {
        file: String,
        /// The enclosing `impl` / `trait` type, so a method is keyed
        /// `Type::method`.
        type_stack: Vec<String>,
        fn_stack: Vec<String>,
        sites: Vec<Site>,
    }

    impl CloseWriteCensus {
        fn new(file: &str) -> Self {
            Self {
                file: file.to_string(),
                type_stack: Vec::new(),
                fn_stack: Vec::new(),
                sites: Vec::new(),
            }
        }
        fn method_name(&self, ident: &syn::Ident) -> String {
            match self.type_stack.last() {
                Some(t) => format!("{t}::{ident}"),
                None => ident.to_string(),
            }
        }
        fn push(&mut self, recorded: Recorded) {
            self.sites.push(Site {
                file: self.file.clone(),
                func: self.fn_stack.last().cloned().unwrap_or_default(),
                recorded,
            });
        }
    }

    impl<'ast> syn::visit::Visit<'ast> for CloseWriteCensus {
        fn visit_item_mod(&mut self, i: &'ast syn::ItemMod) {
            if !is_test_only(&i.attrs) {
                syn::visit::visit_item_mod(self, i);
            }
        }
        fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
            if !is_test_only(&i.attrs) {
                let ty = match &*i.self_ty {
                    syn::Type::Path(p) => p
                        .path
                        .segments
                        .last()
                        .map(|s| s.ident.to_string())
                        .unwrap_or_default(),
                    _ => "<impl>".to_string(),
                };
                self.type_stack.push(ty);
                syn::visit::visit_item_impl(self, i);
                self.type_stack.pop();
            }
        }
        fn visit_item_trait(&mut self, i: &'ast syn::ItemTrait) {
            if !is_test_only(&i.attrs) {
                self.type_stack.push(i.ident.to_string());
                syn::visit::visit_item_trait(self, i);
                self.type_stack.pop();
            }
        }
        fn visit_item_fn(&mut self, i: &'ast syn::ItemFn) {
            if !is_test_only(&i.attrs) {
                // A free fn — even one nested inside a method — is keyed by
                // its own name, not its parent's.
                let outer = std::mem::take(&mut self.type_stack);
                self.fn_stack.push(i.sig.ident.to_string());
                syn::visit::visit_item_fn(self, i);
                self.fn_stack.pop();
                self.type_stack = outer;
            }
        }
        fn visit_impl_item_fn(&mut self, i: &'ast syn::ImplItemFn) {
            if !is_test_only(&i.attrs) {
                let name = self.method_name(&i.sig.ident);
                self.fn_stack.push(name);
                syn::visit::visit_impl_item_fn(self, i);
                self.fn_stack.pop();
            }
        }
        fn visit_trait_item_fn(&mut self, i: &'ast syn::TraitItemFn) {
            if !is_test_only(&i.attrs) {
                let name = self.method_name(&i.sig.ident);
                self.fn_stack.push(name);
                syn::visit::visit_trait_item_fn(self, i);
                self.fn_stack.pop();
            }
        }
        // `store.record_close(id, r)` and the Option mutators on the field:
        // `x.close_reason.replace(r)` / `.insert(r)` / `.get_or_insert(r)` /
        // `.get_or_insert_with(|| r)`.
        fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
            let method = m.method.to_string();
            if RECORD_CLOSE_FN_NAMES.contains(&method.as_str()) {
                if let Some(reason) = close_reason_arg(&method, &m.args) {
                    self.push(classify_close_arg(reason));
                }
            } else if OPTION_MUTATORS.contains(&method.as_str())
                && is_close_reason_field(&m.receiver)
            {
                if let Some(v) = m.args.first() {
                    self.push(classify(v));
                }
            }
            syn::visit::visit_expr_method_call(self, m);
        }
        // UFCS `SessionLifecycleStore::record_close(&s, id, r)` and
        // `std::mem::replace(&mut x.close_reason, r)`.
        fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
            if let syn::Expr::Path(p) = &*c.func {
                let last = p
                    .path
                    .segments
                    .last()
                    .map(|s| s.ident.to_string())
                    .unwrap_or_default();
                if RECORD_CLOSE_FN_NAMES.contains(&last.as_str()) {
                    if let Some(reason) = close_reason_arg(&last, &c.args) {
                        self.push(classify_close_arg(reason));
                    }
                } else if last == "replace"
                    && c.args.len() == 2
                    && matches!(&c.args[0], syn::Expr::Reference(r) if is_close_reason_field(&r.expr))
                {
                    self.push(classify(&c.args[1]));
                }
            }
            syn::visit::visit_expr_call(self, c);
        }
        // `x.close_reason = r` and `*x.close_reason = r`.
        fn visit_expr_assign(&mut self, a: &'ast syn::ExprAssign) {
            let target = match &*a.left {
                syn::Expr::Unary(u) if matches!(u.op, syn::UnOp::Deref(_)) => &*u.expr,
                other => other,
            };
            if is_close_reason_field(target) {
                self.push(classify(&a.right));
            }
            syn::visit::visit_expr_assign(self, a);
        }
        // `close_reason: r` in a struct literal. The field-init shorthand
        // (`close_reason,`) is a LOCAL named `close_reason` — whatever bound
        // it — so it is Indirect, like any other local.
        // `HeldClose { reason: r, .. }` — a close a respawn HOLDS
        // (`SessionLifecycleStore::hold_close_if_moving`) and later replays
        // into `record_close_checked`. Its `reason` field is a close reason
        // in transit, so building one is a write site like any other.
        fn visit_expr_struct(&mut self, e: &'ast syn::ExprStruct) {
            if e.path
                .segments
                .last()
                .is_some_and(|s| s.ident == "HeldClose")
            {
                for fv in &e.fields {
                    if matches!(&fv.member, syn::Member::Named(n) if n == "reason") {
                        self.push(classify_close_arg(&fv.expr));
                    }
                }
            }
            syn::visit::visit_expr_struct(self, e);
        }
        fn visit_field_value(&mut self, fv: &'ast syn::FieldValue) {
            if matches!(&fv.member, syn::Member::Named(n) if n == "close_reason") {
                self.push(classify(&fv.expr));
            }
            syn::visit::visit_field_value(self, fv);
        }
        // Macro bodies are opaque token streams to `syn`. One that names a
        // record_close fn is parsed as a comma-separated expression list and
        // visited; if it does not parse, that is a site the census cannot see,
        // and it fails the census rather than passing silently.
        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            if tokens_name(&mac.tokens, &RECORD_CLOSE_FN_NAMES)
                || tokens_name(&mac.tokens, &["close_reason"])
            {
                use syn::punctuated::Punctuated;
                match mac.parse_body_with(Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated)
                {
                    Ok(exprs) => {
                        for e in &exprs {
                            self.visit_expr(e);
                        }
                    }
                    Err(_) if tokens_name(&mac.tokens, &RECORD_CLOSE_FN_NAMES) => {
                        self.push(Recorded::UnparsedMacro);
                    }
                    // Names only `close_reason` and does not parse as
                    // expressions — a tracing field (`close_reason = ?x`) or
                    // `json!` object. Known blind spot; see the test doc.
                    Err(_) => {}
                }
            }
            syn::visit::visit_macro(self, mac);
        }
    }

    /// Every `CLOSE_REASON_*` constant `session_lifecycle_store` declares,
    /// read from its source so a constant added there without a mapping here
    /// fails this file's tests.
    fn declared_close_reason_consts(src: &syn::File) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for item in &src.items {
            if let syn::Item::Const(c) = item {
                let name = c.ident.to_string();
                if name.starts_with("CLOSE_REASON_") {
                    match &*c.expr {
                        syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        }) => out.push((name, s.value())),
                        // `CloseReason::<Variant>.as_str()`: the typed set's
                        // own wire string.
                        syn::Expr::MethodCall(m) if m.method == "as_str" && m.args.is_empty() => {
                            let wire = typed_close_reason(&m.receiver)
                                .as_deref()
                                .and_then(typed_close_reason_wire)
                                .unwrap_or_else(|| panic!("{name} names no CloseReason variant"));
                            out.push((name, wire.to_string()));
                        }
                        _ => panic!(
                            "{name} is neither a string literal nor a CloseReason wire string"
                        ),
                    }
                }
            }
        }
        out
    }

    /// Run the census over every production `.rs` file under `src/`.
    fn close_write_census() -> Vec<Site> {
        use syn::visit::Visit;
        let src_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sites = Vec::new();
        for entry in walkdir::WalkDir::new(&src_root) {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let rel = path
                .strip_prefix(&src_root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let name = path.file_name().unwrap().to_string_lossy();
            // Whole-file test modules (declared `#[cfg(test)] mod x;` in
            // their parent) carry no attribute of their own to skip.
            if name == "tests.rs"
                || name.ends_with("_tests.rs")
                || name == "test_fixtures.rs"
                || rel.split('/').any(|seg| seg == "tests")
            {
                continue;
            }
            let text = std::fs::read_to_string(path).unwrap();
            if !text.contains("close_reason") && !text.contains("record_close") {
                continue;
            }
            let file = syn::parse_file(&text).unwrap_or_else(|e| panic!("{rel}: {e}"));
            let mut census = CloseWriteCensus::new(&rel);
            census.visit_file(&file);
            sites.extend(census.sites);
        }
        sites
    }

    /// The census the plan's "an unenumerated reason renders unknown" rule
    /// rests on. It parses every production `.rs` file under `src/` with
    /// `syn`, skipping items under `#[test]`, `#[cfg(test)]` or
    /// `#[cfg(all(.., test, ..))]` (and whole-file test modules by name), and
    /// checks these write shapes:
    ///
    /// - calls of every [`RECORD_CLOSE_FNS`] entry (`record_close`,
    ///   `record_close_checked`, `record_close_gated`, `record_close_for`),
    ///   as methods and in their UFCS form
    ///   `SessionLifecycleStore::record_close(&s, ..)`;
    /// - `x.close_reason = ..` and `*x.close_reason = ..`;
    /// - `x.close_reason.replace/insert/get_or_insert/get_or_insert_with(..)`
    ///   and `std::mem::replace(&mut x.close_reason, ..)`;
    /// - `close_reason: ..` in a struct literal;
    /// - `reason: ..` in a `HeldClose { .. }` literal — a close a respawn holds
    ///   and later replays;
    /// - the same shapes inside a macro whose body parses as an expression
    ///   list. A macro that names a record_close fn and does NOT parse (a
    ///   `tokio::select!` arm) fails the census instead of hiding a site.
    ///
    /// Each site must record a string literal or a `CLOSE_REASON_*` constant
    /// that maps to a known code (pinned exactly, by file and fn, in
    /// `MINTS`), or be one of the named forwarders in `FORWARDERS`: pinned by
    /// file, enclosing fn (`Type::method` for a method) and the exact
    /// expression forwarded, with an exact count, and backed by a test of its
    /// own source. A copy of another record's `close_reason` FIELD into an
    /// assignment or a struct field mints nothing; the same field read passed
    /// to `record_close` is a new close and must be a forwarder. A local named
    /// `close_reason` (including the field-init shorthand) is a forwarder too.
    ///
    /// Known blind spots, stated so the census is not trusted past them:
    /// - a write through an alias (`let slot = &mut r.close_reason; *slot =
    ///   ..`, `if let Some(r) = &mut x.close_reason { *r = .. }`, or an
    ///   `x.close_reason.as_mut()` chain);
    /// - UFCS Option mutators other than `mem::replace`
    ///   (`Option::insert(&mut x.close_reason, ..)`) and `std::mem::swap`;
    /// - a field read off a deserialized value moved into a record by
    ///   assignment or struct field (`rec.close_reason =
    ///   args.close_reason.clone()`) — read as a copy, so it mints nothing
    ///   here even though the value is new to the registry;
    /// - a macro that names only `close_reason` and does not parse as
    ///   expressions (a tracing field or a `json!` object);
    /// - code outside `src/`.
    #[test]
    fn every_close_reason_a_production_writer_records_maps_to_a_known_code() {
        let src_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let store_path = src_root.join("session/session_lifecycle_store.rs");
        let store = syn::parse_file(&std::fs::read_to_string(&store_path).unwrap()).unwrap();
        let consts = declared_close_reason_consts(&store);
        assert!(!consts.is_empty(), "no CLOSE_REASON_* constants found");
        for (name, value) in &consts {
            assert!(
                code_for_close_reason(value).is_some(),
                "{name} = {value:?} has no stop code"
            );
        }

        // Every literal / constant write site, by (file, enclosing fn, value).
        // A new writer must be added here — deliberately: this list IS the
        // enumeration the plan asked for.
        const MINTS: &[(&str, &str, &str)] = &[
            ("main.rs", "run_app", "CloseReason::NeverStarted"),
            ("main.rs", "run_app", "CloseReason::NoTerminal"),
            (
                "orchestration_loop/ai_session_executor.rs",
                "teardown_unbound_worker",
                "CloseReason::WorkerNotBound",
            ),
            (
                "session/session_lifecycle_store.rs",
                "SessionLifecycleStore::record_open",
                "CloseReason::Superseded",
            ),
            (
                "session/session_lifecycle_store.rs",
                "SessionLifecycleStore::record_open",
                "CloseReason::SupersededTerminalReuse",
            ),
            (
                "session/session_lifecycle_store.rs",
                "SessionLifecycleStore::repair_terminal_id_collisions",
                "CloseReason::SupersededTerminalReuse",
            ),
            (
                "terminal/respawn_liveness.rs",
                "end_failed",
                "CloseReason::PtyExit",
            ),
        ];
        // Sites that forward a reason chosen elsewhere, by (file, enclosing
        // fn, the exact expression forwarded) with the exact number of such
        // sites. What each forwards is pinned elsewhere:
        // - `SessionLifecycleStore::record_close` hands its `reason` parameter
        //   to `record_close_checked`, which stores it in the field; their
        //   callers are the other sites in this census;
        // - `terminal_session_record_close` forwards the FRONTEND's `reason`
        //   (`frontend_close_reasons_are_enumerated`) to `record_close_for`,
        //   which hands it to `SessionLifecycleStore::record_close_gated`;
        // - `record_close_gated` records either that `reason` unchanged or the
        //   launch-failure verdict on it (`verdict.reason`), and
        //   `terminal::launch_failure::close_dead_row` — the liveness poll's
        //   close in `run_app` — records the verdict on
        //   `close_reason_for_dead_shell(..)`. A verdict only ever returns its
        //   proposal or `launch-failed`
        //   (`the_launch_failure_verdict_only_records_mapped_reasons`), and
        //   the dead-shell proposals are mapped
        //   (`the_dead_shell_close_reasons_are_enumerated`);
        // - `build_enriched` (a read-side VIEW, not a registry write) takes
        //   `close_reason` as a parameter its callers read off a record or a
        //   history snapshot, and stores it by field-init shorthand.
        const FORWARDERS: &[(&str, &str, &str, usize)] = &[
            (
                "session/session_lifecycle_store.rs",
                "SessionLifecycleStore::record_close",
                "reason",
                1,
            ),
            (
                "session/session_lifecycle_store.rs",
                "SessionLifecycleStore::record_close_checked",
                "reason",
                1,
            ),
            (
                "session/session_lifecycle_store.rs",
                "SessionLifecycleStore::record_close_gated",
                "reason",
                1,
            ),
            (
                "session/session_lifecycle_store.rs",
                "SessionLifecycleStore::record_close_gated",
                "verdict.reason",
                1,
            ),
            (
                "commands/terminal.rs",
                "terminal_session_record_close",
                "reason",
                1,
            ),
            ("commands/terminal.rs", "record_close_for", "reason", 1),
            (
                "terminal/launch_failure.rs",
                "close_dead_row",
                "verdict.reason",
                1,
            ),
            // The respawn close-hold: `hold_close_if_moving` captures the
            // reason a `record_close_checked` caller passed (each pinned in
            // this census) into a `HeldClose`, and the hold's exits replay a
            // held `HeldClose::reason` — or `USER_CLOSE_REASON`, which is
            // `CLOSE_REASON_EXPLICIT` (`the_respawn_hold_replays_only_mapped_reasons`)
            // — into `record_close_checked`.
            (
                "session/session_lifecycle_store.rs",
                "SessionLifecycleStore::hold_close_if_moving",
                "reason",
                1,
            ),
            (
                "terminal/respawn_liveness.rs",
                "SessionCloseHold::abort_for_closed_old",
                "reason",
                1,
            ),
            (
                "terminal/respawn_liveness.rs",
                "SessionCloseHold::drop",
                "reason",
                1,
            ),
            (
                "terminal/respawn_liveness.rs",
                "SessionCloseHold::finish_replaying_all",
                "c.reason",
                1,
            ),
            ("terminal/respawn_liveness.rs", "end_failed", "c.reason", 1),
            (
                "terminal/respawn_liveness.rs",
                "finish_replaying",
                "held.reason",
                1,
            ),
            (
                "session/past_sessions.rs",
                "build_enriched",
                "close_reason",
                1,
            ),
        ];

        let sites = close_write_census();
        let mut problems = Vec::new();
        let mut mints: Vec<(String, String, String)> = Vec::new();
        let mut forwards: std::collections::BTreeMap<(String, String, String), usize> =
            std::collections::BTreeMap::new();
        for s in &sites {
            match &s.recorded {
                Recorded::Literal(v) => {
                    if code_for_close_reason(v).is_none() {
                        problems.push(format!("{} in {}: unmapped literal {v:?}", s.file, s.func));
                    }
                    mints.push((s.file.clone(), s.func.clone(), format!("{v:?}")));
                }
                Recorded::Const(n) => {
                    let value = typed_close_reason_wire(n)
                        .map(str::to_string)
                        .or_else(|| consts.iter().find(|(c, _)| c == n).map(|(_, v)| v.clone()));
                    match value {
                        Some(v) if code_for_close_reason(&v).is_some() => {}
                        _ => problems.push(format!("{} in {}: constant {n}", s.file, s.func)),
                    }
                    mints.push((s.file.clone(), s.func.clone(), n.clone()));
                }
                Recorded::Indirect(expr) => {
                    *forwards
                        .entry((s.file.clone(), s.func.clone(), expr.clone()))
                        .or_default() += 1;
                }
                Recorded::UnparsedMacro => problems.push(format!(
                    "{} in {}: a macro names record_close but its body does not parse — \
                     the census cannot see the reason it records",
                    s.file, s.func
                )),
                Recorded::NotAMint => {}
            }
        }
        for (key, n) in &forwards {
            let expected = FORWARDERS
                .iter()
                .find(|(f, func, e, _)| {
                    (*f, *func, *e) == (key.0.as_str(), key.1.as_str(), key.2.as_str())
                })
                .map(|(.., c)| *c);
            if expected != Some(*n) {
                problems.push(format!(
                    "{} in {}: forwards `{}` at {n} site(s), expected {expected:?}",
                    key.0, key.1, key.2
                ));
            }
        }
        for (f, func, e, _) in FORWARDERS {
            if !forwards.contains_key(&(f.to_string(), func.to_string(), e.to_string())) {
                problems.push(format!(
                    "forwarder {f} in {func} `{e}` no longer exists — drop it"
                ));
            }
        }
        mints.sort();
        let mut expected_mints: Vec<(String, String, String)> = MINTS
            .iter()
            .map(|(f, func, v)| (f.to_string(), func.to_string(), v.to_string()))
            .collect();
        expected_mints.sort();
        if mints != expected_mints {
            problems.push(format!(
                "literal/constant write sites changed — update MINTS.\n  found:    {mints:?}\n  expected: {expected_mints:?}"
            ));
        }
        assert!(
            problems.is_empty(),
            "close-reason census (add a CLOSE_REASON_* constant and a code_for_close_reason arm, \
             then name the site in MINTS or FORWARDERS):\n{}",
            problems.join("\n")
        );
    }

    /// The census's own reach, on planted sources: every write shape it
    /// claims is recorded with the value it records; a local named
    /// `close_reason` (shorthand included) and a field read passed to
    /// `record_close` are forwarders, not copies; test-only items are skipped
    /// while a widened `cfg(any(test, ..))` item is scanned; and each site is
    /// attributed to the right fn — a closure to its enclosing fn, a nested fn
    /// to itself, a method to `Type::method`.
    #[test]
    fn the_close_reason_census_sees_the_shapes_it_claims() {
        use syn::visit::Visit;
        let src = r#"
            fn planted(store: &S, r: &mut Rec, rec: Rec, args: Args, id: &str) {
                let close_reason = format!("x{}", 1);
                store.record_close(id, &close_reason);
                store.record_close(id, &args.close_reason);
                store.record_close_checked(id, None, "checked");
                SessionLifecycleStore::record_close(store, id, "ufcs");
                store.record_close_gated(false, id, None, "gated", Some(1));
                SessionLifecycleStore::record_close_gated(store, true, id, None, "gated-ufcs", None);
                record_close_for(tm, store, id, None, "for");
                let held = HeldClose { claude_session_id: id.into(), terminal_id: t, reason: "held".into() };
                r.close_reason = Some("plain".into());
                *r.close_reason = Some("deref".to_string());
                r.close_reason.replace("replaced".to_string());
                r.close_reason.insert("inserted".to_string());
                r.close_reason.get_or_insert("eager".to_string());
                r.close_reason.get_or_insert_with(|| "lazy".to_string());
                std::mem::replace(&mut r.close_reason, Some("swapped".to_string()));
                let copy = View { close_reason: r.close_reason.clone() };
                let close_reason = Some(format!("x{}", 2));
                let shorthand = Rec { close_reason, ..rec };
                let picked = View { close_reason: self.pick_close_reason(t) };
                let cb = || store.record_close(id, "in-a-closure");
                fn nested(store: &S, id: &str) { store.record_close(id, "in-a-nested-fn"); }
                tokio::spawn(async move { store.record_close(id, "in-a-block") });
                vec![store.record_close(id, "in-a-macro")];
                tokio::select! { _ = a => store.record_close(id, "in-select"), }
            }
            impl Store {
                fn method(&self, id: &str) {
                    self.record_close(id, "in-a-method");
                    fn helper(s: &S, id: &str) { s.record_close(id, "in-a-method-helper"); }
                }
            }
            #[cfg(any(test, feature = "x"))]
            fn widened(store: &S, id: &str) { store.record_close(id, "widened"); }
            #[cfg(all(unix, test))]
            fn all_test(store: &S, id: &str) { store.record_close(id, "hidden-all"); }
            #[cfg(test)]
            fn cfg_test(store: &S, id: &str) { store.record_close(id, "hidden-cfg"); }
            #[test]
            fn a_test() { store.record_close(id, "hidden-test"); }
            #[cfg(test)]
            mod tests {
                fn f(store: &S, id: &str) { store.record_close(id, "hidden-mod"); }
            }
        "#;
        let file = syn::parse_file(src).unwrap();
        let mut c = CloseWriteCensus::new("planted.rs");
        c.visit_file(&file);
        let got: Vec<(String, Recorded)> = c
            .sites
            .iter()
            .map(|s| (s.func.clone(), s.recorded.clone()))
            .collect();
        let at = |f: &str, r: Recorded| (f.to_string(), r);
        let lit = |f: &str, s: &str| (f.to_string(), Recorded::Literal(s.to_string()));
        let ind = |f: &str, s: &str| (f.to_string(), Recorded::Indirect(s.to_string()));
        assert_eq!(
            got,
            vec![
                ind("planted", "close_reason"),
                ind("planted", "args.close_reason"),
                lit("planted", "checked"),
                lit("planted", "ufcs"),
                lit("planted", "gated"),
                lit("planted", "gated-ufcs"),
                lit("planted", "for"),
                lit("planted", "held"),
                lit("planted", "plain"),
                lit("planted", "deref"),
                lit("planted", "replaced"),
                lit("planted", "inserted"),
                lit("planted", "eager"),
                lit("planted", "lazy"),
                lit("planted", "swapped"),
                at("planted", Recorded::NotAMint),
                ind("planted", "close_reason"),
                ind("planted", "self.pick_close_reason(t)"),
                lit("planted", "in-a-closure"),
                lit("nested", "in-a-nested-fn"),
                lit("planted", "in-a-block"),
                lit("planted", "in-a-macro"),
                at("planted", Recorded::UnparsedMacro),
                lit("Store::method", "in-a-method"),
                lit("helper", "in-a-method-helper"),
                lit("widened", "widened"),
            ]
        );
    }

    /// The respawn close-hold replays either a held close's own reason (which
    /// a census-pinned `record_close_checked` caller chose) or
    /// `USER_CLOSE_REASON` — and that constant is the operator's
    /// `CLOSE_REASON_EXPLICIT`, not a reason of its own.
    #[test]
    fn the_respawn_hold_replays_only_mapped_reasons() {
        use crate::session::session_lifecycle_store::USER_CLOSE_REASON;
        assert_eq!(USER_CLOSE_REASON, CLOSE_REASON_EXPLICIT);
        assert_eq!(
            code_for_close_reason(USER_CLOSE_REASON),
            Some(StopCode::ClosedByOperator)
        );
    }

    /// The launch-failure verdict that `record_close_gated` and
    /// `close_dead_row` forward returns its proposal or `launch-failed` — so a
    /// mapped proposal can only ever record a mapped reason, on both the
    /// re-close arm and the no-evidence arm.
    #[test]
    fn the_launch_failure_verdict_only_records_mapped_reasons() {
        use crate::session::close_reason::CloseReason;
        use crate::terminal::launch_failure::{close_reason_for_launch, PriorClose};
        // The typed set every production close writes is mapped whole, so a
        // variant added there without a stop code fails here.
        for reason in CloseReason::ALL {
            assert!(
                code_for_close_reason(reason.as_str()).is_some(),
                "{reason:?} = {:?} has no stop code",
                reason.as_str()
            );
        }
        for proposed in CloseReason::ALL {
            for prior in [
                None,
                Some(PriorClose {
                    reason: CloseReason::LaunchFailed,
                    at_ms: 0,
                }),
            ] {
                let got = close_reason_for_launch(proposed, prior, None, None, None, None);
                assert!(
                    got == proposed || got == CloseReason::LaunchFailed,
                    "{proposed} -> {got}"
                );
                assert!(
                    code_for_close_reason(got.as_str()).is_some(),
                    "{proposed} -> {got}"
                );
            }
        }
    }

    /// The liveness poll's proposed reason (`close_reason_for_dead_shell`)
    /// can only produce mapped values.
    #[test]
    fn the_dead_shell_close_reasons_are_enumerated() {
        use crate::session::session_lifecycle_store::{
            close_reason_for_dead_shell, RESTORE_TIER_FAILED, RESTORE_TIER_RESUMED,
            RESTORE_TIER_TERMINAL_ONLY,
        };
        for tier in [
            None,
            Some(RESTORE_TIER_TERMINAL_ONLY),
            Some(RESTORE_TIER_RESUMED),
            Some(RESTORE_TIER_FAILED),
        ] {
            let reason = close_reason_for_dead_shell(tier);
            assert!(
                code_for_close_reason(reason.as_str()).is_some(),
                "{tier:?} -> {reason}"
            );
        }
    }
}
