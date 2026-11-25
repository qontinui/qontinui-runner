//! The registry record (`~/.claude/sessions/<pid>.json`), the Stop payload and
//! the input funnel, folded into the A and I axes (§2.1).
//!
//! Registry `busy` alone is NOT `Turn` (`busy = isLoading ∨ delegatedActive`,
//! SIG-1): a turn is positively open only when a hook or the transcript says so,
//! so a `busy` with no turn evidence and no delegated work reads `Unknown`.

use crate::state::{Activity, DialogKind, Input, Timestamp, Unk, Writer};

/// The registry's status word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegStatus {
    Idle,
    Busy,
    Shell,
    Waiting,
    Other(String),
}

/// One registry record, as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryRecord {
    pub status: RegStatus,
    /// `waitingFor`, e.g. a permission or limit-menu text.
    pub waiting_for: Option<String>,
    pub status_updated_at: Timestamp,
}

/// Background work a Stop payload lists (`background_tasks`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StopPayload {
    pub agents: u32,
    pub workflows: u32,
    pub shells: u32,
    pub monitors: u32,
    pub mcp_tasks: u32,
}

/// Whether a turn is open, from the hooks / transcript (never the registry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnState {
    Open,
    Ended {
        at: Timestamp,
        /// The end of the last compaction, if later than the turn end.
        last_compact_end: Option<Timestamp>,
    },
    Compacting {
        since_turn_end: Option<Timestamp>,
    },
    Unreadable(Unk),
}

/// A (registry, stop payload, turn state) read -> the A axis.
///
/// `stray_shells` is how many of the live shells [`crate::classify_child`]
/// proved stray (§4.1); `shells` is the larger of the registry's and the
/// payload's count.
pub fn activity_from(
    reg: Option<&RegistryRecord>,
    stop: Option<&StopPayload>,
    turn: &TurnState,
    pending_agents: u32,
    pending_workflows: u32,
    stray_shells: u32,
) -> Activity {
    match turn {
        TurnState::Unreadable(u) => return Activity::Unknown(u.clone()),
        TurnState::Compacting { since_turn_end } => {
            return Activity::Compacting {
                since_turn_end: *since_turn_end,
            }
        }
        TurnState::Open => return Activity::Turn,
        TurnState::Ended { .. } => {}
    }
    let Some(reg) = reg else {
        return Activity::Unknown(Unk::new("registry", "no registry record"));
    };
    let agents = stop.map_or(0, |s| s.agents).max(pending_agents);
    let workflows = stop.map_or(0, |s| s.workflows).max(pending_workflows);
    if agents > 0 || workflows > 0 {
        return Activity::Delegated { agents, workflows };
    }
    let reg_shell = u32::from(reg.status == RegStatus::Shell);
    let shells = stop.map_or(0, |s| s.shells).max(reg_shell);
    let monitors = stop.map_or(0, |s| s.monitors);
    let mcp_tasks = stop.map_or(0, |s| s.mcp_tasks);
    if shells > 0 || monitors > 0 || mcp_tasks > 0 {
        return Activity::ShellPinned {
            shells,
            stray_shells: stray_shells.min(shells),
            monitors,
            mcp_tasks,
        };
    }
    match (&reg.status, turn) {
        (
            RegStatus::Idle | RegStatus::Waiting,
            TurnState::Ended {
                at,
                last_compact_end,
            },
        ) => Activity::Idle {
            since_turn_end: *at,
            grace_from: last_compact_end.map_or(*at, |c| c.max(*at)),
        },
        (RegStatus::Busy, _) => Activity::Unknown(Unk::new(
            "registry",
            "busy with no open turn and no delegated work (busy alone is not Turn)",
        )),
        (RegStatus::Other(w), _) => {
            Activity::Unknown(Unk::new("registry", format!("unrecognised status {w}")))
        }
        _ => Activity::Unknown(Unk::new("registry", "unreadable")),
    }
}

/// The input funnel's count of bytes written since the last CR, by every writer
/// (`insert_prompt` included once recorded, R2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunnelDraft {
    pub bytes: u32,
    pub writer: Writer,
    pub since: Timestamp,
}

/// The interim composer read (a VT emulator over `GET /terminals/{id}/buffer`),
/// used ONLY as a positive witness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerWitness {
    /// An empty input row with the cursor at its start.
    Empty,
    /// Text on the input row.
    Text {
        bytes: u32,
    },
    Unreadable,
}

/// The I axis. A funnel that is exact wins; otherwise only a witness can say
/// `Clear` (a draft is never assumed absent). `countdown_armed`: text attributed
/// to an armed usage-limit countdown (composer pre-filled `continue`) is no
/// draft (T18) — P holds the close anyway.
pub fn input_from(
    reg: Option<&RegistryRecord>,
    funnel: Option<&FunnelDraft>,
    witness: Option<&ComposerWitness>,
    countdown_armed: bool,
    now: Timestamp,
) -> Input {
    if let Some(r) = reg {
        if r.status == RegStatus::Waiting {
            let w = r.waiting_for.clone().unwrap_or_default().to_lowercase();
            let kind = if w.contains("limit") {
                DialogKind::LimitMenu
            } else if w.contains("permission") {
                DialogKind::Permission
            } else if w.contains("question") {
                DialogKind::AskUserQuestion
            } else if w.contains("sandbox") {
                DialogKind::Sandbox
            } else if w.contains("elicit") {
                DialogKind::Elicitation
            } else {
                DialogKind::Other
            };
            return Input::Dialog {
                kind,
                since: r.status_updated_at,
            };
        }
    }
    if let Some(f) = funnel {
        return if f.bytes > 0 {
            Input::Draft {
                bytes: f.bytes,
                writer: f.writer.clone(),
                since: f.since,
            }
        } else {
            Input::Clear
        };
    }
    match witness {
        Some(ComposerWitness::Empty) => Input::Clear,
        Some(ComposerWitness::Text { .. }) if countdown_armed => Input::Clear,
        Some(ComposerWitness::Text { bytes }) => Input::Draft {
            bytes: *bytes,
            writer: Writer::OperatorKeystroke,
            since: now,
        },
        Some(ComposerWitness::Unreadable) | None => {
            Input::Unknown(Unk::new("composer", "no positive witness"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(status: RegStatus) -> RegistryRecord {
        RegistryRecord {
            status,
            waiting_for: None,
            status_updated_at: Timestamp(1),
        }
    }
    fn ended(at: u64) -> TurnState {
        TurnState::Ended {
            at: Timestamp(at),
            last_compact_end: None,
        }
    }

    #[test]
    fn registry_busy_alone_is_not_a_turn() {
        let a = activity_from(Some(&reg(RegStatus::Busy)), None, &ended(5), 0, 0, 0);
        assert!(matches!(a, Activity::Unknown(_)));
        assert_eq!(
            activity_from(Some(&reg(RegStatus::Busy)), None, &TurnState::Open, 0, 0, 0),
            Activity::Turn
        );
    }

    #[test]
    fn delegated_and_pinned_outrank_idle() {
        let r = reg(RegStatus::Idle);
        let a = activity_from(Some(&r), None, &ended(5), 2, 0, 0);
        assert_eq!(
            a,
            Activity::Delegated {
                agents: 2,
                workflows: 0
            }
        );
        let a = activity_from(Some(&reg(RegStatus::Shell)), None, &ended(5), 0, 0, 1);
        assert_eq!(
            a,
            Activity::ShellPinned {
                shells: 1,
                stray_shells: 1,
                monitors: 0,
                mcp_tasks: 0
            }
        );
        // Stop payload monitors / MCP tasks are invisible to the registry (SIG-1).
        let s = StopPayload {
            monitors: 1,
            ..Default::default()
        };
        let a = activity_from(Some(&r), Some(&s), &ended(5), 0, 0, 0);
        assert!(matches!(a, Activity::ShellPinned { monitors: 1, .. }));
    }

    #[test]
    fn a_compaction_restarts_the_grace_window_not_the_idle_clock() {
        let turn = TurnState::Ended {
            at: Timestamp(100),
            last_compact_end: Some(Timestamp(900)),
        };
        let a = activity_from(Some(&reg(RegStatus::Idle)), None, &turn, 0, 0, 0);
        assert_eq!(
            a,
            Activity::Idle {
                since_turn_end: Timestamp(100),
                grace_from: Timestamp(900)
            }
        );
    }

    #[test]
    fn input_is_clear_only_on_a_positive_reading() {
        let now = Timestamp(10);
        assert!(matches!(
            input_from(None, None, None, false, now),
            Input::Unknown(_)
        ));
        assert!(matches!(
            input_from(None, None, Some(&ComposerWitness::Unreadable), false, now),
            Input::Unknown(_)
        ));
        assert_eq!(
            input_from(None, None, Some(&ComposerWitness::Empty), false, now),
            Input::Clear
        );
        assert!(matches!(
            input_from(
                None,
                None,
                Some(&ComposerWitness::Text { bytes: 3 }),
                false,
                now
            ),
            Input::Draft { .. }
        ));
        // T18: text attributed to an armed countdown is no draft.
        assert_eq!(
            input_from(
                None,
                None,
                Some(&ComposerWitness::Text { bytes: 8 }),
                true,
                now
            ),
            Input::Clear
        );
        let waiting = RegistryRecord {
            status: RegStatus::Waiting,
            waiting_for: Some("usage limit menu".into()),
            status_updated_at: Timestamp(4),
        };
        assert!(matches!(
            input_from(
                Some(&waiting),
                None,
                Some(&ComposerWitness::Empty),
                false,
                now
            ),
            Input::Dialog {
                kind: DialogKind::LimitMenu,
                ..
            }
        ));
        let f = FunnelDraft {
            bytes: 0,
            writer: Writer::OperatorKeystroke,
            since: now,
        };
        assert_eq!(input_from(None, Some(&f), None, false, now), Input::Clear);
    }
}
