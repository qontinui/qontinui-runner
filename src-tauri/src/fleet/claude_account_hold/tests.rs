//! Tests of the account hold: the table ([`decide_hold_action`]) row by row, the
//! wire parsing, the memory's answer handling, and the tick driven against a fake
//! coord over HTTP — the arming test enters through [`account_hold_tick`], the
//! tick body `run_account_hold_loop` runs on every interval. The loop around it
//! (the boot-read wait, the ticker, the per-tick timeout) and the `main.rs`
//! spawn are not driven here; the spawn is pinned by `worker_supervisor`'s
//! source-text test.

use super::*;
use chrono::TimeZone;

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 4, 12, 0, 0).unwrap()
}

fn own_reason() -> String {
    format!("restart-hold:{PREPARE_ID}: {HOLD_REASON}")
}

fn own_lane(until: DateTime<Utc>) -> LaneView {
    LaneView::Drained {
        until: Some(until),
        reason: Some(own_reason()),
    }
}

fn other_lane(until: DateTime<Utc>) -> LaneView {
    LaneView::Drained {
        until: Some(until),
        reason: Some("operator: disk replacement".to_string()),
    }
}

fn read(lane: LaneView) -> DrainView {
    DrainView::Read {
        lane,
        dispatch_role: Some("workhorse".to_string()),
    }
}

fn inputs(ready: bool, drain: DrainView) -> HoldInputs {
    HoldInputs {
        owns_shared_root_state: true,
        ready: Some(ready),
        drain,
        now: t0(),
    }
}

/// A memory that has seen `streak` not-ready readings in a row.
fn streak(n: u32) -> HoldMemory {
    HoldMemory {
        not_ready_streak: n,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// The table, row by row
// ---------------------------------------------------------------------------

#[test]
fn a_secondary_never_holds() {
    let mut inp = inputs(false, read(LaneView::Clear));
    inp.owns_shared_root_state = false;
    assert_eq!(
        decide_hold_action(&streak(5), &inp),
        HoldAction::Nothing(Idle::Secondary)
    );
    // Not even its own hold is released by a secondary.
    let mut inp = inputs(true, read(own_lane(t0() + chrono::Duration::hours(3))));
    inp.owns_shared_root_state = false;
    assert_eq!(
        decide_hold_action(&HoldMemory::default(), &inp),
        HoldAction::Nothing(Idle::Secondary)
    );
}

#[test]
fn a_runner_that_is_not_enrolled_writes_nothing() {
    assert_eq!(
        decide_hold_action(&streak(5), &inputs(false, DrainView::NotEnrolled)),
        HoldAction::Nothing(Idle::NotEnrolled)
    );
}

#[test]
fn an_unknown_drain_or_an_absent_role_is_never_written() {
    assert_eq!(
        decide_hold_action(&streak(5), &inputs(false, DrainView::Unknown)),
        HoldAction::Nothing(Idle::DrainUnknown)
    );
    let no_role = DrainView::Read {
        lane: LaneView::Clear,
        dispatch_role: None,
    };
    assert_eq!(
        decide_hold_action(&streak(5), &inputs(false, no_role)),
        HoldAction::Nothing(Idle::RoleUnread)
    );
    // An own hold behind an unread role is not released either.
    let no_role_own = DrainView::Read {
        lane: own_lane(t0() + chrono::Duration::hours(3)),
        dispatch_role: None,
    };
    assert_eq!(
        decide_hold_action(&HoldMemory::default(), &inputs(true, no_role_own)),
        HoldAction::Nothing(Idle::RoleUnread)
    );
}

#[test]
fn an_unread_account_predicate_decides_nothing() {
    let mut inp = inputs(false, read(LaneView::Clear));
    inp.ready = None;
    assert_eq!(
        decide_hold_action(&streak(5), &inp),
        HoldAction::Nothing(Idle::ReadinessUnread)
    );
}

#[test]
fn a_bench_or_ci_node_role_already_closes_the_lane() {
    for role in ["bench", "ci_node", "a-role-this-build-does-not-know"] {
        let drain = DrainView::Read {
            lane: LaneView::Clear,
            dispatch_role: Some(role.to_string()),
        };
        assert_eq!(
            decide_hold_action(&streak(5), &inputs(false, drain)),
            HoldAction::Nothing(Idle::RoleClosesLane),
            "{role}"
        );
    }
}

#[test]
fn one_not_ready_reading_never_holds_and_the_second_does() {
    let mut mem = HoldMemory::default();
    mem.observe_readiness(false);
    assert_eq!(
        decide_hold_action(&mem, &inputs(false, read(LaneView::Clear))),
        HoldAction::Nothing(Idle::Debouncing)
    );
    mem.observe_readiness(false);
    assert_eq!(
        decide_hold_action(&mem, &inputs(false, read(LaneView::Clear))),
        HoldAction::Hold {
            until: t0() + HOLD_FOR
        }
    );
}

#[test]
fn a_ready_reading_restarts_the_debounce() {
    let mut mem = HoldMemory::default();
    mem.observe_readiness(false);
    mem.observe_readiness(true);
    mem.observe_readiness(false);
    assert_eq!(
        decide_hold_action(&mem, &inputs(false, read(LaneView::Clear))),
        HoldAction::Nothing(Idle::Debouncing)
    );
}

#[test]
fn an_own_hold_under_six_hours_left_is_renewed_and_never_shortened() {
    let lane = own_lane(t0() + chrono::Duration::hours(5));
    let action = decide_hold_action(&HoldMemory::default(), &inputs(false, read(lane)));
    assert_eq!(
        action,
        HoldAction::Renew {
            until: t0() + HOLD_FOR
        }
    );
    // Renewal does not wait on the debounce: the lane is already held.
    let HoldAction::Renew { until } = action else {
        unreachable!()
    };
    assert!(until > t0() + chrono::Duration::hours(5), "extends only");
}

#[test]
fn an_own_hold_with_more_than_six_hours_left_writes_nothing() {
    for left in [6, 12, 24] {
        let lane = own_lane(t0() + chrono::Duration::hours(left));
        assert_eq!(
            decide_hold_action(&streak(9), &inputs(false, read(lane))),
            HoldAction::Nothing(Idle::HeldWithMargin),
            "{left} h left"
        );
    }
}

#[test]
fn a_lane_drained_by_any_other_owner_is_left_alone() {
    let lane = other_lane(t0() + chrono::Duration::hours(1));
    assert_eq!(
        decide_hold_action(&streak(9), &inputs(false, read(lane.clone()))),
        HoldAction::Nothing(Idle::DrainedByOther)
    );
    // Nor is it "released" when an account comes back: it is not ours.
    assert_eq!(
        decide_hold_action(&streak(0), &inputs(true, read(lane))),
        HoldAction::Nothing(Idle::Ready)
    );
}

#[test]
fn an_operator_undrain_is_respected_until_the_recorded_until() {
    let recorded_until = t0() + chrono::Duration::hours(10);
    let mem = HoldMemory {
        not_ready_streak: 9,
        recorded: Some(RecordedHold {
            until: recorded_until,
            tenant_id: None,
        }),
        ..Default::default()
    };
    assert_eq!(
        decide_hold_action(&mem, &inputs(false, read(LaneView::Clear))),
        HoldAction::Nothing(Idle::OperatorUndrained {
            until: recorded_until
        })
    );
    // Once that until passes, the same reading holds again: the recorded hold
    // has expired, so the streak (9) decides.
    let mut later = inputs(false, read(LaneView::Clear));
    later.now = recorded_until + chrono::Duration::seconds(1);
    assert_eq!(
        decide_hold_action(&mem, &later),
        HoldAction::Hold {
            until: hold_deadline(later.now)
        }
    );
}

#[test]
fn a_lapsed_hold_is_followed_by_a_fresh_hold() {
    // The hold lapsed (a missed renewal, or a dead runner's until): the lane
    // reads clear and the recorded until is behind us.
    let mem = HoldMemory {
        not_ready_streak: 3,
        recorded: Some(RecordedHold {
            until: t0() - chrono::Duration::minutes(1),
            tenant_id: Some(Uuid::nil()),
        }),
        ..Default::default()
    };
    assert_eq!(
        decide_hold_action(&mem, &inputs(false, read(LaneView::Clear))),
        HoldAction::Hold {
            until: t0() + HOLD_FOR
        }
    );
}

#[test]
fn the_first_ready_reading_releases_an_own_hold_with_no_debounce() {
    let tenant = Uuid::now_v7();
    let mut mem = HoldMemory {
        not_ready_streak: 7,
        recorded: Some(RecordedHold {
            until: t0() + chrono::Duration::hours(20),
            tenant_id: Some(tenant),
        }),
        ..Default::default()
    };
    mem.observe_readiness(true);
    assert_eq!(
        decide_hold_action(
            &mem,
            &inputs(true, read(own_lane(t0() + chrono::Duration::hours(20))))
        ),
        HoldAction::Release {
            tenant: Some(tenant)
        }
    );
    // After a restart (no memory) the own reason alone is enough, and a
    // bench role does not stop a release of our own record.
    let bench_own = DrainView::Read {
        lane: own_lane(t0() + chrono::Duration::hours(20)),
        dispatch_role: Some("bench".to_string()),
    };
    assert_eq!(
        decide_hold_action(&HoldMemory::default(), &inputs(true, bench_own)),
        HoldAction::Release { tenant: None }
    );
}

#[test]
fn ready_with_no_own_hold_writes_nothing() {
    assert_eq!(
        decide_hold_action(&streak(0), &inputs(true, read(LaneView::Clear))),
        HoldAction::Nothing(Idle::Ready)
    );
}

#[test]
fn a_release_left_to_its_deadline_is_not_retried_every_tick() {
    let until = t0() + chrono::Duration::hours(20);
    let mem = HoldMemory {
        release_left_to: Some(Some(until)),
        ..Default::default()
    };
    assert_eq!(
        decide_hold_action(&mem, &inputs(true, read(own_lane(until)))),
        HoldAction::Nothing(Idle::ReleaseLeftToDeadline)
    );
}

#[test]
fn a_409_waits_for_the_lane_to_change() {
    let mut mem = streak(5);
    let lane = LaneView::Clear;
    let effect = mem.apply_hold_reply(
        &HoldReply::Conflict {
            code: "lane_held_by_other".to_string(),
        },
        &lane,
        t0(),
    );
    assert_eq!(effect, HoldEffect::Conflict);
    assert_eq!(
        decide_hold_action(&mem, &inputs(false, read(lane.clone()))),
        HoldAction::Nothing(Idle::WaitingOnConflict)
    );
    // The lane reads drained by the owner that answered 409, then clear again:
    // that is a change, so the wait ends.
    mem.note_lane(&other_lane(t0() + chrono::Duration::hours(1)));
    mem.note_lane(&LaneView::Clear);
    assert_eq!(
        decide_hold_action(&mem, &inputs(false, read(LaneView::Clear))),
        HoldAction::Hold {
            until: t0() + HOLD_FOR
        }
    );
}

#[test]
fn a_409_wait_ends_after_the_backoff_even_if_the_lane_never_changes() {
    let mut mem = streak(5);
    let lane = LaneView::Clear;
    mem.apply_hold_reply(
        &HoldReply::Conflict {
            code: "lane_unreadable".to_string(),
        },
        &lane,
        t0(),
    );
    let mut soon = inputs(false, read(lane.clone()));
    soon.now = t0() + chrono::Duration::minutes(9);
    assert_eq!(
        decide_hold_action(&mem, &soon),
        HoldAction::Nothing(Idle::WaitingOnConflict)
    );
    let mut later = inputs(false, read(lane));
    later.now = t0() + chrono::Duration::minutes(10);
    assert_eq!(
        decide_hold_action(&mem, &later),
        HoldAction::Hold {
            until: later.now + chrono::Duration::hours(24)
        }
    );
}

/// A hold this process never saw accepted (the previous run's, or one whose 200
/// was lost) is recorded from the lane read, so an operator undrain that follows
/// is respected rather than re-held on the next tick.
#[test]
fn an_own_hold_read_from_the_lane_marks_a_later_clear_as_an_operator_undrain() {
    let until = t0() + chrono::Duration::hours(20);
    let mut mem = streak(5);
    assert_eq!(mem.recorded, None);
    mem.note_lane(&own_lane(until));
    assert_eq!(
        mem.recorded,
        Some(RecordedHold {
            until,
            tenant_id: None
        })
    );
    mem.note_lane(&LaneView::Clear);
    assert_eq!(
        decide_hold_action(&mem, &inputs(false, read(LaneView::Clear))),
        HoldAction::Nothing(Idle::OperatorUndrained { until })
    );
    // A recorded tenant survives a read that names none, and the later until wins.
    let tenant = Uuid::from_u128(7);
    let mut mem = HoldMemory {
        recorded: Some(RecordedHold {
            until: t0() + chrono::Duration::hours(1),
            tenant_id: Some(tenant),
        }),
        ..Default::default()
    };
    mem.note_lane(&own_lane(until));
    assert_eq!(
        mem.recorded,
        Some(RecordedHold {
            until,
            tenant_id: Some(tenant)
        })
    );
    // A lane held by anyone else records nothing.
    let mut mem = HoldMemory::default();
    mem.note_lane(&other_lane(until));
    assert_eq!(mem.recorded, None);
}

/// The timing constants as literals: a test written against the constants
/// themselves would pass whatever they were changed to.
#[test]
fn the_timing_constants_are_what_the_plan_states() {
    assert_eq!(HOLD_FOR, chrono::Duration::hours(24));
    assert_eq!(RENEW_UNDER, chrono::Duration::hours(6));
    assert_eq!(WRITE_BACKOFF, chrono::Duration::minutes(10));
    assert_eq!(NOT_READY_TICKS_TO_HOLD, 2);
}

#[test]
fn a_403_warns_once_and_retries_every_ten_minutes() {
    let mut mem = streak(5);
    let lane = LaneView::Clear;
    let forbidden = HoldReply::Forbidden { status: 403 };
    assert_eq!(
        mem.apply_hold_reply(&forbidden, &lane, t0()),
        HoldEffect::Refused { warn: true }
    );
    let mut inp = inputs(false, read(lane.clone()));
    inp.now = t0() + chrono::Duration::minutes(9);
    assert_eq!(
        decide_hold_action(&mem, &inp),
        HoldAction::Nothing(Idle::WriteBackoff)
    );
    inp.now = t0() + WRITE_BACKOFF;
    assert_eq!(
        decide_hold_action(&mem, &inp),
        HoldAction::Hold {
            until: hold_deadline(inp.now)
        }
    );
    // A second 403 keeps the cadence but does not warn again.
    assert_eq!(
        mem.apply_hold_reply(&forbidden, &lane, inp.now),
        HoldEffect::Refused { warn: false }
    );
    // A success re-arms the warning.
    let held = HoldReply::Held {
        tenant_id: None,
        until: inp.now + HOLD_FOR,
        changed: true,
    };
    assert_eq!(
        mem.apply_hold_reply(&held, &lane, inp.now),
        HoldEffect::Held
    );
    assert!(!mem.refusal_warned);
    assert_eq!(mem.backoff_until, None);
}

#[test]
fn a_5xx_retries_next_tick_with_no_backoff() {
    let mut mem = streak(5);
    let lane = LaneView::Clear;
    assert_eq!(
        mem.apply_hold_reply(&HoldReply::ServerError { status: 503 }, &lane, t0()),
        HoldEffect::RetryNextTick
    );
    assert_eq!(
        decide_hold_action(&mem, &inputs(false, read(lane))),
        HoldAction::Hold {
            until: t0() + HOLD_FOR
        }
    );
}

#[test]
fn a_200_records_the_until_and_tenant() {
    let mut mem = streak(5);
    let tenant = Uuid::now_v7();
    let until = t0() + HOLD_FOR;
    mem.apply_hold_reply(
        &HoldReply::Held {
            tenant_id: Some(tenant),
            until,
            changed: true,
        },
        &LaneView::Clear,
        t0(),
    );
    assert_eq!(
        mem.recorded,
        Some(RecordedHold {
            until,
            tenant_id: Some(tenant)
        })
    );
}

// ---------------------------------------------------------------------------
// Release outcomes
// ---------------------------------------------------------------------------

fn answered(outcome: ReleaseOutcome, read_back: Option<DrainObservation>) -> ReleaseReply {
    ReleaseReply::Answered {
        outcome,
        tenant_id: None,
        read_back,
    }
}

#[test]
fn each_release_outcome_has_its_row() {
    let own_back = Some(DrainObservation::Drained {
        until: Some(t0() + chrono::Duration::hours(3)),
        reason: Some(own_reason()),
    });
    assert_eq!(
        after_release(&answered(ReleaseOutcome::Released, None)),
        ReleaseNext::Done
    );
    assert_eq!(
        after_release(&answered(ReleaseOutcome::NotOwner, None)),
        ReleaseNext::Done
    );
    assert_eq!(
        after_release(&answered(ReleaseOutcome::Absent, own_back.clone())),
        ReleaseNext::RetryUnderOtherTenants
    );
    assert_eq!(
        after_release(&answered(
            ReleaseOutcome::Absent,
            Some(DrainObservation::Clear)
        )),
        ReleaseNext::Done
    );
    assert_eq!(
        after_release(&answered(ReleaseOutcome::Unreadable, own_back.clone())),
        ReleaseNext::RetryNextTick
    );
    assert_eq!(
        after_release(&answered(ReleaseOutcome::Other("later".into()), None)),
        ReleaseNext::RetryNextTick
    );
    assert_eq!(
        after_release(&ReleaseReply::Forbidden { status: 403 }),
        ReleaseNext::Backoff
    );
    assert_eq!(
        after_release(&ReleaseReply::ServerError { status: 502 }),
        ReleaseNext::RetryNextTick
    );
}

// ---------------------------------------------------------------------------
// Wire parsing
// ---------------------------------------------------------------------------

#[test]
fn a_hold_200_parses_its_record() {
    let tenant = Uuid::now_v7();
    let body = serde_json::json!({
        "device_id": Uuid::now_v7(),
        "tenant_id": tenant,
        "prepare_id": PREPARE_ID,
        "lane": "agent",
        "outcome": "restored",
        "changed": false,
        "drained_by": "device:x:restart-hold:no-claude-account",
        "drained_at": "2026-11-04T11:00:00Z",
        "until": "2026-11-05T12:00:00Z",
        "reason": own_reason(),
        "version": 7,
    })
    .to_string();
    assert_eq!(
        parse_hold_reply(200, &body),
        HoldReply::Held {
            tenant_id: Some(tenant),
            until: Utc.with_ymd_and_hms(2026, 11, 5, 12, 0, 0).unwrap(),
            changed: false,
        }
    );
    assert!(matches!(
        parse_hold_reply(200, "{\"oops\":1}"),
        HoldReply::Unparseable { status: 200, .. }
    ));
}

#[test]
fn a_hold_409_403_and_5xx_parse_to_their_arms() {
    let conflict = serde_json::json!({
        "error": "lane_held_by_other",
        "detail": "…",
        "lane": "agent",
        "lane_outcome": "not_owner",
        "read_back": {"state": "drained", "until": "2026-11-05T00:00:00Z"},
    })
    .to_string();
    assert_eq!(
        parse_hold_reply(409, &conflict),
        HoldReply::Conflict {
            code: "lane_held_by_other".to_string()
        }
    );
    assert_eq!(
        parse_hold_reply(409, "not json"),
        HoldReply::Conflict {
            code: "unknown".to_string()
        }
    );
    assert_eq!(
        parse_hold_reply(403, "{}"),
        HoldReply::Forbidden { status: 403 }
    );
    assert_eq!(
        parse_hold_reply(401, ""),
        HoldReply::Forbidden { status: 401 }
    );
    assert_eq!(
        parse_hold_reply(503, "upstream"),
        HoldReply::ServerError { status: 503 }
    );
    assert!(matches!(
        parse_hold_reply(422, "deny_unknown_fields"),
        HoldReply::Rejected { status: 422, .. }
    ));
}

#[test]
fn a_release_answer_parses_its_outcome_and_read_back() {
    let tenant = Uuid::now_v7();
    for (wire, want) in [
        ("released", ReleaseOutcome::Released),
        ("not_owner", ReleaseOutcome::NotOwner),
        ("absent", ReleaseOutcome::Absent),
        ("unreadable", ReleaseOutcome::Unreadable),
        ("restored", ReleaseOutcome::Other("restored".into())),
    ] {
        let body = serde_json::json!({
            "device_id": Uuid::now_v7(),
            "tenant_id": tenant,
            "prepare_id": PREPARE_ID,
            "lane": "agent",
            "outcome": wire,
            "changed": true,
            "version": 3,
            "read_back": {
                "state": "drained",
                "until": "2026-11-05T00:00:00Z",
                "reason": own_reason(),
                "dispatch_role": "workhorse",
            },
        })
        .to_string();
        assert_eq!(
            parse_release_reply(200, &body),
            ReleaseReply::Answered {
                outcome: want,
                tenant_id: Some(tenant),
                read_back: Some(DrainObservation::Drained {
                    until: Some(Utc.with_ymd_and_hms(2026, 11, 5, 0, 0, 0).unwrap()),
                    reason: Some(own_reason()),
                }),
            },
            "{wire}"
        );
    }
    assert_eq!(
        parse_release_reply(403, ""),
        ReleaseReply::Forbidden { status: 403 }
    );
    assert_eq!(
        parse_release_reply(500, ""),
        ReleaseReply::ServerError { status: 500 }
    );
}

#[test]
fn the_wire_constants_match_coords_door() {
    // coord stores `restart-hold:<prepare_id>: <reason>`; the prefix this
    // module recognises must be exactly that, up to the reason.
    assert!(own_reason().starts_with(OWN_REASON_PREFIX));
    assert!(is_own_reason(Some(&own_reason())));
    assert!(!is_own_reason(Some("restart-hold:prep-1234: rebuilding")));
    assert!(!is_own_reason(None));
    // coord refuses a prepare_id outside [A-Za-z0-9_-] and over 64 chars.
    assert!(PREPARE_ID.len() <= 64);
    assert!(PREPARE_ID
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    assert_eq!(
        hold_body(t0() + HOLD_FOR),
        serde_json::json!({
            "reason": HOLD_REASON,
            "until": "2026-11-05T12:00:00Z",
            "prepare_id": "no-claude-account",
        })
    );
    assert_eq!(
        release_body(),
        serde_json::json!({ "prepare_id": "no-claude-account" })
    );
}

// ---------------------------------------------------------------------------
// The tick against a fake coord
// ---------------------------------------------------------------------------

mod over_http {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// coord's self-hold door and drain read, as far as the tick can see them.
    #[derive(Debug, Default)]
    struct FakeCoord {
        /// The agent lane's record: `(stored reason, until)`.
        lane: Option<(String, DateTime<Utc>)>,
        holds: Vec<serde_json::Value>,
        releases: Vec<serde_json::Value>,
        drain_reads: usize,
        /// Scripted release answers, served before the door's own logic.
        release_script: VecDeque<serde_json::Value>,
        tenant: Uuid,
    }

    type Shared = Arc<Mutex<FakeCoord>>;

    fn lock(c: &Shared) -> std::sync::MutexGuard<'_, FakeCoord> {
        c.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn drain_wire(c: &FakeCoord) -> serde_json::Value {
        match &c.lane {
            Some((reason, until)) => serde_json::json!({
                "state": "drained", "until": until, "reason": reason,
                "dispatch_role": "workhorse",
            }),
            None => serde_json::json!({"state": "clear", "dispatch_role": "workhorse"}),
        }
    }

    async fn route(
        axum::extract::State(shared): axum::extract::State<Shared>,
        method: axum::http::Method,
        uri: axum::http::Uri,
        body: bytes::Bytes,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let body: serde_json::Value =
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
        let mut c = lock(&shared);
        match (method.as_str(), uri.path()) {
            ("GET", "/coord/devices/me/drain") => {
                c.drain_reads += 1;
                let mut wire = drain_wire(&c);
                wire["device_id"] = serde_json::json!(Uuid::nil());
                axum::Json(wire).into_response()
            }
            ("POST", "/coord/devices/me/hold") => {
                c.holds.push(body.clone());
                let prepare = body["prepare_id"].as_str().unwrap_or_default().to_string();
                let until: DateTime<Utc> =
                    serde_json::from_value(body["until"].clone()).expect("an until");
                let stored = format!(
                    "restart-hold:{prepare}: {}",
                    body["reason"].as_str().unwrap_or_default()
                );
                let changed = match &c.lane {
                    // The door's repeat rule: keep the reason, move the until
                    // only past its 60 s tolerance.
                    Some((reason, old))
                        if reason.starts_with(&format!("restart-hold:{prepare}:")) =>
                    {
                        if (until - *old).num_seconds().abs() > 60 {
                            c.lane = Some((reason.clone(), until));
                            true
                        } else {
                            false
                        }
                    }
                    Some(_) => {
                        return (
                            axum::http::StatusCode::CONFLICT,
                            axum::Json(serde_json::json!({"error": "lane_held_by_other"})),
                        )
                            .into_response()
                    }
                    None => {
                        c.lane = Some((stored, until));
                        true
                    }
                };
                let (reason, until) = c.lane.clone().expect("held");
                axum::Json(serde_json::json!({
                    "device_id": Uuid::nil(), "tenant_id": c.tenant,
                    "prepare_id": prepare, "lane": "agent", "outcome": "restored",
                    "changed": changed, "drained_by": "device:x", "drained_at": until,
                    "until": until, "reason": reason, "version": 1,
                }))
                .into_response()
            }
            ("POST", "/coord/devices/me/release") => {
                c.releases.push(body.clone());
                if let Some(scripted) = c.release_script.pop_front() {
                    return axum::Json(scripted).into_response();
                }
                let outcome = if c.lane.is_some() {
                    c.lane = None;
                    "released"
                } else {
                    "absent"
                };
                let read_back = drain_wire(&c);
                axum::Json(serde_json::json!({
                    "device_id": Uuid::nil(), "tenant_id": c.tenant,
                    "prepare_id": body["prepare_id"], "lane": "agent",
                    "outcome": outcome, "changed": true, "version": 2,
                    "read_back": read_back,
                }))
                .into_response()
            }
            _ => axum::Json(serde_json::json!({})).into_response(),
        }
    }

    async fn start_fake_coord() -> (String, Shared) {
        let shared = Shared::default();
        lock(&shared).tenant = Uuid::now_v7();
        let app = axum::Router::new()
            .fallback(route)
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the fake coord");
        let base = format!("http://{}", listener.local_addr().expect("its address"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (base, shared)
    }

    fn write_creds(dir: &std::path::Path, live: bool) {
        let expires_at = if live {
            Utc::now().timestamp_millis() + 86_400_000
        } else {
            1_000
        };
        std::fs::write(
            dir.join(".credentials.json"),
            serde_json::json!({
                "claudeAiOauth": {
                    "accessToken": "sk-oauth-test",
                    "refreshToken": "",
                    "expiresAt": expires_at,
                    "scopes": ["user:inference"],
                }
            })
            .to_string(),
        )
        .expect("write the credentials");
    }

    /// Puts back what the fixture published on process-global state: the pick,
    /// and the drain tracker (three misses are what make it read `Unknown`, the
    /// state a test process otherwise sits in).
    struct RestoreGlobals {
        resolved: Option<String>,
    }

    impl Drop for RestoreGlobals {
        fn drop(&mut self) {
            crate::ai_provider::set_resolved_config_dir(self.resolved.take());
            for _ in 0..3 {
                crate::coord_drain_state::note_heartbeat_failure("account-hold test finished");
            }
        }
    }

    /// An enrolled runner with no credential-valid account, pointed at a fake
    /// coord. Fields drop in order: globals first, then the temp dirs, then the
    /// env lock.
    struct NoAccountRunner {
        _restore: RestoreGlobals,
        default_location: tempfile::TempDir,
        _guards: (
            crate::mcp::fleet_policy_poller::AccountSelectionPin,
            qontinui_runner_lib::ambient::test_support::IsolatedAmbient,
        ),
    }

    impl NoAccountRunner {
        async fn arrange(coord_base: &str) -> Self {
            let guards = crate::mcp::fleet_policy_poller::isolated_ambient_with_fleet_pin();
            // `dirs` answers the REAL profile on Windows whatever HOME and
            // USERPROFILE say, so without these two the box's own roster and
            // `~/.claude` decide the test. An instance root moves the roster
            // into the fixture, and an existing `$CLAUDE_CONFIG_DIR` file is the
            // default location's first hit, so an expired one reads as no login.
            std::env::set_var("QONTINUI_INSTANCE_ROOT", guards.1.dir().join("subject"));
            let default_location = tempfile::tempdir().expect("tempdir");
            write_creds(default_location.path(), false);
            std::env::set_var("CLAUDE_CONFIG_DIR", default_location.path());
            std::env::set_var("COORD_HTTP_URL", coord_base);
            guards
                .1
                .write_machine_json(&format!("{{\"device_id\":\"{}\"}}", Uuid::now_v7()));

            let restore = RestoreGlobals {
                resolved: crate::ai_provider::get_resolved_config_dir(),
            };
            crate::ai_provider::set_resolved_config_dir(None);
            // A test process boots `Unknown`; the heartbeat's read makes it clear.
            crate::coord_drain_state::note_heartbeat(
                crate::coord_drain_state::HeartbeatOutcome::Registered {
                    drain: DrainObservation::Clear,
                },
            )
            .await;

            // The premises: if these do not hold, nothing below tests what it
            // claims to.
            assert!(
                !crate::ai_provider::spawn_account_ready(),
                "the fixture must hold no credential-valid account"
            );
            assert_eq!(
                crate::coord_drain_state::enrolled_coord_base().as_deref(),
                Some(coord_base),
                "the fixture must be an enrolled coord device pointed at the fake"
            );
            Self {
                _restore: restore,
                default_location,
                _guards: guards,
            }
        }

        /// `/login`: the default location now holds a live login.
        fn log_in(&self) {
            write_creds(self.default_location.path(), true);
        }
    }

    /// The production env with ONE substitution: `owns_shared_root_state`. The
    /// fixture needs an instance root to keep the box's real accounts out (see
    /// [`NoAccountRunner::arrange`]), and an instance root is exactly what makes
    /// a runner a secondary, so the real predicate would answer for the fixture
    /// and not for the primary this test stands in for. The account predicate
    /// and the coord calls are the production ones.
    fn primary_env() -> HoldEnv {
        HoldEnv {
            owns_shared_root_state: || true,
            ..HoldEnv::production()
        }
    }

    /// ARMING. A primary with no credential-valid account POSTs a hold whose
    /// reason starts `restart-hold:no-claude-account:` on its SECOND not-ready
    /// tick and not before, writes nothing on a repeat, and POSTs a release on
    /// its first ready tick — entered through [`account_hold_tick`], the tick
    /// body `run_account_hold_loop` runs on every interval, against coord over
    /// HTTP. The loop itself and the spawn are not driven (see the module doc),
    /// and the fake does not check `Authorization`, so the bearer choice is not
    /// covered here.
    #[tokio::test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "the default-location check reads the real login Keychain on macOS"
    )]
    async fn a_primary_with_no_account_holds_its_lane_on_the_second_tick_and_releases_on_login() {
        let (base, coord) = start_fake_coord().await;
        let runner = NoAccountRunner::arrange(&base).await;
        let env = primary_env();
        let mut mem = HoldMemory::default();

        // Tick 1: not ready once. Read, but nothing written.
        account_hold_tick(&mut mem, &env).await;
        {
            let c = lock(&coord);
            assert!(c.drain_reads >= 1, "the tick read the drain: {c:?}");
            assert!(
                c.holds.is_empty(),
                "one not-ready reading must never hold: {c:?}"
            );
        }

        // Tick 2: not ready twice. Held.
        account_hold_tick(&mut mem, &env).await;
        {
            let c = lock(&coord);
            assert_eq!(c.holds.len(), 1, "the second not-ready tick holds: {c:?}");
            assert_eq!(c.holds[0]["prepare_id"], PREPARE_ID);
            let (reason, until) = c.lane.clone().expect("coord holds the lane");
            assert!(
                reason.starts_with("restart-hold:no-claude-account:"),
                "{reason}"
            );
            let ahead = until - Utc::now();
            assert!(
                ahead > chrono::Duration::hours(23) && ahead <= HOLD_FOR,
                "until is about 24 h ahead: {until}"
            );
        }
        assert!(
            matches!(
                crate::coord_drain_state::current(),
                CoordDrainState::Drained { reason: Some(ref r), .. } if is_own_reason(Some(r.as_str()))
            ),
            "the write is folded into the cached drain at once: {:?}",
            crate::coord_drain_state::current()
        );

        // Tick 3: still not ready, hold has ~24 h left. A repeat writes nothing.
        account_hold_tick(&mut mem, &env).await;
        assert_eq!(lock(&coord).holds.len(), 1, "a repeat writes nothing");

        // `/login`, then tick 4: released on the first ready reading.
        runner.log_in();
        account_hold_tick(&mut mem, &env).await;
        {
            let c = lock(&coord);
            assert_eq!(c.releases.len(), 1, "the first ready tick releases: {c:?}");
            assert_eq!(c.releases[0]["prepare_id"], PREPARE_ID);
            assert!(c.lane.is_none(), "the lane is open again: {c:?}");
        }
        assert_eq!(
            crate::coord_drain_state::current(),
            CoordDrainState::Clear,
            "the release's read-back is folded"
        );
        assert_eq!(mem.recorded, None);
    }

    /// ARMING, through the loop. The supervised loop body
    /// [`run_account_hold_loop`] — what [`spawn_claude_account_hold`] runs — with
    /// a short interval: it holds the lane only once two ticks have read no
    /// account, and releases it after `/login`.
    #[tokio::test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "the default-location check reads the real login Keychain on macOS"
    )]
    async fn the_account_hold_loop_holds_a_lane_with_no_account_and_releases_it_on_login() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let (base, coord) = start_fake_coord().await;
        let runner = NoAccountRunner::arrange(&base).await;
        let ticks = Arc::new(AtomicU32::new(0));
        let counted = ticks.clone();
        let looped = run_account_hold_loop(Duration::from_millis(100), primary_env(), move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        let watched = async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
            while lock(&coord).holds.is_empty() {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the loop never held"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(
                ticks.load(Ordering::SeqCst) >= NOT_READY_TICKS_TO_HOLD,
                "held before two not-ready ticks"
            );
            let reason = lock(&coord).lane.clone().expect("coord holds the lane").0;
            assert!(
                reason.starts_with("restart-hold:no-claude-account:"),
                "{reason}"
            );
            runner.log_in();
            while lock(&coord).releases.is_empty() {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the loop never released"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let c = lock(&coord);
            assert_eq!(c.holds.len(), 1, "one hold, no repeat writes: {c:?}");
            assert!(c.lane.is_none(), "the lane is open again: {c:?}");
        };
        tokio::select! {
            () = looped => unreachable!("the account-hold loop never returns"),
            () = watched => {}
        }
    }

    /// A runner that boots into its own hold (left by the previous run) and is
    /// then undrained by an operator does not re-hold: the hold it read from the
    /// lane counts as recorded until its `until`.
    #[tokio::test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "the default-location check reads the real login Keychain on macOS"
    )]
    async fn an_operator_undrain_of_a_hold_left_by_the_previous_run_is_respected() {
        let (base, coord) = start_fake_coord().await;
        let _runner = NoAccountRunner::arrange(&base).await;
        lock(&coord).lane = Some((own_reason(), Utc::now() + chrono::Duration::hours(20)));
        let env = primary_env();
        let mut mem = HoldMemory::default();

        account_hold_tick(&mut mem, &env).await;
        account_hold_tick(&mut mem, &env).await;
        assert!(
            lock(&coord).holds.is_empty(),
            "a held lane with margin writes nothing"
        );
        assert!(
            mem.recorded.is_some(),
            "the own hold on the lane is recorded"
        );

        // The operator undrains.
        lock(&coord).lane = None;
        let action = account_hold_tick(&mut mem, &env).await;
        assert!(
            matches!(action, HoldAction::Nothing(Idle::OperatorUndrained { .. })),
            "{action:?}"
        );
        account_hold_tick(&mut mem, &env).await;
        assert!(
            lock(&coord).holds.is_empty(),
            "an operator undrain is never overridden before the recorded until"
        );
    }

    /// `absent` while the read-back still shows the own reason: the hold is in
    /// another tenant's drain map, so the release is retried under each bound
    /// tenant until one answers.
    #[tokio::test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "the default-location check reads the real login Keychain on macOS"
    )]
    async fn an_absent_release_with_the_own_reason_still_served_retries_under_each_bound_tenant() {
        let (base, coord) = start_fake_coord().await;
        let runner = NoAccountRunner::arrange(&base).await;
        let until = Utc::now() + chrono::Duration::hours(20);
        let still_own = serde_json::json!({
            "state": "drained", "until": until, "reason": own_reason(),
            "dispatch_role": "workhorse",
        });
        {
            let mut c = lock(&coord);
            c.lane = Some((own_reason(), until));
            let absent = serde_json::json!({
                "tenant_id": c.tenant, "outcome": "absent", "read_back": still_own,
            });
            c.release_script.push_back(absent.clone());
            c.release_script.push_back(absent);
        }
        crate::coord_drain_state::observe_me_drain(DrainObservation::Drained {
            until: Some(until),
            reason: Some(own_reason()),
        });
        runner.log_in();
        let env = HoldEnv {
            owns_shared_root_state: || true,
            bound_tenants: || vec![Uuid::from_u128(1), Uuid::from_u128(2)],
            ..HoldEnv::production()
        };
        let mut mem = HoldMemory::default();

        account_hold_tick(&mut mem, &env).await;
        {
            let c = lock(&coord);
            // Default binding (absent), tenant 1 (absent), tenant 2 (released).
            assert_eq!(c.releases.len(), 3, "{c:?}");
            assert!(c.lane.is_none(), "{c:?}");
        }

        // Every tenant answering absent leaves the hold to its until, and the
        // next tick does not retry.
        let until = Utc::now() + chrono::Duration::hours(20);
        {
            let mut c = lock(&coord);
            c.lane = Some((own_reason(), until));
            c.releases.clear();
            let still_own = serde_json::json!({
                "state": "drained", "until": until, "reason": own_reason(),
                "dispatch_role": "workhorse",
            });
            for _ in 0..3 {
                let absent = serde_json::json!({
                    "tenant_id": c.tenant, "outcome": "absent", "read_back": still_own,
                });
                c.release_script.push_back(absent);
            }
        }
        crate::coord_drain_state::observe_me_drain(DrainObservation::Drained {
            until: Some(until),
            reason: Some(own_reason()),
        });
        account_hold_tick(&mut mem, &env).await;
        assert_eq!(lock(&coord).releases.len(), 3);
        assert_eq!(
            account_hold_tick(&mut mem, &env).await,
            HoldAction::Nothing(Idle::ReleaseLeftToDeadline)
        );
        assert_eq!(lock(&coord).releases.len(), 3, "not retried every tick");
    }

    /// A secondary reads nothing and writes nothing, whatever its accounts say.
    #[tokio::test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "the default-location check reads the real login Keychain on macOS"
    )]
    async fn a_secondary_tick_reads_and_writes_nothing() {
        let (base, coord) = start_fake_coord().await;
        let _runner = NoAccountRunner::arrange(&base).await;
        // The fixture's instance root makes it a secondary, so the production
        // predicate is the one under test here.
        assert!(!crate::instance::owns_shared_root_state());
        let env = HoldEnv::production();
        let mut mem = HoldMemory::default();
        for _ in 0..3 {
            assert_eq!(
                account_hold_tick(&mut mem, &env).await,
                HoldAction::Nothing(Idle::Secondary)
            );
        }
        let c = lock(&coord);
        assert_eq!(c.drain_reads, 0);
        assert!(c.holds.is_empty() && c.releases.is_empty());
    }
}
