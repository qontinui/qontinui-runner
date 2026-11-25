use super::*;
use crate::model::{Budgets, ContinueCap, Dial, ItemKind, ItemState, Owner, SemVer, WakeBudget};
use crate::state::{
    ApiErrorKind, AuthError, ContinueRecord, ContinueSource, DialogKind, ExitInitiator, Hold,
    Instance, Latch, NextMoveIntervention, Observed, OwnedAxis, PaneWrite, TypedExit,
};
use crate::testkit::*;

fn d(s: &SessionState, items: &[WorkItemView]) -> Decision {
    decide(s, items, &dials(), X, NOW)
}

fn rule(dec: &Decision) -> RuleId {
    dec.rule
}

fn idle_unfinished(s: &mut SessionState) {
    s.work = Observed::new(Declared::Undeclared, NOW);
    s.coord_finished = Tri::False;
    s.activity = Observed::new(
        Activity::Idle {
            since_turn_end: Timestamp(NOW.0 - 20 * MIN),
            grace_from: Timestamp(NOW.0 - 20 * MIN),
        },
        NOW,
    );
}

fn exited(facts: ExitFacts) -> SessionState {
    let mut s = closable();
    s.hosting = Observed::new(Hosting::Exited(facts), NOW);
    s
}

fn facts() -> ExitFacts {
    ExitFacts {
        code: Some(0),
        initiator: ExitInitiator::Crash,
        door: None,
        typed: None,
        at: Timestamp(NOW.0 - MIN),
        creator: Principal::Session("creator".into()),
        writes: vec![],
        read_back_gone: true,
    }
}

fn unfinish(s: &mut SessionState) {
    s.work = Observed::new(Declared::Working, NOW);
    s.coord_finished = Tri::False;
}

// ---- D0 --------------------------------------------------------------------

#[test]
fn d0_an_unread_session_only_observes_and_reports_after_thirty_minutes() {
    let s = SessionState::unread("s", NOW);
    let dec = d(&s, &[]);
    assert_eq!(rule(&dec), RuleId::D0);
    assert!(matches!(dec.action, Action::Observe { .. }));
    let mut s2 = s.clone();
    s2.ledger
        .unknown_since
        .insert("hosting".into(), Timestamp(NOW.0 - 31 * MIN));
    let dec = d(&s2, &[]);
    assert!(matches!(
        dec.action,
        Action::Report {
            detection: Detection {
                kind: DetectionKind::UnknownPersisted { .. },
                ..
            }
        }
    ));
}

#[test]
fn d0_stale_inputs_and_schema_skew_withhold() {
    let mut s = closable();
    s.input = Observed::new(Input::Clear, Timestamp(NOW.0 - 10 * MIN));
    assert_eq!(rule(&d(&s, &[])), RuleId::D0);
    let mut s = closable();
    s.schema_skew = Some(Unk::new("schema", "newer"));
    assert_eq!(rule(&d(&s, &[])), RuleId::D0);
}

// ---- D5 - D7 ---------------------------------------------------------------

#[test]
fn d5_working_delegated_spawning_observe() {
    let mut s = closable();
    s.activity = Observed::new(Activity::Turn, NOW);
    assert_eq!(rule(&d(&s, &[])), RuleId::D5);
    s.activity = Observed::new(
        Activity::Delegated {
            agents: 1,
            workflows: 0,
        },
        NOW,
    );
    assert_eq!(rule(&d(&s, &[])), RuleId::D5);
    s.activity = Observed::new(
        Activity::ShellPinned {
            shells: 2,
            stray_shells: 1,
            monitors: 0,
            mcp_tasks: 0,
        },
        NOW,
    );
    assert_eq!(rule(&d(&s, &[])), RuleId::D5);
}

#[test]
fn d6_a_draft_is_never_closed_over_and_reports_only_a_stale_measured_one() {
    let mut s = closable();
    s.input = Observed::new(
        Input::Draft {
            bytes: 4,
            writer: Writer::OperatorKeystroke,
            since: NOW,
        },
        NOW,
    );
    let dec = d(&s, &[]);
    assert_eq!(rule(&dec), RuleId::D6);
    assert!(matches!(dec.action, Action::Observe { .. }));
    s.input = Observed::new(
        Input::Draft {
            bytes: 4,
            writer: Writer::OperatorKeystroke,
            since: Timestamp(NOW.0 - 25 * 60 * MIN),
        },
        NOW,
    );
    assert!(matches!(d(&s, &[]).action, Action::Report { .. }));
}

#[test]
fn d7_dialogs_are_never_auto_answered_and_a_credential_prompt_asks_once_per_account() {
    let mut s = closable();
    s.input = Observed::new(
        Input::Dialog {
            kind: DialogKind::Permission,
            since: NOW,
        },
        NOW,
    );
    let dec = d(&s, &[]);
    assert_eq!(rule(&dec), RuleId::D7);
    assert!(matches!(dec.action, Action::Observe { .. }));
    s.input = Observed::new(
        Input::Dialog {
            kind: DialogKind::Permission,
            since: Timestamp(NOW.0 - 31 * MIN),
        },
        NOW,
    );
    assert!(matches!(d(&s, &[]).action, Action::Report { .. }));
    s.input = Observed::new(
        Input::Dialog {
            kind: DialogKind::CredentialPrompt {
                account: "acct".into(),
                ui_location: "terminal".into(),
            },
            since: NOW,
        },
        NOW,
    );
    let dec = d(&s, &[]);
    let Action::NotifyOperator { task } = &dec.action else {
        panic!("{:?}", dec.action)
    };
    assert!(matches!(
        task.basis(),
        crate::model::Basis::Closed(ClosedListItem::CredentialOnlyHumanHolds { .. })
    ));
    // Deduped across its sessions.
    s.ledger.notices.insert("credential:acct".into(), NOW);
    assert!(matches!(d(&s, &[]).action, Action::Observe { .. }));
}

// ---- D8 - D12 --------------------------------------------------------------

fn limit(s: &mut SessionState, resets_in: u64, armed: bool) {
    s.parked = Observed::new(
        Parked::Limit {
            limit_type: "seven_day".into(),
            resets_at: Timestamp(NOW.0 + resets_in),
            account: "a".into(),
            auto_continue: if armed {
                AutoContinue::Armed {
                    armed_by: instance(),
                }
            } else {
                AutoContinue::Withdrawn
            },
        },
        NOW,
    );
}

#[test]
fn d8_an_armed_countdown_inside_t_mig_is_left_alone() {
    let mut s = closable();
    limit(&mut s, 30 * MIN, true);
    let dec = d(&s, &[]);
    assert_eq!(rule(&dec), RuleId::D8);
    assert!(matches!(
        dec.action,
        Action::Observe {
            why: ObserveWhy::ClaudeContinuesItself
        }
    ));
}

#[test]
fn d9_migrates_to_an_account_with_budget_while_migrations_remain() {
    let mut s = closable();
    limit(&mut s, 3 * 60 * MIN, true);
    let mut di = dials();
    di.budgets = Budgets::Known(vec!["b".into()]);
    let dec = decide(&s, &[], &di, X, NOW);
    assert_eq!(rule(&dec), RuleId::D9);
    assert!(matches!(&dec.action, Action::Migrate { to_account } if to_account == "b"));
    // The cap: three migrations in 24 h and the rule falls to D10.
    for i in 0..3 {
        s.ledger.migrations.push(Timestamp(NOW.0 - (i + 1) * MIN));
    }
    assert_eq!(rule(&decide(&s, &[], &di, X, NOW)), RuleId::D10);
}

#[test]
fn d10_no_budget_schedules_against_the_reset_and_reports_exhaustion() {
    let mut s = closable();
    limit(&mut s, 3 * 60 * MIN, false);
    let dec = d(&s, &[]);
    assert_eq!(rule(&dec), RuleId::D10);
    assert!(matches!(
        dec.action,
        Action::ResumeAt {
            via: ResumeVia::AtReset,
            ..
        }
    ));
    assert!(dec.caps.iter().any(|c| matches!(c, Cap::AlsoReport(_))));
    assert!(!dec.action.is_close());
}

#[test]
fn d10u_unknown_budget_is_never_full_and_never_closes() {
    let mut s = closable();
    limit(&mut s, 3 * 60 * MIN, false);
    let mut di = dials();
    di.budgets = Budgets::Unknown(Unk::new("budget", "roster unreadable"));
    let dec = decide(&s, &[], &di, X, NOW);
    assert_eq!(rule(&dec), RuleId::D10u);
    assert!(matches!(dec.action, Action::Observe { .. }));
    s.ledger
        .unknown_since
        .insert("budget".into(), Timestamp(NOW.0 - 31 * MIN));
    assert!(matches!(
        decide(&s, &[], &di, X, NOW).action,
        Action::Report { .. }
    ));
}

#[test]
fn d11_auth_error_migrates_and_asks_the_operator_once_per_account() {
    let mut s = closable();
    s.parked = Observed::new(
        Parked::Auth {
            error: AuthError::AuthenticationFailed,
            account: "a".into(),
        },
        NOW,
    );
    let mut di = dials();
    di.budgets = Budgets::Known(vec!["b".into()]);
    let dec = decide(&s, &[], &di, X, NOW);
    assert_eq!(rule(&dec), RuleId::D11);
    assert!(matches!(dec.action, Action::Migrate { .. }));
    assert!(dec.caps.iter().any(|c| matches!(c, Cap::AlsoNotify(_))));
    // No other account: the ask is the action, once.
    let dec = d(&s, &[]);
    assert!(matches!(dec.action, Action::NotifyOperator { .. }));
    s.ledger.notices.insert("credential:a".into(), NOW);
    assert!(matches!(d(&s, &[]).action, Action::Observe { .. }));
}

#[test]
fn d12_a_retryable_error_continues_once_then_reports() {
    let mut s = closable();
    s.parked = Observed::new(
        Parked::ApiError {
            kind: ApiErrorKind::Overloaded,
            retryable: true,
        },
        NOW,
    );
    let dec = d(&s, &[]);
    assert_eq!(rule(&dec), RuleId::D12);
    assert!(matches!(
        dec.action,
        Action::Continue {
            message: ContinueMsg::RetryAfterError { .. },
            ..
        }
    ));
    s.ledger.continues.push(ContinueRecord {
        at: Timestamp(NOW.0 - 20 * MIN),
        items: item_set_hash(&[RETRY_SET.to_string()]),
        source: ContinueSource::Lifecycle,
        new_fact_since: false,
    });
    assert!(matches!(d(&s, &[]).action, Action::Report { .. }));
    // An attended session is never nudged.
    let mut a = closable();
    a.mode = Mode::Attended;
    a.parked = s.parked.clone();
    assert!(matches!(d(&a, &[]).action, Action::Report { .. }));
}

// ---- D13 -------------------------------------------------------------------

fn with_s(s: &mut SessionState, entries: Vec<SelfCont>) {
    s.selfcont = Observed::new(SelfConts::Known(entries), NOW);
}

#[test]
fn d13_scheduled_observes_and_reports_overdue_never_closes() {
    let mut s = closable();
    unfinish(&mut s);
    with_s(
        &mut s,
        vec![SelfCont::Wakeup {
            at: Timestamp(NOW.0 + 30 * MIN),
            armed_by: instance(),
        }],
    );
    let dec = d(&s, &[]);
    assert_eq!(rule(&dec), RuleId::D13);
    assert!(matches!(dec.action, Action::Observe { .. }));
    with_s(
        &mut s,
        vec![SelfCont::Wakeup {
            at: Timestamp(NOW.0 - 11 * MIN),
            armed_by: instance(),
        }],
    );
    assert!(matches!(d(&s, &[]).action, Action::Report { .. }));
    with_s(
        &mut s,
        vec![SelfCont::KeepaliveInferred {
            at: Timestamp(NOW.0 - 11 * MIN),
            armed_by: instance(),
        }],
    );
    assert!(
        matches!(&d(&s, &[]).action, Action::Report { detection } if detection.kind == DetectionKind::KeepaliveUnconfirmed)
    );
}

#[test]
fn d13_a_lost_wake_continues_when_unattended_on_build_x_and_reports_otherwise() {
    let mut s = closable();
    unfinish(&mut s);
    with_s(
        &mut s,
        vec![SelfCont::LostOnNewInstance {
            at: Timestamp(NOW.0 - 11 * MIN),
        }],
    );
    assert!(matches!(
        d(&s, &[]).action,
        Action::Continue {
            message: ContinueMsg::LostWake { .. },
            ..
        }
    ));
    // Interim host: Report only.
    assert!(matches!(
        decide(&s, &[], &dials(), INTERIM, NOW).action,
        Action::Report { .. }
    ));
    s.mode = Mode::Attended;
    assert!(matches!(d(&s, &[]).action, Action::Report { .. }));
}

#[test]
fn d13_finished_yet_armed() {
    let mut s = closable();
    with_s(
        &mut s,
        vec![SelfCont::Cron {
            id: "c".into(),
            next: Timestamp(NOW.0 + MIN),
            kind: CronKind::Durable,
            armed_by: instance(),
        }],
    );
    assert!(matches!(
        d(&s, &[]).action,
        Action::Continue {
            message: ContinueMsg::FinishedYetArmed,
            ..
        }
    ));
    assert!(matches!(
        decide(&s, &[], &dials(), INTERIM, NOW).action,
        Action::Report { .. }
    ));
}

// ---- D1 / D14 / D15 --------------------------------------------------------

#[test]
fn d14_a_revival_withdraws_the_finish() {
    let mut s = closable();
    s.revived = Tri::True;
    let dec = d(&s, &[]);
    assert_eq!(rule(&dec), RuleId::D14);
    assert_eq!(dec.action, Action::WithdrawFinish);
}

#[test]
fn d15_owed_items_continue_an_unattended_session() {
    let mut s = closable();
    idle_unfinish_work(&mut s);
    let dec = d(&s, &[owed("pr-1")]);
    assert_eq!(rule(&dec), RuleId::D15);
    let Action::Continue {
        message: ContinueMsg::OwedItems { facts },
        lane,
    } = &dec.action
    else {
        panic!("{:?}", dec.action)
    };
    assert_eq!(facts, &["pr-1 is owed".to_string()]);
    assert_eq!(lane.lanes, vec![Lane::StopBlock, Lane::Pump, Lane::Paste]);
    // The interim host pastes.
    let dec = decide(&s, &[owed("pr-1")], &dials(), INTERIM, NOW);
    assert!(
        matches!(&dec.action, Action::Continue { lane, .. } if lane.lanes == vec![Lane::Paste])
    );
}

fn idle_unfinish_work(s: &mut SessionState) {
    idle_unfinished(s);
    s.work = Observed::new(Declared::Working, NOW);
}

#[test]
fn d15_a_finished_session_that_owes_is_withdrawn_first() {
    let s = closable();
    let dec = d(&s, &[owed("pr-1")]);
    assert_eq!(rule(&dec), RuleId::D15);
    assert_eq!(dec.action, Action::WithdrawFinish);
}

#[test]
fn d15_an_impeded_item_is_owed_and_an_unknown_one_triggers_nothing() {
    let mut s = closable();
    idle_unfinish_work(&mut s);
    let impeded = item(
        "e",
        ItemKind::EscalateHit,
        ItemState::Impeded {
            refusal: "reversal_gate_not_green".into(),
        },
    );
    assert!(matches!(d(&s, &[impeded]).action, Action::Continue { .. }));
    // An item whose state is Unknown fires nothing: the phase is not Owing.
    let unknown = item(
        "u",
        ItemKind::CiRed,
        ItemState::Unknown(Unk::new("c2", "x")),
    );
    let dec = d(&s, &[unknown]);
    assert_ne!(rule(&dec), RuleId::D15);
}

#[test]
fn d15_attended_gets_one_informational_continue_for_a_new_fact_about_its_own_prs() {
    let mut s = closable();
    idle_unfinish_work(&mut s);
    s.mode = Mode::Attended;
    let dec = d(&s, &[owed("pr-1")]);
    assert!(matches!(dec.action, Action::Continue { .. }));
    // Never on the interim host, and never to an unknown-mode session.
    assert!(!decide(&s, &[owed("pr-1")], &dials(), INTERIM, NOW)
        .action
        .is_continue());
    s.mode = Mode::Unknown;
    assert!(!d(&s, &[owed("pr-1")]).action.is_continue());
}

#[test]
fn d15_a_dirty_tree_is_owed_until_the_session_finishes_then_it_is_d18() {
    let dirty = CustodyAxis::Known(vec![(
        "/w".into(),
        Custody::Dirty {
            reason: "uncommitted: a".into(),
        },
    )]);
    let mut s = closable();
    idle_unfinish_work(&mut s);
    s.custody = Observed::new(dirty.clone(), NOW);
    assert_eq!(rule(&d(&s, &[])), RuleId::D15);
    let mut f = closable();
    f.custody = Observed::new(dirty, NOW);
    let dec = d(&f, &[]);
    assert_eq!(rule(&dec), RuleId::D18);
    assert!(matches!(
        dec.action,
        Action::Continue {
            message: ContinueMsg::DirtyTree { .. },
            ..
        }
    ));
}

#[test]
fn d15_a_blocking_unacked_message_is_owed() {
    let mut s = closable();
    idle_unfinish_work(&mut s);
    with_s(
        &mut s,
        vec![SelfCont::PendingMessage {
            msg_id: "m".into(),
            blocking_unacked: true,
        }],
    );
    // S holds a pending message, so the phase is Scheduled and nothing is sent
    // into a session that will receive it; the message itself is the delivery.
    assert_eq!(rule(&d(&s, &[])), RuleId::D13);
}

// ---- D16 - D19b ------------------------------------------------------------

#[test]
fn d16_undeclared_unattended_gets_a_closeout_request() {
    let mut s = closable();
    idle_unfinished(&mut s);
    let dec = d(&s, &[]);
    assert_eq!(rule(&dec), RuleId::D16);
    assert!(matches!(
        dec.action,
        Action::Continue {
            message: ContinueMsg::Closeout {
                reverify_first: false
            },
            ..
        }
    ));
    // Without the Stop hook it waits a 15-minute settle.
    let mut hostless = X;
    hostless.hooks = false;
    s.activity = Observed::new(
        Activity::Idle {
            since_turn_end: Timestamp(NOW.0 - 5 * MIN),
            grace_from: Timestamp(NOW.0 - 5 * MIN),
        },
        NOW,
    );
    assert!(matches!(
        decide(&s, &[], &dials(), hostless, NOW).action,
        Action::Observe {
            why: ObserveWhy::Backoff { .. }
        }
    ));
    // Idle over 24 h: re-verify against origin first.
    s.activity = Observed::new(
        Activity::Idle {
            since_turn_end: Timestamp(NOW.0 - 25 * 60 * MIN),
            grace_from: Timestamp(NOW.0 - 25 * 60 * MIN),
        },
        NOW,
    );
    assert!(matches!(
        d(&s, &[]).action,
        Action::Continue {
            message: ContinueMsg::Closeout {
                reverify_first: true
            },
            ..
        }
    ));
}

#[test]
fn d16_attended_only_after_24h_once_with_the_per_tab_switch() {
    let mut s = closable();
    idle_unfinished(&mut s);
    s.mode = Mode::Attended;
    assert!(!d(&s, &[]).action.is_continue());
    s.activity = Observed::new(
        Activity::Idle {
            since_turn_end: Timestamp(NOW.0 - 25 * 60 * MIN),
            grace_from: Timestamp(NOW.0 - 25 * 60 * MIN),
        },
        NOW,
    );
    let mut di = dials();
    di.per_tab_never_prompt = true;
    assert!(decide(&s, &[], &di, X, NOW).action.is_continue());
    assert!(!decide(&s, &[], &di, INTERIM, NOW).action.is_continue());
}

#[test]
fn d17_awaiting_first_verdict_observes() {
    let s = closable();
    let mut hold = item(
        "v",
        ItemKind::CiFirstVerdict,
        ItemState::Pending { until: NOW },
    );
    hold.inside_first_verdict_hold = true;
    let dec = d(&s, &[hold]);
    assert_eq!(rule(&dec), RuleId::D17);
    assert!(matches!(dec.action, Action::Observe { .. }));
}

#[test]
fn d19_unknown_custody_observes_and_reports_after_thirty_minutes() {
    let mut s = closable();
    s.custody = Observed::new(
        CustodyAxis::Known(vec![(
            "/w".into(),
            Custody::Unknown {
                reason: "shared_checkout".into(),
            },
        )]),
        NOW,
    );
    let dec = d(&s, &[]);
    assert_eq!(rule(&dec), RuleId::D19);
    assert!(matches!(dec.action, Action::Observe { .. }));
    s.ledger
        .unknown_since
        .insert("finished_unverified".into(), Timestamp(NOW.0 - 31 * MIN));
    assert!(matches!(d(&s, &[]).action, Action::Report { .. }));
}

#[test]
fn d19b_a_finished_session_pinned_by_strays_is_told_to_stop_them_and_never_closed() {
    let mut s = closable();
    s.activity = Observed::new(
        Activity::ShellPinned {
            shells: 2,
            stray_shells: 2,
            monitors: 0,
            mcp_tasks: 0,
        },
        NOW,
    );
    let dec = d(&s, &[]);
    assert_eq!(rule(&dec), RuleId::D19b);
    assert!(matches!(
        dec.action,
        Action::Continue {
            message: ContinueMsg::StrayWaits { shells: 2 },
            ..
        }
    ));
    // Interim host and attended sessions: Report only.
    assert!(matches!(
        decide(&s, &[], &dials(), INTERIM, NOW).action,
        Action::Report { .. }
    ));
    s.mode = Mode::Attended;
    assert!(matches!(d(&s, &[]).action, Action::Report { .. }));
}

// ---- D20 -------------------------------------------------------------------

#[test]
fn d20_a_cleared_finished_session_closes_after_the_grace_and_waits_inside_it() {
    let s = closable();
    let dec = d(&s, &[done("w")]);
    assert_eq!(rule(&dec), RuleId::D20);
    assert!(dec.action.is_close());
    assert!(dec.guards.iter().all(|g| g.value.is_true()));
    let mut early = closable();
    early.activity = Observed::new(
        Activity::Idle {
            since_turn_end: NOW,
            grace_from: Timestamp(NOW.0 - 5 * MIN),
        },
        NOW,
    );
    let dec = d(&early, &[done("w")]);
    assert!(
        matches!(dec.action, Action::Observe { why: ObserveWhy::GraceRunning { until } } if until == Timestamp(NOW.0 + 5 * MIN))
    );
}

#[test]
fn d20_a_refused_close_observes_then_reports_after_thirty_minutes() {
    let mut s = closable();
    s.coord_finished = Tri::unknown("coord_row", "unreadable");
    let dec = d(&s, &[done("w")]);
    assert!(matches!(
        dec.action,
        Action::Observe {
            why: ObserveWhy::Gated(_)
        }
    ));
    s.ledger
        .unknown_since
        .insert("close_not_cleared".into(), Timestamp(NOW.0 - 31 * MIN));
    assert!(matches!(d(&s, &[done("w")]).action, Action::Report { .. }));
}

#[test]
fn the_dials_gate_the_close() {
    let s = closable();
    let mut di = dials();
    di.close = Dial::Off;
    assert!(!decide(&s, &[done("w")], &di, X, NOW).action.is_close());
    let mut di = dials();
    di.latch = Latch::Tripped;
    assert!(!decide(&s, &[done("w")], &di, X, NOW).action.is_close());
}

// ---- D21 and budgets -------------------------------------------------------

fn rec(at: u64, ids: &[&str], new_fact: bool) -> ContinueRecord {
    ContinueRecord {
        at: Timestamp(at),
        items: item_set_hash(&ids.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
        source: ContinueSource::Lifecycle,
        new_fact_since: new_fact,
    }
}

#[test]
fn the_ten_minute_budget_holds_an_unchanged_set_and_a_changed_set_is_a_new_fact() {
    let mut s = closable();
    idle_unfinish_work(&mut s);
    s.ledger
        .continues
        .push(rec(NOW.0 - 3 * MIN, &["pr-1"], false));
    assert!(matches!(
        d(&s, &[owed("pr-1")]).action,
        Action::Observe {
            why: ObserveWhy::Backoff { .. }
        }
    ));
    assert!(d(&s, &[owed("pr-1"), owed("pr-2")]).action.is_continue());
    // Past the window the unchanged set may go out once more.
    s.ledger.continues[0].at = Timestamp(NOW.0 - 11 * MIN);
    assert!(d(&s, &[owed("pr-1")]).action.is_continue());
}

#[test]
fn d21_two_unproductive_continues_stop_the_continuing_and_stall_an_unattended_session() {
    let mut s = closable();
    idle_unfinish_work(&mut s);
    s.ledger
        .continues
        .push(rec(NOW.0 - 60 * MIN, &["pr-1"], false));
    s.ledger
        .continues
        .push(rec(NOW.0 - 30 * MIN, &["pr-1"], false));
    let dec = d(&s, &[owed("pr-1")]);
    assert_eq!(rule(&dec), RuleId::D21);
    assert!(
        matches!(&dec.action, Action::Report { detection } if detection.kind == DetectionKind::ContinueExhausted)
    );
    assert!(dec.caps.contains(&Cap::MarkStalled));
    // A new fact in between resets it.
    s.ledger.continues[1].new_fact_since = true;
    assert!(d(&s, &[owed("pr-1")]).action.is_continue());
}

#[test]
fn d21_three_blocks_in_an_hour_and_a_dispute_hand_the_items_on() {
    let mut s = closable();
    idle_unfinish_work(&mut s);
    let set = item_set_hash(&["pr-1".to_string()]);
    for i in 0..3 {
        s.ledger.blocks.push(crate::state::BlockRecord {
            at: Timestamp(NOW.0 - (i + 1) * 12 * MIN),
            items: set,
        });
    }
    assert_eq!(rule(&d(&s, &[owed("pr-1")])), RuleId::D21);
    let mut s = closable();
    idle_unfinish_work(&mut s);
    s.ledger.disputes.push("pr-1".into());
    let dec = d(&s, &[owed("pr-1")]);
    assert_eq!(rule(&dec), RuleId::D21);
    assert!(dec
        .caps
        .iter()
        .any(|c| matches!(c, Cap::RequeueDisputed { .. })));
}

#[test]
fn d21_never_writes_a_finished_session_stalled() {
    let mut s = closable();
    s.ledger.disputes.push("pr-1".into());
    // Owing + Finished: the disputed items go back to the ledger; no stall.
    let dec = d(&s, &[owed("pr-1")]);
    assert_eq!(rule(&dec), RuleId::D21);
    assert!(!dec.caps.contains(&Cap::MarkStalled));
}

#[test]
fn next_move_nudges_count_against_the_continue_budget_but_its_finish_does_not() {
    let mut s = closable();
    idle_unfinish_work(&mut s);
    s.ledger.next_move.push(NextMoveIntervention {
        at: Timestamp(NOW.0 - 2 * MIN),
        kind: NextMoveKind::Nudge,
    });
    assert!(!d(&s, &[owed("pr-1")]).action.is_continue());
    s.ledger.next_move[0].kind = NextMoveKind::Redirect;
    assert!(!d(&s, &[owed("pr-1")]).action.is_continue());
    s.ledger.next_move[0].kind = NextMoveKind::FinishE;
    assert!(d(&s, &[owed("pr-1")]).action.is_continue());
}

#[test]
fn the_wake_budget_defers_without_starving_and_k_never_drops_below_one() {
    let mut s = closable();
    idle_unfinish_work(&mut s);
    let mut di = dials();
    di.wake.used_this_tick = 4;
    assert!(matches!(
        decide(&s, &[owed("p")], &di, X, NOW).action,
        Action::Observe {
            why: ObserveWhy::BudgetExhausted
        }
    ));
    di.wake = WakeBudget {
        headroom: Some(0),
        used_this_tick: 0,
    };
    assert_eq!(di.wake.k(), 1);
    assert!(decide(&s, &[owed("p")], &di, X, NOW).action.is_continue());
    di.wake.used_this_tick = 1;
    assert!(!decide(&s, &[owed("p")], &di, X, NOW).action.is_continue());
    assert_eq!(
        WakeBudget {
            headroom: None,
            used_this_tick: 0
        }
        .k(),
        4
    );
}

#[test]
fn the_continue_dial_and_the_host_gate_every_continue() {
    let mut s = closable();
    idle_unfinish_work(&mut s);
    let mut di = dials();
    di.cont = Dial::Off;
    assert!(!decide(&s, &[owed("p")], &di, X, NOW).action.is_continue());
    let mut h = X;
    h.cont = ContinueCap::None;
    assert!(!decide(&s, &[owed("p")], &dials(), h, NOW)
        .action
        .is_continue());
}

// ---- D2 - D4 ---------------------------------------------------------------

#[test]
fn d2_the_lifecycles_own_close_is_reported_and_a_cut_revival_is_resumed_d1() {
    let mut f = facts();
    f.door = Some(CloseDoor::LifecycleClose);
    let s = exited(f.clone());
    let dec = d(&s, &[]);
    assert_eq!(rule(&dec), RuleId::D2);
    assert!(
        matches!(&dec.action, Action::Report { detection } if detection.kind == DetectionKind::Closed)
    );
    let mut s = exited(f);
    s.revived = Tri::True;
    let dec = d(&s, &[]);
    assert_eq!(rule(&dec), RuleId::D1);
    assert!(matches!(
        dec.action,
        Action::ResumeAt {
            via: ResumeVia::WithMessage,
            ..
        }
    ));
}

#[test]
fn d3_requires_positive_operator_provenance() {
    let mk = |door: Option<CloseDoor>, typed: Option<TypedExit>| {
        let mut f = facts();
        f.door = door;
        f.typed = typed;
        let mut s = exited(f);
        unfinish(&mut s);
        d(&s, &[])
    };
    for (door, typed) in [
        (Some(CloseDoor::OperatorTab), None),
        (
            Some(CloseDoor::RelayUserSession {
                remote_block: false,
            }),
            None,
        ),
        (None, Some(TypedExit::OperatorKeystroke)),
        (
            None,
            Some(TypedExit::RelayedUserSession {
                remote_block: false,
            }),
        ),
    ] {
        let dec = mk(door.clone(), typed.clone());
        assert_eq!(rule(&dec), RuleId::D3, "{door:?} {typed:?}");
        assert!(
            matches!(&dec.action, Action::Report { detection } if detection.kind == DetectionKind::ClosedByOperator)
        );
    }
    // Nothing a device credential can produce counts, and nothing unstamped.
    for (door, typed) in [
        (Some(CloseDoor::RelayDevice), None),
        (
            Some(CloseDoor::RelayUserSession { remote_block: true }),
            None,
        ),
        (Some(CloseDoor::Webview), None),
        (
            Some(CloseDoor::Agent {
                closer: Principal::Unresolvable,
            }),
            None,
        ),
        (Some(CloseDoor::RunnerExit), None),
        (None, None),
        (None, Some(TypedExit::Other)),
        (None, Some(TypedExit::RelayedUnstamped)),
    ] {
        assert_ne!(
            rule(&mk(door.clone(), typed.clone())),
            RuleId::D3,
            "{door:?} {typed:?}"
        );
    }
}

#[test]
fn d4_a_crash_is_resumed_when_resumable_and_the_interim_host_only_reports() {
    let mut s = exited(facts());
    unfinish(&mut s);
    let dec = d(&s, &[]);
    assert_eq!(rule(&dec), RuleId::D4);
    assert!(matches!(dec.action, Action::ResumeAt { .. }));
    assert!(matches!(
        decide(&s, &[], &dials(), INTERIM, NOW).action,
        Action::Report { .. }
    ));
    // Unknown W is not a finish: still resumed.
    s.work = Observed::new(Declared::Unknown(Unk::new("d", "x")), NOW);
    assert!(matches!(d(&s, &[]).action, Action::ResumeAt { .. }));
}

#[test]
fn d4_resume_standing_conditions() {
    let mut s = exited(facts());
    unfinish(&mut s);
    let mut di = dials();
    di.resume_unfinished = Dial::Off;
    assert!(matches!(
        decide(&s, &[], &di, X, NOW).action,
        Action::Report { .. }
    ));
    let mut di = dials();
    di.resume_account_has_budget = Tri::unknown("budget", "x");
    assert!(matches!(
        decide(&s, &[], &di, X, NOW).action,
        Action::Report { .. }
    ));
    for i in 0..3 {
        s.ledger
            .resume_attempts
            .push(Timestamp(NOW.0 - (i + 1) * MIN));
    }
    assert!(
        matches!(&d(&s, &[]).action, Action::Report { detection } if detection.kind == DetectionKind::ResumeItem)
    );
    let mut s = exited(facts());
    unfinish(&mut s);
    s.operator_hold = true;
    assert!(matches!(d(&s, &[]).action, Action::Report { .. }));
    let mut s = exited(facts());
    unfinish(&mut s);
    s.pane_kind = PaneKind::Steward;
    assert!(matches!(d(&s, &[]).action, Action::Report { .. }));
}

#[test]
fn d4_a_finished_exit_resumes_nothing() {
    let s = exited(facts());
    let dec = d(&s, &[]);
    assert!(
        matches!(&dec.action, Action::Report { detection } if detection.kind == DetectionKind::ExitedFinished)
    );
}

#[test]
fn d4_closed_by_creator_needs_the_creator_alone() {
    let creator = Principal::Session("creator".into());
    let mut f = facts();
    f.door = Some(CloseDoor::Agent {
        closer: creator.clone(),
    });
    f.writes = vec![
        PaneWrite {
            writer: Writer::HttpCreateInitialCommand,
            at: NOW,
        },
        PaneWrite {
            writer: Writer::ControlResponse,
            at: NOW,
        },
        PaneWrite {
            writer: Writer::AgentDoor(creator.clone()),
            at: NOW,
        },
    ];
    assert_eq!(closed_by_creator(&f), Tri::True);
    let mut s = exited(f.clone());
    unfinish(&mut s);
    assert!(
        matches!(&d(&s, &[]).action, Action::Report { detection } if detection.kind == DetectionKind::ClosedByCreator)
    );
    // Someone else wrote: it stays D4.
    for w in [
        Writer::OperatorKeystroke,
        Writer::RelayedInput,
        Writer::LifecycleDelivery,
        Writer::AgentDoor(Principal::Session("other".into())),
    ] {
        let mut g = f.clone();
        g.writes.push(PaneWrite { writer: w, at: NOW });
        assert_eq!(closed_by_creator(&g), Tri::False);
    }
    // An unresolvable closer, creator or writer is UNKNOWN and stays D4.
    let mut g = f.clone();
    g.door = Some(CloseDoor::Agent {
        closer: Principal::Unresolvable,
    });
    assert!(closed_by_creator(&g).is_unknown());
    let mut g = f.clone();
    g.creator = Principal::Unresolvable;
    assert!(closed_by_creator(&g).is_unknown());
    let mut g = f.clone();
    g.writes.push(PaneWrite {
        writer: Writer::AgentDoor(Principal::Unresolvable),
        at: NOW,
    });
    assert!(closed_by_creator(&g).is_unknown());
    let mut g = f;
    g.door = Some(CloseDoor::Webview);
    assert_eq!(closed_by_creator(&g), Tri::False);
}

// ---- The hold --------------------------------------------------------------

fn relayed() -> SessionState {
    let mut f = facts();
    f.typed = Some(TypedExit::RelayedUnstamped);
    let mut s = exited(f);
    unfinish(&mut s);
    s.exit_schedules = Some(SelfConts::Known(vec![]));
    s
}

#[test]
fn a_relayed_exit_with_no_restorable_schedule_is_resumed_bare_and_held() {
    let s = relayed();
    let dec = d(&s, &[]);
    assert_eq!(rule(&dec), RuleId::D4);
    assert!(matches!(
        dec.action,
        Action::ResumeAt {
            via: ResumeVia::BareResume,
            ..
        }
    ));
    assert!(dec.caps.contains(&Cap::Hold { resumed: true }));
    assert!(matches!(
        dec.notices[..],
        [Notice::HoldStart { resumed: true, .. }]
    ));
}

#[test]
fn a_relayed_exit_whose_resume_would_bring_back_a_schedule_stays_down() {
    let at = Timestamp(NOW.0 - MIN);
    for entry in [
        SelfCont::Wakeup {
            at: Timestamp(NOW.0 + 5 * MIN),
            armed_by: instance(),
        },
        SelfCont::Cron {
            id: "c".into(),
            next: NOW,
            kind: CronKind::Recurring {
                created: Timestamp(NOW.0 - 60 * MIN),
            },
            armed_by: instance(),
        },
        SelfCont::Cron {
            id: "c".into(),
            next: NOW,
            kind: CronKind::OneShot {
                fire: Timestamp(NOW.0 + MIN),
            },
            armed_by: instance(),
        },
        SelfCont::Cron {
            id: "c".into(),
            next: NOW,
            kind: CronKind::Durable,
            armed_by: instance(),
        },
        SelfCont::KeepaliveInferred {
            at: NOW,
            armed_by: instance(),
        },
    ] {
        let mut s = relayed();
        s.exit_schedules = Some(SelfConts::Known(vec![entry.clone()]));
        let dec = d(&s, &[]);
        assert!(matches!(dec.action, Action::Report { .. }), "{entry:?}");
        assert!(dec.caps.contains(&Cap::Hold { resumed: false }));
        let _ = at;
    }
    // An unreadable S keeps it down; an expired recurring cron and a past
    // one-shot do not.
    let mut s = relayed();
    s.exit_schedules = Some(SelfConts::Unknown(Unk::new("s", "x")));
    assert!(matches!(d(&s, &[]).action, Action::Report { .. }));
    let mut s = relayed();
    s.exit_schedules = None;
    assert!(matches!(d(&s, &[]).action, Action::Report { .. }));
    let mut s = relayed();
    s.exit_schedules = Some(SelfConts::Known(vec![SelfCont::Cron {
        id: "d".into(),
        next: NOW,
        kind: CronKind::OneShot {
            fire: Timestamp(NOW.0 - 5 * MIN),
        },
        armed_by: instance(),
    }]));
    assert!(matches!(
        d(&s, &[]).action,
        Action::ResumeAt {
            via: ResumeVia::BareResume,
            ..
        }
    ));
}

#[test]
fn the_interim_host_never_resumes_a_relayed_exit_but_still_holds() {
    let s = relayed();
    let dec = decide(&s, &[], &dials(), INTERIM, NOW);
    assert!(matches!(dec.action, Action::Report { .. }));
    assert!(dec.caps.contains(&Cap::Hold { resumed: false }));
}

fn held_live() -> SessionState {
    let mut s = closable();
    idle_unfinish_work(&mut s);
    s.hold = Some(Hold {
        since: Timestamp(NOW.0 - 60 * MIN),
        resumed: true,
        last_notice: Some(Timestamp(NOW.0 - 60 * MIN)),
    });
    s
}

#[test]
fn a_held_session_gets_no_continue_of_any_rule() {
    // D15, D16, D12, D13 (lost wake), D18, D19b.
    let mut cases: Vec<(SessionState, Vec<WorkItemView>)> = Vec::new();
    cases.push((held_live(), vec![owed("pr-1")]));
    cases.push((held_live(), vec![]));
    let mut p = held_live();
    p.parked = Observed::new(
        Parked::ApiError {
            kind: ApiErrorKind::ServerError,
            retryable: true,
        },
        NOW,
    );
    cases.push((p, vec![]));
    let mut w = held_live();
    with_s(
        &mut w,
        vec![SelfCont::LostOnNewInstance {
            at: Timestamp(NOW.0 - 20 * MIN),
        }],
    );
    cases.push((w, vec![]));
    let mut dt = held_live();
    dt.work = Observed::new(
        Declared::Finished {
            at: NOW,
            kind: crate::state::FinishKind::Done,
            items: vec![],
        },
        NOW,
    );
    dt.custody = Observed::new(
        CustodyAxis::Known(vec![("/w".into(), Custody::Dirty { reason: "x".into() })]),
        NOW,
    );
    cases.push((dt, vec![]));
    let mut pin = held_live();
    pin.work = Observed::new(
        Declared::Finished {
            at: NOW,
            kind: crate::state::FinishKind::Done,
            items: vec![],
        },
        NOW,
    );
    pin.activity = Observed::new(
        Activity::ShellPinned {
            shells: 1,
            stray_shells: 1,
            monitors: 0,
            mcp_tasks: 0,
        },
        NOW,
    );
    cases.push((pin, vec![]));
    let mut dis = held_live();
    dis.ledger.disputes.push("pr-1".into());
    cases.push((dis, vec![owed("pr-1")]));
    for (s, items) in &cases {
        let dec = d(s, items);
        assert!(!dec.action.is_continue(), "{:?}", dec);
        assert!(dec.caps.iter().any(|c| matches!(c, Cap::Hold { .. })));
        assert!(!dec.caps.contains(&Cap::MarkStalled));
    }
}

#[test]
fn a_held_session_is_noticed_again_only_when_it_newly_owes_and_once_per_day() {
    let mut s = held_live();
    let dec = d(&s, &[]);
    assert!(dec.notices.is_empty(), "owes nothing: no notice");
    let dec = d(&s, &[owed("pr-1")]);
    // Notice interval not elapsed (60 min < 24 h).
    assert!(dec.notices.is_empty());
    s.hold.as_mut().unwrap().last_notice = Some(Timestamp(NOW.0 - 25 * 60 * MIN));
    let dec = d(&s, &[owed("pr-1")]);
    assert!(matches!(dec.notices[..], [Notice::HoldNewlyOwes { .. }]));
    // The same owed set already noticed: nothing new.
    let key = format!("hold-owed:{}", item_set_hash(&["pr-1".to_string()]));
    s.ledger.notices.insert(key, NOW);
    assert!(d(&s, &[owed("pr-1")]).notices.is_empty());
}

#[test]
fn a_held_session_owing_a_red_stays_with_it_no_route_takes_it() {
    let s = held_live();
    let red = item(
        "red",
        ItemKind::CiRed,
        ItemState::Open {
            owner: Owner::AuthorSession,
            deadline: NOW,
        },
    );
    let dec = d(&s, &[red]);
    assert_eq!(rule(&dec), RuleId::D15);
    assert!(
        matches!(&dec.action, Action::Report { detection } if detection.kind == DetectionKind::HeldAfterRelayedExit)
    );
}

// ---- #2373 -----------------------------------------------------------------

#[test]
fn a_restored_continuations_exit_is_unknown_or_closed_never_crashed_or_clean() {
    let mut f = facts();
    f.code = None;
    f.initiator = ExitInitiator::Runner;
    let mut s = exited(f.clone());
    unfinish(&mut s);
    s.continuation_gate = Some("gate-1".into());
    s.shell_hosted = true;
    let dec = d(&s, &[]);
    assert!(dec
        .caps
        .contains(&Cap::ContinuationOutcome(ContinuationOutcome::Unknown)));
    f.door = Some(CloseDoor::OperatorTab);
    let mut s = exited(f);
    unfinish(&mut s);
    s.continuation_gate = Some("gate-1".into());
    s.shell_hosted = true;
    assert!(d(&s, &[])
        .caps
        .contains(&Cap::ContinuationOutcome(ContinuationOutcome::Closed)));
    // A session that is no continuation carries no outcome.
    let mut s = exited(facts());
    unfinish(&mut s);
    assert!(!d(&s, &[])
        .caps
        .iter()
        .any(|c| matches!(c, Cap::ContinuationOutcome(_))));
}

// ---- Operator lane (S3) ----------------------------------------------------

#[test]
fn s3_notify_operator_comes_only_from_a_typed_basis() {
    let t = OperatorTask::new(
        ReservedAct::RunningRunnerRestart {
            device: "d".into(),
            build: "sha".into(),
        },
        "restart".into(),
        vec![],
        vec!["production-and-cost runner-lifecycle".into()],
        "no clause lets an agent".into(),
        "finding".into(),
        "approved".into(),
    );
    assert!(matches!(t.basis(), crate::model::Basis::Reserved(_)));
    // Every NotifyOperator the kernel produces has such a basis. Sweep a grid.
    for dec in sweep() {
        if let Action::NotifyOperator { task } = &dec.action {
            assert!(matches!(
                task.basis(),
                crate::model::Basis::Closed(_) | crate::model::Basis::Reserved(_)
            ));
        }
    }
}

use crate::model::ReservedAct;

// ---- Invariants ------------------------------------------------------------

/// A grid of states: each base crossed with each axis turned Unknown, and
/// a few host / dial / mode variations.
fn bases() -> Vec<(SessionState, Vec<WorkItemView>)> {
    let mut v: Vec<(SessionState, Vec<WorkItemView>)> = Vec::new();
    v.push((closable(), vec![done("w")]));
    let mut working = closable();
    working.activity = Observed::new(Activity::Turn, NOW);
    v.push((working, vec![]));
    let mut o = closable();
    idle_unfinish_work(&mut o);
    v.push((o.clone(), vec![owed("a")]));
    v.push((o, vec![]));
    let mut f = closable();
    f.revived = Tri::True;
    v.push((f, vec![]));
    let mut l = closable();
    limit(&mut l, 3 * 60 * MIN, true);
    v.push((l, vec![]));
    let mut soon = closable();
    limit(&mut soon, 30 * MIN, true);
    v.push((soon, vec![]));
    let mut e = closable();
    e.parked = Observed::new(
        Parked::ApiError {
            kind: ApiErrorKind::Overloaded,
            retryable: true,
        },
        NOW,
    );
    v.push((e, vec![]));
    let mut r = relayed();
    r.hold = Some(Hold {
        since: NOW,
        resumed: true,
        last_notice: None,
    });
    v.push((r, vec![]));
    let mut x = exited(facts());
    unfinish(&mut x);
    v.push((x, vec![]));
    let mut dirty = closable();
    dirty.custody = Observed::new(
        CustodyAxis::Known(vec![("/w".into(), Custody::Dirty { reason: "x".into() })]),
        NOW,
    );
    v.push((dirty, vec![]));
    v
}

fn unknownify(s: &SessionState, axis: usize) -> SessionState {
    let u = || Unk::new("probe", "forced unknown");
    let mut s = s.clone();
    match axis {
        0 => s.hosting = Observed::new(Hosting::Unknown(u()), NOW),
        1 => s.activity = Observed::new(Activity::Unknown(u()), NOW),
        2 => s.selfcont = Observed::new(SelfConts::Unknown(u()), NOW),
        3 => s.parked = Observed::new(Parked::Unknown(u()), NOW),
        4 => s.input = Observed::new(Input::Unknown(u()), NOW),
        5 => s.work = Observed::new(Declared::Unknown(u()), NOW),
        6 => s.owned = Observed::new(OwnedAxis::Unknown(u()), NOW),
        7 => s.custody = Observed::new(CustodyAxis::Unknown(u()), NOW),
        8 => s.revived = Tri::Unknown(u()),
        9 => s.coord_finished = Tri::Unknown(u()),
        _ => s.schema_skew = Some(u()),
    }
    s
}

const AXES: usize = 11;

fn sweep() -> Vec<Decision> {
    let mut out = Vec::new();
    for (s, items) in bases() {
        out.push(d(&s, &items));
        for a in 0..AXES {
            let u = unknownify(&s, a);
            out.push(decide(&u, &items, &dials(), X, NOW));
            out.push(decide(&u, &items, &dials(), INTERIM, NOW));
        }
    }
    out
}

#[test]
fn s2_an_unknown_axis_never_closes_withdraws_migrates_or_continues_into_unknown_input() {
    for (s, items) in bases() {
        for a in 0..AXES {
            let u = unknownify(&s, a);
            for host in [X, INTERIM] {
                let dec = decide(&u, &items, &dials(), host, NOW);
                // Exited hosting needs no live axis: the unknown axes that the
                // closed path does not read leave its (resume / report) answer
                // alone; none of the forbidden actions are reachable from it.
                assert!(!dec.action.is_close(), "axis {a}: {dec:?}");
                if matches!(a, 0..=4 | 10) {
                    // Needed axes: the verdict withholds or is a typed Report.
                    assert!(
                        !matches!(
                            dec.action,
                            Action::WithdrawFinish
                                | Action::Migrate { .. }
                                | Action::Continue { .. }
                        ),
                        "axis {a}: {dec:?}"
                    );
                }
                if a == 4 {
                    assert!(!dec.action.is_continue());
                }
                if a == 8 || a == 9 || a == 6 || a == 7 {
                    assert!(!dec.action.is_close(), "axis {a}");
                }
            }
        }
    }
}

#[test]
fn monotone_in_safety_unknown_conjuncts_only_move_toward_observe_or_report() {
    let (s, items) = (closable(), vec![done("w")]);
    assert!(d(&s, &items).action.is_close());
    for a in 0..AXES {
        let dec = decide(&unknownify(&s, a), &items, &dials(), X, NOW);
        assert!(
            matches!(dec.action, Action::Observe { .. } | Action::Report { .. }),
            "axis {a}: {:?}",
            dec.action
        );
    }
}

#[test]
fn s5_capacity_only_moves_a_closes_timing_never_creates_one() {
    for (s, items) in bases() {
        let base = decide(&s, &items, &dials(), X, NOW);
        for ov in [
            Some(Dur::mins(2)),
            Some(Dur::mins(5)),
            Some(Dur::ms(1)),
            Some(Dur::hours(5)),
        ] {
            let mut di = dials();
            di.grace_override = ov;
            let dec = decide(&s, &items, &di, X, NOW);
            if dec.action.is_close() {
                assert!(
                    base.action.is_close()
                        || matches!(
                            base.action,
                            Action::Observe {
                                why: ObserveWhy::GraceRunning { .. }
                            }
                        ),
                    "capacity created a close: {base:?} -> {dec:?}"
                );
            } else {
                assert_eq!(
                    std::mem::discriminant(&dec.action),
                    std::mem::discriminant(&base.action),
                    "capacity changed a non-close action"
                );
                assert_eq!(dec.rule, base.rule);
            }
        }
    }
    // Capacity never shortens a first-verdict hold.
    let s = closable();
    let mut hold = item(
        "v",
        ItemKind::CiFirstVerdict,
        ItemState::Pending { until: NOW },
    );
    hold.inside_first_verdict_hold = true;
    let mut di = dials();
    di.grace_override = Some(Dur::mins(2));
    assert!(!decide(&s, &[hold], &di, X, NOW).action.is_close());
}

#[test]
fn l2_a_held_session_never_receives_a_continue_whatever_its_phase() {
    for (s, items) in bases() {
        let mut held = s.clone();
        held.hold = Some(Hold {
            since: NOW,
            resumed: true,
            last_notice: Some(NOW),
        });
        for a in 0..AXES {
            let u = unknownify(&held, a);
            let dec = decide(&u, &items, &dials(), X, NOW);
            assert!(!dec.action.is_continue(), "axis {a}: {dec:?}");
        }
        let dec = decide(&held, &items, &dials(), X, NOW);
        assert!(!dec.action.is_continue(), "{dec:?}");
    }
}

#[test]
fn a_recurring_cron_over_seven_days_old_is_not_restored_by_a_resume() {
    let at = Timestamp(10 * 24 * 60 * MIN);
    let cron = |created: u64| {
        SelfConts::Known(vec![SelfCont::Cron {
            id: "c".into(),
            next: at,
            kind: CronKind::Recurring {
                created: Timestamp(created),
            },
            armed_by: instance(),
        }])
    };
    assert_eq!(
        resume_would_restore_schedule(Some(&cron(0)), at),
        Tri::False
    );
    assert_eq!(
        resume_would_restore_schedule(Some(&cron(5 * 24 * 60 * MIN)), at),
        Tri::True
    );
}

#[test]
fn deterministic_and_version_stamped() {
    for (s, items) in bases() {
        let a = d(&s, &items);
        let b = d(&s, &items);
        assert_eq!(a, b);
        assert_eq!(
            a.fn_version,
            SemVer {
                major: 0,
                minor: 1,
                patch: 0
            }
        );
    }
    // A different input changes the digest.
    let s = closable();
    let a = d(&s, &[done("w")]);
    let b = d(&s, &[done("w2")]);
    assert_ne!(a.inputs_digest, b.inputs_digest);
}

#[test]
fn a_close_only_ever_has_every_guard_true() {
    for dec in sweep() {
        if dec.action.is_close() {
            assert!(dec.guards.iter().all(|g| g.value.is_true()), "{dec:?}");
        }
    }
}

#[test]
fn phases_are_covered_by_some_rule() {
    // Every rule id the table names is produced by some state in the suite's
    // grid or its named tests; this guards the grid's breadth, not the table.
    let seen: std::collections::BTreeSet<RuleId> = sweep().iter().map(|d| d.rule).collect();
    for r in [
        RuleId::D0,
        RuleId::D4,
        RuleId::D5,
        RuleId::D8,
        RuleId::D12,
        RuleId::D14,
        RuleId::D15,
        RuleId::D20,
    ] {
        assert!(seen.contains(&r), "{r:?} unreached by the grid: {seen:?}");
    }
    let _ = Instance {
        pid: 0,
        proc_start: 0,
    };
}
