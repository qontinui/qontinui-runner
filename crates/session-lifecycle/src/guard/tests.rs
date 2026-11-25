use super::*;
use crate::model::{Dial, ItemKind, ItemState};
use crate::state::{
    ApiErrorKind, Custody, CustodyAxis, DialogKind, Input, Latch, Observed, OwnedAxis, Parked,
    SelfCont, SelfConts,
};
use crate::testkit::*;

fn report(s: &SessionState) -> GuardReport {
    evaluate(s, &[done("w")], &dials(), X, NOW)
}

#[test]
fn a_clean_finished_idle_session_clears_every_conjunct() {
    let r = report(&closable());
    assert!(r.verdict().is_true(), "{:?}", r.first_not_true());
    assert!(r.close_proof().is_some());
    assert!(matches!(close_action(&r, NOW), Some(Action::Close { .. })));
}

fn expect_not_true(mut s: SessionState, mutate: impl FnOnce(&mut SessionState), id: GuardId) {
    mutate(&mut s);
    let r = report(&s);
    assert!(!r.verdict().is_true(), "{id:?} should refuse");
    let hit = r
        .guards
        .iter()
        .find(|g| g.id == id)
        .expect("conjunct present");
    assert!(!hit.value.is_true(), "{id:?}: {hit:?}");
    assert!(close_action(&r, NOW).is_none());
}

#[test]
fn g1_coord_row() {
    expect_not_true(closable(), |s| s.coord_finished = Tri::False, GuardId::G1);
    expect_not_true(
        closable(),
        |s| s.coord_finished = Tri::unknown("c", "x"),
        GuardId::G1,
    );
}

#[test]
fn g2_declaration_and_revival() {
    expect_not_true(closable(), |s| s.revived = Tri::True, GuardId::G2);
    expect_not_true(
        closable(),
        |s| s.revived = Tri::unknown("r", "x"),
        GuardId::G2,
    );
    expect_not_true(
        closable(),
        |s| s.work = Observed::new(Declared::Undeclared, NOW),
        GuardId::G2,
    );
    expect_not_true(
        closable(),
        |s| {
            s.work = Observed::new(
                Declared::Unknown(Unk::new("d", "declaration_unverified")),
                NOW,
            )
        },
        GuardId::G2,
    );
}

#[test]
fn g3_grace_children_and_pane_kind() {
    // Inside the grace window.
    expect_not_true(
        closable(),
        |s| {
            s.activity = Observed::new(
                Activity::Idle {
                    since_turn_end: NOW,
                    grace_from: Timestamp(NOW.0 - 5 * MIN),
                },
                NOW,
            )
        },
        GuardId::G3,
    );
    // A proven stray still blocks (G3: a stray is still Work).
    expect_not_true(
        closable(),
        |s| {
            s.activity = Observed::new(
                Activity::ShellPinned {
                    shells: 1,
                    stray_shells: 1,
                    monitors: 0,
                    mcp_tasks: 0,
                },
                NOW,
            )
        },
        GuardId::G3,
    );
    expect_not_true(closable(), |s| s.pane_kind = PaneKind::Steward, GuardId::G3);
    expect_not_true(
        closable(),
        |s| s.pane_kind = PaneKind::Unreadable,
        GuardId::G3,
    );
}

#[test]
fn capacity_shortens_grace_to_the_floor_and_never_below() {
    let mut s = closable();
    s.activity = Observed::new(
        Activity::Idle {
            since_turn_end: NOW,
            grace_from: Timestamp(NOW.0 - 3 * MIN),
        },
        NOW,
    );
    assert!(!evaluate(&s, &[done("w")], &dials(), X, NOW)
        .verdict()
        .is_true());
    let mut d = dials();
    d.grace_override = Some(Dur::mins(2));
    assert!(evaluate(&s, &[done("w")], &d, X, NOW).verdict().is_true());
    // An override below the floor is clamped to it.
    d.grace_override = Some(Dur::secs(1));
    s.activity = Observed::new(
        Activity::Idle {
            since_turn_end: NOW,
            grace_from: Timestamp(NOW.0 - MIN),
        },
        NOW,
    );
    assert!(!evaluate(&s, &[done("w")], &d, X, NOW).verdict().is_true());
}

#[test]
fn g4_g5_g6() {
    expect_not_true(
        closable(),
        |s| {
            s.selfcont = Observed::new(
                SelfConts::Known(vec![SelfCont::QueuedNotice { since: NOW }]),
                NOW,
            )
        },
        GuardId::G4,
    );
    expect_not_true(
        closable(),
        |s| s.selfcont = Observed::new(SelfConts::Unknown(Unk::new("s", "x")), NOW),
        GuardId::G4,
    );
    expect_not_true(
        closable(),
        |s| {
            s.parked = Observed::new(
                Parked::ApiError {
                    kind: ApiErrorKind::Overloaded,
                    retryable: true,
                },
                NOW,
            )
        },
        GuardId::G5,
    );
    expect_not_true(
        closable(),
        |s| {
            s.input = Observed::new(
                Input::Dialog {
                    kind: DialogKind::Permission,
                    since: NOW,
                },
                NOW,
            )
        },
        GuardId::G6,
    );
    expect_not_true(
        closable(),
        |s| s.input = Observed::new(Input::Unknown(Unk::new("i", "x")), NOW),
        GuardId::G6,
    );
}

#[test]
fn g7_custody() {
    for c in [
        Custody::Dirty {
            reason: "uncommitted".into(),
        },
        Custody::Unknown {
            reason: "shared_checkout".into(),
        },
        Custody::Absent {
            evidence: "gone".into(),
        },
    ] {
        expect_not_true(
            closable(),
            |s| s.custody = Observed::new(CustodyAxis::Known(vec![("/w".into(), c.clone())]), NOW),
            GuardId::G7,
        );
    }
    // Absence of a discovered tree is not cleanliness.
    expect_not_true(
        closable(),
        |s| s.custody = Observed::new(CustodyAxis::Known(vec![]), NOW),
        GuardId::G7,
    );
}

#[test]
fn g8_owned_items() {
    let s = closable();
    for it in [
        crate::testkit::owed("a"),
        item(
            "b",
            ItemKind::CiFirstVerdict,
            ItemState::Impeded {
                refusal: "r".into(),
            },
        ),
        item(
            "c",
            ItemKind::Question,
            ItemState::Unknown(Unk::new("q", "x")),
        ),
    ] {
        let r = evaluate(&s, &[it], &dials(), X, NOW);
        assert!(!r.verdict().is_true());
    }
    // A first-verdict hold is not terminal.
    let mut hold = item(
        "h",
        ItemKind::CiFirstVerdict,
        ItemState::Pending { until: NOW },
    );
    hold.inside_first_verdict_hold = true;
    assert!(!evaluate(&s, &[hold], &dials(), X, NOW).verdict().is_true());
    // Watched and Deferred are terminal.
    let ok = [
        item(
            "w",
            ItemKind::WaitGate,
            ItemState::Watched {
                lane: "gate".into(),
                ack: true,
            },
        ),
        item(
            "d",
            ItemKind::Finding,
            ItemState::Deferred {
                trigger: "t".into(),
            },
        ),
    ];
    assert!(evaluate(&s, &ok, &dials(), X, NOW).verdict().is_true());
    expect_not_true(
        closable(),
        |s| s.owned = Observed::new(OwnedAxis::Unknown(Unk::new("o", "x")), NOW),
        GuardId::G8,
    );
}

#[test]
fn g9_freshness_dials_and_latch() {
    expect_not_true(
        closable(),
        |s| s.input = Observed::new(Input::Clear, Timestamp(NOW.0 - 10 * MIN)),
        GuardId::G9,
    );
    expect_not_true(
        closable(),
        |s| s.schema_skew = Some(Unk::new("schema", "newer")),
        GuardId::G9,
    );
    let s = closable();
    let mut d = dials();
    d.latch = Latch::Tripped;
    assert!(!evaluate(&s, &[done("w")], &d, X, NOW).verdict().is_true());
    // Unreadable past one poll interval reads tripped; within it, Unknown.
    d.latch = Latch::Unreadable {
        since: Timestamp(NOW.0 - 5 * MIN),
    };
    let r = evaluate(&s, &[done("w")], &d, X, NOW);
    assert!(r
        .guards
        .iter()
        .any(|g| g.id == GuardId::G9 && g.value.is_false()));
    d.latch = Latch::Unreadable {
        since: Timestamp(NOW.0 - 1_000),
    };
    let r = evaluate(&s, &[done("w")], &d, X, NOW);
    assert!(r
        .guards
        .iter()
        .any(|g| g.id == GuardId::G9 && g.value.is_unknown()));
    // The close dial.
    for dial in [Dial::Off, Dial::Unknown] {
        let mut d = dials();
        d.close = dial;
        assert!(!evaluate(&s, &[done("w")], &d, X, NOW).verdict().is_true());
    }
}

#[test]
fn interim_narrowing_applies_on_the_interim_host_only() {
    let mut s = closable();
    s.mode = Mode::Attended;
    // Attended and idle only 30 min: the interim host refuses (I6); build X does not.
    assert!(!evaluate(&s, &[done("w")], &dials(), INTERIM, NOW)
        .verdict()
        .is_true());
    assert!(evaluate(&s, &[done("w")], &dials(), X, NOW)
        .verdict()
        .is_true());
    s.activity = Observed::new(
        Activity::Idle {
            since_turn_end: Timestamp(NOW.0 - 61 * MIN),
            grace_from: Timestamp(NOW.0 - 61 * MIN),
        },
        NOW,
    );
    assert!(evaluate(&s, &[done("w")], &dials(), INTERIM, NOW)
        .verdict()
        .is_true());
    // I8: a drain or the restart executor's hold.
    let mut d = dials();
    d.drain_or_self_hold = true;
    assert!(!evaluate(&s, &[done("w")], &d, INTERIM, NOW)
        .verdict()
        .is_true());
    let mut h = INTERIM;
    h.cell_allowlisted = false;
    assert!(!evaluate(&s, &[done("w")], &dials(), h, NOW)
        .verdict()
        .is_true());
}

#[test]
fn cleared_but_for_grace_names_a_pending_grace_only() {
    let mut s = closable();
    s.activity = Observed::new(
        Activity::Idle {
            since_turn_end: NOW,
            grace_from: Timestamp(NOW.0 - 5 * MIN),
        },
        NOW,
    );
    let r = report(&s);
    assert!(r.cleared_but_for_grace());
    assert_eq!(r.grace_until, Some(Timestamp(NOW.0 + 5 * MIN)));
    s.coord_finished = Tri::False;
    assert!(!report(&s).cleared_but_for_grace());
}

#[allow(clippy::string_slice)]
/// S1: `Action::Close` has exactly one construction site in the crate's
/// non-test source. A construction is `Action::Close {` that is not a pattern
/// (`{ .. }`); comment lines and everything after a `#[cfg(test)]` line are
/// ignored.
#[test]
fn s1_action_close_has_exactly_one_construction_site() {
    let files: &[(&str, &str)] = &[
        ("lib.rs", include_str!("../lib.rs")),
        ("classify_child.rs", include_str!("../classify_child.rs")),
        ("state.rs", include_str!("../state.rs")),
        ("stray.rs", include_str!("../stray.rs")),
        ("model.rs", include_str!("../model.rs")),
        ("guard.rs", include_str!("../guard.rs")),
        ("decide.rs", include_str!("../decide.rs")),
        ("evaluate/mod.rs", include_str!("../evaluate/mod.rs")),
        (
            "evaluate/registry.rs",
            include_str!("../evaluate/registry.rs"),
        ),
        (
            "evaluate/transcript.rs",
            include_str!("../evaluate/transcript.rs"),
        ),
        (
            "evaluate/custody.rs",
            include_str!("../evaluate/custody.rs"),
        ),
        (
            "evaluate/restore.rs",
            include_str!("../evaluate/restore.rs"),
        ),
    ];
    let mut sites = Vec::new();
    for (name, src) in files {
        for (n, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("#[cfg(test)]") {
                break;
            }
            if trimmed.starts_with("//") {
                continue;
            }
            let mut rest = line;
            while let Some(i) = rest.find("Action::Close") {
                let after = rest[i + "Action::Close".len()..].trim_start();
                if let Some(body) = after.strip_prefix('{') {
                    if !body.trim_start().starts_with("..") {
                        sites.push(format!("{name}:{}", n + 1));
                    }
                }
                rest = &rest[i + "Action::Close".len()..];
            }
        }
    }
    assert_eq!(sites.len(), 1, "construction sites: {sites:?}");
    assert!(sites[0].starts_with("guard.rs"), "{sites:?}");
}

/// S2's guard half: turning any one conjunct from True to Unknown can never
/// leave the verdict True (monotone in safety).
#[test]
fn an_unknown_conjunct_never_clears() {
    let base = report(&closable());
    for (i, _) in base.guards.iter().enumerate() {
        let mut r = base.clone();
        r.guards[i].value = Tri::unknown("probe", "x");
        assert!(!r.verdict().is_true());
        assert!(r.close_proof().is_none());
        assert!(close_action(&r, NOW).is_none());
    }
}
