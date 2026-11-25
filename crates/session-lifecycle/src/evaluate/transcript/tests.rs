use super::*;
use crate::state::{Instance, PaneShape};

fn t(ms: u64) -> Timestamp {
    Timestamp(ms)
}
fn rec(at: u64, kind: RecordKind) -> TranscriptRecord {
    TranscriptRecord {
        at: t(at),
        scope: Scope::Main,
        kind,
    }
}
fn sub(at: u64, kind: RecordKind) -> TranscriptRecord {
    TranscriptRecord {
        at: t(at),
        scope: Scope::Subagent,
        kind,
    }
}
fn open(at: u64, origin: TurnOrigin) -> TranscriptRecord {
    rec(
        at,
        RecordKind::TurnOpen {
            origin,
            compact_summary: false,
            no_turn: false,
        },
    )
}
fn declare(at: u64, tool: DeclareTool) -> TranscriptRecord {
    rec(
        at,
        RecordKind::ToolUse(ToolUse::Declare {
            tool,
            kind: FinishKind::Done,
            items: vec![],
        }),
    )
}
fn live(start: u64) -> InstanceBounds {
    let _ = PaneShape::Holder;
    InstanceBounds {
        instance: Instance {
            pid: 7,
            proc_start: 70,
        },
        started_at: t(start),
    }
}
const MIN: u64 = 60_000;

fn finished(coord_since: u64, local: Option<u64>) -> FinishEvidence {
    FinishEvidence {
        coord: CoordWork::Finished {
            since: t(coord_since),
        },
        local_finish_at: local.map(t),
        relay: None,
    }
}

#[test]
fn a_verified_declaration_reads_finished() {
    let r = [declare(1_000, DeclareTool::FinishSession)];
    let w = declared_work(&r, &finished(2_000, Some(2_000)));
    assert!(matches!(w, Declared::Finished { at, .. } if at == t(1_000)));
}

#[test]
fn a_subagents_finish_is_not_a_declaration_t28() {
    let r = [sub(
        1_000,
        RecordKind::ToolUse(ToolUse::Declare {
            tool: DeclareTool::FinishSession,
            kind: FinishKind::Done,
            items: vec![],
        }),
    )];
    assert!(matches!(
        declared_work(&r, &finished(2_000, Some(2_000))),
        Declared::Unknown(_)
    ));
}

#[test]
fn a_peers_misresolved_finish_and_next_moves_e_arm_declare_nothing_t29_t30() {
    assert!(matches!(
        declared_work(&[], &finished(2_000, Some(2_000))),
        Declared::Unknown(_)
    ));
    let r = [declare(1_000, DeclareTool::NextMoveFinishE)];
    assert!(matches!(
        declared_work(&r, &finished(2_000, Some(2_000))),
        Declared::Unknown(_)
    ));
}

#[test]
fn the_declaration_must_precede_and_be_within_ten_minutes_of_the_local_record() {
    let r = [declare(0, DeclareTool::FinishSession)];
    // 11 min before: no match (an outbox delay cannot manufacture one either).
    assert!(matches!(
        declared_work(&r, &finished(0, Some(11 * MIN))),
        Declared::Unknown(_)
    ));
    assert!(matches!(
        declared_work(&r, &finished(0, Some(9 * MIN))),
        Declared::Finished { .. }
    ));
    // After the local record: no match.
    let r = [declare(5 * MIN, DeclareTool::FinishSession)];
    assert!(matches!(
        declared_work(&r, &finished(0, Some(MIN))),
        Declared::Unknown(_)
    ));
    // The local record unreadable: coord's `since` is the anchor.
    let r = [declare(0, DeclareTool::FinishSession)];
    assert!(matches!(
        declared_work(&r, &finished(3 * MIN, None)),
        Declared::Finished { .. }
    ));
}

#[test]
fn a_coord_only_declaration_matches_coords_since() {
    let r = [declare(0, DeclareTool::CoordReportStatus)];
    // The local record is irrelevant to a declaration that crossed no outbox.
    assert!(matches!(
        declared_work(&r, &finished(2 * MIN, Some(99 * MIN))),
        Declared::Finished { .. }
    ));
}

#[test]
fn a_relayed_gate_outcome_is_matched_by_its_row_with_no_time_bound_t44() {
    let r = [declare(0, DeclareTool::GateOutcome)];
    let mut ev = finished(500 * MIN, Some(500 * MIN));
    assert!(
        matches!(declared_work(&r, &ev), Declared::Unknown(_)),
        "no relay row"
    );
    ev.relay = Some(RelayedFinish {
        outcome_row_present: true,
    });
    assert!(matches!(declared_work(&r, &ev), Declared::Finished { .. }));
    // A relay finish with no declaration in the transcript is unverified.
    assert!(matches!(declared_work(&[], &ev), Declared::Unknown(_)));
}

#[test]
fn absence_of_a_declaration_is_a_fact() {
    let ev = FinishEvidence {
        coord: CoordWork::Undeclared,
        local_finish_at: None,
        relay: None,
    };
    assert_eq!(declared_work(&[], &ev), Declared::Undeclared);
    let ev = FinishEvidence {
        coord: CoordWork::Working,
        local_finish_at: None,
        relay: None,
    };
    assert_eq!(declared_work(&[], &ev), Declared::Working);
}

#[test]
fn every_turn_origin_revives_but_summaries_and_local_commands_do_not_t31_t32_t42() {
    let stamp = Some(t(1_000));
    for o in [
        TurnOrigin::Human,
        TurnOrigin::Peer,
        TurnOrigin::Scheduled,
        TurnOrigin::TaskNotification,
        TurnOrigin::AutoContinuation,
    ] {
        assert_eq!(revived(&[open(2_000, o)], true, stamp), Tri::True);
    }
    let summary = rec(
        2_000,
        RecordKind::TurnOpen {
            origin: TurnOrigin::Other("c".into()),
            compact_summary: true,
            no_turn: false,
        },
    );
    let exit_cmd = rec(
        2_000,
        RecordKind::TurnOpen {
            origin: TurnOrigin::Human,
            compact_summary: false,
            no_turn: true,
        },
    );
    assert_eq!(revived(&[summary, exit_cmd], true, stamp), Tri::False);
    // A subagent's turn is not the session's.
    assert_eq!(
        revived(
            &[sub(
                2_000,
                RecordKind::TurnOpen {
                    origin: TurnOrigin::Human,
                    compact_summary: false,
                    no_turn: false
                }
            )],
            true,
            stamp
        ),
        Tri::False
    );
    // Before the stamp: not a revival.
    assert_eq!(
        revived(&[open(500, TurnOrigin::Human)], true, stamp),
        Tri::False
    );
    assert!(revived(&[], false, stamp).is_unknown());
    assert!(revived(&[], true, None).is_unknown());
}

#[test]
fn finish_stamp_is_the_earlier_of_the_two() {
    let w = Declared::Finished {
        at: t(5),
        kind: FinishKind::Done,
        items: vec![],
    };
    assert_eq!(finish_stamp(&w, Some(t(3))), Some(t(3)));
    assert_eq!(finish_stamp(&w, Some(t(9))), Some(t(5)));
    assert_eq!(finish_stamp(&w, None), Some(t(5)));
    assert_eq!(finish_stamp(&Declared::Working, Some(t(1))), None);
}

fn s_of(r: &[TranscriptRecord], start: u64, now: u64) -> Vec<SelfCont> {
    match selfconts_from(r, true, live(start), &Arrivals::default(), None, t(now)) {
        SelfConts::Known(v) => v,
        SelfConts::Unknown(u) => panic!("{u:?}"),
    }
}

#[test]
fn a_pending_wakeup_arms_s_and_its_fire_clears_it_t2() {
    let r = [rec(
        10,
        RecordKind::ToolUse(ToolUse::ScheduleWakeup {
            at: t(9_000),
            stop: false,
        }),
    )];
    assert_eq!(s_of(&r, 0, 100).len(), 1);
    let mut r2 = r.to_vec();
    r2.push(rec(9_001, RecordKind::WakeupFired));
    assert!(s_of(&r2, 0, 9_100).is_empty());
}

#[test]
fn a_subagent_armed_wakeup_counts() {
    let r = [sub(
        10,
        RecordKind::ToolUse(ToolUse::ScheduleWakeup {
            at: t(9_000),
            stop: false,
        }),
    )];
    assert_eq!(s_of(&r, 0, 100).len(), 1);
}

#[test]
fn a_wakeup_armed_before_a_resume_is_lost_on_the_new_instance_t34() {
    let r = [rec(
        10,
        RecordKind::ToolUse(ToolUse::ScheduleWakeup {
            at: t(9_000),
            stop: false,
        }),
    )];
    let s = s_of(&r, 500, 600);
    assert_eq!(s, vec![SelfCont::LostOnNewInstance { at: t(9_000) }]);
    // ... unless CC re-armed it.
    let mut r2 = r.to_vec();
    r2.push(rec(
        510,
        RecordKind::WakeupRearmed {
            scheduled_for: t(9_000),
        },
    ));
    assert!(matches!(s_of(&r2, 500, 600)[0], SelfCont::Wakeup { .. }));
}

#[test]
fn loop_keepalive_rule_t3_t35() {
    // A loop turn that ended with no ScheduleWakeup infers a keepalive.
    let r = [
        open(0, TurnOrigin::Human),
        rec(
            100,
            RecordKind::TurnEnd {
                loop_turn: true,
                keepalive_fired: false,
            },
        ),
    ];
    let s = s_of(&r, 0, 200);
    assert_eq!(s.len(), 1);
    assert!(matches!(s[0], SelfCont::KeepaliveInferred { at, .. } if at == t(100 + 1_200_000)));
    // A keepalive-FIRED tick that ends without ScheduleWakeup arms nothing.
    let r = [
        open(0, TurnOrigin::Scheduled),
        rec(
            100,
            RecordKind::TurnEnd {
                loop_turn: true,
                keepalive_fired: true,
            },
        ),
    ];
    assert!(s_of(&r, 0, 200).is_empty());
    // A turn that rescheduled arms a wakeup, not a keepalive.
    let r = [
        open(0, TurnOrigin::Human),
        rec(
            50,
            RecordKind::ToolUse(ToolUse::ScheduleWakeup {
                at: t(5_000),
                stop: false,
            }),
        ),
        rec(
            100,
            RecordKind::TurnEnd {
                loop_turn: true,
                keepalive_fired: false,
            },
        ),
    ];
    let s = s_of(&r, 0, 200);
    assert_eq!(s.len(), 1);
    assert!(matches!(s[0], SelfCont::Wakeup { .. }));
    // ScheduleWakeup{stop:true} clears every loop entry.
    let r = [
        open(0, TurnOrigin::Human),
        rec(
            50,
            RecordKind::ToolUse(ToolUse::ScheduleWakeup {
                at: t(5_000),
                stop: false,
            }),
        ),
        rec(
            60,
            RecordKind::ToolUse(ToolUse::ScheduleWakeup {
                at: t(0),
                stop: true,
            }),
        ),
        rec(
            100,
            RecordKind::TurnEnd {
                loop_turn: true,
                keepalive_fired: false,
            },
        ),
        rec(
            110,
            RecordKind::ToolUse(ToolUse::ScheduleWakeup {
                at: t(0),
                stop: true,
            }),
        ),
    ];
    assert!(s_of(&r, 0, 200).is_empty());
}

#[test]
fn crons_expire_after_seven_days_unless_durable() {
    let create = |durable: bool| {
        rec(
            0,
            RecordKind::ToolUse(ToolUse::CronCreate {
                id: "c".into(),
                next: t(1_000),
                interval: Some(Dur::mins(5)),
                recurring: true,
                durable,
            }),
        )
    };
    let day = 86_400_000;
    assert_eq!(s_of(&[create(false)], 0, 6 * day).len(), 1);
    assert!(s_of(&[create(false)], 0, 8 * day).is_empty());
    assert_eq!(s_of(&[create(true)], 0, 8 * day).len(), 1);
    // CronDelete clears it.
    let r = [
        create(false),
        rec(
            5,
            RecordKind::ToolUse(ToolUse::CronDelete { id: "c".into() }),
        ),
    ];
    assert!(s_of(&r, 0, 100).is_empty());
    // Armed before a resume: gone unless resurrected.
    assert!(s_of(&[create(false)], 500, 600).is_empty());
    let r = [create(false), rec(510, RecordKind::CronsResurrected)];
    assert_eq!(s_of(&r, 500, 600).len(), 1);
    // A durable cron survives a restart on its own.
    assert_eq!(s_of(&[create(true)], 500, 600).len(), 1);
}

#[test]
fn an_unbalanced_enqueue_newer_than_the_process_is_a_queued_notice_t9() {
    let r = [rec(
        100,
        RecordKind::Queue {
            id: "q".into(),
            dequeue: false,
        },
    )];
    assert!(matches!(s_of(&r, 0, 200)[0], SelfCont::QueuedNotice { .. }));
    let r = [
        rec(
            100,
            RecordKind::Queue {
                id: "q".into(),
                dequeue: false,
            },
        ),
        rec(
            110,
            RecordKind::Queue {
                id: "q".into(),
                dequeue: true,
            },
        ),
    ];
    assert!(s_of(&r, 0, 200).is_empty());
    // Older than the process: not this instance's.
    let r = [rec(
        100,
        RecordKind::Queue {
            id: "q".into(),
            dequeue: false,
        },
    )];
    assert!(s_of(&r, 500, 600).is_empty());
}

#[test]
fn pending_messages_and_in_flight_deliveries_arm_s_t26() {
    let arr = Arrivals {
        pending: vec![PendingMessage {
            id: "m".into(),
            blocking: true,
            delivered: true,
            acked: false,
        }],
        in_flight: vec!["d".into()],
    };
    let SelfConts::Known(v) = selfconts_from(&[], true, live(0), &arr, None, t(1)) else {
        panic!()
    };
    assert!(v.contains(&SelfCont::PendingMessage {
        msg_id: "m".into(),
        blocking_unacked: true
    }));
    assert!(v.contains(&SelfCont::InFlightDelivery { msg_id: "d".into() }));
}

#[test]
fn an_unreadable_transcript_makes_s_and_p_unknown() {
    assert!(matches!(
        selfconts_from(&[], false, live(0), &Arrivals::default(), None, t(1)),
        SelfConts::Unknown(_)
    ));
    assert!(matches!(
        parked_from(&[], false, live(0), t(1)),
        Parked::Unknown(_)
    ));
}

fn limit(at: u64, resets: u64) -> TranscriptRecord {
    rec(
        at,
        RecordKind::QuotaLimit {
            resets_at: t(resets),
            limit_type: "seven_day".into(),
            account: "a".into(),
        },
    )
}

#[test]
fn a_limit_with_a_live_countdown_is_armed_and_without_one_withdrawn_t4_t33() {
    let r = [
        limit(100, 90 * MIN),
        rec(
            101,
            RecordKind::AutoContinueNotice {
                resets_at: t(90 * MIN),
            },
        ),
    ];
    assert!(matches!(
        parked_from(&r, true, live(0), t(200)),
        Parked::Limit {
            auto_continue: AutoContinue::Armed { .. },
            ..
        }
    ));
    // The arming instance is gone (a resume at 500): withdrawn.
    assert!(matches!(
        parked_from(&r, true, live(500), t(600)),
        Parked::Limit {
            auto_continue: AutoContinue::Withdrawn,
            ..
        }
    ));
    // Reset passed by > 10 min with no fire: withdrawn.
    assert!(matches!(
        parked_from(&r, true, live(0), t(90 * MIN + 11 * MIN)),
        Parked::Limit {
            auto_continue: AutoContinue::Withdrawn,
            ..
        }
    ));
    // No notice at all (reset > 24 h out, `11d283c5`): withdrawn.
    assert!(matches!(
        parked_from(&[limit(100, 90 * MIN)], true, live(0), t(200)),
        Parked::Limit {
            auto_continue: AutoContinue::Withdrawn,
            ..
        }
    ));
}

#[test]
fn a_real_turn_after_the_error_ends_parking() {
    let r = [rec(
        100,
        RecordKind::ApiError(ApiClass::Retryable(ApiErrorKind::Overloaded)),
    )];
    assert!(matches!(
        parked_from(&r, true, live(0), t(200)),
        Parked::ApiError { .. }
    ));
    let r = [
        rec(
            100,
            RecordKind::ApiError(ApiClass::Auth(AuthError::AuthenticationFailed)),
        ),
        open(150, TurnOrigin::Human),
    ];
    assert_eq!(parked_from(&r, true, live(0), t(200)), Parked::None);
}
