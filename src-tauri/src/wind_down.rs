//! Wind-down eligibility — the pure core of "may a drained runner close this
//! session?" (plan `2026-09-13-drained-runner-never-reaches-idle`, Phase 1,
//! design decisions D4 and D6).
//!
//! ## What this answers, and what it never does
//!
//! [`eligibility`] folds five observations about ONE live `claude` session into
//! a four-way verdict ([`Eligibility`]). It performs no I/O, reads no clock
//! (`now_ms` is an argument) and acts on nothing: in Phase 1 the verdict is
//! reported read-only on `GET /restart-readiness` and nothing closes a session
//! on its strength. The closing primitive (`TerminalSession::graceful_exit`)
//! exists beside it but is not wired to it.
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

/// The verdict. Only [`Eligibility::Eligible`] could ever authorise a close,
/// and in Phase 1 nothing acts on it at all.
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
        }
    }

    pub fn is_eligible(&self) -> bool {
        self.eligibility == "eligible"
    }
}

/// Continuity tracker behind [`GridIdle::Idle`]'s `since_ms`, one per
/// terminal.
///
/// An idle observation can only EXTEND the previous idle window when nothing
/// could have happened in between: the previous observation was idle, and the
/// terminal's grid generation counter (bumped on every grid mutation) has not
/// moved since, neither before this observation's first read nor during its
/// debounce. Otherwise a session that worked between two observations minutes
/// apart would read as continuously idle, so the window restarts at this
/// observation.
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
}
