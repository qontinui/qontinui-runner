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
//! wind-down executor (`session::wind_down_executor`) ACTS on it. Its drain
//! arm closes any `Eligible` session while coord holds this device drained;
//! its `finished_close` arm closes an `Eligible` TERMINAL session on any tick,
//! drained or not, once [`custody_gate`] also answers `Clean`. So read the
//! rules below as the authorisation they are, not as a report.
//!
//! ## The rules (D4, D6)
//!
//! Evaluated in this fixed order, first match wins:
//!
//! 1. **Any unknown input → [`Eligibility::Unknown`].** An unreadable coord
//!    work axis, an unreadable sideband slot, a grid that could not be
//!    observed, or a children hint that could not be computed. *"`finished` is
//!    a declaration, never an inference"* — so an unknown is never promoted to
//!    anything that could later authorise a close.
//! 2. **A `working` sideband state → [`Eligibility::Ineligible`].** Because
//!    the grace clock starts at the LATER of the grid-idle time and the
//!    sideband's set-time (rule 6), a `working` report also restarts the grace
//!    period once the session stops reporting `working`.
//! 3. **A grid that does not look idle → `Ineligible`.**
//! 4. **Live child processes → `Ineligible`.** `has_live_children` is a hint,
//!    so it is used as a necessary condition and never a sufficient one.
//! 5. **A terminal session that is not `finished` → `Ineligible`, forever.**
//!    An idle-but-unfinished session is never closed automatically; the
//!    operator's *finish-and-close* click is the declaration. Looping and
//!    steward sessions are EXEMPT from this rule (D6): they never finish by
//!    design, and their iteration boundary is the idle window itself. The
//!    work axis is not even consulted for them, so an unreadable coord does
//!    not make a loop `Unknown`.
//! 6. **Grace.** Eligible once `now >= since + grace`, where `since` is the
//!    latest of the grid-idle-since time, the sideband set-time and — for a
//!    terminal session whose coord row carried it — the time the `finished`
//!    declaration was made; before that, [`Eligibility::NotYet`] with the
//!    instant it would become eligible. When coord serves no transition time
//!    the declaration does not bound the window.
//!
//! ## The custody gate is a SECOND verdict, not a sixth input
//!
//! Since plan `2026-10-03-finished-runner-sessions-close-their-window-without-a-drain`
//! the executor has a second arm, `finished_close`, that closes finished
//! terminal sessions on every tick, drained or not. Outside a drain a session
//! may have declared itself finished while still holding uncommitted or
//! unpushed work, so that arm additionally requires [`custody_gate`] to answer
//! [`CustodyVerdict::Clean`] for every worktree the session touched (D2).
//!
//! It is deliberately NOT folded into [`eligibility`] (D2a): `GET
//! /restart-readiness` evaluates `eligibility` for every session on every Stop
//! turn under a 2 s client timeout, and the custody inputs need git probes.
//! The executor gathers them only for sessions `eligibility` already rated
//! `Eligible`, and the drain arm's verdict stays exactly what it was.
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
    /// Does not look idle right now.
    Busy,
    /// The grid could not be observed (no live terminal, poisoned lock).
    Unknown,
}

/// Everything [`eligibility`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EligibilityInputs {
    pub kind: SessionKind,
    pub work_status: WorkStatus,
    pub sideband: Sideband,
    pub grid: GridIdle,
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
    GridBusy,
    LiveChildren,
    NotFinished,
}

impl IneligibleReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SidebandWorking => "sideband_working",
            Self::GridBusy => "grid_busy",
            Self::LiveChildren => "live_children",
            Self::NotFinished => "not_finished",
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

/// The verdict. Only [`Eligibility::Eligible`] authorises a close: the
/// executor's drain arm acts on exactly that while drained, and its
/// `finished_close` arm acts on it for terminal sessions whose custody is
/// also [`CustodyVerdict::Clean`].
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
    if inputs.grid == GridIdle::Unknown {
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

    // (3) The grid.
    let GridIdle::Idle {
        since_ms: grid_since,
    } = inputs.grid
    else {
        return Eligibility::Ineligible {
            reason: IneligibleReason::GridBusy,
        };
    };

    // (4) Children.
    if has_live_children {
        return Eligibility::Ineligible {
            reason: IneligibleReason::LiveChildren,
        };
    }

    // (5) The finished declaration, for terminal sessions only.
    if finished_required && inputs.work_status != WorkStatus::Finished {
        return Eligibility::Ineligible {
            reason: IneligibleReason::NotFinished,
        };
    }

    // (6) Grace, from the latest of the clocks: grid idle, the sideband report,
    // and (terminal sessions only) the `finished` declaration.
    let mut since_ms = sideband_set_at.map_or(grid_since, |s| s.max(grid_since));
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
    pub kind: SessionKind,
    /// The `finished_close` arm's custody verdict for this session, from the
    /// arm's most recent pass — present ONLY when that arm ran on it. Absent
    /// is "the arm did not judge this session", never "clean".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custody: Option<CustodyView>,
    /// The `finished_close` arm's obligations verdict (the REMOTE half of
    /// `closeable`) for this session, from the arm's most recent pass. Present
    /// ONLY when that arm read coord for it — which it does only for a session
    /// whose custody was already clean. Absent is "not judged", never
    /// "watched".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub obligations: Option<ObligationsView>,
    /// The `finished_close` arm's close guard for this session, from the arm's
    /// most recent close batch (plan
    /// `2026-11-16-finished-close-mints-its-close-authority-from-the-served-close-conditions`,
    /// Phase 1). Present ONLY when the guard judged the session; absent is
    /// "not judged", never "would close".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_guard: Option<CloseGuardView>,
    /// coord's finish time for the session (unix ms) as the pass that built
    /// this view read it — the stamp the close guard's revival arm judges
    /// against. Not served: `sessionStatusSince` already carries it.
    #[serde(skip)]
    pub finished_at_ms: Option<i64>,
}

/// One input of the close guard, as `windDown.closeGuard.arms[]` serves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseGuardArmView {
    /// `eligibility` | `custody` | `obligations` | `first_verdict` | `revival`
    /// | `undelivered_messages`.
    pub arm: &'static str,
    /// `ok` | `held` | `unknown`.
    pub verdict: &'static str,
    /// The reason code, for `held` and `unknown`.
    pub reason: Option<&'static str>,
    /// What was found.
    pub detail: Option<String>,
}

/// The close guard's verdict for one session — `windDown.closeGuard`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseGuardView {
    /// `would_mint` | `held` | `unknown`.
    pub verdict: &'static str,
    /// The input that decided a `held` or `unknown` verdict.
    pub decided_by: Option<&'static str>,
    /// Its reason code.
    pub reason: Option<&'static str>,
    /// Every input, in the order the guard evaluates them.
    pub arms: Vec<CloseGuardArmView>,
    /// When the guard judged it (unix millis).
    pub judged_at: i64,
}

impl WindDownView {
    pub fn from_verdict(verdict: &Eligibility, kind: SessionKind) -> Self {
        let (since, until, reason) = match *verdict {
            Eligibility::Eligible { since_ms } => (Some(since_ms), None, None),
            Eligibility::NotYet { since_ms, until_ms } => (Some(since_ms), Some(until_ms), None),
            Eligibility::Ineligible { reason } => (None, None, Some(reason.as_str())),
            Eligibility::Unknown { reason } => (None, None, Some(reason.as_str())),
        };
        Self {
            eligibility: verdict.as_str(),
            since,
            until,
            reason,
            kind,
            custody: None,
            obligations: None,
            close_guard: None,
            finished_at_ms: None,
        }
    }

    pub fn is_eligible(&self) -> bool {
        self.eligibility == "eligible"
    }
}

// ---------------------------------------------------------------------------
// The custody gate (plan
// `2026-10-03-finished-runner-sessions-close-their-window-without-a-drain`,
// D2a and D2c)
// ---------------------------------------------------------------------------

/// The custody-slot `wip_state` values that mean the session's own record says
/// its work was NOT left clean: snapshotted (`captured`), not snapshotted yet
/// (`deferred`), a snapshot attempt that failed (`stash_create_failed`), or a
/// record the hook could not make consistent (`inconsistent`). A cross-check
/// only — the git probes are the source.
pub const DIRTY_WIP_STATES: [&str; 4] = [
    "captured",
    "deferred",
    "stash_create_failed",
    "inconsistent",
];

/// What probing ONE worktree found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberState {
    /// `.git` is a directory: a primary checkout, whose working tree mixes
    /// peers' state, so its cleanliness is not this session's to judge.
    PrimaryCheckout,
    /// A linked worktree whose probes all answered.
    Probed {
        /// `git status --porcelain` printed anything, untracked files included.
        dirty: bool,
        /// Commits reachable from `HEAD` that are not pushed. With an upstream:
        /// `rev-list --count @{upstream}..HEAD`. With none (including a
        /// detached `HEAD`): `rev-list --count HEAD --not --remotes` — commits
        /// on NO remote-tracking ref. So a freshly allocated worktree that made
        /// no commits, sitting on `origin/main`'s commit with no upstream, is
        /// `0`: nothing of its own exists to lose.
        ahead: u64,
    },
    /// A probe could not be completed — it failed, timed out, or the path is
    /// not a worktree at all.
    ProbeFailed(String),
}

/// One worktree the session touched, as probed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyMember {
    pub path: String,
    pub state: MemberState,
    /// `wip_state` of THIS session's own custody slot in that worktree
    /// (`qontinui-custody.d/<id>.json` with a matching `session_id` — never
    /// the mirror, which may name a peer). `None` when no such slot exists.
    pub slot_wip_state: Option<String>,
}

/// Everything [`custody_gate`] reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyInputs {
    /// The session's directory (its lifecycle record's `working_dir`, else
    /// the process cwd). `None` when neither is known.
    pub session_dir: Option<String>,
    /// Whether the worktrees attributed to the session could be read. `Err`
    /// carries why not.
    pub ownership: Result<(), String>,
    /// Every distinct worktree in the session's set: the repo holding its
    /// directory (absent when the directory is in no repo, e.g. the workspace
    /// root), its coord-attributed worktrees, and its isolated-edit context's
    /// materialized worktrees.
    pub members: Vec<CustodyMember>,
    /// Every worktree path the gather COVERED: each attributed path it was
    /// handed (a reaped one included, since the gather positively saw it
    /// gone), each isolated-edit worktree, and each member root it probed. The
    /// obligations cross-check reads it: a worktree coord's obligations read
    /// attributes to the session that is not covered here was never probed,
    /// so its state is UNKNOWN (see `ObligationsUnknown::WorktreeNotProbed`).
    /// Empty when the gather stopped before probing anything.
    pub covered: Vec<String>,
}

/// Why custody rules a session out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustodyDirty {
    /// A member is a primary checkout.
    SharedCheckout { path: String },
    /// A member has uncommitted changes or untracked files.
    Uncommitted { path: String },
    /// A member has commits its upstream does not — or, with no upstream,
    /// commits that are on no remote-tracking ref at all.
    Unpushed { path: String, ahead: u64 },
    /// The session's own custody slot says its work was not left clean.
    WipSlot { path: String, wip_state: String },
}

/// Which custody input could not be determined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustodyUnknown {
    /// No directory is known for the session.
    NoSessionDir,
    /// The attributed-worktree read failed.
    OwnershipUnreadable { detail: String },
    /// A member's probe failed or timed out.
    ProbeFailed { path: String, detail: String },
}

/// The custody verdict. Only [`CustodyVerdict::Clean`] lets the
/// `finished_close` arm close a session; `Dirty` and `Unknown` never do —
/// served policy `verification-and-evidence`
/// `unknown-must-not-render-as-a-default`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustodyVerdict {
    Clean,
    Dirty(CustodyDirty),
    Unknown(CustodyUnknown),
}

impl CustodyDirty {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SharedCheckout { .. } => "shared_checkout",
            Self::Uncommitted { .. } => "uncommitted",
            Self::Unpushed { .. } => "unpushed",
            Self::WipSlot { .. } => "wip_slot",
        }
    }

    /// One human-readable line naming the worktree and what was found.
    pub fn detail(&self) -> String {
        match self {
            Self::SharedCheckout { path } => format!("{path} is a primary checkout"),
            Self::Uncommitted { path } => {
                format!("{path} has uncommitted changes or untracked files")
            }
            Self::Unpushed { path, ahead } => {
                format!("{path} has {ahead} commit(s) that are not pushed")
            }
            Self::WipSlot { path, wip_state } => {
                format!("this session's custody slot in {path} reads wip_state={wip_state}")
            }
        }
    }
}

impl CustodyUnknown {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoSessionDir => "no_session_dir",
            Self::OwnershipUnreadable { .. } => "ownership_unreadable",
            Self::ProbeFailed { .. } => "probe_failed",
        }
    }

    /// One human-readable line naming what could not be read.
    pub fn detail(&self) -> String {
        match self {
            Self::NoSessionDir => {
                "neither the lifecycle record nor the process names a directory".to_string()
            }
            Self::OwnershipUnreadable { detail } => {
                format!("the session's attributed worktrees could not be read: {detail}")
            }
            Self::ProbeFailed { path, detail } => format!("{path}: {detail}"),
        }
    }
}

impl CustodyVerdict {
    pub fn is_clean(&self) -> bool {
        matches!(self, Self::Clean)
    }

    /// The wire word: `clean` | `dirty` | `unknown`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Dirty(_) => "dirty",
            Self::Unknown(_) => "unknown",
        }
    }

    /// The reason code, for `dirty` and `unknown`.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Clean => None,
            Self::Dirty(d) => Some(d.as_str()),
            Self::Unknown(u) => Some(u.as_str()),
        }
    }

    /// The human-readable detail, for `dirty` and `unknown`.
    pub fn detail(&self) -> Option<String> {
        match self {
            Self::Clean => None,
            Self::Dirty(d) => Some(d.detail()),
            Self::Unknown(u) => Some(u.detail()),
        }
    }
}

/// Decide custody. Pure.
///
/// Evaluated in this fixed order:
///
/// 1. **No session directory → `Unknown`.** Without it the session's own
///    repo cannot even be named.
/// 2. **An unreadable ownership read → `Unknown`.** The attributed worktrees
///    are the part of the set the directory cannot reveal.
/// 3. **Any member that is definitely not clean → `Dirty`**, in member
///    order: a primary checkout, then uncommitted changes, the session's own
///    custody slot, and unpushed commits. Dirty outranks a sibling's unknown
///    because it is the more useful thing to report, and neither closes.
/// 4. **Any member that could not be judged → `Unknown`**: a probe that
///    failed or timed out. A branch with no upstream is NOT one — the probe
///    then counts the commits on no remote-tracking ref (see
///    [`MemberState::Probed::ahead`]), which is a definite answer.
/// 5. Otherwise **`Clean`** — including a session with NO members at all,
///    which is a session sitting outside any repo (the workspace root) that
///    no worktree is attributed to.
pub fn custody_gate(inputs: &CustodyInputs) -> CustodyVerdict {
    if inputs.session_dir.is_none() {
        return CustodyVerdict::Unknown(CustodyUnknown::NoSessionDir);
    }
    if let Err(detail) = &inputs.ownership {
        return CustodyVerdict::Unknown(CustodyUnknown::OwnershipUnreadable {
            detail: detail.clone(),
        });
    }
    for member in &inputs.members {
        let path = member.path.clone();
        match &member.state {
            MemberState::PrimaryCheckout => {
                return CustodyVerdict::Dirty(CustodyDirty::SharedCheckout { path })
            }
            MemberState::Probed { dirty: true, .. } => {
                return CustodyVerdict::Dirty(CustodyDirty::Uncommitted { path })
            }
            MemberState::Probed { .. } | MemberState::ProbeFailed(_) => {}
        }
        if let Some(state) = member
            .slot_wip_state
            .as_deref()
            .filter(|s| DIRTY_WIP_STATES.contains(s))
        {
            return CustodyVerdict::Dirty(CustodyDirty::WipSlot {
                path,
                wip_state: state.to_string(),
            });
        }
        if let MemberState::Probed { ahead, .. } = member.state {
            if ahead > 0 {
                return CustodyVerdict::Dirty(CustodyDirty::Unpushed { path, ahead });
            }
        }
    }
    for member in &inputs.members {
        match &member.state {
            MemberState::ProbeFailed(detail) => {
                return CustodyVerdict::Unknown(CustodyUnknown::ProbeFailed {
                    path: member.path.clone(),
                    detail: detail.clone(),
                })
            }
            MemberState::Probed { .. } | MemberState::PrimaryCheckout => {}
        }
    }
    CustodyVerdict::Clean
}

/// The `windDown.custody` block `GET /restart-readiness` attaches when the
/// `finished_close` arm judged a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustodyView {
    /// `clean` | `dirty` | `unknown`.
    pub verdict: &'static str,
    /// The reason code, for `dirty` and `unknown`.
    pub reason: Option<&'static str>,
    /// What was found, naming the worktree.
    pub detail: Option<String>,
    /// When the arm judged it (unix millis).
    pub judged_at: i64,
}

impl CustodyView {
    pub fn from_verdict(verdict: &CustodyVerdict, judged_at_ms: i64) -> Self {
        Self {
            verdict: verdict.as_str(),
            reason: verdict.reason(),
            detail: verdict.detail(),
            judged_at: judged_at_ms,
        }
    }
}

// ---------------------------------------------------------------------------
// The obligations gate — the REMOTE half of `closeable` (plan
// `2026-10-18-a-finished-session-is-safe-to-close-when-nothing-lives-only-in-it-and-coord-owns-whether-its-watchers-work`,
// D1/D2 and Phase 6)
// ---------------------------------------------------------------------------

/// The verdict words of coord's `coord_session_obligations` read that let a
/// session close. `landed` and `watched` are the two the plan names; `retired`
/// is coord's word for a gate that reached a terminal state "leaving nothing
/// owed on it" (a cleared gate whose continuation reported `work_completed`, a
/// withdrawn gate whose object is still held) — nothing is owed, so nothing
/// lives only in the session. Leaving it out would make every session that ever
/// had a gate clear read Unknown, and the arm would close nothing.
/// `scripts/session-closeable.sh` passes `retired` as well, so the two agree
/// on that word.
pub const WATCHED_OBLIGATION_VERDICTS: [&str; 3] = ["landed", "watched", "retired"];

/// The source blocks of coord's obligations read whose items ALL carry a
/// verdict. Each must be present, read, and free of a verdict-less item: a body
/// that omits one has not said the session owes nothing there.
pub const VERDICT_SOURCES: [&str; 2] = ["prs", "gates"];

/// A tracked obligation coord says nothing watches (`not_watched`) — work the
/// session owes that would be lost if it closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObligationsUnwatched {
    /// The coord source block the item came from (`prs`, `gates`, …).
    pub source: String,
    /// The item, as coord served it, compactly rendered.
    pub item: String,
    /// The reasons coord gave, joined; empty when it gave none.
    pub reasons: String,
}

/// Which obligations input could not be determined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObligationsUnknown {
    /// The read itself failed — no coord base, transport error, timeout,
    /// non-2xx, or an undecodable body.
    Unreadable { detail: String },
    /// The answer is not the shape coord's read serves.
    Malformed { detail: String },
    /// `all_tracked_sources_read` was not `true`: a source was degraded or
    /// truncated, and its list is "NOT evidence of absence" (coord's own
    /// words).
    SourcesNotRead { reported: String },
    /// `session_proven_in_tenant` was not `true`: no tenant-scoped source puts
    /// this session in the tenant the read ran as, so an empty answer is the
    /// absence of a session, not of obligations. This is what a read made with
    /// the wrong tenant's credential looks like.
    SessionNotProven { reported: String },
    /// A source block with `items` whose `coverage` is not `read`.
    SourceNotRead { source: String, coverage: String },
    /// An item whose verdict is `unknown`, a word this gate does not know, or
    /// a `not_watched` whose attribution to this session is `uncertain`.
    ItemUnknown {
        source: String,
        item: String,
        verdict: String,
        uncertain: bool,
    },
    /// A worktree coord attributes to the session (its `worktrees` block)
    /// that the custody gate did not cover: no probe looked at it, so whether
    /// it holds unpushed or uncommitted work is UNKNOWN. A `worktree_path`
    /// coord served as null is the same, spelled `<absent>`.
    WorktreeNotProbed { worktree: String },
    /// Not read on this pass: the pass's obligations-read budget was spent
    /// (`OBLIGATIONS_READS_PER_PASS` in the executor). UNKNOWN, never watched.
    Deferred { detail: String },
}

/// The obligations verdict. Only [`ObligationsVerdict::Watched`] lets the
/// `finished_close` arm close a session; `Unwatched` and `Unknown` never do —
/// served policy `verification-and-evidence` `unknown-must-not-render-as-a-default`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObligationsVerdict {
    /// Every tracked item that carries a verdict reads [`WATCHED_OBLIGATION_VERDICTS`]
    /// and every tracked source was read. `judged` counts the items judged.
    Watched {
        judged: usize,
    },
    Unwatched(ObligationsUnwatched),
    Unknown(ObligationsUnknown),
}

impl ObligationsUnknown {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unreadable { .. } => "obligations_unreadable",
            Self::Malformed { .. } => "malformed",
            Self::SourcesNotRead { .. } => "sources_not_read",
            Self::SessionNotProven { .. } => "session_not_proven",
            Self::SourceNotRead { .. } => "source_not_read",
            Self::ItemUnknown { .. } => "item_unknown",
            Self::WorktreeNotProbed { .. } => "worktree_not_probed",
            Self::Deferred { .. } => "obligations_deferred",
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Self::Unreadable { detail } => {
                format!("coord's obligations read could not be completed: {detail}")
            }
            Self::Malformed { detail } => {
                format!("coord's obligations answer is not the expected shape: {detail}")
            }
            Self::SourcesNotRead { reported } => format!(
                "all_tracked_sources_read is {reported}: a degraded or truncated source is not \
                 evidence of absence"
            ),
            Self::SessionNotProven { reported } => format!(
                "session_proven_in_tenant is {reported}: coord cannot place this session in the \
                 tenant the read ran as, so an empty answer proves nothing"
            ),
            Self::SourceNotRead { source, coverage } => {
                format!("source `{source}` has coverage `{coverage}`, not `read`")
            }
            Self::ItemUnknown {
                source,
                item,
                verdict,
                uncertain,
            } => {
                let uncertain = if *uncertain {
                    " (attribution uncertain: coord cannot say it is this session's)"
                } else {
                    ""
                };
                format!("{source} item {item} reads `{verdict}`, which is not a watched verdict{uncertain}")
            }
            Self::WorktreeNotProbed { worktree } => format!(
                "coord attributes worktree {worktree} to the session, but the custody gate never probed it, so its state is unknown"
            ),
            Self::Deferred { detail } => format!("not read this pass: {detail}"),
        }
    }
}

impl ObligationsVerdict {
    pub fn is_watched(&self) -> bool {
        matches!(self, Self::Watched { .. })
    }

    /// The wire word: `watched` | `unwatched` | `unknown`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Watched { .. } => "watched",
            Self::Unwatched(_) => "unwatched",
            Self::Unknown(_) => "unknown",
        }
    }

    /// The reason code, for `unwatched` and `unknown`.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Watched { .. } => None,
            Self::Unwatched(_) => Some("not_watched"),
            Self::Unknown(u) => Some(u.as_str()),
        }
    }

    /// The human-readable detail, for `unwatched` and `unknown`.
    pub fn detail(&self) -> Option<String> {
        match self {
            Self::Watched { .. } => None,
            Self::Unwatched(u) => Some(if u.reasons.is_empty() {
                format!("{} item {} reads `not_watched`", u.source, u.item)
            } else {
                format!(
                    "{} item {} reads `not_watched`: {}",
                    u.source, u.item, u.reasons
                )
            }),
            Self::Unknown(u) => Some(u.detail()),
        }
    }

    /// An `Unknown` for a read that failed — the single spelling every
    /// transport failure takes, so none can be mistaken for an answer.
    pub fn unreadable(detail: impl Into<String>) -> Self {
        Self::Unknown(ObligationsUnknown::Unreadable {
            detail: detail.into(),
        })
    }
}

/// Decide whether every obligation coord attributes to a session is watched.
/// Pure over the decoded body of `coord_session_obligations` (or its HTTP twin
/// `GET /coord/overlord/session-obligations/:id`). It accepts the same passing
/// words as `scripts/session-closeable.sh`'s coord judge. The two differ on one
/// case: a non-passing item whose `attribution` is `uncertain` is Unknown here,
/// while the script notes some such items without letting them move its
/// verdict. This gate is the stricter of the two.
///
/// Evaluated in this order:
///
/// 1. **A failed read → `Unknown`.** Never a default: a timeout is not "no
///    obligations".
/// 2. **A body that is not an object → `Unknown`.**
/// 3. **`all_tracked_sources_read` anything but `true` → `Unknown`.**
/// 4. **`session_proven_in_tenant` anything but `true` → `Unknown`.** The
///    runner reads as its default tenant's credential; a session of another
///    tenant answers with every list empty, which must not read as "owes
///    nothing".
/// 5. **Every source block that carries an `items` list** (`prs`, `gates`,
///    `worktrees`, `work_units`, `findings`, …): its `coverage` must be `read`,
///    except that an EMPTY block coord declares `not_tracked`
///    (`spawned_followup` — coord holds no store for it, and it is outside
///    `all_tracked_sources_read`) says nothing and is skipped. Anything else is
///    `Unknown`. Items with no `verdict` (worktrees, work units, findings carry
///    none) are not judged here; the worktree half is the custody gate's.
/// 6. **An item whose verdict is `not_watched` → `Unwatched`**, unless coord
///    marks its attribution `uncertain` (then `Unknown`: it may be a peer's).
///    Any other non-watched word, including `unknown`, is `Unknown`.
///    `Unwatched` outranks `Unknown`, as `Dirty` outranks `Unknown` in
///    [`custody_gate`]; neither closes.
/// 7. **`prs` and `gates` must each be present with `coverage: read`**, and an
///    item in either with no verdict is `Unknown`; a body that omits them is
///    `Unknown` however clean the rest looks.
/// 8. **No source block read at all → `Unknown`**: an answer with nothing in it
///    is not evidence of an empty session.
pub fn obligations_gate(read: &Result<serde_json::Value, String>) -> ObligationsVerdict {
    use serde_json::Value;
    let doc = match read {
        Ok(doc) => doc,
        Err(detail) => return ObligationsVerdict::unreadable(detail.clone()),
    };
    let Some(root) = doc.as_object() else {
        return ObligationsVerdict::Unknown(ObligationsUnknown::Malformed {
            detail: "the body is not a JSON object".to_string(),
        });
    };
    if root.get("all_tracked_sources_read") != Some(&Value::Bool(true)) {
        let reported = root
            .get("all_tracked_sources_read")
            .map_or_else(|| "absent".to_string(), Value::to_string);
        return ObligationsVerdict::Unknown(ObligationsUnknown::SourcesNotRead { reported });
    }
    if root.get("session_proven_in_tenant") != Some(&Value::Bool(true)) {
        let reported = root
            .get("session_proven_in_tenant")
            .map_or_else(|| "absent".to_string(), Value::to_string);
        return ObligationsVerdict::Unknown(ObligationsUnknown::SessionNotProven { reported });
    }
    let mut judged = 0usize;
    let mut blocks_read = 0usize;
    let mut verdict_sources_read = 0usize;
    let mut unwatched: Option<ObligationsUnwatched> = None;
    let mut unknown: Option<ObligationsUnknown> = None;
    for (source, block) in root {
        let Some(block) = block.as_object() else {
            continue;
        };
        let coverage = block.get("coverage").and_then(Value::as_str);
        let Some(items) = block.get("items").and_then(Value::as_array) else {
            if block.contains_key("coverage") {
                unknown.get_or_insert(ObligationsUnknown::SourceNotRead {
                    source: source.clone(),
                    coverage: coverage.unwrap_or("?").to_string(),
                });
            }
            continue;
        };
        match coverage {
            Some("read") => blocks_read += 1,
            Some("not_tracked") if items.is_empty() => continue,
            other => {
                unknown.get_or_insert(ObligationsUnknown::SourceNotRead {
                    source: source.clone(),
                    coverage: other.unwrap_or("?").to_string(),
                });
                continue;
            }
        }
        if VERDICT_SOURCES.contains(&source.as_str()) {
            verdict_sources_read += 1;
        }
        for entry in items {
            let Some(verdict) = entry.get("verdict") else {
                // `prs` and `gates` items always carry one; elsewhere
                // (worktrees, work units, findings) none is expected.
                if VERDICT_SOURCES.contains(&source.as_str()) {
                    unknown.get_or_insert(ObligationsUnknown::ItemUnknown {
                        source: source.clone(),
                        item: entry.to_string().chars().take(200).collect(),
                        verdict: "<absent>".to_string(),
                        uncertain: false,
                    });
                }
                continue;
            };
            judged += 1;
            let word = verdict.as_str();
            if word.is_some_and(|w| WATCHED_OBLIGATION_VERDICTS.contains(&w)) {
                continue;
            }
            let item = entry
                .get("item")
                .map_or_else(|| "-".to_string(), Value::to_string);
            let uncertain = entry.get("attribution").and_then(Value::as_str) == Some("uncertain");
            if word == Some("not_watched") && !uncertain {
                let reasons = match entry.get("reasons") {
                    Some(Value::Array(rs)) => rs
                        .iter()
                        .map(|r| r.as_str().map_or_else(|| r.to_string(), str::to_string))
                        .collect::<Vec<_>>()
                        .join("; "),
                    Some(Value::String(r)) => r.clone(),
                    _ => String::new(),
                };
                unwatched.get_or_insert(ObligationsUnwatched {
                    source: source.clone(),
                    item,
                    reasons: reasons.chars().take(300).collect(),
                });
            } else {
                unknown.get_or_insert(ObligationsUnknown::ItemUnknown {
                    source: source.clone(),
                    item,
                    verdict: word.map_or_else(|| verdict.to_string(), str::to_string),
                    uncertain,
                });
            }
        }
    }
    if let Some(u) = unwatched {
        return ObligationsVerdict::Unwatched(u);
    }
    if let Some(u) = unknown {
        return ObligationsVerdict::Unknown(u);
    }
    if blocks_read == 0 {
        return ObligationsVerdict::Unknown(ObligationsUnknown::Malformed {
            detail: "all_tracked_sources_read is true but no source block with items was read"
                .to_string(),
        });
    }
    if verdict_sources_read != VERDICT_SOURCES.len() {
        let missing: Vec<&str> = VERDICT_SOURCES
            .iter()
            .copied()
            .filter(|name| {
                root.get(*name)
                    .and_then(|b| b.get("coverage"))
                    .and_then(Value::as_str)
                    != Some("read")
            })
            .collect();
        return ObligationsVerdict::Unknown(ObligationsUnknown::Malformed {
            detail: format!(
                "the source block(s) {} are absent or not read, so the answer says nothing about them",
                missing.join(", ")
            ),
        });
    }
    ObligationsVerdict::Watched { judged }
}

/// The `windDown.obligations` block `GET /restart-readiness` attaches when the
/// `finished_close` arm read a session's obligations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObligationsView {
    /// `watched` | `unwatched` | `unknown`.
    pub verdict: &'static str,
    /// The reason code, for `unwatched` and `unknown`.
    pub reason: Option<&'static str>,
    /// What was found, naming the item.
    pub detail: Option<String>,
    /// How many verdict-carrying items a `watched` verdict rests on.
    pub items_judged: Option<usize>,
    /// When the arm judged it (unix millis).
    pub judged_at: i64,
}

impl ObligationsView {
    pub fn from_verdict(verdict: &ObligationsVerdict, judged_at_ms: i64) -> Self {
        Self {
            verdict: verdict.as_str(),
            reason: verdict.reason(),
            detail: verdict.detail(),
            items_judged: match verdict {
                ObligationsVerdict::Watched { judged } => Some(*judged),
                _ => None,
            },
            judged_at: judged_at_ms,
        }
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
    idle_since_ms: Option<i64>,
}

impl TrackedObservation {
    fn verdict(&self) -> GridIdle {
        match self.idle_since_ms {
            Some(since_ms) => GridIdle::Idle { since_ms },
            None => GridIdle::Busy,
        }
    }
}

impl GridIdleTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one observation and return what it shows.
    ///
    /// `generation_before` is the grid generation read immediately before the
    /// grid snapshot, `generation_after` immediately after it; `looks_idle` is
    /// the idle verdict over that snapshot; `observed_at_ms` is when the
    /// snapshot was read.
    ///
    /// An observation that is OLDER than the last one recorded — a lower
    /// starting generation, or an earlier timestamp — is a late arrival from a
    /// concurrent observer, and is ignored: the previous verdict is returned
    /// unchanged, so `since` can never move backwards.
    pub fn observe(
        &mut self,
        looks_idle: bool,
        generation_before: u64,
        generation_after: u64,
        observed_at_ms: i64,
    ) -> GridIdle {
        if let Some(last) = self.last {
            if generation_before < last.generation || observed_at_ms < last.observed_at_ms {
                return last.verdict();
            }
        }
        if !looks_idle {
            self.last = Some(TrackedObservation {
                generation: generation_after,
                observed_at_ms,
                idle_since_ms: None,
            });
            return GridIdle::Busy;
        }
        let continuous = generation_before == generation_after
            && matches!(
                self.last,
                Some(TrackedObservation {
                    generation,
                    idle_since_ms: Some(_),
                    ..
                }) if generation == generation_before
            );
        let since_ms = match (continuous, self.last) {
            (
                true,
                Some(TrackedObservation {
                    idle_since_ms: Some(since),
                    ..
                }),
            ) => since,
            _ => observed_at_ms,
        };
        self.last = Some(TrackedObservation {
            generation: generation_after,
            observed_at_ms,
            idle_since_ms: Some(since_ms),
        });
        GridIdle::Idle { since_ms }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRACE: Duration = Duration::from_secs(600);
    const GRACE_MS: i64 = 600_000;

    /// A terminal session satisfying every condition, idle since t=1000.
    fn eligible_terminal() -> EligibilityInputs {
        EligibilityInputs {
            kind: SessionKind::Terminal,
            work_status: WorkStatus::Finished,
            sideband: Sideband::NeverReported,
            grid: GridIdle::Idle { since_ms: 1_000 },
            has_live_children: Some(false),
            finished_at_ms: None,
            grace: GRACE,
        }
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
        i.grid = GridIdle::Idle {
            since_ms: 5_000_000,
        };
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
        i.grid = GridIdle::Idle { since_ms: 900_000 };
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

    // ---- rule 3: grid -----------------------------------------------------

    #[test]
    fn busy_grid_is_ineligible() {
        let mut i = eligible_terminal();
        i.grid = GridIdle::Busy;
        assert_eq!(
            eligibility(&i, i64::MAX),
            Eligibility::Ineligible {
                reason: IneligibleReason::GridBusy
            }
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
            busy.grid = GridIdle::Busy;
            assert!(matches!(
                eligibility(&busy, i64::MAX),
                Eligibility::Ineligible {
                    reason: IneligibleReason::GridBusy
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
        );
        assert_eq!((u.eligibility, u.reason), ("unknown", Some("grid_unknown")));

        let json = serde_json::to_value(&n).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "eligibility": "not_yet", "since": 7, "until": 9,
                "reason": null, "kind": "looping"
            })
        );
    }

    // ---- the custody gate (D2) ---------------------------------------------

    fn clean_member(path: &str) -> CustodyMember {
        CustodyMember {
            path: path.to_string(),
            state: MemberState::Probed {
                dirty: false,
                ahead: 0,
            },
            slot_wip_state: Some("clean".to_string()),
        }
    }

    fn custody(members: Vec<CustodyMember>) -> CustodyInputs {
        CustodyInputs {
            session_dir: Some("/ws/agent-worktrees/a/repo".to_string()),
            ownership: Ok(()),
            members,
            covered: Vec::new(),
        }
    }

    #[test]
    fn a_clean_pushed_worktree_is_clean() {
        let v = custody_gate(&custody(vec![clean_member("/ws/a")]));
        assert_eq!(v, CustodyVerdict::Clean);
        assert!(v.is_clean());
        assert_eq!((v.as_str(), v.reason()), ("clean", None));
    }

    #[test]
    fn an_untracked_or_uncommitted_change_is_dirty() {
        let mut m = clean_member("/ws/a");
        m.state = MemberState::Probed {
            dirty: true,
            ahead: 0,
        };
        assert_eq!(
            custody_gate(&custody(vec![m])),
            CustodyVerdict::Dirty(CustodyDirty::Uncommitted {
                path: "/ws/a".to_string()
            })
        );
    }

    #[test]
    fn an_unpushed_commit_is_dirty() {
        let mut m = clean_member("/ws/a");
        m.state = MemberState::Probed {
            dirty: false,
            ahead: 2,
        };
        let v = custody_gate(&custody(vec![m]));
        assert_eq!(
            v,
            CustodyVerdict::Dirty(CustodyDirty::Unpushed {
                path: "/ws/a".to_string(),
                ahead: 2
            })
        );
        assert_eq!(v.reason(), Some("unpushed"));
    }

    #[test]
    fn a_failed_probe_is_unknown() {
        let mut m = clean_member("/ws/a");
        m.state = MemberState::ProbeFailed("git status timed out".to_string());
        let v = custody_gate(&custody(vec![m]));
        assert_eq!(v.as_str(), "unknown");
        assert_eq!(v.reason(), Some("probe_failed"));
    }

    #[test]
    fn a_failed_ownership_read_is_unknown_even_with_a_clean_directory() {
        let mut i = custody(vec![clean_member("/ws/a")]);
        i.ownership = Err("coord returned 503".to_string());
        assert_eq!(
            custody_gate(&i),
            CustodyVerdict::Unknown(CustodyUnknown::OwnershipUnreadable {
                detail: "coord returned 503".to_string()
            })
        );
    }

    #[test]
    fn no_known_directory_is_unknown() {
        let mut i = custody(vec![]);
        i.session_dir = None;
        assert_eq!(
            custody_gate(&i),
            CustodyVerdict::Unknown(CustodyUnknown::NoSessionDir)
        );
    }

    #[test]
    fn a_primary_checkout_is_dirty_whatever_its_status() {
        let m = CustodyMember {
            path: "/ws/qontinui-runner".to_string(),
            state: MemberState::PrimaryCheckout,
            slot_wip_state: None,
        };
        assert_eq!(
            custody_gate(&custody(vec![m])),
            CustodyVerdict::Dirty(CustodyDirty::SharedCheckout {
                path: "/ws/qontinui-runner".to_string()
            })
        );
    }

    #[test]
    fn a_captured_deferred_or_failed_custody_slot_is_dirty() {
        for state in DIRTY_WIP_STATES {
            let mut m = clean_member("/ws/a");
            m.slot_wip_state = Some(state.to_string());
            assert_eq!(
                custody_gate(&custody(vec![m])),
                CustodyVerdict::Dirty(CustodyDirty::WipSlot {
                    path: "/ws/a".to_string(),
                    wip_state: state.to_string()
                }),
                "{state}"
            );
        }
        // `clean`, `unchanged` and an absent slot are not refusals on their
        // own — the git probes are the source.
        for state in [Some("clean"), Some("unchanged"), None] {
            let mut m = clean_member("/ws/a");
            m.slot_wip_state = state.map(str::to_string);
            assert!(custody_gate(&custody(vec![m])).is_clean(), "{state:?}");
        }
    }

    #[test]
    fn a_workspace_root_session_is_judged_by_its_attributed_worktrees_alone() {
        // No member for the directory itself: it is in no repo.
        let none = custody(vec![]);
        assert!(custody_gate(&none).is_clean(), "nothing attributed → clean");

        let mut dirty = clean_member("/ws/agent-worktrees/x/repo");
        dirty.state = MemberState::Probed {
            dirty: true,
            ahead: 0,
        };
        assert_eq!(custody_gate(&custody(vec![dirty])).as_str(), "dirty");
    }

    #[test]
    fn one_dirty_sibling_rules_out_a_multi_worktree_session() {
        let mut sibling = clean_member("/ws/b");
        sibling.state = MemberState::Probed {
            dirty: false,
            ahead: 1,
        };
        let v = custody_gate(&custody(vec![
            clean_member("/ws/a"),
            sibling,
            clean_member("/ws/c"),
        ]));
        assert_eq!(
            v,
            CustodyVerdict::Dirty(CustodyDirty::Unpushed {
                path: "/ws/b".to_string(),
                ahead: 1
            })
        );
    }

    #[test]
    fn a_dirty_member_is_reported_ahead_of_a_siblings_unknown() {
        let mut unknown = clean_member("/ws/a");
        unknown.state = MemberState::ProbeFailed("timed out".to_string());
        let mut dirty = clean_member("/ws/b");
        dirty.state = MemberState::Probed {
            dirty: true,
            ahead: 0,
        };
        assert_eq!(
            custody_gate(&custody(vec![unknown, dirty])).as_str(),
            "dirty"
        );
    }

    #[test]
    fn the_custody_view_carries_the_verdict_and_is_omitted_when_absent() {
        let v = CustodyView::from_verdict(
            &CustodyVerdict::Unknown(CustodyUnknown::ProbeFailed {
                path: "/ws/a".to_string(),
                detail: "git status timed out".to_string(),
            }),
            42,
        );
        assert_eq!(
            serde_json::to_value(&v).unwrap(),
            serde_json::json!({
                "verdict": "unknown", "reason": "probe_failed",
                "detail": "/ws/a: git status timed out", "judgedAt": 42
            })
        );
        // A view the arm never judged serialises with no `custody` key at all.
        let plain = WindDownView::from_verdict(
            &Eligibility::Eligible { since_ms: 1 },
            SessionKind::Terminal,
        );
        assert!(serde_json::to_value(&plain)
            .unwrap()
            .get("custody")
            .is_none());
    }

    // ---- GridIdleTracker ---------------------------------------------------

    #[test]
    fn tracker_first_idle_observation_starts_the_window() {
        let mut t = GridIdleTracker::new();
        assert_eq!(t.observe(true, 5, 5, 100), GridIdle::Idle { since_ms: 100 });
    }

    #[test]
    fn tracker_extends_the_window_while_the_grid_does_not_move() {
        let mut t = GridIdleTracker::new();
        t.observe(true, 5, 5, 100);
        assert_eq!(t.observe(true, 5, 5, 900), GridIdle::Idle { since_ms: 100 });
        assert_eq!(
            t.observe(true, 5, 5, 5_000),
            GridIdle::Idle { since_ms: 100 }
        );
    }

    #[test]
    fn tracker_restarts_when_the_grid_moved_between_observations() {
        // Idle at t=100 and idle again at t=900, but the generation advanced
        // in between: the session may have worked in the gap.
        let mut t = GridIdleTracker::new();
        t.observe(true, 5, 5, 100);
        assert_eq!(t.observe(true, 8, 8, 900), GridIdle::Idle { since_ms: 900 });
        // ...and then extends from the new window.
        assert_eq!(
            t.observe(true, 8, 8, 1_500),
            GridIdle::Idle { since_ms: 900 }
        );
    }

    #[test]
    fn tracker_restarts_when_the_grid_moved_during_the_debounce() {
        let mut t = GridIdleTracker::new();
        t.observe(true, 5, 5, 100);
        assert_eq!(t.observe(true, 5, 6, 900), GridIdle::Idle { since_ms: 900 });
    }

    #[test]
    fn tracker_busy_observation_breaks_the_window() {
        let mut t = GridIdleTracker::new();
        t.observe(true, 5, 5, 100);
        assert_eq!(t.observe(false, 5, 5, 200), GridIdle::Busy);
        assert_eq!(t.observe(true, 5, 5, 300), GridIdle::Idle { since_ms: 300 });
    }

    #[test]
    fn tracker_ignores_an_observation_from_an_older_generation() {
        let mut t = GridIdleTracker::new();
        t.observe(true, 9, 9, 1_000);
        // A concurrent observer's late snapshot of generation 7 must neither
        // restart nor backdate the window.
        assert_eq!(
            t.observe(true, 7, 7, 2_000),
            GridIdle::Idle { since_ms: 1_000 }
        );
        assert_eq!(
            t.observe(false, 7, 8, 2_000),
            GridIdle::Idle { since_ms: 1_000 }
        );
        assert_eq!(
            t.observe(true, 9, 9, 3_000),
            GridIdle::Idle { since_ms: 1_000 }
        );
    }

    #[test]
    fn tracker_ignores_an_observation_older_in_time_so_since_never_moves_back() {
        let mut t = GridIdleTracker::new();
        t.observe(false, 4, 4, 5_000);
        assert_eq!(
            t.observe(true, 4, 4, 6_000),
            GridIdle::Idle { since_ms: 6_000 }
        );
        // Same generation, earlier timestamp: a late arrival, ignored.
        assert_eq!(
            t.observe(true, 4, 4, 5_500),
            GridIdle::Idle { since_ms: 6_000 }
        );
        assert_eq!(
            t.observe(true, 4, 4, 9_000),
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
        i.grid = GridIdle::Idle { since_ms: 50_000 };
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

    // ── The obligations gate ─────────────────────────────────────────────

    /// A coord answer shaped like `session_obligations_core`'s: tracked
    /// sources `read`, `spawned_followup` always `not_tracked`.
    fn obligations(prs: serde_json::Value, gates: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "claude_code_session_id": "11111111-1111-1111-1111-111111111111",
            "all_tracked_sources_read": true,
            "session_proven_in_tenant": true,
            "prs": {"coverage": "read", "items": prs, "count": 0, "truncated": false},
            "gates": {"coverage": "read", "items": gates, "count": 0, "truncated": false},
            "worktrees": {"coverage": "read", "items": [], "truncated": false},
            "work_units": {"coverage": "read", "items": [{"id": "wu-1"}], "truncated": false},
            "findings": {"coverage": "read", "items": [], "truncated": false},
            "spawned_followup": {"coverage": "not_tracked", "note": "n", "items": []},
            "note": "n",
        })
    }

    fn gate_item(verdict: &str) -> serde_json::Value {
        serde_json::json!({
            "item": {"gate_id": "g-1"}, "verdict": verdict,
            "reasons": ["why"], "attribution": "proven",
        })
    }

    fn judge(doc: serde_json::Value) -> ObligationsVerdict {
        obligations_gate(&Ok(doc))
    }

    #[test]
    fn every_item_landed_watched_or_retired_is_watched() {
        let doc = obligations(
            serde_json::json!([gate_item("landed")]),
            serde_json::json!([gate_item("watched"), gate_item("retired")]),
        );
        assert_eq!(judge(doc), ObligationsVerdict::Watched { judged: 3 });
    }

    /// A session that owes nothing is closeable: tracked sources read, empty.
    #[test]
    fn a_session_with_no_obligations_at_all_is_watched() {
        let doc = obligations(serde_json::json!([]), serde_json::json!([]));
        assert_eq!(judge(doc), ObligationsVerdict::Watched { judged: 0 });
    }

    #[test]
    fn a_not_watched_item_is_unwatched_and_names_the_item() {
        let doc = obligations(
            serde_json::json!([]),
            serde_json::json!([gate_item("watched"), gate_item("not_watched")]),
        );
        let ObligationsVerdict::Unwatched(u) = judge(doc) else {
            panic!("a not_watched item must not read watched");
        };
        assert_eq!(u.source, "gates");
        assert!(u.item.contains("g-1"), "{u:?}");
        assert_eq!(u.reasons, "why");
    }

    /// Unwatched outranks an Unknown elsewhere — the more useful thing to
    /// report, and neither closes.
    #[test]
    fn unwatched_outranks_unknown() {
        let doc = obligations(
            serde_json::json!([gate_item("unknown")]),
            serde_json::json!([gate_item("not_watched")]),
        );
        assert_eq!(judge(doc).as_str(), "unwatched");
    }

    #[test]
    fn an_unknown_or_unrecognised_item_verdict_is_unknown_never_watched() {
        for word in ["unknown", "recorded", "owed", "WATCHED", ""] {
            let doc = obligations(serde_json::json!([gate_item(word)]), serde_json::json!([]));
            let v = judge(doc);
            assert_eq!(v.as_str(), "unknown", "{word:?}");
            assert_eq!(v.reason(), Some("item_unknown"), "{word:?}");
        }
        // A verdict that is not a string at all.
        let doc = obligations(
            serde_json::json!([{"item": {}, "verdict": null}]),
            serde_json::json!([]),
        );
        assert_eq!(judge(doc).as_str(), "unknown");
    }

    /// An uncertain attribution may be a peer's gate: even `not_watched` is
    /// then Unknown, never a definite owed word.
    #[test]
    fn an_uncertain_not_watched_item_is_unknown() {
        let mut item = gate_item("not_watched");
        item["attribution"] = serde_json::json!("uncertain");
        let v = judge(obligations(
            serde_json::json!([]),
            serde_json::json!([item]),
        ));
        assert_eq!(v.reason(), Some("item_unknown"));
        assert!(v.detail().unwrap().contains("attribution uncertain"));
    }

    /// (c) An unreadable read is UNKNOWN — a timeout, a 503, a decode failure
    /// all arrive here as `Err` and none is ever "nothing owed".
    #[test]
    fn an_unreadable_read_is_unknown_not_watched() {
        let v = obligations_gate(&Err("GET …: operation timed out".to_string()));
        assert_eq!(v.as_str(), "unknown");
        assert_eq!(v.reason(), Some("obligations_unreadable"));
        assert!(!v.is_watched());
        assert!(v.detail().unwrap().contains("timed out"));
    }

    #[test]
    fn a_degraded_or_truncated_answer_is_unknown_even_with_clean_items() {
        for reported in [
            serde_json::json!(false),
            serde_json::json!("true"),
            serde_json::Value::Null,
        ] {
            let mut doc = obligations(serde_json::json!([]), serde_json::json!([]));
            doc["all_tracked_sources_read"] = reported;
            assert_eq!(judge(doc).reason(), Some("sources_not_read"));
        }
        // The key absent altogether (a coord predating it) is not "true".
        let mut doc = obligations(serde_json::json!([]), serde_json::json!([]));
        doc.as_object_mut()
            .unwrap()
            .remove("all_tracked_sources_read");
        assert_eq!(judge(doc).reason(), Some("sources_not_read"));
    }

    /// A read made as the wrong tenant answers with empty lists and
    /// `session_proven_in_tenant: false`; that is not "owes nothing".
    #[test]
    fn a_session_coord_cannot_place_in_the_tenant_is_unknown() {
        for reported in [serde_json::json!(false), serde_json::Value::Null] {
            let mut doc = obligations(serde_json::json!([]), serde_json::json!([]));
            doc["session_proven_in_tenant"] = reported;
            assert_eq!(judge(doc).reason(), Some("session_not_proven"));
        }
        let mut doc = obligations(serde_json::json!([]), serde_json::json!([]));
        doc.as_object_mut()
            .unwrap()
            .remove("session_proven_in_tenant");
        assert_eq!(judge(doc).reason(), Some("session_not_proven"));
    }

    /// A body with both flags true and only a non-verdict block read says
    /// nothing about PRs or gates; neither does a verdict-less PR or gate item.
    #[test]
    fn missing_prs_or_gates_and_verdictless_items_there_are_unknown() {
        for drop in ["prs", "gates"] {
            let mut doc = obligations(serde_json::json!([]), serde_json::json!([]));
            doc.as_object_mut().unwrap().remove(drop);
            let v = judge(doc);
            assert_eq!(v.reason(), Some("malformed"), "{drop}");
            assert!(v.detail().unwrap().contains(drop), "{drop}");
        }
        let doc = serde_json::json!({
            "all_tracked_sources_read": true, "session_proven_in_tenant": true,
            "findings": {"coverage": "read", "items": []},
        });
        assert_eq!(judge(doc).reason(), Some("malformed"));
        for bare in [
            serde_json::json!({"item": {"gate_id": "g"}}),
            serde_json::json!("g"),
        ] {
            let v = judge(obligations(
                serde_json::json!([]),
                serde_json::json!([bare]),
            ));
            assert_eq!(v.reason(), Some("item_unknown"));
        }
    }

    #[test]
    fn a_source_with_items_that_is_not_read_is_unknown() {
        let mut doc = obligations(serde_json::json!([]), serde_json::json!([]));
        doc["gates"]["coverage"] = serde_json::json!("degraded");
        assert_eq!(judge(doc).reason(), Some("source_not_read"));
        // `not_tracked` with items in it is not the declared-empty shape.
        let mut doc = obligations(serde_json::json!([]), serde_json::json!([]));
        doc["spawned_followup"]["items"] = serde_json::json!([gate_item("watched")]);
        assert_eq!(judge(doc).reason(), Some("source_not_read"));
    }

    #[test]
    fn a_body_that_is_not_the_shape_is_unknown() {
        for doc in [
            serde_json::json!([]),
            serde_json::json!("ok"),
            serde_json::json!({"all_tracked_sources_read": true}),
            serde_json::json!({"all_tracked_sources_read": true, "session_proven_in_tenant": true, "note": "no blocks"}),
        ] {
            let v = judge(doc.clone());
            assert!(!v.is_watched(), "{doc}");
        }
        assert_eq!(
            judge(serde_json::json!({"all_tracked_sources_read": true, "session_proven_in_tenant": true}))
                .reason(),
            Some("malformed")
        );
    }

    #[test]
    fn the_obligations_view_serialises_the_verdict_and_its_basis() {
        let watched = ObligationsView::from_verdict(&ObligationsVerdict::Watched { judged: 2 }, 9);
        let json = serde_json::to_value(&watched).unwrap();
        assert_eq!(json["verdict"], "watched");
        assert_eq!(json["itemsJudged"], 2);
        assert_eq!(json["judgedAt"], 9);
        let unknown = ObligationsView::from_verdict(&ObligationsVerdict::unreadable("boom"), 9);
        let json = serde_json::to_value(&unknown).unwrap();
        assert_eq!(json["verdict"], "unknown");
        assert_eq!(json["reason"], "obligations_unreadable");
        assert!(json["itemsJudged"].is_null());
    }
}
