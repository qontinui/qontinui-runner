use super::*;

fn t(ms: u64) -> Timestamp {
    Timestamp(ms)
}

fn u() -> Tri {
    Tri::unknown("p", "r")
}

#[test]
fn kleene_and_or_not() {
    use Tri::{False, True};
    assert_eq!(True.and(True), True);
    assert_eq!(True.and(False), False);
    // False dominates Unknown in a conjunction; Unknown is never True.
    assert_eq!(u().and(False), False);
    assert_eq!(False.and(u()), False);
    assert!(u().and(True).is_unknown());
    assert!(!u().is_true());
    assert_eq!(True.or(u()), True);
    assert!(False.or(u()).is_unknown());
    assert_eq!(False.or(False), False);
    assert!(u().not().is_unknown());
    assert_eq!(True.not(), False);
    assert_eq!(Tri::all([]), True);
    assert!(Tri::all([True, u(), True]).is_unknown());
    assert_eq!(Tri::all([True, u(), False]), False);
}

#[test]
fn timestamp_math_saturates() {
    assert_eq!(t(5).since(t(9)), Dur(0));
    assert_eq!(t(9).since(t(5)), Dur(4));
    assert_eq!(t(1).minus(Dur(5)), t(0));
    assert_eq!(Dur::mins(10), Dur::secs(600));
}

fn inst(pid: u32) -> Instance {
    Instance {
        pid,
        proc_start: pid as u64 * 10,
    }
}

fn live_event(pid: u32, at: u64, seq: u64) -> Event {
    Event {
        at: t(at),
        seq,
        kind: EventKind::HostLive {
            instance: inst(pid),
            config_dir: "/c".into(),
            cc_version: "2.1.310".into(),
            pane_shape: PaneShape::Holder,
        },
    }
}

fn ev(at: u64, seq: u64, kind: EventKind) -> Event {
    Event {
        at: t(at),
        seq,
        kind,
    }
}

fn known(now: u64) -> SessionState {
    let mut s = SessionState::unread("s1", t(now));
    s.hosting = Observed::new(
        Hosting::Live {
            instance: inst(1),
            config_dir: "/c".into(),
            cc_version: "x".into(),
            pane_shape: PaneShape::Holder,
            registry_seen: true,
        },
        t(now),
    );
    s.activity = Observed::new(
        Activity::Idle {
            since_turn_end: t(0),
            grace_from: t(0),
        },
        t(now),
    );
    s.selfcont = Observed::new(SelfConts::Known(vec![]), t(now));
    s.parked = Observed::new(Parked::None, t(now));
    s.input = Observed::new(Input::Clear, t(now));
    s.work = Observed::new(Declared::Undeclared, t(now));
    s.owned = Observed::new(OwnedAxis::Known, t(now));
    s.custody = Observed::new(CustodyAxis::Known(vec![]), t(now));
    s.revived = Tri::False;
    s
}

#[test]
fn fold_is_order_independent_for_same_stamps() {
    let a = vec![
        ev(10, 2, EventKind::TurnEnd),
        ev(10, 1, EventKind::TurnStart),
    ];
    let b = vec![
        ev(10, 1, EventKind::TurnStart),
        ev(10, 2, EventKind::TurnEnd),
    ];
    let s0 = SessionState::unread("s", t(0));
    assert_eq!(fold(s0.clone(), &a), fold(s0, &b));
}

#[test]
fn compaction_restarts_grace_but_not_the_idle_clock() {
    let s = fold(
        SessionState::unread("s", t(0)),
        &[
            ev(100, 1, EventKind::TurnEnd),
            ev(500, 2, EventKind::CompactStart),
            ev(545, 3, EventKind::CompactEnd),
        ],
    );
    assert_eq!(
        s.activity.value,
        Activity::Idle {
            since_turn_end: t(100),
            grace_from: t(545)
        }
    );
}

#[test]
fn new_instance_turns_a_wakeup_into_a_lost_wake_and_withdraws_the_countdown() {
    // T33 / T34.
    let mut s0 = SessionState::unread("s", t(0));
    s0 = fold(s0, &[live_event(1, 1, 1)]);
    s0 = fold(
        s0,
        &[
            ev(
                2,
                2,
                EventKind::SelfContRead(vec![
                    SelfCont::Wakeup {
                        at: t(9_000),
                        armed_by: inst(1),
                    },
                    SelfCont::QueuedNotice { since: t(3) },
                ]),
            ),
            ev(
                2,
                3,
                EventKind::StopFailure(Parked::Limit {
                    limit_type: "seven_day".into(),
                    resets_at: t(1_000_000),
                    account: "a".into(),
                    auto_continue: AutoContinue::Armed { armed_by: inst(1) },
                }),
            ),
        ],
    );
    assert_eq!(s0.holding_selfcont().len(), 2);
    let s1 = fold(s0, &[live_event(2, 50, 4)]);
    assert!(s1
        .holding_selfcont()
        .contains(&&SelfCont::LostOnNewInstance { at: t(9_000) }));
    assert!(s1
        .holding_selfcont()
        .contains(&&SelfCont::QueuedNotice { since: t(3) }));
    assert!(matches!(
        s1.parked.value,
        Parked::Limit {
            auto_continue: AutoContinue::Withdrawn,
            ..
        }
    ));
}

#[test]
fn an_arming_entry_of_a_dead_instance_does_not_hold() {
    let e = SelfCont::Wakeup {
        at: t(1),
        armed_by: inst(1),
    };
    assert!(e.holds(Some(inst(1))));
    assert!(!e.holds(Some(inst(2))));
    assert!(!e.holds(None));
    assert!(SelfCont::PendingMessage {
        msg_id: "m".into(),
        blocking_unacked: false
    }
    .holds(None));
}

#[test]
fn unknown_event_kind_withholds() {
    let s = fold(
        known(10),
        &[ev(11, 1, EventKind::UnknownKind("future".into()))],
    );
    assert!(matches!(
        phase_of(&s, ItemFacts::default(), false, t(11)),
        Phase::Unknown(_)
    ));
}

#[test]
fn phases_follow_priority_order() {
    let now = 1_000;
    let items = ItemFacts::default();
    let mut s = known(now);
    assert_eq!(
        phase_of(&s, items, false, t(now)),
        Phase::Stopped(Stopped::Undeclared)
    );

    // Scheduled outranks Stopped.
    s.selfcont = Observed::new(
        SelfConts::Known(vec![SelfCont::Wakeup {
            at: t(9),
            armed_by: inst(1),
        }]),
        t(now),
    );
    assert_eq!(phase_of(&s, items, false, t(now)), Phase::Scheduled);
    // Parked outranks Scheduled.
    s.parked = Observed::new(
        Parked::ApiError {
            kind: ApiErrorKind::Overloaded,
            retryable: true,
        },
        t(now),
    );
    assert_eq!(phase_of(&s, items, false, t(now)), Phase::Parked);
    // Interacting outranks Parked.
    s.input = Observed::new(
        Input::Draft {
            bytes: 3,
            writer: Writer::OperatorKeystroke,
            since: t(1),
        },
        t(now),
    );
    assert_eq!(phase_of(&s, items, false, t(now)), Phase::Interacting);
    // Delegated outranks Interacting.
    s.activity = Observed::new(
        Activity::Delegated {
            agents: 1,
            workflows: 0,
        },
        t(now),
    );
    assert_eq!(phase_of(&s, items, false, t(now)), Phase::Delegated);
    // Working outranks Delegated.
    s.activity = Observed::new(Activity::Turn, t(now));
    assert_eq!(phase_of(&s, items, false, t(now)), Phase::Working);
    // Unknown outranks everything.
    s.parked = Observed::new(Parked::Unknown(Unk::new("p", "r")), t(now));
    assert!(matches!(
        phase_of(&s, items, false, t(now)),
        Phase::Unknown(_)
    ));
}

#[test]
fn a_stale_axis_reads_unknown() {
    let s = known(1_000);
    assert!(matches!(
        phase_of(&s, ItemFacts::default(), false, t(1_000 + 61_000)),
        Phase::Unknown(_)
    ));
    // H goes stale at 10 s.
    assert!(matches!(
        phase_of(&s, ItemFacts::default(), false, t(1_000 + 11_000)),
        Phase::Unknown(_)
    ));
    assert!(!matches!(
        phase_of(&s, ItemFacts::default(), false, t(1_000 + 9_000)),
        Phase::Unknown(_)
    ));
}

#[test]
fn stray_only_shell_pin_reads_stopped_pinned_by_strays() {
    let now = 1_000;
    let mut s = known(now);
    s.work = Observed::new(
        Declared::Finished {
            at: t(1),
            kind: FinishKind::Done,
            items: vec![],
        },
        t(now),
    );
    s.activity = Observed::new(
        Activity::ShellPinned {
            shells: 8,
            stray_shells: 8,
            monitors: 0,
            mcp_tasks: 0,
        },
        t(now),
    );
    assert_eq!(
        phase_of(&s, ItemFacts::default(), false, t(now)),
        Phase::Stopped(Stopped::FinishedPinned)
    );
    // One real shell among them is work: Delegated (T46 cell 2).
    s.activity = Observed::new(
        Activity::ShellPinned {
            shells: 9,
            stray_shells: 8,
            monitors: 0,
            mcp_tasks: 0,
        },
        t(now),
    );
    assert_eq!(
        phase_of(&s, ItemFacts::default(), false, t(now)),
        Phase::Delegated
    );
    // A monitor beside strays is not proven pinned-by-strays-only.
    s.activity = Observed::new(
        Activity::ShellPinned {
            shells: 8,
            stray_shells: 8,
            monitors: 1,
            mcp_tasks: 0,
        },
        t(now),
    );
    assert_eq!(
        phase_of(&s, ItemFacts::default(), false, t(now)),
        Phase::Delegated
    );
}

#[test]
fn finished_sub_phases() {
    let now = 1_000;
    let mut s = known(now);
    s.work = Observed::new(
        Declared::Finished {
            at: t(1),
            kind: FinishKind::Done,
            items: vec![],
        },
        t(now),
    );
    let none = ItemFacts::default();
    assert_eq!(
        phase_of(&s, none, false, t(now)),
        Phase::Stopped(Stopped::Finished)
    );
    assert_eq!(
        phase_of(
            &s,
            ItemFacts {
                awaiting_first_verdict: true,
                ..none
            },
            false,
            t(now)
        ),
        Phase::Stopped(Stopped::AwaitingFirstVerdict)
    );
    assert_eq!(
        phase_of(&s, ItemFacts { owed: true, ..none }, false, t(now)),
        Phase::Stopped(Stopped::Owing)
    );
    s.custody = Observed::new(
        CustodyAxis::Known(vec![(
            "/t".into(),
            Custody::Absent {
                evidence: "gone".into(),
            },
        )]),
        t(now),
    );
    // T14: Absent is not Clean.
    assert_eq!(
        phase_of(&s, none, false, t(now)),
        Phase::Stopped(Stopped::FinishedUnverified)
    );
    s.custody = Observed::new(CustodyAxis::Known(vec![]), t(now));
    s.revived = Tri::True;
    assert_eq!(
        phase_of(&s, none, false, t(now)),
        Phase::Stopped(Stopped::Revived)
    );
    s.revived = Tri::unknown("r", "x");
    assert!(matches!(
        phase_of(&s, none, false, t(now)),
        Phase::Unknown(_)
    ));
}

#[test]
fn exited_host_does_not_need_the_live_axes() {
    let mut s = SessionState::unread("s", t(5));
    s.hosting = Observed::new(
        Hosting::Exited(ExitFacts {
            code: Some(0),
            initiator: ExitInitiator::SelfExit,
            door: None,
            typed: None,
            at: t(5),
            creator: Principal::Unresolvable,
            writes: vec![],
            read_back_gone: false,
        }),
        t(5),
    );
    assert!(matches!(
        phase_of(&s, ItemFacts::default(), false, t(500_000)),
        Phase::Closed { .. }
    ));
}
