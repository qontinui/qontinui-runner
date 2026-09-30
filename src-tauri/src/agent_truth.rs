//! Agent truth — the ONE pure reducer that answers "what is the agent in this
//! terminal doing right now, and how do we know?" (plan
//! `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
//! Phase 2, design decision D1).
//!
//! ## What this is, and what it never does
//!
//! [`AgentTruth`] is a per-terminal fold over typed observations from six
//! [`Source`]s. It performs no I/O, reads no clock (`now_ms` is always an
//! argument, unix millis), spawns nothing and acts on nothing. The impure glue
//! that feeds it — the hook ingest route, the OSC 9999 sideband dispatch, the
//! grid observer — lives in the runner bin and adapts its own types into the
//! projections defined here ([`InputEvidence`], [`GridIdle`],
//! [`StateCapabilities`]), exactly as `session::wind_down_observer` adapts into
//! [`crate::wind_down`]. The lib crate cannot name `terminal::*` or
//! `session::*`, and it should not: one direction of dependency.
//!
//! ## The rules
//!
//! 1. **No observation ⇒ [`AgentState::Unknown`], never idle.** Input, grid and
//!    children evidence alone never produce a state; they only reinterpret or
//!    annotate a state some source reported.
//! 2. **Events are EDGES that set a LEVEL.** A source's last accepted
//!    observation holds until that source reports again — even once it is
//!    stale, as long as nothing newer contradicts it. The two *pulse* sources
//!    ([`Source::Statusline`], [`Source::Transcript`]) are the exception: they
//!    are liveness ticks, so their level ("working") exists only while fresh.
//! 3. **Precedence** (highest first): `Hook > Sideband > Statusline >
//!    Transcript > ScreenStability > Regex` ([`Source::rank`]). The verdict is
//!    the highest-ranked source with a level, unless a LOWER source's edge is
//!    newer than the higher source's last report AND either
//!    - the higher source is stale ([`Source::freshness_ttl_ms`]), or
//!    - the provider's [`StateCapabilities`] say the higher source cannot
//!      express the state the lower one reports (the capability fallthrough —
//!      e.g. a provider with no "waiting" event gets `NeedsYou` from the
//!      screen, labelled [`Confidence::Fallback`]).
//!
//!    "No higher source has ever reported" is the degenerate case: there is
//!    nothing to outrank the lower one.
//! 4. **A lower source contradicting the winner never overrides it** — it is
//!    recorded as [`Disagreement::Contradiction`] (the first, highest-ranked
//!    fresh lower source whose state CLASS differs from the verdict's).
//! 5. **The human answered.** A `NeedsYou` level followed by a PTY submit
//!    ([`InputEvidence::last_submit_ms`] strictly after the level began) reads
//!    as `Working` with [`Confidence::Inferred`], since the submit. This is
//!    applied to every `NeedsYou` reason, not only `Permission`: an answered
//!    `AskUserQuestion` or elicitation fires no hook either, and a keystroke
//!    writer must never re-approve a pane the human already answered. A NEW
//!    same-state edge after the submit (a second permission ask) starts a new
//!    level and is authoritative again.
//! 6. **Quiet is not idle.** `Working` from an authoritative source while the
//!    grid has been idle for more than [`QUIET_WHILE_WORKING_AFTER_MS`] with no
//!    live children leaves the state unchanged and records
//!    [`Disagreement::QuietWhileWorking`] — never an override (the 12-minute
//!    `rm -rf` was legitimately quiet). A recorded contradiction takes the one
//!    disagreement slot first; quiet is reported only when none exists.
//! 7. **Subagent events are ignored in v1** ([`Observation::is_subagent`]): the
//!    reducer keys on the top-level session only. `Stop` inside a subagent
//!    arrives as `SubagentStop`, which no caller should offer at all.
//!
//! ## Confidence
//!
//! [`Source::confidence`]: `Hook`/`Sideband` ⇒ [`Confidence::Authoritative`]
//! (the agent reporting on itself); `Statusline`/`Transcript` ⇒
//! [`Confidence::Inferred`]; `ScreenStability`/`Regex` ⇒
//! [`Confidence::Fallback`]. Rule 5 downgrades to `Inferred`.
//!
//! The ONE question keystroke writers may ask is
//! [`Verdict::is_authoritative_permission_ask`].

use serde::Serialize;

// ---------------------------------------------------------------------------
// Freshness constants
// ---------------------------------------------------------------------------

/// How long a hook-reported level blocks lower sources. Long, because the
/// runner registers TURN-grain events only (no `PreToolUse`/`PostToolUse`): a
/// long agentic turn legitimately produces no hook event between
/// `UserPromptSubmit` and `Stop`. After this, a newer lower report may fill
/// (the relay or the hooks may have died).
pub const HOOK_TTL_MS: u64 = 2 * 60 * 60 * 1000;

/// OSC 9999 self-reports. Emitters are third-party and may be turn-grain too,
/// but they are expected to re-report more often than hooks.
pub const SIDEBAND_TTL_MS: u64 = 30 * 60 * 1000;

/// Statusline ticks run on each assistant message (300 ms debounce). A pulse:
/// its "working" exists only within this window.
pub const STATUSLINE_TTL_MS: u64 = 2 * 60 * 1000;

/// Transcript activity (a new assistant/tool record). A pulse, like the
/// statusline.
pub const TRANSCRIPT_TTL_MS: u64 = 2 * 60 * 1000;

/// Screen-stability observations are re-offered by the grid observer on its
/// own cadence.
pub const SCREEN_STABILITY_TTL_MS: u64 = 2 * 60 * 1000;

/// Regex-detector states are re-offered on every detection pass.
pub const REGEX_TTL_MS: u64 = 2 * 60 * 1000;

/// Rule 6: how long the grid must have been idle under an authoritative
/// `Working` before [`Disagreement::QuietWhileWorking`] is recorded.
pub const QUIET_WHILE_WORKING_AFTER_MS: u64 = 10 * 60 * 1000;

/// A grid or children observation older than this is not used for rule 6.
pub const CONTEXT_OBSERVATION_TTL_MS: u64 = 2 * 60 * 1000;

// ---------------------------------------------------------------------------
// Sources and confidence
// ---------------------------------------------------------------------------

/// Where an observation came from. Totally ordered by [`Source::rank`], so
/// `Source::Hook > Source::Regex`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A Claude Code hook event relayed by the runner's carrier.
    Hook,
    /// An OSC 9999 agent-status self-report on the PTY.
    Sideband,
    /// A statusline tick (pulse).
    Statusline,
    /// Transcript activity (pulse).
    Transcript,
    /// "Has the rendered grid changed" — the grid-generation observer.
    ScreenStability,
    /// The text-pattern detector over rendered output.
    Regex,
}

impl Source {
    /// Every source, highest rank first.
    pub const ALL: [Source; 6] = [
        Source::Hook,
        Source::Sideband,
        Source::Statusline,
        Source::Transcript,
        Source::ScreenStability,
        Source::Regex,
    ];

    /// Precedence rank; higher wins. `Hook` = 5 … `Regex` = 0.
    pub const fn rank(self) -> u8 {
        match self {
            Source::Hook => 5,
            Source::Sideband => 4,
            Source::Statusline => 3,
            Source::Transcript => 2,
            Source::ScreenStability => 1,
            Source::Regex => 0,
        }
    }

    /// Position in [`Source::ALL`] (and in per-source arrays).
    const fn index(self) -> usize {
        5 - self.rank() as usize
    }

    /// The confidence a verdict taken from this source carries.
    pub const fn confidence(self) -> Confidence {
        match self {
            Source::Hook | Source::Sideband => Confidence::Authoritative,
            Source::Statusline | Source::Transcript => Confidence::Inferred,
            Source::ScreenStability | Source::Regex => Confidence::Fallback,
        }
    }

    /// How long a level from this source blocks lower sources.
    pub const fn freshness_ttl_ms(self) -> u64 {
        match self {
            Source::Hook => HOOK_TTL_MS,
            Source::Sideband => SIDEBAND_TTL_MS,
            Source::Statusline => STATUSLINE_TTL_MS,
            Source::Transcript => TRANSCRIPT_TTL_MS,
            Source::ScreenStability => SCREEN_STABILITY_TTL_MS,
            Source::Regex => REGEX_TTL_MS,
        }
    }

    /// Pulse sources report liveness, not edges: their level exists only
    /// while fresh (rule 2).
    pub const fn is_pulse(self) -> bool {
        matches!(self, Source::Statusline | Source::Transcript)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Source::Hook => "hook",
            Source::Sideband => "sideband",
            Source::Statusline => "statusline",
            Source::Transcript => "transcript",
            Source::ScreenStability => "screen_stability",
            Source::Regex => "regex",
        }
    }
}

impl PartialOrd for Source {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Source {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}

/// How much a verdict may be acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// The agent reported this about itself (hook, sideband).
    Authoritative,
    /// Derived from a liveness signal, or reinterpreted from input evidence.
    Inferred,
    /// Read off the rendered screen.
    Fallback,
}

// ---------------------------------------------------------------------------
// States
// ---------------------------------------------------------------------------

/// Why the agent is waiting on a human.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedsYouReason {
    /// A tool-permission ask (`PermissionRequest`, `Notification:
    /// permission_prompt`, an approval-shaped screen).
    Permission,
    /// A question to the human (`AskUserQuestion`, a question-shaped screen).
    Question,
    /// An MCP elicitation dialog.
    Elicitation,
    /// The input box has been idle (`Notification: idle_prompt`).
    IdlePrompt,
    /// The source said "waiting on a human" without saying why (sideband
    /// `waiting_human`). Never treated as a permission ask.
    Unspecified,
}

/// Why a turn failed. Mapped totally from `StopFailure.error_type` by
/// [`FailureKind::from_error_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// Rate limited. Deliberately NOT [`FailureKind::QuotaExhausted`]: the CLI
    /// reports both a usage limit and transient server throttling as
    /// `rate_limit` as far as we know (plan Phase 1 Q4); only the OAuth usage
    /// probe (Phase 8) may conclude the quota is exhausted.
    RateLimited,
    /// The account cannot serve more requests (credit/spend exhausted).
    QuotaExhausted,
    /// The context window is exhausted. No documented `error_type` maps here
    /// today; reserved for context signals (statusline / transcript).
    ContextExhausted,
    /// A human must fix the credential or the account before retrying.
    AuthRequired,
    /// The provider is overloaded; a retry later may succeed.
    Overloaded,
    /// The connection to the provider was lost. No documented `error_type`
    /// maps here today; reserved for runner-observed transport loss.
    TransportLost,
    /// The provider rejected or failed the request for a request- or
    /// server-side reason a retry will not obviously fix.
    ProviderError,
    /// `unknown`, or an `error_type` this build does not recognise.
    Unknown,
}

/// Every `StopFailure.error_type` documented for Claude Code (as of CLI
/// 2.1.278; `cloud_credential_error` since 2.1.267).
pub const DOCUMENTED_STOP_FAILURE_ERROR_TYPES: [&str; 12] = [
    "rate_limit",
    "overloaded",
    "authentication_failed",
    "oauth_org_not_allowed",
    "account_on_hold",
    "billing_error",
    "invalid_request",
    "model_not_found",
    "server_error",
    "max_output_tokens",
    "cloud_credential_error",
    "unknown",
];

impl FailureKind {
    /// Total mapping from a `StopFailure.error_type` wire string. Exact match;
    /// anything unrecognised is [`FailureKind::Unknown`].
    ///
    /// | `error_type` | kind | why |
    /// |---|---|---|
    /// | `rate_limit` | `RateLimited` | never `QuotaExhausted` — the probe distinguishes |
    /// | `overloaded` | `Overloaded` | transient, retry later |
    /// | `authentication_failed` | `AuthRequired` | credential must be fixed |
    /// | `oauth_org_not_allowed` | `AuthRequired` | the credential's org is refused; re-auth or switch account |
    /// | `account_on_hold` | `AuthRequired` | the account is unusable until a human acts on it |
    /// | `billing_error` | `QuotaExhausted` | the account cannot pay for more requests |
    /// | `invalid_request` | `ProviderError` | request rejected; retrying the same request will not help |
    /// | `model_not_found` | `ProviderError` | configuration error on the request |
    /// | `server_error` | `ProviderError` | provider-side failure, not load |
    /// | `max_output_tokens` | `ProviderError` | the response hit its OUTPUT cap — not the context window |
    /// | `cloud_credential_error` | `AuthRequired` | cloud-provider credential must be fixed |
    /// | `unknown` / anything else | `Unknown` | |
    pub fn from_error_type(error_type: &str) -> Self {
        match error_type {
            "rate_limit" => FailureKind::RateLimited,
            "overloaded" => FailureKind::Overloaded,
            "authentication_failed"
            | "oauth_org_not_allowed"
            | "account_on_hold"
            | "cloud_credential_error" => FailureKind::AuthRequired,
            "billing_error" => FailureKind::QuotaExhausted,
            "invalid_request" | "model_not_found" | "server_error" | "max_output_tokens" => {
                FailureKind::ProviderError
            }
            _ => FailureKind::Unknown,
        }
    }
}

/// Why a session ended (`SessionEnd.reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Clear,
    Logout,
    PromptInputExit,
    BypassPermissionsDisabled,
    /// `other`, or a reason this build does not recognise.
    Other,
}

impl EndReason {
    /// Total mapping from the `SessionEnd.reason` wire string.
    pub fn from_wire(reason: &str) -> Self {
        match reason {
            "clear" => EndReason::Clear,
            "logout" => EndReason::Logout,
            "prompt_input_exit" => EndReason::PromptInputExit,
            "bypass_permissions_disabled" => EndReason::BypassPermissionsDisabled,
            _ => EndReason::Other,
        }
    }
}

/// What the agent is doing. Serialized internally tagged by `name`, e.g.
/// `{"name":"needs_you","reason":"permission"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "name", rename_all = "snake_case")]
pub enum AgentState {
    /// No source has reported anything usable. Never rendered as idle.
    Unknown,
    /// The session (re)started and has not been prompted yet.
    Starting,
    /// A turn is in progress.
    Working,
    /// The agent is waiting on a human.
    NeedsYou { reason: NeedsYouReason },
    /// The last turn ended normally; the agent is at its prompt.
    TurnEnded,
    /// The last turn ended in an error.
    Failed { kind: FailureKind },
    /// The session ended.
    Ended { why: EndReason },
}

/// The coarse class of an [`AgentState`] — the granularity capabilities and
/// disagreements are judged at (a `NeedsYou{Question}` screen does not
/// "disagree" with a `NeedsYou{Permission}` hook).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateClass {
    Starting,
    Working,
    NeedsYou,
    TurnEnded,
    Failed,
    Ended,
}

impl StateClass {
    pub const ALL: [StateClass; 6] = [
        StateClass::Starting,
        StateClass::Working,
        StateClass::NeedsYou,
        StateClass::TurnEnded,
        StateClass::Failed,
        StateClass::Ended,
    ];

    const fn bit(self) -> u8 {
        match self {
            StateClass::Starting => 1 << 0,
            StateClass::Working => 1 << 1,
            StateClass::NeedsYou => 1 << 2,
            StateClass::TurnEnded => 1 << 3,
            StateClass::Failed => 1 << 4,
            StateClass::Ended => 1 << 5,
        }
    }
}

impl AgentState {
    /// `None` only for [`AgentState::Unknown`].
    pub const fn class(self) -> Option<StateClass> {
        match self {
            AgentState::Unknown => None,
            AgentState::Starting => Some(StateClass::Starting),
            AgentState::Working => Some(StateClass::Working),
            AgentState::NeedsYou { .. } => Some(StateClass::NeedsYou),
            AgentState::TurnEnded => Some(StateClass::TurnEnded),
            AgentState::Failed { .. } => Some(StateClass::Failed),
            AgentState::Ended { .. } => Some(StateClass::Ended),
        }
    }
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

/// Per provider: which [`StateClass`]es each [`Source`] can express. Declared
/// by `SessionProviderAdapter::state_capabilities` in the runner bin. Only
/// consulted for the capability fallthrough (rule 3): an observation from a
/// source the matrix says cannot report is still accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StateCapabilities {
    by_source: [u8; 6],
}

impl StateCapabilities {
    /// No source can express anything.
    pub const fn none() -> Self {
        Self { by_source: [0; 6] }
    }

    /// Every source can express every class (test and reference use).
    pub const fn all() -> Self {
        Self {
            by_source: [0b11_1111; 6],
        }
    }

    /// `self` plus `classes` for `source`.
    pub fn with(mut self, source: Source, classes: &[StateClass]) -> Self {
        for class in classes {
            self.by_source[source.index()] |= class.bit();
        }
        self
    }

    /// `self` with nothing for `source`.
    pub fn without_source(mut self, source: Source) -> Self {
        self.by_source[source.index()] = 0;
        self
    }

    pub fn can_express(&self, source: Source, class: StateClass) -> bool {
        self.by_source[source.index()] & class.bit() != 0
    }

    /// The sources that can express `class`, highest rank first.
    pub fn sources_for(&self, class: StateClass) -> Vec<Source> {
        Source::ALL
            .into_iter()
            .filter(|s| self.can_express(*s, class))
            .collect()
    }

    /// What every provider gets from the runner's own observers, whatever the
    /// CLI: the OSC 9999 sideband vocabulary (`working` / `waiting_human` /
    /// `blocked`+`stalled` / `finished`), screen stability (busy / settled)
    /// and the regex detector (working / approval- and question-shaped /
    /// completed / error).
    pub fn fallback_only() -> Self {
        use StateClass as C;
        Self::none()
            .with(
                Source::Sideband,
                &[C::Working, C::NeedsYou, C::TurnEnded, C::Failed],
            )
            .with(Source::ScreenStability, &[C::Working, C::TurnEnded])
            .with(
                Source::Regex,
                &[C::Working, C::NeedsYou, C::TurnEnded, C::Failed],
            )
    }

    /// Claude Code: the fallbacks plus hooks that express every class
    /// (`SessionStart` ⇒ Starting, `UserPromptSubmit` ⇒ Working,
    /// `PermissionRequest`/`Notification` ⇒ NeedsYou, `Stop` ⇒ TurnEnded,
    /// `StopFailure` ⇒ Failed, `SessionEnd` ⇒ Ended) and the statusline and
    /// transcript liveness pulses (Working only).
    pub fn claude() -> Self {
        Self::fallback_only()
            .with(Source::Hook, &StateClass::ALL)
            .with(Source::Statusline, &[StateClass::Working])
            .with(Source::Transcript, &[StateClass::Working])
    }
}

// ---------------------------------------------------------------------------
// Observations
// ---------------------------------------------------------------------------

/// `Notification.notification_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotificationType {
    PermissionPrompt,
    IdlePrompt,
    ElicitationDialog,
    AgentNeedsInput,
    AgentCompleted,
    /// Anything else — claims no state.
    Other,
}

impl NotificationType {
    /// Total mapping from the wire string.
    pub fn from_wire(notification_type: &str) -> Self {
        match notification_type {
            "permission_prompt" => NotificationType::PermissionPrompt,
            "idle_prompt" => NotificationType::IdlePrompt,
            "elicitation_dialog" => NotificationType::ElicitationDialog,
            "agent_needs_input" => NotificationType::AgentNeedsInput,
            "agent_completed" => NotificationType::AgentCompleted,
            _ => NotificationType::Other,
        }
    }
}

/// `SessionStart.source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionStartSource {
    Startup,
    Resume,
    Clear,
    /// Fires after a compaction, which can happen MID-TURN — so it claims no
    /// state (the turn, if any, continues).
    Compact,
    Other,
}

impl SessionStartSource {
    /// Total mapping from the wire string.
    pub fn from_wire(source: &str) -> Self {
        match source {
            "startup" => SessionStartSource::Startup,
            "resume" => SessionStartSource::Resume,
            "clear" => SessionStartSource::Clear,
            "compact" => SessionStartSource::Compact,
            _ => SessionStartSource::Other,
        }
    }
}

/// The tool a `PermissionRequest` is for, reduced to what the state needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionTool {
    /// `AskUserQuestion` — a question, NOT an approval: typing `y` into it
    /// would answer a question the human never read. Provisional pending plan
    /// Phase 1 Q4 (which events fire for `AskUserQuestion`).
    AskUserQuestion,
    /// Any other tool (including `ExitPlanMode`, which is an approval).
    Other,
}

impl PermissionTool {
    pub fn from_tool_name(tool_name: Option<&str>) -> Self {
        match tool_name {
            Some("AskUserQuestion") => PermissionTool::AskUserQuestion,
            _ => PermissionTool::Other,
        }
    }
}

/// A typed Claude Code hook event (top-level session only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookEvent {
    UserPromptSubmit,
    PermissionRequest {
        tool: PermissionTool,
    },
    Notification {
        notification_type: NotificationType,
    },
    Stop,
    /// Replaces `Stop` when the turn ended on an API error — without it a
    /// failed turn would stay `Working`.
    StopFailure {
        kind: FailureKind,
    },
    SessionStart {
        source: SessionStartSource,
    },
    SessionEnd {
        reason: EndReason,
    },
}

/// An OSC 9999 sideband state word (`agent_status_sideband` vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SidebandWord {
    Working,
    Blocked,
    Stalled,
    WaitingHuman,
    Finished,
}

impl SidebandWord {
    /// `None` for anything outside the vocabulary (the sideband drops those).
    pub fn from_wire(word: &str) -> Option<Self> {
        match word {
            "working" => Some(SidebandWord::Working),
            "blocked" => Some(SidebandWord::Blocked),
            "stalled" => Some(SidebandWord::Stalled),
            "waiting_human" => Some(SidebandWord::WaitingHuman),
            "finished" => Some(SidebandWord::Finished),
            _ => None,
        }
    }
}

/// The regex detector's verdict over rendered output
/// (`sessionStateDetector.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegexState {
    Working,
    ApprovalShaped,
    QuestionShaped,
    Completed,
    Error,
    Idle,
}

/// What an observation says, by source. The source is implied by the kind, so
/// an observation cannot claim a source it did not come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObservationKind {
    Hook(HookEvent),
    Sideband(SidebandWord),
    /// The statusline command ran (an assistant message happened).
    StatuslineTick,
    /// A new record appeared in the session transcript.
    TranscriptActivity,
    /// The rendered grid is changing (`busy`) or has settled.
    ScreenStability {
        busy: bool,
    },
    Regex(RegexState),
}

impl ObservationKind {
    pub const fn source(self) -> Source {
        match self {
            ObservationKind::Hook(_) => Source::Hook,
            ObservationKind::Sideband(_) => Source::Sideband,
            ObservationKind::StatuslineTick => Source::Statusline,
            ObservationKind::TranscriptActivity => Source::Transcript,
            ObservationKind::ScreenStability { .. } => Source::ScreenStability,
            ObservationKind::Regex(_) => Source::Regex,
        }
    }

    /// The state this observation asserts, or `None` when it asserts nothing
    /// (it is then dropped, and does not count as "the source reported").
    pub const fn claimed_state(self) -> Option<AgentState> {
        use AgentState as S;
        use NeedsYouReason as R;
        match self {
            ObservationKind::Hook(ev) => match ev {
                HookEvent::UserPromptSubmit => Some(S::Working),
                HookEvent::PermissionRequest {
                    tool: PermissionTool::AskUserQuestion,
                } => Some(S::NeedsYou {
                    reason: R::Question,
                }),
                HookEvent::PermissionRequest {
                    tool: PermissionTool::Other,
                } => Some(S::NeedsYou {
                    reason: R::Permission,
                }),
                HookEvent::Notification { notification_type } => match notification_type {
                    NotificationType::PermissionPrompt => Some(S::NeedsYou {
                        reason: R::Permission,
                    }),
                    NotificationType::IdlePrompt => Some(S::NeedsYou {
                        reason: R::IdlePrompt,
                    }),
                    NotificationType::ElicitationDialog => Some(S::NeedsYou {
                        reason: R::Elicitation,
                    }),
                    NotificationType::AgentNeedsInput => Some(S::NeedsYou {
                        reason: R::Question,
                    }),
                    // A background agent finishing says nothing about the
                    // top-level turn.
                    NotificationType::AgentCompleted | NotificationType::Other => None,
                },
                HookEvent::Stop => Some(S::TurnEnded),
                HookEvent::StopFailure { kind } => Some(S::Failed { kind }),
                HookEvent::SessionStart { source } => match source {
                    SessionStartSource::Compact => None,
                    SessionStartSource::Startup
                    | SessionStartSource::Resume
                    | SessionStartSource::Clear
                    | SessionStartSource::Other => Some(S::Starting),
                },
                HookEvent::SessionEnd { reason } => Some(S::Ended { why: reason }),
            },
            ObservationKind::Sideband(word) => Some(match word {
                SidebandWord::Working => S::Working,
                SidebandWord::WaitingHuman => S::NeedsYou {
                    reason: R::Unspecified,
                },
                SidebandWord::Blocked | SidebandWord::Stalled => S::Failed {
                    kind: FailureKind::Unknown,
                },
                SidebandWord::Finished => S::TurnEnded,
            }),
            ObservationKind::StatuslineTick | ObservationKind::TranscriptActivity => {
                Some(S::Working)
            }
            ObservationKind::ScreenStability { busy: true } => Some(S::Working),
            ObservationKind::ScreenStability { busy: false } => Some(S::TurnEnded),
            ObservationKind::Regex(r) => Some(match r {
                RegexState::Working => S::Working,
                RegexState::ApprovalShaped => S::NeedsYou {
                    reason: R::Permission,
                },
                RegexState::QuestionShaped => S::NeedsYou {
                    reason: R::Question,
                },
                RegexState::Completed | RegexState::Idle => S::TurnEnded,
                RegexState::Error => S::Failed {
                    kind: FailureKind::Unknown,
                },
            }),
        }
    }
}

/// One observation offered to [`AgentTruth::observe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Observation {
    pub kind: ObservationKind,
    /// When it happened (unix millis). Clamped to `now_ms` on intake.
    pub at_ms: u64,
    /// The event came from a subagent (carried an `agent_id`). Ignored in v1:
    /// the reducer keys on the top-level session only.
    pub is_subagent: bool,
}

impl Observation {
    pub const fn new(kind: ObservationKind, at_ms: u64) -> Self {
        Self {
            kind,
            at_ms,
            is_subagent: false,
        }
    }

    pub const fn hook(event: HookEvent, at_ms: u64) -> Self {
        Self::new(ObservationKind::Hook(event), at_ms)
    }

    pub const fn source(&self) -> Source {
        self.kind.source()
    }
}

/// What [`AgentTruth::observe`] did with an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObserveOutcome {
    Accepted,
    /// A subagent event (rule 7).
    DroppedSubagent,
    /// The observation asserts no state (e.g. `SessionStart: compact`,
    /// `Notification: agent_completed`).
    DroppedNoState,
    /// Older than the source's last accepted observation (a late arrival).
    DroppedOutOfOrder,
}

// ---------------------------------------------------------------------------
// Context projections
// ---------------------------------------------------------------------------

/// Projection of the runner's `PtyInputSlots`: when a human last SUBMITTED
/// input (Enter), control responses excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct InputEvidence {
    pub last_submit_ms: Option<u64>,
}

/// Projection of the grid-idle tracker (`wind_down::GridIdleTracker`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GridIdle {
    /// Idle continuously since `since_ms`.
    Idle {
        since_ms: u64,
    },
    Busy,
    /// Could not be observed.
    Unknown,
}

// ---------------------------------------------------------------------------
// Verdict
// ---------------------------------------------------------------------------

/// A lower source disagreeing with the verdict — recorded, never obeyed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Disagreement {
    /// A fresh lower source reports a different state class.
    Contradiction {
        higher: Source,
        lower: Source,
        lower_state: AgentState,
    },
    /// Authoritative `Working` while the grid has been idle past
    /// [`QUIET_WHILE_WORKING_AFTER_MS`] with no live children (rule 6).
    QuietWhileWorking { grid_idle_since_ms: u64 },
}

/// The merged answer for one terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    pub state: AgentState,
    /// `None` only when `state` is `Unknown`.
    pub source: Option<Source>,
    /// When the state began (unix millis); `None` only when `Unknown`.
    pub since_ms: Option<u64>,
    /// `None` only when `Unknown`.
    pub confidence: Option<Confidence>,
    pub disagreement: Option<Disagreement>,
}

impl Verdict {
    pub const UNKNOWN: Verdict = Verdict {
        state: AgentState::Unknown,
        source: None,
        since_ms: None,
        confidence: None,
        disagreement: None,
    };

    pub fn is_unknown(&self) -> bool {
        self.state == AgentState::Unknown
    }

    /// THE selector every keystroke writer (`/approve-all`, auto-approve,
    /// Ctrl+Shift+Enter, …) must use: true only for a permission ask the agent
    /// reported about itself. An inferred or screen-read ask is never enough to
    /// type into a pane.
    pub fn is_authoritative_permission_ask(&self) -> bool {
        matches!(
            self.state,
            AgentState::NeedsYou {
                reason: NeedsYouReason::Permission
            }
        ) && self.confidence == Some(Confidence::Authoritative)
    }
}

// ---------------------------------------------------------------------------
// The reducer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Level {
    state: AgentState,
    /// When this level began (the edge).
    since_ms: u64,
    /// The source's most recent observation re-asserting it (freshness).
    last_seen_ms: u64,
}

/// Per-terminal agent truth. See the module docs for the rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTruth {
    caps: StateCapabilities,
    levels: [Option<Level>; 6],
    input: InputEvidence,
    grid: Option<(GridIdle, u64)>,
    children: Option<(Option<bool>, u64)>,
}

impl AgentTruth {
    pub fn new(caps: StateCapabilities) -> Self {
        Self {
            caps,
            levels: [None; 6],
            input: InputEvidence::default(),
            grid: None,
            children: None,
        }
    }

    pub fn capabilities(&self) -> StateCapabilities {
        self.caps
    }

    /// When `source` last reported (unix millis), if ever — for per-source
    /// last-seen ages on a diagnostics surface.
    pub fn last_seen_ms(&self, source: Source) -> Option<u64> {
        self.levels[source.index()].map(|l| l.last_seen_ms)
    }

    /// Offer one observation.
    pub fn observe(&mut self, obs: &Observation, now_ms: u64) -> ObserveOutcome {
        if obs.is_subagent {
            return ObserveOutcome::DroppedSubagent;
        }
        let Some(state) = obs.kind.claimed_state() else {
            return ObserveOutcome::DroppedNoState;
        };
        let source = obs.source();
        let at = obs.at_ms.min(now_ms);
        let submit = self.input.last_submit_ms;
        let slot = &mut self.levels[source.index()];
        let next = match *slot {
            Some(prev) if at < prev.last_seen_ms => return ObserveOutcome::DroppedOutOfOrder,
            Some(prev) if prev.state == state && continues(source, prev, submit, at) => Level {
                last_seen_ms: at,
                ..prev
            },
            _ => Level {
                state,
                since_ms: at,
                last_seen_ms: at,
            },
        };
        *slot = Some(next);
        ObserveOutcome::Accepted
    }

    /// Offer the latest PTY input evidence. The submit time never moves
    /// backwards and is clamped to `now_ms`.
    pub fn observe_input(&mut self, input: InputEvidence, now_ms: u64) {
        let offered = input.last_submit_ms.map(|t| t.min(now_ms));
        self.input.last_submit_ms = match (self.input.last_submit_ms, offered) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }

    /// Offer the latest grid-idle observation (used by rule 6 only; it never
    /// sets a state — offer [`ObservationKind::ScreenStability`] for that).
    pub fn observe_grid(&mut self, grid: GridIdle, now_ms: u64) {
        self.grid = Some((grid, now_ms));
    }

    /// Offer the latest live-children hint (`None` = could not be computed).
    pub fn observe_children(&mut self, has_live_children: Option<bool>, now_ms: u64) {
        self.children = Some((has_live_children, now_ms));
    }

    /// The merged verdict at `now_ms`. Pure: the same state and `now_ms`
    /// always yield the same verdict.
    pub fn verdict(&self, now_ms: u64) -> Verdict {
        // Rule 3: pick the winning level.
        let mut best: Option<(Source, Level)> = None;
        for source in Source::ALL {
            let Some(level) = self.live_level(source, now_ms) else {
                continue;
            };
            best = match best {
                None => Some((source, level)),
                Some((higher, hl)) => {
                    let newer = level.since_ms > hl.last_seen_ms;
                    let higher_blocks = fresh(higher, hl, now_ms)
                        && level
                            .state
                            .class()
                            .is_some_and(|c| self.caps.can_express(higher, c));
                    if newer && !higher_blocks {
                        Some((source, level))
                    } else {
                        Some((higher, hl))
                    }
                }
            };
        }
        let Some((source, level)) = best else {
            return Verdict::UNKNOWN;
        };

        // Rule 5: the human answered a NeedsYou.
        let (state, since_ms, confidence) = match (level.state, self.input.last_submit_ms) {
            (AgentState::NeedsYou { .. }, Some(submit)) if submit > level.since_ms => {
                (AgentState::Working, submit, Confidence::Inferred)
            }
            (s, _) => (s, level.since_ms, source.confidence()),
        };

        // Rule 4: the first fresh lower source whose class differs.
        let contradiction = Source::ALL
            .into_iter()
            .filter(|s| *s < source)
            .find_map(|lower| {
                let l = self.live_level(lower, now_ms)?;
                (fresh(lower, l, now_ms) && l.state.class() != state.class()).then_some(
                    Disagreement::Contradiction {
                        higher: source,
                        lower,
                        lower_state: l.state,
                    },
                )
            });

        // Rule 6: quiet is not idle.
        let quiet = (state == AgentState::Working && confidence == Confidence::Authoritative)
            .then(|| self.quiet_since(since_ms, now_ms))
            .flatten()
            .map(|grid_idle_since_ms| Disagreement::QuietWhileWorking { grid_idle_since_ms });

        Verdict {
            state,
            source: Some(source),
            since_ms: Some(since_ms),
            confidence: Some(confidence),
            disagreement: contradiction.or(quiet),
        }
    }

    /// The level `source` currently holds: any level for an edge source, a
    /// fresh one only for a pulse source (rule 2).
    fn live_level(&self, source: Source, now_ms: u64) -> Option<Level> {
        let level = self.levels[source.index()]?;
        (!source.is_pulse() || fresh(source, level, now_ms)).then_some(level)
    }

    /// Rule 6's grid-idle start, when the grid has been idle (and the children
    /// absent) long enough under a `Working` that began at `working_since`.
    fn quiet_since(&self, working_since: u64, now_ms: u64) -> Option<u64> {
        let (grid, grid_at) = self.grid?;
        let (children, children_at) = self.children?;
        let GridIdle::Idle { since_ms } = grid else {
            return None;
        };
        let context_fresh = now_ms.saturating_sub(grid_at) <= CONTEXT_OBSERVATION_TTL_MS
            && now_ms.saturating_sub(children_at) <= CONTEXT_OBSERVATION_TTL_MS;
        let quiet_from = since_ms.max(working_since);
        (context_fresh
            && children == Some(false)
            && now_ms.saturating_sub(quiet_from) > QUIET_WHILE_WORKING_AFTER_MS)
            .then_some(since_ms)
    }
}

/// Is `level` still fresh for `source` at `now_ms`?
fn fresh(source: Source, level: Level, now_ms: u64) -> bool {
    now_ms.saturating_sub(level.last_seen_ms) <= source.freshness_ttl_ms()
}

/// Does a same-state observation at `at` CONTINUE `prev` (keep its `since`),
/// rather than start a new level?
///
/// - A pulse continues only while the previous pulse is still fresh; after a
///   gap the run restarts.
/// - A `NeedsYou` edge after a human submit is a NEW ask (rule 5), so it starts
///   a new level rather than being masked by the old submit.
fn continues(source: Source, prev: Level, submit: Option<u64>, at: u64) -> bool {
    if source.is_pulse() {
        return at.saturating_sub(prev.last_seen_ms) <= source.freshness_ttl_ms();
    }
    let answered = matches!(prev.state, AgentState::NeedsYou { .. })
        && submit.is_some_and(|t| t > prev.since_ms);
    !answered
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_000_000;

    fn obs(kind: ObservationKind, at: u64) -> Observation {
        Observation::new(kind, at)
    }

    /// An observation from `source` claiming `Working`.
    fn working(source: Source) -> ObservationKind {
        match source {
            Source::Hook => ObservationKind::Hook(HookEvent::UserPromptSubmit),
            Source::Sideband => ObservationKind::Sideband(SidebandWord::Working),
            Source::Statusline => ObservationKind::StatuslineTick,
            Source::Transcript => ObservationKind::TranscriptActivity,
            Source::ScreenStability => ObservationKind::ScreenStability { busy: true },
            Source::Regex => ObservationKind::Regex(RegexState::Working),
        }
    }

    /// An observation from a non-pulse `source` claiming `TurnEnded`.
    fn turn_ended(source: Source) -> ObservationKind {
        match source {
            Source::Hook => ObservationKind::Hook(HookEvent::Stop),
            Source::Sideband => ObservationKind::Sideband(SidebandWord::Finished),
            Source::ScreenStability => ObservationKind::ScreenStability { busy: false },
            Source::Regex => ObservationKind::Regex(RegexState::Completed),
            Source::Statusline | Source::Transcript => {
                unreachable!("pulse sources only claim Working")
            }
        }
    }

    /// For a (higher, lower) pair: the two contradicting claims, or `None`
    /// when both are pulses (which can only claim `Working`).
    fn contradicting_claims(
        higher: Source,
        lower: Source,
    ) -> Option<(ObservationKind, AgentState, ObservationKind, AgentState)> {
        if !lower.is_pulse() {
            Some((
                working(higher),
                AgentState::Working,
                turn_ended(lower),
                AgentState::TurnEnded,
            ))
        } else if !higher.is_pulse() {
            Some((
                turn_ended(higher),
                AgentState::TurnEnded,
                working(lower),
                AgentState::Working,
            ))
        } else {
            None
        }
    }

    fn pairs() -> Vec<(Source, Source)> {
        let mut v = Vec::new();
        for h in Source::ALL {
            for l in Source::ALL {
                if h > l {
                    v.push((h, l));
                }
            }
        }
        v
    }

    // ---- ordering ---------------------------------------------------------

    #[test]
    fn agent_truth_source_order_is_total_and_hook_highest() {
        assert_eq!(pairs().len(), 15);
        for w in Source::ALL.windows(2) {
            assert!(w[0] > w[1], "{:?} must outrank {:?}", w[0], w[1]);
            assert!(
                w[0].freshness_ttl_ms() >= w[1].freshness_ttl_ms(),
                "TTLs are non-increasing by rank"
            );
        }
        assert_eq!(Source::ALL.iter().max(), Some(&Source::Hook));
        for (i, s) in Source::ALL.iter().enumerate() {
            assert_eq!(s.index(), i);
        }
    }

    // ---- rule 3/4: precedence table ---------------------------------------

    #[test]
    fn agent_truth_fresh_higher_wins_and_lower_records_disagreement() {
        for (h, l) in pairs() {
            let Some((hk, hs, lk, ls)) = contradicting_claims(h, l) else {
                continue;
            };
            let mut t = AgentTruth::new(StateCapabilities::all());
            assert_eq!(t.observe(&obs(hk, T0), T0), ObserveOutcome::Accepted);
            assert_eq!(
                t.observe(&obs(lk, T0 + 10), T0 + 10),
                ObserveOutcome::Accepted
            );
            let v = t.verdict(T0 + 20);
            assert_eq!(v.state, hs, "{h:?} over {l:?}");
            assert_eq!(v.source, Some(h), "{h:?} over {l:?}");
            assert_eq!(v.confidence, Some(h.confidence()));
            assert_eq!(
                v.disagreement,
                Some(Disagreement::Contradiction {
                    higher: h,
                    lower: l,
                    lower_state: ls
                }),
                "{h:?} over {l:?}"
            );
        }
    }

    #[test]
    fn agent_truth_stale_higher_lets_newer_lower_fill() {
        for (h, l) in pairs() {
            let Some((hk, _, lk, ls)) = contradicting_claims(h, l) else {
                continue;
            };
            let mut t = AgentTruth::new(StateCapabilities::all());
            t.observe(&obs(hk, T0), T0);
            let l_at = T0 + h.freshness_ttl_ms();
            t.observe(&obs(lk, l_at), l_at);
            let v = t.verdict(l_at + 1);
            assert_eq!(v.state, ls, "stale {h:?} lets {l:?} fill");
            assert_eq!(v.source, Some(l));
            assert_eq!(v.confidence, Some(l.confidence()));
            assert_eq!(v.since_ms, Some(l_at));
        }
    }

    #[test]
    fn agent_truth_lower_alone_sets_state_when_no_higher_ever_reported() {
        for s in Source::ALL {
            let mut t = AgentTruth::new(StateCapabilities::claude());
            t.observe(&obs(working(s), T0), T0);
            let v = t.verdict(T0 + 1);
            assert_eq!(v.state, AgentState::Working);
            assert_eq!(v.source, Some(s));
            assert_eq!(v.disagreement, None);
        }
    }

    #[test]
    fn agent_truth_older_lower_edge_never_overrides_even_when_higher_is_stale() {
        let mut t = AgentTruth::new(StateCapabilities::all());
        t.observe(&obs(turn_ended(Source::Regex), T0), T0);
        t.observe(&obs(working(Source::Hook), T0 + 5), T0 + 5);
        let v = t.verdict(T0 + 5 + HOOK_TTL_MS + 1);
        assert_eq!(v.state, AgentState::Working);
        assert_eq!(v.source, Some(Source::Hook));
        // Both stale: no disagreement is recorded from stale history.
        assert_eq!(v.disagreement, None);
    }

    #[test]
    fn agent_truth_stale_pulse_contributes_nothing() {
        let mut t = AgentTruth::new(StateCapabilities::claude());
        t.observe(&obs(ObservationKind::StatuslineTick, T0), T0);
        assert_eq!(t.verdict(T0 + STATUSLINE_TTL_MS).state, AgentState::Working);
        assert!(t.verdict(T0 + STATUSLINE_TTL_MS + 1).is_unknown());
    }

    // ---- StopFailure -------------------------------------------------------

    #[test]
    fn agent_truth_stop_failure_without_stop_ends_working() {
        let mut t = AgentTruth::new(StateCapabilities::claude());
        t.observe(&Observation::hook(HookEvent::UserPromptSubmit, T0), T0);
        assert_eq!(t.verdict(T0 + 1).state, AgentState::Working);
        let failure = HookEvent::StopFailure {
            kind: FailureKind::from_error_type("rate_limit"),
        };
        t.observe(&Observation::hook(failure, T0 + 100), T0 + 100);
        let v = t.verdict(T0 + 200);
        assert_eq!(
            v.state,
            AgentState::Failed {
                kind: FailureKind::RateLimited
            }
        );
        assert_eq!(v.confidence, Some(Confidence::Authoritative));
        assert_eq!(v.since_ms, Some(T0 + 100));
        // And it does not decay back to Working with time.
        assert_ne!(t.verdict(T0 + HOOK_TTL_MS * 10).state, AgentState::Working);
    }

    // ---- rule 5: the human answered ---------------------------------------

    #[test]
    fn agent_truth_permission_then_submit_is_inferred_working() {
        let mut t = AgentTruth::new(StateCapabilities::claude());
        let ask = HookEvent::PermissionRequest {
            tool: PermissionTool::from_tool_name(Some("Bash")),
        };
        t.observe(&Observation::hook(ask, T0), T0);
        let v = t.verdict(T0 + 1);
        assert!(v.is_authoritative_permission_ask());

        // A submit BEFORE the ask does not answer it.
        t.observe_input(
            InputEvidence {
                last_submit_ms: Some(T0 - 1),
            },
            T0 + 2,
        );
        assert!(t.verdict(T0 + 3).is_authoritative_permission_ask());

        t.observe_input(
            InputEvidence {
                last_submit_ms: Some(T0 + 50),
            },
            T0 + 60,
        );
        let v = t.verdict(T0 + 70);
        assert_eq!(v.state, AgentState::Working);
        assert_eq!(v.confidence, Some(Confidence::Inferred));
        assert_eq!(v.since_ms, Some(T0 + 50));
        assert_eq!(v.source, Some(Source::Hook));
        assert!(!v.is_authoritative_permission_ask());

        // A duplicate of the SAME ask before any submit keeps the level; a new
        // ask after the submit is a new edge and authoritative again.
        t.observe(&Observation::hook(ask, T0 + 100), T0 + 100);
        let v = t.verdict(T0 + 110);
        assert!(v.is_authoritative_permission_ask());
        assert_eq!(v.since_ms, Some(T0 + 100));
    }

    #[test]
    fn agent_truth_input_never_moves_backwards_or_into_the_future() {
        let mut t = AgentTruth::new(StateCapabilities::claude());
        t.observe_input(
            InputEvidence {
                last_submit_ms: Some(500),
            },
            1_000,
        );
        t.observe_input(
            InputEvidence {
                last_submit_ms: Some(100),
            },
            1_000,
        );
        t.observe_input(
            InputEvidence {
                last_submit_ms: None,
            },
            1_000,
        );
        assert_eq!(t.input.last_submit_ms, Some(500));
        t.observe_input(
            InputEvidence {
                last_submit_ms: Some(9_999),
            },
            2_000,
        );
        assert_eq!(t.input.last_submit_ms, Some(2_000));
    }

    #[test]
    fn agent_truth_ask_user_question_is_never_a_permission_ask() {
        let mut t = AgentTruth::new(StateCapabilities::claude());
        let ask = HookEvent::PermissionRequest {
            tool: PermissionTool::from_tool_name(Some("AskUserQuestion")),
        };
        t.observe(&Observation::hook(ask, T0), T0);
        let v = t.verdict(T0 + 1);
        assert_eq!(
            v.state,
            AgentState::NeedsYou {
                reason: NeedsYouReason::Question
            }
        );
        assert!(!v.is_authoritative_permission_ask());
    }

    // ---- rule 6: quiet is not idle ----------------------------------------

    #[test]
    fn agent_truth_quiet_while_working_is_recorded_never_overrides() {
        let now = T0 + QUIET_WHILE_WORKING_AFTER_MS + 1_001;
        let base = || {
            let mut t = AgentTruth::new(StateCapabilities::claude());
            t.observe(&Observation::hook(HookEvent::UserPromptSubmit, T0), T0);
            t.observe_grid(
                GridIdle::Idle {
                    since_ms: T0 + 1_000,
                },
                now,
            );
            t
        };

        let mut t = base();
        t.observe_children(Some(false), now);
        let v = t.verdict(now);
        assert_eq!(v.state, AgentState::Working);
        assert_eq!(v.source, Some(Source::Hook));
        assert_eq!(
            v.disagreement,
            Some(Disagreement::QuietWhileWorking {
                grid_idle_since_ms: T0 + 1_000
            })
        );
        // One ms earlier the window has not elapsed.
        assert_eq!(t.verdict(now - 1).disagreement, None);

        for children in [Some(true), None] {
            let mut t = base();
            t.observe_children(children, now);
            assert_eq!(t.verdict(now).disagreement, None, "children {children:?}");
        }
        let mut t = base();
        t.observe_children(Some(false), now);
        t.observe_grid(GridIdle::Busy, now);
        assert_eq!(t.verdict(now).disagreement, None);

        // A stale grid observation is not evidence of quiet.
        let mut t = base();
        t.observe_children(Some(false), now);
        let later = now + CONTEXT_OBSERVATION_TTL_MS + 1;
        assert_eq!(t.verdict(later).disagreement, None);
    }

    // ---- capability fallthrough ------------------------------------------

    #[test]
    fn agent_truth_capability_matrix_lets_lower_fill_what_higher_cannot_express() {
        let no_hook_ask = StateCapabilities::claude()
            .without_source(Source::Hook)
            .with(
                Source::Hook,
                &[
                    StateClass::Starting,
                    StateClass::Working,
                    StateClass::TurnEnded,
                    StateClass::Failed,
                    StateClass::Ended,
                ],
            );
        assert!(!no_hook_ask.can_express(Source::Hook, StateClass::NeedsYou));
        let approval = ObservationKind::Regex(RegexState::ApprovalShaped);

        let mut t = AgentTruth::new(no_hook_ask);
        t.observe(&Observation::hook(HookEvent::UserPromptSubmit, T0), T0);
        t.observe(&obs(approval, T0 + 10), T0 + 10);
        let v = t.verdict(T0 + 20);
        assert_eq!(
            v.state,
            AgentState::NeedsYou {
                reason: NeedsYouReason::Permission
            }
        );
        assert_eq!(v.source, Some(Source::Regex));
        assert_eq!(v.confidence, Some(Confidence::Fallback));
        assert!(!v.is_authoritative_permission_ask());

        // Claude's hooks CAN express NeedsYou: the same screen only disagrees
        // (the 2026-06-07 phantom latch).
        let mut t = AgentTruth::new(StateCapabilities::claude());
        t.observe(&Observation::hook(HookEvent::UserPromptSubmit, T0), T0);
        t.observe(&obs(approval, T0 + 10), T0 + 10);
        let v = t.verdict(T0 + 20);
        assert_eq!(v.state, AgentState::Working);
        assert_eq!(v.source, Some(Source::Hook));
        assert!(matches!(
            v.disagreement,
            Some(Disagreement::Contradiction {
                higher: Source::Hook,
                lower: Source::Regex,
                ..
            })
        ));
    }

    #[test]
    fn agent_truth_claude_and_fallback_matrices() {
        let c = StateCapabilities::claude();
        for class in StateClass::ALL {
            assert!(c.can_express(Source::Hook, class), "{class:?}");
        }
        assert_eq!(c.sources_for(StateClass::Starting), vec![Source::Hook]);
        let f = StateCapabilities::fallback_only();
        for class in StateClass::ALL {
            assert!(!f.can_express(Source::Hook, class));
            assert!(!f.can_express(Source::Statusline, class));
        }
        assert!(f.can_express(Source::Regex, StateClass::NeedsYou));
        // Every non-Unknown state a source can claim is declared for Claude.
        let claims: [(ObservationKind, Source); 4] = [
            (
                ObservationKind::Sideband(SidebandWord::WaitingHuman),
                Source::Sideband,
            ),
            (
                ObservationKind::Sideband(SidebandWord::Blocked),
                Source::Sideband,
            ),
            (
                ObservationKind::ScreenStability { busy: false },
                Source::ScreenStability,
            ),
            (ObservationKind::Regex(RegexState::Error), Source::Regex),
        ];
        for (k, s) in claims {
            let class = k.claimed_state().and_then(AgentState::class).unwrap();
            assert!(c.can_express(s, class), "{k:?}");
        }
    }

    // ---- mappings ----------------------------------------------------------

    #[test]
    fn agent_truth_error_type_mapping_is_total() {
        use FailureKind as F;
        let expected = [
            F::RateLimited,
            F::Overloaded,
            F::AuthRequired,
            F::AuthRequired,
            F::AuthRequired,
            F::QuotaExhausted,
            F::ProviderError,
            F::ProviderError,
            F::ProviderError,
            F::ProviderError,
            F::AuthRequired,
            F::Unknown,
        ];
        for (wire, kind) in DOCUMENTED_STOP_FAILURE_ERROR_TYPES.iter().zip(expected) {
            assert_eq!(FailureKind::from_error_type(wire), kind, "{wire}");
        }
        assert_ne!(
            FailureKind::from_error_type("rate_limit"),
            F::QuotaExhausted
        );
        for junk in [
            "",
            "RATE_LIMIT",
            " rate_limit",
            "rate-limit",
            "quota",
            "\u{0}",
        ] {
            assert_eq!(FailureKind::from_error_type(junk), F::Unknown, "{junk:?}");
        }
    }

    #[test]
    fn agent_truth_hook_claims() {
        let claim = |e: HookEvent| ObservationKind::Hook(e).claimed_state();
        assert_eq!(
            claim(HookEvent::SessionStart {
                source: SessionStartSource::from_wire("compact")
            }),
            None,
            "compaction happens mid-turn"
        );
        assert_eq!(
            claim(HookEvent::SessionStart {
                source: SessionStartSource::from_wire("resume")
            }),
            Some(AgentState::Starting)
        );
        assert_eq!(
            claim(HookEvent::Notification {
                notification_type: NotificationType::from_wire("agent_completed")
            }),
            None
        );
        assert_eq!(
            claim(HookEvent::Notification {
                notification_type: NotificationType::from_wire("idle_prompt")
            }),
            Some(AgentState::NeedsYou {
                reason: NeedsYouReason::IdlePrompt
            })
        );
        assert_eq!(
            claim(HookEvent::SessionEnd {
                reason: EndReason::from_wire("weird")
            }),
            Some(AgentState::Ended {
                why: EndReason::Other
            })
        );
        assert_eq!(SidebandWord::from_wire("idle"), None);
    }

    #[test]
    fn agent_truth_drops_subagent_no_state_and_out_of_order() {
        let mut t = AgentTruth::new(StateCapabilities::claude());
        let mut sub = Observation::hook(HookEvent::Stop, T0);
        sub.is_subagent = true;
        assert_eq!(t.observe(&sub, T0), ObserveOutcome::DroppedSubagent);
        let compact = Observation::hook(
            HookEvent::SessionStart {
                source: SessionStartSource::Compact,
            },
            T0,
        );
        assert_eq!(t.observe(&compact, T0), ObserveOutcome::DroppedNoState);
        assert!(t.verdict(T0).is_unknown());
        assert_eq!(t.last_seen_ms(Source::Hook), None);

        t.observe(&Observation::hook(HookEvent::Stop, T0 + 10), T0 + 10);
        assert_eq!(
            t.observe(
                &Observation::hook(HookEvent::UserPromptSubmit, T0 + 5),
                T0 + 10
            ),
            ObserveOutcome::DroppedOutOfOrder
        );
        assert_eq!(t.verdict(T0 + 11).state, AgentState::TurnEnded);
    }

    #[test]
    fn agent_truth_future_observation_is_clamped_to_now() {
        let mut t = AgentTruth::new(StateCapabilities::claude());
        t.observe(&Observation::hook(HookEvent::Stop, T0 + 1_000_000), T0);
        assert_eq!(t.verdict(T0).since_ms, Some(T0));
    }

    #[test]
    fn agent_truth_verdict_serializes_for_the_wire() {
        let mut t = AgentTruth::new(StateCapabilities::claude());
        t.observe(&Observation::hook(HookEvent::UserPromptSubmit, 1), 1);
        t.observe(
            &obs(ObservationKind::Regex(RegexState::ApprovalShaped), 2),
            2,
        );
        let json = serde_json::to_value(t.verdict(3)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "state": {"name": "working"},
                "source": "hook",
                "sinceMs": 1,
                "confidence": "authoritative",
                "disagreement": {
                    "kind": "contradiction",
                    "higher": "hook",
                    "lower": "regex",
                    "lowerState": {"name": "needs_you", "reason": "permission"}
                }
            })
        );
        let failed = AgentState::Failed {
            kind: FailureKind::RateLimited,
        };
        assert_eq!(
            serde_json::to_value(failed).unwrap(),
            serde_json::json!({"name": "failed", "kind": "rate_limited"})
        );
        assert_eq!(
            serde_json::to_value(Verdict::UNKNOWN).unwrap(),
            serde_json::json!({
                "state": {"name": "unknown"},
                "source": null,
                "sinceMs": null,
                "confidence": null,
                "disagreement": null
            })
        );
    }

    // ---- property-style ---------------------------------------------------

    /// Deterministic xorshift PRNG (no proptest dev-dependency in this crate).
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum Op {
        Obs(Observation),
        Input(Option<u64>),
        Grid(GridIdle),
        Children(Option<bool>),
    }

    fn random_kind(r: &mut Rng) -> ObservationKind {
        let hook = [
            HookEvent::UserPromptSubmit,
            HookEvent::PermissionRequest {
                tool: PermissionTool::Other,
            },
            HookEvent::PermissionRequest {
                tool: PermissionTool::AskUserQuestion,
            },
            HookEvent::Notification {
                notification_type: NotificationType::PermissionPrompt,
            },
            HookEvent::Notification {
                notification_type: NotificationType::AgentCompleted,
            },
            HookEvent::Stop,
            HookEvent::StopFailure {
                kind: FailureKind::RateLimited,
            },
            HookEvent::SessionStart {
                source: SessionStartSource::Startup,
            },
            HookEvent::SessionStart {
                source: SessionStartSource::Compact,
            },
            HookEvent::SessionEnd {
                reason: EndReason::Other,
            },
        ];
        match r.below(6) {
            0 => ObservationKind::Hook(hook[r.below(hook.len() as u64) as usize]),
            1 => ObservationKind::Sideband(
                [
                    SidebandWord::Working,
                    SidebandWord::Blocked,
                    SidebandWord::WaitingHuman,
                    SidebandWord::Finished,
                ][r.below(4) as usize],
            ),
            2 => ObservationKind::StatuslineTick,
            3 => ObservationKind::TranscriptActivity,
            4 => ObservationKind::ScreenStability {
                busy: r.below(2) == 0,
            },
            _ => ObservationKind::Regex(
                [
                    RegexState::Working,
                    RegexState::ApprovalShaped,
                    RegexState::QuestionShaped,
                    RegexState::Completed,
                    RegexState::Error,
                    RegexState::Idle,
                ][r.below(6) as usize],
            ),
        }
    }

    fn random_context_op(r: &mut Rng, now: u64) -> Op {
        match r.below(3) {
            0 => Op::Input((r.below(2) == 0).then(|| r.below(now + 1_000))),
            1 => Op::Grid(match r.below(3) {
                0 => GridIdle::Idle {
                    since_ms: r.below(now + 1),
                },
                1 => GridIdle::Busy,
                _ => GridIdle::Unknown,
            }),
            _ => Op::Children(match r.below(3) {
                0 => Some(true),
                1 => Some(false),
                _ => None,
            }),
        }
    }

    fn apply(t: &mut AgentTruth, op: Op, now: u64) {
        match op {
            Op::Obs(o) => {
                t.observe(&o, now);
            }
            Op::Input(s) => t.observe_input(InputEvidence { last_submit_ms: s }, now),
            Op::Grid(g) => t.observe_grid(g, now),
            Op::Children(c) => t.observe_children(c, now),
        }
    }

    fn random_caps(r: &mut Rng) -> StateCapabilities {
        match r.below(3) {
            0 => StateCapabilities::claude(),
            1 => StateCapabilities::fallback_only(),
            _ => StateCapabilities::all(),
        }
    }

    #[test]
    fn agent_truth_property_no_observation_means_unknown() {
        let mut r = Rng(0x9E37_79B9_7F4A_7C15);
        for _ in 0..500 {
            let mut t = AgentTruth::new(random_caps(&mut r));
            let mut now = T0;
            for _ in 0..r.below(40) {
                now += r.below(HOOK_TTL_MS);
                let op = match r.below(4) {
                    // Observations that assert nothing, or subagent ones.
                    0 => {
                        let mut o = Observation::new(random_kind(&mut r), now);
                        o.is_subagent = true;
                        Op::Obs(o)
                    }
                    1 => Op::Obs(Observation::hook(
                        HookEvent::SessionStart {
                            source: SessionStartSource::Compact,
                        },
                        now,
                    )),
                    _ => random_context_op(&mut r, now),
                };
                apply(&mut t, op, now);
                let v = t.verdict(now);
                assert_eq!(v, Verdict::UNKNOWN);
            }
        }
    }

    #[test]
    fn agent_truth_property_verdict_is_deterministic_and_well_formed() {
        let mut r = Rng(0xD1B5_4A32_D192_ED03);
        for _ in 0..500 {
            let caps = random_caps(&mut r);
            let mut ops = Vec::new();
            let mut now = T0;
            for _ in 0..r.below(60) {
                now += r.below(20 * 60 * 1000);
                let op = if r.below(2) == 0 {
                    // Some observations arrive late or from the future.
                    let at = now + r.below(2_000) - 1_000;
                    Op::Obs(Observation::new(random_kind(&mut r), at))
                } else {
                    random_context_op(&mut r, now)
                };
                ops.push((op, now));
            }
            let mut a = AgentTruth::new(caps);
            let mut b = AgentTruth::new(caps);
            let mut accepted_any = false;
            for &(op, at) in &ops {
                if let Op::Obs(o) = op {
                    accepted_any |= a.clone().observe(&o, at) == ObserveOutcome::Accepted;
                }
                apply(&mut a, op, at);
                apply(&mut b, op, at);
                let va = a.verdict(at);
                assert_eq!(va, a.verdict(at), "verdict is pure");
                assert_eq!(va, b.verdict(at), "same inputs, same verdict");
                if !accepted_any {
                    assert!(va.is_unknown(), "no accepted observation ⇒ Unknown");
                }
                // Well-formed: Unknown iff no source/since/confidence.
                assert_eq!(va.is_unknown(), va.source.is_none());
                assert_eq!(va.is_unknown(), va.since_ms.is_none());
                assert_eq!(va.is_unknown(), va.confidence.is_none());
                if let Some(since) = va.since_ms {
                    assert!(since <= at, "since is never in the future");
                }
                if va.is_authoritative_permission_ask() {
                    assert!(matches!(va.source, Some(Source::Hook | Source::Sideband)));
                }
            }
        }
    }
}
