//! Wind-down eligibility — the pure core of "may a drained runner close this
//! session?" (plan `2026-09-13-drained-runner-never-reaches-idle`, Phase 1,
//! design decisions D4 and D6).
//!
//! ## What this answers, and what it never does
//!
//! [`eligibility`] folds five observations about ONE live `claude` session into
//! a four-way verdict ([`Eligibility`]). It performs no I/O, reads no clock
//! (`now_ms` is an argument) and acts on nothing itself.
//!
//! Two callers read the verdict, and they are not the same:
//! `GET /restart-readiness` REPORTS it, whatever the drain state; the Phase 4
//! wind-down executor (`session::wind_down_executor`) ACTS on it, and only
//! while coord holds this device drained. Since that phase landed, an
//! [`Eligibility::Eligible`] on a drained runner is a session that will be
//! graceful-`/exit`ed — so read the rules below as the authorisation they are,
//! not as a report.
//!
//! ## The rules (D4, D6)
//!
//! Evaluated in this fixed order, first match wins:
//!
//! 1. **Any unknown input → [`Eligibility::Unknown`].** An unreadable coord
//!    work axis, an unreadable sideband slot, a pane that could not be
//!    observed (its grid or its input area), or a children hint that could
//!    not be computed. *"`finished` is a declaration, never an inference"* —
//!    so an unknown is never promoted to anything that could later authorise
//!    a close.
//! 2. **A `working` sideband state → [`Eligibility::Ineligible`].** Because
//!    the grace clock starts at the LATER of the transcript's last message and
//!    the sideband's set-time (rule 8), a `working` report also restarts the
//!    grace period once the session stops reporting `working`.
//! 3. **Claude Code's own record is not `idle` → `Ineligible`** —
//!    `record_busy`, `record_waiting` (blocked on a human: a permission prompt
//!    or a question), `record_shell` (a background command), or
//!    `record_unknown` (absent, unreadable, unrecognised). This is the "is it
//!    working?" answer (plan
//!    `2026-10-23-wind-down-idle-check-mistakes-the-completed-turn-line-for-a-spinner`,
//!    D3): the typed signal outranks the screen. The rendered grid's
//!    turn-complete heuristic no longer decides it — a grid that reads busy
//!    (reported as `idleEvidence`) does not block a close by itself.
//! 4. **The input area is not an empty prompt → `Ineligible`
//!    (`prompt_not_ready`).** The grid's one remaining job: a VETO on positive
//!    evidence the record cannot see — an unsent draft, an open dialog, or no
//!    input prompt under the cursor.
//! 5. **Live child processes → `Ineligible`.** `has_live_children` is a hint,
//!    so it is used as a necessary condition and never a sufficient one.
//! 6. **A terminal session that is not `finished` → `Ineligible`, forever.**
//!    An idle-but-unfinished session is never closed automatically; the
//!    operator's *finish-and-close* click is the declaration. Looping and
//!    steward sessions are EXEMPT from this rule (D6): they never finish by
//!    design, and their iteration boundary is the idle window itself. The
//!    work axis is not even consulted for them, so an unreadable coord does
//!    not make a loop `Unknown`.
//! 7. **An idle record whose transcript age cannot be read → `Ineligible`
//!    (`transcript_age_unknown`).** The grace needs a clock to run from.
//! 8. **Grace.** Eligible once `now >= since + grace`, where `since` is the
//!    latest of the transcript's last `user`/`assistant` message, the sideband
//!    set-time, the SCREEN-QUIET FLOOR, and — for a terminal session whose
//!    coord row carried it — the time the `finished` declaration was made;
//!    before that, [`Eligibility::NotYet`] with the instant it would become
//!    eligible. When coord serves no transition time the declaration does not
//!    bound the window.
//!
//!    The screen-quiet floor ([`ScreenQuietTracker`]) is the latest time THIS
//!    PROCESS observed the pane's screen change, or its first observation of
//!    the pane. It keeps two properties the transcript clock alone loses:
//!    the window can never predate the process (after a runner start or a
//!    handoff-adopted pane, a full grace must pass under this process's eyes,
//!    however old the transcript is), and operator activity that writes no
//!    transcript message — a local slash command, a menu, a draft typed and
//!    deleted — restarts the window when an observation sees it. It hashes
//!    screen CONTENT rather than reading the grid generation, so a status
//!    line that is redrawn unchanged does not reset it, and it skips the
//!    context-hint row whose update notice churns on an idle pane
//!    (`looping_agent::idle::screen_fingerprint`). A change that comes and
//!    goes entirely between two observations is not seen.
//!
//! ## A sideband that never reported is not an unknown
//!
//! The OSC 9999 agent-status sideband is the SECOND, independent status
//! channel (`terminal/agent_status_sideband.rs` module docs): a session that
//! never emits it is *degraded, not invisible*. Condition (b) of D4 is "the
//! sideband's last state is not `working`", which a session with no reported
//! state satisfies, so [`Sideband::NeverReported`] passes that condition while
//! the grid, children and work-axis conditions still apply in full. Only a
//! slot that could not be READ ([`Sideband::Unreadable`]) is an unknown.

use std::time::Duration;

use serde::Serialize;

use crate::looping_agent::idle::IdleVerdict;

/// Default grace period: how long every condition must hold, continuously,
/// before a session becomes eligible. Plan open question resolved at vet.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(10 * 60);

/// Environment override for the grace period, in whole seconds.
pub const GRACE_ENV: &str = "QONTINUI_WIND_DOWN_GRACE_SECS";

/// Resolve the grace period from a raw override value. `None`, an empty
/// string, a non-integer, or zero all yield [`DEFAULT_GRACE`]: a zero grace
/// would make the continuity requirement meaningless, so it is refused rather
/// than honoured.
pub fn grace_from(raw: Option<&str>) -> Duration {
    raw.map(str::trim)
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&secs| secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_GRACE)
}

/// [`grace_from`] over the process environment ([`GRACE_ENV`]).
pub fn grace_from_env() -> Duration {
    grace_from(std::env::var(GRACE_ENV).ok().as_deref())
}

/// What kind of session this is. Decides whether the `finished` declaration
/// is required (rule 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// An ordinary terminal-hosted agent session. Requires `finished`.
    Terminal,
    /// A looping-agent tab (`looping_agent_supervisor`). Exempt from
    /// `finished` (D6).
    Looping,
    /// A steward `/loop` session (`mcp/steward.rs`). Exempt from `finished`
    /// (D6).
    Steward,
}

/// The coord WORK axis for the session, reduced to what eligibility needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkStatus {
    /// coord served an explicit `finished`.
    Finished,
    /// coord served a recognised status other than `finished`.
    NotFinished,
    /// No row, an unset axis, an unrecognised word, an ambiguous attribution,
    /// or coord could not be read.
    Unknown,
}

/// The sideband's reported state, reduced to the one distinction D4 draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebandState {
    Working,
    /// `blocked`, `stalled`, `waiting_human` or `finished`.
    NotWorking,
}

/// The last OSC 9999 agent-status observation for the session's terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sideband {
    /// The terminal has never reported a state (see module docs).
    NeverReported,
    /// The most recent state-bearing payload and when it arrived.
    Reported {
        state: SidebandState,
        set_at_ms: i64,
    },
    /// The slot could not be read.
    Unreadable,
}

/// The rendered-grid idle observation for the session's terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridIdle {
    /// Looks idle, and has looked idle continuously since `since_ms` as far
    /// as the observer can prove ([`GridIdleTracker`]).
    Idle { since_ms: i64 },
    /// Does not look idle right now, and the clause that said so.
    Busy { evidence: GridBusyEvidence },
    /// The grid could not be observed (no live terminal, poisoned lock).
    Unknown,
}

/// Why a grid observation read busy — reported as `windDown.idleEvidence` on
/// `GET /restart-readiness` (plan
/// `2026-10-23-wind-down-idle-check-mistakes-the-completed-turn-line-for-a-spinner`,
/// D2). The first four are the clauses of the turn-complete recognizer
/// ([`IdleVerdict`]); [`GridBusyEvidence::GenerationMoved`] is the observer's
/// own: the grid changed while it was being read.
///
/// There is deliberately no `Idle` value: evidence exists only for a busy
/// grid. Since D3 a busy grid no longer blocks a close by itself, so this is
/// reported beside whatever `reason` decided — the audit trail for the
/// screen heuristic, never a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridBusyEvidence {
    /// A processing indicator — the matched `PROCESSING_INDICATORS`
    /// CONSTANT, never screen text (panes carry session content).
    Indicator(&'static str),
    /// A spinner glyph outside a completed-turn line.
    Glyph(char),
    /// No `❯` prompt row on screen.
    NoPrompt,
    /// The cursor above the prompt row.
    CursorAbovePrompt,
    /// The grid generation moved during the read, so the snapshot cannot be
    /// trusted to show one settled frame.
    GenerationMoved,
}

impl GridBusyEvidence {
    /// The evidence behind a busy screen verdict; `None` for
    /// [`IdleVerdict::Idle`].
    pub fn from_screen(verdict: IdleVerdict) -> Option<Self> {
        match verdict {
            IdleVerdict::Idle => None,
            IdleVerdict::Indicator(ind) => Some(Self::Indicator(ind)),
            IdleVerdict::Glyph(ch) => Some(Self::Glyph(ch)),
            IdleVerdict::NoPrompt => Some(Self::NoPrompt),
            IdleVerdict::CursorAbovePrompt => Some(Self::CursorAbovePrompt),
        }
    }

    /// The evidence of one grid observation: `generation_moved` when the
    /// generation moved during the read — it outranks whatever the torn
    /// snapshot showed, since that is not one settled frame — else the screen
    /// verdict's clause. `None` means the observation looked idle.
    pub fn of_observation(
        generation_before: u64,
        generation_after: u64,
        screen: IdleVerdict,
    ) -> Option<Self> {
        if generation_before != generation_after {
            Some(Self::GenerationMoved)
        } else {
            Self::from_screen(screen)
        }
    }

    /// The wire value: `indicator:<constant>`, `glyph:<char>`, `no_prompt`,
    /// `cursor_above_prompt` or `generation_moved`.
    pub fn wire(self) -> String {
        match self {
            Self::Indicator(ind) => format!("indicator:{ind}"),
            Self::Glyph(ch) => format!("glyph:{ch}"),
            Self::NoPrompt => "no_prompt".to_string(),
            Self::CursorAbovePrompt => "cursor_above_prompt".to_string(),
            Self::GenerationMoved => "generation_moved".to_string(),
        }
    }
}

/// Claude Code's own per-process status (`<config-dir>/sessions/<pid>.json`
/// `status`), reduced to the distinctions a close needs. The runner reads it
/// in `session::claude_activity`; anything unreadable, absent, unparseable,
/// ambiguous, from a reused pid or with an unrecognised status word is
/// [`RecordStatus::Unknown`].
///
/// Measured on Claude Code 2.1.309 (plan
/// `2026-10-23-wind-down-idle-check-mistakes-the-completed-turn-line-for-a-spinner`,
/// Phase 0): `idle` appears only once a turn has ended at an empty prompt;
/// a pending permission prompt or an open question reads `waiting`; mid-turn
/// the record reads `busy` (thinking, a background subagent) or `shell` (a
/// background Bash). So only `idle` can authorise a close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordStatus {
    Idle,
    /// Blocked on a human — a permission prompt or a question.
    Waiting,
    Busy,
    /// A background shell command is running.
    Shell,
    Unknown,
}

/// The record input to [`eligibility`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionRecord {
    pub status: RecordStatus,
    /// When the session's transcript last carried a real `user`/`assistant`
    /// message (unix millis). `None` when it could not be read.
    pub last_message_ms: Option<i64>,
}

impl SessionRecord {
    /// No record could be read.
    pub const UNKNOWN: Self = Self {
        status: RecordStatus::Unknown,
        last_message_ms: None,
    };
}

/// What the pane's input area shows, judged by the same checks
/// `terminal::graceful_exit` runs before typing `/exit` — MINUS the
/// turn-complete predicate, so a pane the screen heuristic reads busy can
/// still be [`PromptScreen::Ready`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptScreen {
    /// An empty `❯` input line under the cursor, no dialog near it.
    Ready,
    /// Positive evidence a close would destroy something or answer something:
    /// unsent text in the input, a numbered choice, a dialog marker, or no
    /// input prompt on the cursor row at all.
    NotReady,
    /// The pane could not be read.
    Unknown,
}

/// Everything [`eligibility`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EligibilityInputs {
    pub kind: SessionKind,
    pub work_status: WorkStatus,
    pub sideband: Sideband,
    pub grid: GridIdle,
    /// The pane's input area — the grid's veto (rule 4).
    pub prompt: PromptScreen,
    /// Since when THIS PROCESS has seen the pane's screen unchanged
    /// ([`ScreenQuietTracker`]) — the per-process floor of the grace clock.
    /// `None` when the pane could not be observed.
    pub screen_quiet_since_ms: Option<i64>,
    /// Claude Code's own record and the transcript's last-message time.
    pub record: SessionRecord,
    /// `None` when the process snapshot could not answer.
    pub has_live_children: Option<bool>,
    /// When coord's row became `finished` (unix millis), when coord served the
    /// transition time. Bounds the grace window for a terminal session: idleness
    /// observed BEFORE the declaration does not count towards it, so a stale
    /// `finished` cannot inherit an old idle window. `None` (an older coord, or
    /// a null `since`) falls back to not bounding the window by it.
    pub finished_at_ms: Option<i64>,
    pub grace: Duration,
}

/// Why a session is definitely not eligible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IneligibleReason {
    SidebandWorking,
    LiveChildren,
    NotFinished,
    /// Claude Code's record says `busy`.
    RecordBusy,
    /// Claude Code's record says `waiting` — blocked on a human.
    RecordWaiting,
    /// Claude Code's record says `shell` — a background command is running.
    RecordShell,
    /// No usable record: absent, unparseable, ambiguous, a reused pid, or an
    /// unrecognised status. Unknown is never closable.
    RecordUnknown,
    /// The record says `idle` but the transcript's last message time could
    /// not be read, so the grace cannot be measured from it.
    TranscriptAgeUnknown,
    /// The input area shows an unsent draft, a dialog, or no prompt at all.
    PromptNotReady,
}

impl IneligibleReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SidebandWorking => "sideband_working",
            Self::LiveChildren => "live_children",
            Self::NotFinished => "not_finished",
            Self::RecordBusy => "record_busy",
            Self::RecordWaiting => "record_waiting",
            Self::RecordShell => "record_shell",
            Self::RecordUnknown => "record_unknown",
            Self::TranscriptAgeUnknown => "transcript_age_unknown",
            Self::PromptNotReady => "prompt_not_ready",
        }
    }
}

/// Which input could not be determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    WorkStatusUnknown,
    SidebandUnreadable,
    GridUnknown,
    ChildrenUnknown,
}

impl UnknownReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkStatusUnknown => "work_status_unknown",
            Self::SidebandUnreadable => "sideband_unreadable",
            Self::GridUnknown => "grid_unknown",
            Self::ChildrenUnknown => "children_unknown",
        }
    }
}

/// The verdict. Only [`Eligibility::Eligible`] authorises a close, and since
/// Phase 4 the wind-down executor acts on exactly that — while drained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eligibility {
    /// Every condition has held since `since_ms`, for at least the grace
    /// period.
    Eligible { since_ms: i64 },
    /// Every condition holds now, but not yet for the grace period. Becomes
    /// eligible at `until_ms` if nothing changes.
    NotYet { since_ms: i64, until_ms: i64 },
    /// A known input rules the session out.
    Ineligible { reason: IneligibleReason },
    /// An input could not be determined.
    Unknown { reason: UnknownReason },
}

impl Eligibility {
    pub fn is_eligible(&self) -> bool {
        matches!(self, Self::Eligible { .. })
    }

    /// The wire word.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Eligible { .. } => "eligible",
            Self::NotYet { .. } => "not_yet",
            Self::Ineligible { .. } => "ineligible",
            Self::Unknown { .. } => "unknown",
        }
    }
}

/// Decide wind-down eligibility. Pure; see the module docs for the rules.
pub fn eligibility(inputs: &EligibilityInputs, now_ms: i64) -> Eligibility {
    let finished_required = inputs.kind == SessionKind::Terminal;

    // (1) Any unknown input.
    if finished_required && inputs.work_status == WorkStatus::Unknown {
        return Eligibility::Unknown {
            reason: UnknownReason::WorkStatusUnknown,
        };
    }
    if inputs.sideband == Sideband::Unreadable {
        return Eligibility::Unknown {
            reason: UnknownReason::SidebandUnreadable,
        };
    }
    if inputs.grid == GridIdle::Unknown
        || inputs.prompt == PromptScreen::Unknown
        || inputs.screen_quiet_since_ms.is_none()
    {
        return Eligibility::Unknown {
            reason: UnknownReason::GridUnknown,
        };
    }
    let Some(has_live_children) = inputs.has_live_children else {
        return Eligibility::Unknown {
            reason: UnknownReason::ChildrenUnknown,
        };
    };

    // (2) A working sideband state.
    let sideband_set_at = match inputs.sideband {
        Sideband::Reported {
            state: SidebandState::Working,
            ..
        } => {
            return Eligibility::Ineligible {
                reason: IneligibleReason::SidebandWorking,
            }
        }
        Sideband::Reported { set_at_ms, .. } => Some(set_at_ms),
        Sideband::NeverReported | Sideband::Unreadable => None,
    };

    // (3) Claude Code's record: only `idle` can close.
    let not_idle = match inputs.record.status {
        RecordStatus::Idle => None,
        RecordStatus::Busy => Some(IneligibleReason::RecordBusy),
        RecordStatus::Waiting => Some(IneligibleReason::RecordWaiting),
        RecordStatus::Shell => Some(IneligibleReason::RecordShell),
        RecordStatus::Unknown => Some(IneligibleReason::RecordUnknown),
    };
    if let Some(reason) = not_idle {
        return Eligibility::Ineligible { reason };
    }

    // (4) The grid's veto: an unsent draft, a dialog, no prompt.
    if inputs.prompt == PromptScreen::NotReady {
        return Eligibility::Ineligible {
            reason: IneligibleReason::PromptNotReady,
        };
    }

    // (5) Children.
    if has_live_children {
        return Eligibility::Ineligible {
            reason: IneligibleReason::LiveChildren,
        };
    }

    // (6) The finished declaration, for terminal sessions only.
    if finished_required && inputs.work_status != WorkStatus::Finished {
        return Eligibility::Ineligible {
            reason: IneligibleReason::NotFinished,
        };
    }

    // (7) The transcript clock.
    let Some(last_message_ms) = inputs.record.last_message_ms else {
        return Eligibility::Ineligible {
            reason: IneligibleReason::TranscriptAgeUnknown,
        };
    };

    // (8) Grace, from the latest of the clocks: the transcript's last
    // message, the sideband report, the screen-quiet floor, and (terminal
    // sessions only) the `finished` declaration.
    let mut since_ms = sideband_set_at.map_or(last_message_ms, |s| s.max(last_message_ms));
    if let Some(quiet_since) = inputs.screen_quiet_since_ms {
        since_ms = since_ms.max(quiet_since);
    }
    if finished_required {
        if let Some(finished_at) = inputs.finished_at_ms {
            since_ms = since_ms.max(finished_at);
        }
    }
    let grace_ms = i64::try_from(inputs.grace.as_millis()).unwrap_or(i64::MAX);
    let until_ms = since_ms.saturating_add(grace_ms);
    if now_ms >= until_ms {
        Eligibility::Eligible { since_ms }
    } else {
        Eligibility::NotYet { since_ms, until_ms }
    }
}

/// The `windDown` block `GET /restart-readiness` attaches to a process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindDownView {
    /// `eligible` | `not_yet` | `ineligible` | `unknown`.
    pub eligibility: &'static str,
    /// Start of the continuous-eligibility window (unix millis), for
    /// `eligible` and `not_yet`; `null` otherwise.
    pub since: Option<i64>,
    /// When a `not_yet` session would become eligible (unix millis).
    pub until: Option<i64>,
    /// Why, for `ineligible` and `unknown`.
    pub reason: Option<&'static str>,
    /// Which clause made the grid read busy ([`GridBusyEvidence::wire`])
    /// whenever the pane's grid reads busy, whatever `reason` says; `null`
    /// for an idle or unobservable grid. Since D3 a busy grid is not itself
    /// a reason — it is reported here so the screen heuristic stays
    /// auditable.
    pub idle_evidence: Option<String>,
    pub kind: SessionKind,
}

impl WindDownView {
    /// `grid` is the observation the verdict was computed from; it supplies
    /// `idleEvidence` whenever it reads busy.
    pub fn from_verdict(verdict: &Eligibility, kind: SessionKind, grid: GridIdle) -> Self {
        let (since, until, reason) = match *verdict {
            Eligibility::Eligible { since_ms } => (Some(since_ms), None, None),
            Eligibility::NotYet { since_ms, until_ms } => (Some(since_ms), Some(until_ms), None),
            Eligibility::Ineligible { reason } => (None, None, Some(reason.as_str())),
            Eligibility::Unknown { reason } => (None, None, Some(reason.as_str())),
        };
        let idle_evidence = match grid {
            GridIdle::Busy { evidence } => Some(evidence.wire()),
            GridIdle::Idle { .. } | GridIdle::Unknown => None,
        };
        Self {
            eligibility: verdict.as_str(),
            since,
            until,
            reason,
            idle_evidence,
            kind,
        }
    }

    pub fn is_eligible(&self) -> bool {
        self.eligibility == "eligible"
    }
}

/// The per-process floor of the grace clock: since when this process has seen
/// one pane's screen unchanged. One per terminal; see rule 8 in the module
/// docs.
///
/// The first observation starts the window at its own instant — a pane this
/// process has just started watching has been quiet for no time at all, as
/// far as it can prove. A fingerprint that differs from the previous
/// observation's restarts it. An observation older than the last one recorded
/// (a concurrent observer arriving late) returns the recorded floor unchanged,
/// so the floor never moves backwards.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScreenQuietTracker {
    last: Option<QuietObservation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QuietObservation {
    fingerprint: u64,
    observed_at_ms: i64,
    quiet_since_ms: i64,
}

impl ScreenQuietTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one observation of the screen's `fingerprint` at
    /// `observed_at_ms`, and return since when it has been unchanged.
    pub fn observe(&mut self, fingerprint: u64, observed_at_ms: i64) -> i64 {
        let quiet_since_ms = match self.last {
            Some(last) if observed_at_ms < last.observed_at_ms => return last.quiet_since_ms,
            Some(last) if last.fingerprint == fingerprint => last.quiet_since_ms,
            _ => observed_at_ms,
        };
        self.last = Some(QuietObservation {
            fingerprint,
            observed_at_ms,
            quiet_since_ms,
        });
        quiet_since_ms
    }
}

/// Continuity tracker behind [`GridIdle::Idle`]'s `since_ms`, one per
/// terminal.
///
/// An idle observation can only EXTEND the previous idle window when the
/// previous observation was idle AND the terminal's grid generation counter
/// has not moved since — neither before this observation's first read nor
/// across the read itself. Otherwise a session that worked between two
/// observations minutes apart would read as continuously idle, so the window
/// restarts at this observation.
///
/// **An unmoved counter is strong evidence, not a proof that nothing
/// happened.** `terminal::scan_gate` — the source of this counter — states the
/// limit explicitly: the property it guarantees is NOT "if it has not moved,
/// no byte reached the parser", because the bump lands *after* the grid
/// mutation and *after* the grid lock is released. A byte that reached the
/// parser in that window is drawn but not yet counted, so an observation
/// landing there can extend a window across one missed mutation. Bounded to
/// that one mutation — and Phase 4 now ACTS on this window, so what keeps the
/// residue safe is no longer "nothing acts on it" but the conditions stacked
/// beside it: a terminal session must additionally be declared `finished`, the
/// sideband must not say `working`, no child process may be attached, and all
/// of it must hold for the grace period. One missed mutation cannot satisfy
/// those; this doc still must not claim the stronger property.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GridIdleTracker {
    last: Option<TrackedObservation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TrackedObservation {
    generation: u64,
    observed_at_ms: i64,
    /// [`GridIdle::Idle`] or [`GridIdle::Busy`] — never `Unknown`. Replayed
    /// as-is to a late arrival, so a busy observation keeps its evidence.
    verdict: GridIdle,
}

impl GridIdleTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one observation and return what it shows.
    ///
    /// `generation_before` is the grid generation read immediately before the
    /// grid snapshot, `generation_after` immediately after it; `busy` is the
    /// observation's verdict — `None` when the snapshot looked idle, else the
    /// evidence that it did not; `observed_at_ms` is when the snapshot was
    /// read.
    ///
    /// An observation that is OLDER than the last one recorded — a lower
    /// starting generation, or an earlier timestamp — is a late arrival from a
    /// concurrent observer, and is ignored: the previous verdict is returned
    /// unchanged, so `since` can never move backwards.
    pub fn observe(
        &mut self,
        busy: Option<GridBusyEvidence>,
        generation_before: u64,
        generation_after: u64,
        observed_at_ms: i64,
    ) -> GridIdle {
        if let Some(last) = self.last {
            if generation_before < last.generation || observed_at_ms < last.observed_at_ms {
                return last.verdict;
            }
        }
        if let Some(evidence) = busy {
            let verdict = GridIdle::Busy { evidence };
            self.last = Some(TrackedObservation {
                generation: generation_after,
                observed_at_ms,
                verdict,
            });
            return verdict;
        }
        let continuous = generation_before == generation_after
            && matches!(
                self.last,
                Some(TrackedObservation {
                    generation,
                    verdict: GridIdle::Idle { .. },
                    ..
                }) if generation == generation_before
            );
        let since_ms = match (continuous, self.last) {
            (
                true,
                Some(TrackedObservation {
                    verdict: GridIdle::Idle { since_ms: since },
                    ..
                }),
            ) => since,
            _ => observed_at_ms,
        };
        let verdict = GridIdle::Idle { since_ms };
        self.last = Some(TrackedObservation {
            generation: generation_after,
            observed_at_ms,
            verdict,
        });
        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUSY: GridBusyEvidence = GridBusyEvidence::NoPrompt;
    const BUSY_GRID: GridIdle = GridIdle::Busy { evidence: BUSY };

    const GRACE: Duration = Duration::from_secs(600);
    const GRACE_MS: i64 = 600_000;

    /// A terminal session satisfying every condition, idle since t=1000.
    fn eligible_terminal() -> EligibilityInputs {
        EligibilityInputs {
            kind: SessionKind::Terminal,
            work_status: WorkStatus::Finished,
            sideband: Sideband::NeverReported,
            grid: GridIdle::Idle { since_ms: 1_000 },
            prompt: PromptScreen::Ready,
            screen_quiet_since_ms: Some(1_000),
            record: IDLE_RECORD,
            has_live_children: Some(false),
            finished_at_ms: None,
            grace: GRACE,
        }
    }

    /// Claude Code's record says `idle`; the transcript's last message was at
    /// t=1000, the same instant the fixture's grid went idle.
    const IDLE_RECORD: SessionRecord = SessionRecord {
        status: RecordStatus::Idle,
        last_message_ms: Some(1_000),
    };

    // ---- the typed signal outranks the screen (plan
    //      2026-10-23-wind-down-idle-check-mistakes-the-completed-turn-line-
    //      for-a-spinner, Phase 3 / D3) -------------------------------------

    /// (a) A record that says `busy` is never eligible, across a simulated
    /// turn whose screen goes quiet mid-turn (thinking, a background
    /// subagent) and whose transcript stops moving for longer than the grace.
    #[test]
    fn d3_a_busy_record_is_never_eligible_across_a_turn() {
        let mut i = eligible_terminal();
        i.record = SessionRecord {
            status: RecordStatus::Busy,
            last_message_ms: Some(1_000),
        };
        for (grid, now) in [
            (GridIdle::Idle { since_ms: 1_000 }, 1_000 + GRACE_MS),
            (BUSY_GRID, 1_000 + 2 * GRACE_MS),
            (GridIdle::Idle { since_ms: 1_000 }, 1_000 + 10 * GRACE_MS),
            (GridIdle::Idle { since_ms: 1_000 }, i64::MAX),
        ] {
            i.grid = grid;
            assert_eq!(
                eligibility(&i, now),
                Eligibility::Ineligible {
                    reason: IneligibleReason::RecordBusy
                },
                "{grid:?} at {now}"
            );
        }
    }

    /// (b) `waiting` — a permission prompt or an open question — is blocked
    /// on a human, never finished. The record's `waitingFor` value
    /// ("permission prompt" / "input needed") does not change that; the
    /// runner-side mapping test pins both.
    #[test]
    fn d3_b_a_waiting_record_is_not_eligible() {
        let mut i = eligible_terminal();
        i.record.status = RecordStatus::Waiting;
        assert_eq!(
            eligibility(&i, i64::MAX),
            Eligibility::Ineligible {
                reason: IneligibleReason::RecordWaiting
            }
        );
    }

    /// (c) `shell` — a background Bash is running.
    #[test]
    fn d3_c_a_shell_record_is_not_eligible() {
        let mut i = eligible_terminal();
        i.record.status = RecordStatus::Shell;
        assert_eq!(
            eligibility(&i, i64::MAX),
            Eligibility::Ineligible {
                reason: IneligibleReason::RecordShell
            }
        );
    }

    /// (d) No readable record is not closable, and neither is an idle record
    /// whose transcript age cannot be established.
    #[test]
    fn d3_d_an_unknown_record_or_transcript_age_is_not_eligible() {
        let mut i = eligible_terminal();
        i.record = SessionRecord::UNKNOWN;
        assert_eq!(
            eligibility(&i, i64::MAX),
            Eligibility::Ineligible {
                reason: IneligibleReason::RecordUnknown
            }
        );
        let mut i = eligible_terminal();
        i.record.last_message_ms = None;
        assert_eq!(
            eligibility(&i, i64::MAX),
            Eligibility::Ineligible {
                reason: IneligibleReason::TranscriptAgeUnknown
            }
        );
    }

    /// (e)/(f) An idle record and an old transcript do not outrank POSITIVE
    /// screen evidence of an unsent draft or an open dialog — the grid's one
    /// remaining job. (The screen → `NotReady` mapping for a draft and a
    /// permission dialog is pinned beside `graceful_exit`'s check.)
    #[test]
    fn d3_e_f_a_draft_or_dialog_on_screen_vetoes_an_idle_record() {
        let mut i = eligible_terminal();
        i.prompt = PromptScreen::NotReady;
        for now in [1_000 + GRACE_MS, i64::MAX] {
            assert_eq!(
                eligibility(&i, now),
                Eligibility::Ineligible {
                    reason: IneligibleReason::PromptNotReady
                }
            );
        }
        // A screen that could not be read cannot vouch for an empty input.
        let mut i = eligible_terminal();
        i.prompt = PromptScreen::Unknown;
        assert_eq!(
            eligibility(&i, i64::MAX),
            Eligibility::Unknown {
                reason: UnknownReason::GridUnknown
            }
        );
    }

    /// (g) An idle record, an old transcript and an empty prompt are eligible
    /// once the transcript has been quiet for the grace — EVEN when the
    /// turn-complete screen heuristic reads the grid busy. The grace runs
    /// from the last transcript message, not from the grid.
    #[test]
    fn d3_g_idle_record_old_transcript_clean_prompt_is_eligible_after_grace() {
        let mut i = eligible_terminal();
        i.grid = GridIdle::Busy {
            evidence: GridBusyEvidence::Glyph('✻'),
        };
        assert_eq!(
            eligibility(&i, 1_000 + GRACE_MS - 1),
            Eligibility::NotYet {
                since_ms: 1_000,
                until_ms: 1_000 + GRACE_MS
            }
        );
        assert_eq!(
            eligibility(&i, 1_000 + GRACE_MS),
            Eligibility::Eligible { since_ms: 1_000 }
        );
        // A grid idle window that started LATER no longer delays it: the
        // screen is a veto, not a clock.
        i.grid = GridIdle::Idle { since_ms: 400_000 };
        assert!(eligibility(&i, 1_000 + GRACE_MS).is_eligible());
    }

    // ---- rule 6: grace ----------------------------------------------------

    #[test]
    fn finished_idle_terminal_past_grace_is_eligible() {
        let v = eligibility(&eligible_terminal(), 1_000 + GRACE_MS + 1);
        assert_eq!(v, Eligibility::Eligible { since_ms: 1_000 });
        assert!(v.is_eligible());
    }

    #[test]
    fn grace_boundary_is_inclusive() {
        assert_eq!(
            eligibility(&eligible_terminal(), 1_000 + GRACE_MS),
            Eligibility::Eligible { since_ms: 1_000 }
        );
    }

    #[test]
    fn inside_grace_is_not_yet_with_the_eligible_instant() {
        assert_eq!(
            eligibility(&eligible_terminal(), 1_000 + GRACE_MS - 1),
            Eligibility::NotYet {
                since_ms: 1_000,
                until_ms: 1_000 + GRACE_MS
            }
        );
    }

    #[test]
    fn a_since_in_the_future_is_not_yet_never_eligible() {
        let mut i = eligible_terminal();
        i.record.last_message_ms = Some(5_000_000);
        assert!(matches!(eligibility(&i, 1_000), Eligibility::NotYet { .. }));
    }

    #[test]
    fn grace_is_honoured_as_configured() {
        let mut i = eligible_terminal();
        i.grace = Duration::from_secs(5);
        assert!(eligibility(&i, 1_000 + 5_000).is_eligible());
        assert!(!eligibility(&i, 1_000 + 4_999).is_eligible());
    }

    // ---- rule 1: unknowns -------------------------------------------------

    #[test]
    fn unknown_work_status_on_a_terminal_session_is_unknown() {
        let mut i = eligible_terminal();
        i.work_status = WorkStatus::Unknown;
        assert_eq!(
            eligibility(&i, i64::MAX),
            Eligibility::Unknown {
                reason: UnknownReason::WorkStatusUnknown
            }
        );
    }

    #[test]
    fn unreadable_sideband_is_unknown() {
        let mut i = eligible_terminal();
        i.sideband = Sideband::Unreadable;
        assert_eq!(
            eligibility(&i, i64::MAX),
            Eligibility::Unknown {
                reason: UnknownReason::SidebandUnreadable
            }
        );
    }

    #[test]
    fn unobservable_grid_is_unknown() {
        let mut i = eligible_terminal();
        i.grid = GridIdle::Unknown;
        assert_eq!(
            eligibility(&i, i64::MAX),
            Eligibility::Unknown {
                reason: UnknownReason::GridUnknown
            }
        );
    }

    #[test]
    fn unknown_children_hint_is_unknown() {
        let mut i = eligible_terminal();
        i.has_live_children = None;
        assert_eq!(
            eligibility(&i, i64::MAX),
            Eligibility::Unknown {
                reason: UnknownReason::ChildrenUnknown
            }
        );
    }

    #[test]
    fn an_unknown_input_outranks_a_definite_ineligibility() {
        // "Any unknown input -> Unknown" is rule 1: a working sideband beside
        // an unreadable coord still reads Unknown.
        let mut i = eligible_terminal();
        i.work_status = WorkStatus::Unknown;
        i.sideband = Sideband::Reported {
            state: SidebandState::Working,
            set_at_ms: 2_000,
        };
        assert!(matches!(
            eligibility(&i, i64::MAX),
            Eligibility::Unknown { .. }
        ));
    }

    // ---- rule 2: working sideband -----------------------------------------

    #[test]
    fn working_sideband_is_ineligible() {
        let mut i = eligible_terminal();
        i.sideband = Sideband::Reported {
            state: SidebandState::Working,
            set_at_ms: 0,
        };
        assert_eq!(
            eligibility(&i, i64::MAX),
            Eligibility::Ineligible {
                reason: IneligibleReason::SidebandWorking
            }
        );
    }

    #[test]
    fn a_sideband_report_after_the_grid_went_idle_restarts_the_grace_clock() {
        // The grid has been idle since t=1000, but the session reported a
        // non-working state at t=500_000 (i.e. it stopped `working` then), so
        // grace runs from t=500_000.
        let mut i = eligible_terminal();
        i.sideband = Sideband::Reported {
            state: SidebandState::NotWorking,
            set_at_ms: 500_000,
        };
        assert_eq!(
            eligibility(&i, 1_000 + GRACE_MS),
            Eligibility::NotYet {
                since_ms: 500_000,
                until_ms: 500_000 + GRACE_MS
            }
        );
        assert!(eligibility(&i, 500_000 + GRACE_MS).is_eligible());
    }

    #[test]
    fn an_older_sideband_report_does_not_move_the_clock_back() {
        let mut i = eligible_terminal();
        i.record.last_message_ms = Some(900_000);
        i.sideband = Sideband::Reported {
            state: SidebandState::NotWorking,
            set_at_ms: 10,
        };
        assert_eq!(
            eligibility(&i, 900_000 + GRACE_MS),
            Eligibility::Eligible { since_ms: 900_000 }
        );
    }

    #[test]
    fn a_never_reported_sideband_is_not_an_unknown() {
        let i = eligible_terminal();
        assert_eq!(i.sideband, Sideband::NeverReported);
        assert!(eligibility(&i, i64::MAX).is_eligible());
    }

    // ---- the screen-quiet floor (review of plan
    //      2026-10-23-wind-down-idle-check-mistakes-the-completed-turn-line-
    //      for-a-spinner, finding 2) ------------------------------------------

    /// After a runner start (or a handoff-adopted pane) the tracker's first
    /// observation is the floor: an hours-old transcript does not make the
    /// session eligible on the first tick — a full grace must pass first.
    #[test]
    fn the_first_observation_by_this_process_floors_the_grace_clock() {
        let start = 50_000_000;
        let mut t = ScreenQuietTracker::new();
        let mut i = eligible_terminal(); // transcript quiet since t=1000
        i.screen_quiet_since_ms = Some(t.observe(7, start));
        assert_eq!(
            eligibility(&i, start),
            Eligibility::NotYet {
                since_ms: start,
                until_ms: start + GRACE_MS
            }
        );
        i.screen_quiet_since_ms = Some(t.observe(7, start + GRACE_MS - 1));
        assert!(!eligibility(&i, start + GRACE_MS - 1).is_eligible());
        i.screen_quiet_since_ms = Some(t.observe(7, start + GRACE_MS));
        assert_eq!(
            eligibility(&i, start + GRACE_MS),
            Eligibility::Eligible { since_ms: start }
        );
    }

    /// Operator activity that writes no transcript message still changes the
    /// screen, and a changed screen restarts the window.
    #[test]
    fn a_screen_change_restarts_the_window() {
        let mut t = ScreenQuietTracker::new();
        let mut i = eligible_terminal();
        assert_eq!(t.observe(1, 1_000), 1_000);
        i.screen_quiet_since_ms = Some(t.observe(1, 1_000 + GRACE_MS));
        assert!(eligibility(&i, 1_000 + GRACE_MS).is_eligible());
        // A draft typed (and later deleted) between ticks.
        let changed = 1_000 + GRACE_MS + 30_000;
        i.screen_quiet_since_ms = Some(t.observe(2, changed));
        assert_eq!(
            eligibility(&i, changed),
            Eligibility::NotYet {
                since_ms: changed,
                until_ms: changed + GRACE_MS
            }
        );
        i.screen_quiet_since_ms = Some(t.observe(1, changed + 30_000));
        assert_eq!(i.screen_quiet_since_ms, Some(changed + 30_000));
        // A late, older observation never moves the floor backwards.
        assert_eq!(t.observe(9, changed), changed + 30_000);
        assert_eq!(t.observe(1, changed + 60_000), changed + 30_000);
    }

    /// An old transcript ALONE never makes a session eligible early, and an
    /// unobserved floor is unknown, never "quiet forever".
    #[test]
    fn an_old_transcript_alone_is_not_enough() {
        let mut i = eligible_terminal();
        i.record.last_message_ms = Some(0);
        i.screen_quiet_since_ms = Some(900_000);
        assert!(!eligibility(&i, 900_000 + GRACE_MS - 1).is_eligible());
        assert!(eligibility(&i, 900_000 + GRACE_MS).is_eligible());
        i.screen_quiet_since_ms = None;
        assert_eq!(
            eligibility(&i, i64::MAX),
            Eligibility::Unknown {
                reason: UnknownReason::GridUnknown
            }
        );
    }

    // ---- rules 3 and 4: the record decides, the screen vetoes ---------------

    #[test]
    fn a_busy_grid_alone_no_longer_blocks_a_close() {
        let mut i = eligible_terminal();
        i.grid = BUSY_GRID;
        assert_eq!(
            eligibility(&i, i64::MAX),
            Eligibility::Eligible { since_ms: 1_000 }
        );
    }

    // ---- rule 4: children -------------------------------------------------

    #[test]
    fn live_children_are_ineligible() {
        let mut i = eligible_terminal();
        i.has_live_children = Some(true);
        assert_eq!(
            eligibility(&i, i64::MAX),
            Eligibility::Ineligible {
                reason: IneligibleReason::LiveChildren
            }
        );
    }

    // ---- rule 5: finished -------------------------------------------------

    #[test]
    fn an_idle_unfinished_terminal_session_is_never_eligible() {
        let mut i = eligible_terminal();
        i.work_status = WorkStatus::NotFinished;
        for now in [1_000 + GRACE_MS, 1_000 + 100 * GRACE_MS, i64::MAX] {
            assert_eq!(
                eligibility(&i, now),
                Eligibility::Ineligible {
                    reason: IneligibleReason::NotFinished
                },
                "idle for {now} ms and still not finished"
            );
        }
    }

    #[test]
    fn looping_and_steward_sessions_are_exempt_from_finished() {
        for kind in [SessionKind::Looping, SessionKind::Steward] {
            for work_status in [
                WorkStatus::Unknown,
                WorkStatus::NotFinished,
                WorkStatus::Finished,
            ] {
                let mut i = eligible_terminal();
                i.kind = kind;
                i.work_status = work_status;
                assert_eq!(
                    eligibility(&i, 1_000 + GRACE_MS),
                    Eligibility::Eligible { since_ms: 1_000 },
                    "{kind:?} with {work_status:?}"
                );
            }
        }
    }

    #[test]
    fn exempt_kinds_still_need_idle_no_children_non_working_and_grace() {
        for kind in [SessionKind::Looping, SessionKind::Steward] {
            let base = EligibilityInputs {
                kind,
                work_status: WorkStatus::NotFinished,
                ..eligible_terminal()
            };

            let mut busy = base;
            busy.record.status = RecordStatus::Busy;
            assert!(matches!(
                eligibility(&busy, i64::MAX),
                Eligibility::Ineligible {
                    reason: IneligibleReason::RecordBusy
                }
            ));

            let mut children = base;
            children.has_live_children = Some(true);
            assert!(matches!(
                eligibility(&children, i64::MAX),
                Eligibility::Ineligible {
                    reason: IneligibleReason::LiveChildren
                }
            ));

            let mut working = base;
            working.sideband = Sideband::Reported {
                state: SidebandState::Working,
                set_at_ms: 0,
            };
            assert!(matches!(
                eligibility(&working, i64::MAX),
                Eligibility::Ineligible {
                    reason: IneligibleReason::SidebandWorking
                }
            ));

            let mut unknown_grid = base;
            unknown_grid.grid = GridIdle::Unknown;
            assert!(matches!(
                eligibility(&unknown_grid, i64::MAX),
                Eligibility::Unknown {
                    reason: UnknownReason::GridUnknown
                }
            ));

            assert!(matches!(
                eligibility(&base, 1_000 + GRACE_MS - 1),
                Eligibility::NotYet { .. }
            ));
        }
    }

    // ---- grace resolution -------------------------------------------------

    #[test]
    fn grace_override_parsing() {
        assert_eq!(grace_from(None), DEFAULT_GRACE);
        assert_eq!(DEFAULT_GRACE, Duration::from_secs(600));
        assert_eq!(grace_from(Some("90")), Duration::from_secs(90));
        assert_eq!(grace_from(Some(" 30 ")), Duration::from_secs(30));
        assert_eq!(grace_from(Some("0")), DEFAULT_GRACE, "zero is refused");
        assert_eq!(grace_from(Some("")), DEFAULT_GRACE);
        assert_eq!(grace_from(Some("10m")), DEFAULT_GRACE);
        assert_eq!(grace_from(Some("-5")), DEFAULT_GRACE);
    }

    // ---- wire view ----------------------------------------------------------

    #[test]
    fn wire_view_carries_each_arm() {
        let e = WindDownView::from_verdict(
            &Eligibility::Eligible { since_ms: 7 },
            SessionKind::Terminal,
            GridIdle::Idle { since_ms: 7 },
        );
        assert_eq!(
            (e.eligibility, e.since, e.until, e.reason),
            ("eligible", Some(7), None, None)
        );
        assert!(e.is_eligible());

        let n = WindDownView::from_verdict(
            &Eligibility::NotYet {
                since_ms: 7,
                until_ms: 9,
            },
            SessionKind::Looping,
            GridIdle::Idle { since_ms: 7 },
        );
        assert_eq!(
            (n.eligibility, n.since, n.until, n.reason),
            ("not_yet", Some(7), Some(9), None)
        );
        assert!(!n.is_eligible());

        let i = WindDownView::from_verdict(
            &Eligibility::Ineligible {
                reason: IneligibleReason::NotFinished,
            },
            SessionKind::Terminal,
            GridIdle::Idle { since_ms: 7 },
        );
        assert_eq!(
            (i.eligibility, i.reason),
            ("ineligible", Some("not_finished"))
        );

        let u = WindDownView::from_verdict(
            &Eligibility::Unknown {
                reason: UnknownReason::GridUnknown,
            },
            SessionKind::Steward,
            GridIdle::Unknown,
        );
        assert_eq!((u.eligibility, u.reason), ("unknown", Some("grid_unknown")));

        let json = serde_json::to_value(&n).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "eligibility": "not_yet", "since": 7, "until": 9,
                "reason": null, "idleEvidence": null, "kind": "looping"
            })
        );
    }

    // ---- GridIdleTracker ---------------------------------------------------

    #[test]
    fn tracker_first_idle_observation_starts_the_window() {
        let mut t = GridIdleTracker::new();
        assert_eq!(t.observe(None, 5, 5, 100), GridIdle::Idle { since_ms: 100 });
    }

    #[test]
    fn tracker_extends_the_window_while_the_grid_does_not_move() {
        let mut t = GridIdleTracker::new();
        t.observe(None, 5, 5, 100);
        assert_eq!(t.observe(None, 5, 5, 900), GridIdle::Idle { since_ms: 100 });
        assert_eq!(
            t.observe(None, 5, 5, 5_000),
            GridIdle::Idle { since_ms: 100 }
        );
    }

    #[test]
    fn tracker_restarts_when_the_grid_moved_between_observations() {
        // Idle at t=100 and idle again at t=900, but the generation advanced
        // in between: the session may have worked in the gap.
        let mut t = GridIdleTracker::new();
        t.observe(None, 5, 5, 100);
        assert_eq!(t.observe(None, 8, 8, 900), GridIdle::Idle { since_ms: 900 });
        // ...and then extends from the new window.
        assert_eq!(
            t.observe(None, 8, 8, 1_500),
            GridIdle::Idle { since_ms: 900 }
        );
    }

    #[test]
    fn tracker_restarts_when_the_grid_moved_during_the_debounce() {
        let mut t = GridIdleTracker::new();
        t.observe(None, 5, 5, 100);
        assert_eq!(t.observe(None, 5, 6, 900), GridIdle::Idle { since_ms: 900 });
    }

    #[test]
    fn tracker_busy_observation_breaks_the_window() {
        let mut t = GridIdleTracker::new();
        t.observe(None, 5, 5, 100);
        assert_eq!(t.observe(Some(BUSY), 5, 5, 200), BUSY_GRID);
        assert_eq!(t.observe(None, 5, 5, 300), GridIdle::Idle { since_ms: 300 });
    }

    #[test]
    fn tracker_ignores_an_observation_from_an_older_generation() {
        let mut t = GridIdleTracker::new();
        t.observe(None, 9, 9, 1_000);
        // A concurrent observer's late snapshot of generation 7 must neither
        // restart nor backdate the window.
        assert_eq!(
            t.observe(None, 7, 7, 2_000),
            GridIdle::Idle { since_ms: 1_000 }
        );
        assert_eq!(
            t.observe(Some(BUSY), 7, 8, 2_000),
            GridIdle::Idle { since_ms: 1_000 }
        );
        assert_eq!(
            t.observe(None, 9, 9, 3_000),
            GridIdle::Idle { since_ms: 1_000 }
        );
    }

    #[test]
    fn tracker_ignores_an_observation_older_in_time_so_since_never_moves_back() {
        let mut t = GridIdleTracker::new();
        t.observe(Some(BUSY), 4, 4, 5_000);
        assert_eq!(
            t.observe(None, 4, 4, 6_000),
            GridIdle::Idle { since_ms: 6_000 }
        );
        // Same generation, earlier timestamp: a late arrival, ignored.
        assert_eq!(
            t.observe(None, 4, 4, 5_500),
            GridIdle::Idle { since_ms: 6_000 }
        );
        assert_eq!(
            t.observe(None, 4, 4, 9_000),
            GridIdle::Idle { since_ms: 6_000 }
        );
    }

    // ---- finished_at bounds the window (terminal sessions) ------------------

    #[test]
    fn a_finished_declaration_after_the_idle_window_opened_restarts_grace() {
        let mut i = eligible_terminal();
        i.finished_at_ms = Some(400_000);
        assert_eq!(
            eligibility(&i, 1_000 + GRACE_MS),
            Eligibility::NotYet {
                since_ms: 400_000,
                until_ms: 400_000 + GRACE_MS
            }
        );
        assert!(eligibility(&i, 400_000 + GRACE_MS).is_eligible());
    }

    #[test]
    fn a_finished_declaration_before_the_idle_window_does_not_move_it() {
        let mut i = eligible_terminal();
        i.record.last_message_ms = Some(50_000);
        i.finished_at_ms = Some(10);
        assert_eq!(
            eligibility(&i, 50_000 + GRACE_MS),
            Eligibility::Eligible { since_ms: 50_000 }
        );
    }

    #[test]
    fn finished_at_is_ignored_for_exempt_kinds() {
        for kind in [SessionKind::Looping, SessionKind::Steward] {
            let mut i = eligible_terminal();
            i.kind = kind;
            i.finished_at_ms = Some(900_000);
            assert!(eligibility(&i, 1_000 + GRACE_MS).is_eligible(), "{kind:?}");
        }
    }

    // ---- idleEvidence --------------------------------------------------------

    #[test]
    fn every_busy_evidence_has_its_wire_value() {
        for (evidence, wire) in [
            (
                GridBusyEvidence::Indicator("tokens ·"),
                "indicator:tokens ·",
            ),
            (
                GridBusyEvidence::Indicator("esc to interrupt"),
                "indicator:esc to interrupt",
            ),
            (GridBusyEvidence::Glyph('✻'), "glyph:✻"),
            (GridBusyEvidence::NoPrompt, "no_prompt"),
            (GridBusyEvidence::CursorAbovePrompt, "cursor_above_prompt"),
            (GridBusyEvidence::GenerationMoved, "generation_moved"),
        ] {
            assert_eq!(evidence.wire(), wire);
        }
    }

    #[test]
    fn screen_verdicts_map_onto_evidence_and_idle_has_none() {
        assert_eq!(GridBusyEvidence::from_screen(IdleVerdict::Idle), None);
        assert_eq!(
            GridBusyEvidence::from_screen(IdleVerdict::Indicator("compacting")),
            Some(GridBusyEvidence::Indicator("compacting"))
        );
        assert_eq!(
            GridBusyEvidence::from_screen(IdleVerdict::Glyph('✶')),
            Some(GridBusyEvidence::Glyph('✶'))
        );
        assert_eq!(
            GridBusyEvidence::from_screen(IdleVerdict::NoPrompt),
            Some(GridBusyEvidence::NoPrompt)
        );
        assert_eq!(
            GridBusyEvidence::from_screen(IdleVerdict::CursorAbovePrompt),
            Some(GridBusyEvidence::CursorAbovePrompt)
        );
    }

    #[test]
    fn a_moved_generation_outranks_the_screen() {
        assert_eq!(
            GridBusyEvidence::of_observation(3, 4, IdleVerdict::Idle),
            Some(GridBusyEvidence::GenerationMoved)
        );
        assert_eq!(
            GridBusyEvidence::of_observation(3, 4, IdleVerdict::Glyph('✻')),
            Some(GridBusyEvidence::GenerationMoved)
        );
        assert_eq!(
            GridBusyEvidence::of_observation(4, 4, IdleVerdict::Glyph('✻')),
            Some(GridBusyEvidence::Glyph('✻'))
        );
        assert_eq!(
            GridBusyEvidence::of_observation(4, 4, IdleVerdict::Idle),
            None
        );
    }

    #[test]
    fn idle_evidence_is_reported_for_every_busy_grid_whatever_the_reason() {
        let grid = GridIdle::Busy {
            evidence: GridBusyEvidence::Glyph('✻'),
        };
        let mut i = eligible_terminal();
        i.grid = grid;
        // Eligible on the record, and the screen heuristic's busy reading is
        // still on the record beside it.
        let view =
            WindDownView::from_verdict(&eligibility(&i, i64::MAX), SessionKind::Terminal, grid);
        assert_eq!(view.eligibility, "eligible");
        assert_eq!(view.idle_evidence.as_deref(), Some("glyph:✻"));
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["idleEvidence"], "glyph:✻");

        // Beside a reason that decided on something else, too.
        let mut unknown = i;
        unknown.work_status = WorkStatus::Unknown;
        let v = WindDownView::from_verdict(
            &eligibility(&unknown, i64::MAX),
            SessionKind::Terminal,
            grid,
        );
        assert_eq!(v.reason, Some("work_status_unknown"));
        assert_eq!(v.idle_evidence.as_deref(), Some("glyph:✻"));

        // An idle or unobservable grid never carries evidence.
        let idle = eligible_terminal();
        for grid in [idle.grid, GridIdle::Unknown] {
            let v = WindDownView::from_verdict(
                &eligibility(&idle, i64::MAX),
                SessionKind::Terminal,
                grid,
            );
            assert_eq!(v.idle_evidence, None);
            assert!(serde_json::to_value(&v).unwrap()["idleEvidence"].is_null());
        }
    }

    #[test]
    fn every_ineligible_reason_has_a_distinct_wire_word() {
        let all = [
            IneligibleReason::SidebandWorking,
            IneligibleReason::LiveChildren,
            IneligibleReason::NotFinished,
            IneligibleReason::RecordBusy,
            IneligibleReason::RecordWaiting,
            IneligibleReason::RecordShell,
            IneligibleReason::RecordUnknown,
            IneligibleReason::TranscriptAgeUnknown,
            IneligibleReason::PromptNotReady,
        ];
        let words: std::collections::HashSet<&str> = all.iter().map(|r| r.as_str()).collect();
        assert_eq!(words.len(), all.len());
        for r in all {
            assert_eq!(serde_json::to_value(r).unwrap(), r.as_str());
        }
    }

    #[test]
    fn tracker_reports_and_replays_the_busy_evidence() {
        let mut t = GridIdleTracker::new();
        let gen = GridBusyEvidence::GenerationMoved;
        assert_eq!(
            t.observe(Some(gen), 5, 6, 1_000),
            GridIdle::Busy { evidence: gen }
        );
        // A late arrival gets the recorded verdict back, evidence included.
        assert_eq!(
            t.observe(None, 4, 4, 2_000),
            GridIdle::Busy { evidence: gen }
        );
        let glyph = GridBusyEvidence::Glyph('✻');
        assert_eq!(
            t.observe(Some(glyph), 6, 6, 3_000),
            GridIdle::Busy { evidence: glyph }
        );
    }
}
