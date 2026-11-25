//! The close guard G1-G9 (architecture §3.3) and the interim narrowing (I1, I6,
//! I8). `Close` is emitted only if EVERY conjunct is `True` under Kleene
//! conjunction; `Unknown` is not `True`.
//!
//! **S1 (never close with work left), by construction.** [`close_action`] holds
//! the crate's ONE construction site of `Action::Close`; it needs a
//! [`CloseProof`], which only [`GuardReport::close_proof`] can seal, and only for
//! a report whose conjunction is `True`. A unit test parses the crate source and
//! counts constructions (`guard/tests.rs`). The actuator re-evaluates this guard
//! itself on its own fresh reads (§4.2) — a stale `Decision` cannot close.
//!
//! Decisions recorded where the architecture leaves the reading open:
//!
//! - **G3's grace.** G3 is `True` only once the session has been continuously
//!   `Idle` for the grace window (10 min; capacity may shorten it to `g_min`
//!   = 2 min via `dials.grace_override`, never below, and an unreadable
//!   capacity never shortens it — S5). Any live shell, proven stray or not, makes
//!   G3 `False` (a stray is still `Work`; "never Close while a stray runs").
//! - **G7 over an empty touched set.** No tree discovered is `Unknown`: the
//!   absence of a discovered tree is not cleanliness. `Absent` is `Unknown` too
//!   (T14): a missing tree is not evidence nothing lives only in it.
//! - **G9's latch.** An unreadable latch reads TRIPPED once it has been
//!   unreadable longer than one poll interval (closing is the destructive
//!   direction); within the interval it is `Unknown`.
//! - **Pane kind.** The lifecycle closes only plain terminal panes: a steward or
//!   looping-agent pane is owned by its launcher (RV5-3), an unreadable kind is
//!   `Unknown`; both fold into G3.
//! - **Interim narrowing** (I1, I6, I8) applies only on the interim host; build X
//!   carries G1-G9 and the close dial.

use crate::model::{
    digest_of, Action, CloseProof, Dials, Guard, GuardId, HostCaps, ItemState, WorkItemView,
};
use crate::state::{
    tau, Activity, Custody, CustodyAxis, Declared, Dur, Input, Latch, Mode, OwnedAxis, PaneKind,
    Parked, SelfConts, SessionState, Timestamp, Tri, Unk, GRACE, GRACE_MIN,
};

/// The conjuncts, plus what the decision needs to know about the grace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardReport {
    pub guards: Vec<Guard>,
    /// When the grace window ends, if the session is continuously idle.
    pub grace_until: Option<Timestamp>,
    digest: crate::state::Digest,
}

impl GuardReport {
    /// Kleene conjunction of every conjunct.
    pub fn verdict(&self) -> Tri {
        Tri::all(self.guards.iter().map(|g| g.value.clone()))
    }

    /// The conjunction with G3 set aside as long as ONLY its grace clock is
    /// short: every other conjunct (and G3's non-grace part) is `True`.
    pub fn cleared_but_for_grace(&self) -> bool {
        let Some(until) = self.grace_until else {
            return false;
        };
        let _ = until;
        self.guards.iter().all(|g| match g.id {
            GuardId::G3 => g.value.is_true() || g.note.starts_with("grace"),
            _ => g.value.is_true(),
        })
    }

    /// The first conjunct that is not `True`, for `close_refused`.
    pub fn first_not_true(&self) -> Option<&Guard> {
        self.guards.iter().find(|g| !g.value.is_true())
    }

    /// Seal a proof, or nothing: only a `True` conjunction yields one.
    pub fn close_proof(&self) -> Option<CloseProof> {
        self.verdict()
            .is_true()
            .then(|| CloseProof::seal(self.digest))
    }
}

/// THE construction site of `Action::Close` (S1).
pub fn close_action(report: &GuardReport, not_before: Timestamp) -> Option<Action> {
    let proof = report.close_proof()?;
    Some(Action::Close { not_before, proof })
}

fn g(id: GuardId, value: Tri, note: impl Into<String>) -> Guard {
    Guard {
        id,
        value,
        note: note.into(),
    }
}

fn unknown_of(u: &Unk) -> Tri {
    Tri::Unknown(u.clone())
}

/// Evaluate G1-G9 and the interim conjuncts.
pub fn evaluate(
    s: &SessionState,
    items: &[WorkItemView],
    dials: &Dials,
    host: HostCaps,
    now: Timestamp,
) -> GuardReport {
    let mut guards: Vec<Guard> = Vec::new();
    let mut grace_until = None;

    // G1: the coord row says finished.
    guards.push(g(
        GuardId::G1,
        s.coord_finished.clone(),
        "coord session_status=finished",
    ));

    // G2: W = Finished (provenance verified) and not R.
    let w = match &s.work.value {
        Declared::Finished { .. } => Tri::True,
        Declared::Unknown(u) => unknown_of(u),
        _ => Tri::False,
    };
    guards.push(g(
        GuardId::G2,
        w.and(s.revived.clone().negate()),
        "W=Finished and not R",
    ));

    // G3: idle through the grace window, no work/unknown child, a plain pane.
    let grace = dials
        .grace_override
        .map_or(GRACE, |o| o.max(GRACE_MIN).min(GRACE));
    let (g3, note) = match &s.activity.value {
        Activity::Idle { grace_from, .. } => {
            let until = grace_from.plus(grace);
            grace_until = Some(until);
            if now >= until {
                (Tri::True, "idle through the grace window".to_string())
            } else {
                (Tri::False, "grace window still running".to_string())
            }
        }
        Activity::Unknown(u) => (unknown_of(u), "activity unknown".to_string()),
        Activity::Turn | Activity::Compacting { .. } => (Tri::False, "a turn is open".to_string()),
        Activity::Delegated { .. } => (Tri::False, "delegated work in flight".to_string()),
        Activity::ShellPinned { .. } => (
            Tri::False,
            "a live shell (a stray is still work)".to_string(),
        ),
    };
    let pane = match s.pane_kind {
        PaneKind::Terminal => Tri::True,
        PaneKind::Steward | PaneKind::Looping => Tri::False,
        PaneKind::Unreadable => Tri::unknown("pane_kind", "unreadable"),
    };
    guards.push(g(GuardId::G3, g3.and(pane), note));

    // G4: S = empty.
    let g4 = match &s.selfcont.value {
        SelfConts::Unknown(u) => unknown_of(u),
        SelfConts::Known(_) => Tri::from_bool(s.holding_selfcont().is_empty()),
    };
    guards.push(g(GuardId::G4, g4, "S is empty"));

    // G5: P = None.
    let g5 = match &s.parked.value {
        Parked::None => Tri::True,
        Parked::Unknown(u) => unknown_of(u),
        _ => Tri::False,
    };
    guards.push(g(GuardId::G5, g5, "P is None"));

    // G6: I = Clear.
    let g6 = match &s.input.value {
        Input::Clear => Tri::True,
        Input::Unknown(u) => unknown_of(u),
        Input::Dialog { .. } | Input::Draft { .. } => Tri::False,
    };
    guards.push(g(GuardId::G6, g6, "I is Clear"));

    // G7: every tree of the touched set Clean.
    let g7 = match &s.custody.value {
        CustodyAxis::Unknown(u) => unknown_of(u),
        CustodyAxis::Known(trees) if trees.is_empty() => Tri::unknown(
            "custody",
            "no touched tree discovered; absence is not cleanliness",
        ),
        CustodyAxis::Known(trees) => {
            if trees
                .iter()
                .any(|(_, c)| matches!(c, Custody::Dirty { .. }))
            {
                Tri::False
            } else if let Some((p, c)) = trees
                .iter()
                .find(|(_, c)| matches!(c, Custody::Unknown { .. } | Custody::Absent { .. }))
            {
                Tri::unknown("custody", format!("{p}: {c:?}"))
            } else {
                Tri::True
            }
        }
    };
    guards.push(g(GuardId::G7, g7, "every touched tree Clean"));

    // G8: every owned item terminal for the session.
    let g8 = match &s.owned.value {
        OwnedAxis::Unknown(u) => unknown_of(u),
        OwnedAxis::Known => Tri::all(items.iter().map(|i| match &i.state {
            ItemState::Unknown(u) => Tri::Unknown(u.clone()),
            _ => i.terminal_for_session(),
        })),
    };
    guards.push(g(GuardId::G8, g8, "every owned item terminal-for-session"));

    // G9: every input fresh, dials readable, latch clear for this fn_version.
    let fresh = Tri::all([
        fresh_or_unknown("hosting", s.hosting.fresh(now, tau::H)),
        fresh_or_unknown("activity", s.activity.fresh(now, tau::A)),
        fresh_or_unknown("input", s.input.fresh(now, tau::I)),
        fresh_or_unknown("selfcont", s.selfcont.fresh(now, tau::S)),
        fresh_or_unknown("parked", s.parked.fresh(now, tau::P)),
        fresh_or_unknown("owned", s.owned.fresh(now, tau::O)),
        fresh_or_unknown("custody", s.custody.fresh(now, tau::K)),
        match &s.schema_skew {
            Some(u) => unknown_of(u),
            None => Tri::True,
        },
    ]);
    let latch = match dials.latch {
        Latch::Clear => Tri::True,
        Latch::Tripped => Tri::False,
        Latch::Unreadable { since } => {
            if now.since(since) > dials.latch_poll_interval {
                Tri::False
            } else {
                Tri::unknown("latch", "unreadable within one poll interval")
            }
        }
    };
    guards.push(g(
        GuardId::G9,
        fresh.and(latch),
        "inputs fresh; latch clear",
    ));

    // The close dial is a conjunct too (a preference, held at its last value).
    let dial = match dials.close {
        crate::model::Dial::On => Tri::True,
        crate::model::Dial::Off => Tri::False,
        crate::model::Dial::Unknown => Tri::unknown("dial", "lifecycle.close never read"),
    };
    guards.push(g(GuardId::Dial, dial, "lifecycle.close"));

    // Interim narrowing: fail-closed conjuncts on the CLI host only.
    if host.is_interim() {
        guards.push(g(
            GuardId::I1,
            Tri::from_bool(host.build_allowlisted && host.cell_allowlisted),
            "cell and build on the interim allow-list",
        ));
        let i6 = match (s.mode, &s.activity.value) {
            (Mode::Unattended, _) => Tri::True,
            (_, Activity::Idle { since_turn_end, .. }) => {
                Tri::from_bool(now.since(*since_turn_end) >= Dur::mins(60))
            }
            _ => Tri::False,
        };
        guards.push(g(GuardId::I6, i6, "attended: 60 min of continuous idle"));
        guards.push(g(
            GuardId::I8,
            Tri::from_bool(!dials.drain_or_self_hold),
            "no drain or executor self-hold",
        ));
    }

    let digest = digest_of(&[&s, &items, &dials, &host, &now]);
    GuardReport {
        guards,
        grace_until,
        digest,
    }
}

fn fresh_or_unknown(axis: &str, fresh: bool) -> Tri {
    if fresh {
        Tri::True
    } else {
        Tri::unknown(axis.to_string(), "stale")
    }
}

#[cfg(test)]
mod tests;
