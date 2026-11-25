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
    expect_not_true(
        closable(),
        |s| s.coord_finished = Observed::new(Tri::False, NOW),
        GuardId::G1,
    );
    expect_not_true(
        closable(),
        |s| s.coord_finished = Observed::new(Tri::unknown("c", "x"), NOW),
        GuardId::G1,
    );
}

#[test]
fn g2_declaration_and_revival() {
    expect_not_true(
        closable(),
        |s| s.revived = Observed::new(Tri::True, NOW),
        GuardId::G2,
    );
    expect_not_true(
        closable(),
        |s| s.revived = Observed::new(Tri::unknown("r", "x"), NOW),
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
                    since: NOW,
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
    s.coord_finished = Observed::new(Tri::False, NOW);
    assert!(!report(&s).cleared_but_for_grace());
}

/// S1: `Action::Close` has exactly one construction site in the crate's
/// non-test source. Every `.rs` file under `src/` is scanned (the directory is
/// walked at test time, so a new file cannot hide a second site). The scan
/// works on the whole file with comments and string literals blanked, so a
/// multi-line `Action::Close\n{` is caught, as is `use Action::Close` (an
/// import that would make a bare `Close { .. }` construction possible).
/// Pattern uses (`{ .. }`) are not constructions. Test modules (`tests.rs`,
/// `testkit.rs`, and everything after a `#[cfg(test)]` line) are excluded.
#[test]
fn s1_action_close_has_exactly_one_construction_site() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    fn blank(src: &str) -> String {
        // Blank line comments and string / char literals, keeping newlines.
        let b: Vec<char> = src.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == '/' && b.get(i + 1) == Some(&'/') {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            } else if b[i] == '"' {
                out.push(' ');
                i += 1;
                while i < b.len() && b[i] != '"' {
                    if b[i] == '\\' {
                        i += 1;
                    }
                    if b.get(i) == Some(&'\n') {
                        out.push('\n');
                    }
                    i += 1;
                }
                i += 1;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        out
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&root, &mut files);
    assert!(files.len() >= 10, "the walk found {} files", files.len());
    let mut sites = Vec::new();
    let mut imports = Vec::new();
    for f in &files {
        let name = f.strip_prefix(&root).unwrap().display().to_string();
        if name.ends_with("tests.rs") || name.ends_with("testkit.rs") {
            continue;
        }
        let src = std::fs::read_to_string(f).unwrap();
        let live = match src.find("#[cfg(test)]") {
            Some(i) => src.get(..i).unwrap_or(&src),
            None => &src,
        };
        let text = blank(live);
        let mut rest = text.as_str();
        let mut offset = 0;
        while let Some(i) = rest.find("Action::Close") {
            let abs = offset + i;
            let after = text.get(abs + "Action::Close".len()..).unwrap_or("");
            let before = text.get(..abs).unwrap_or("");
            let line = before.matches('\n').count() + 1;
            if before.trim_end().ends_with("use") {
                imports.push(format!("{name}:{line}"));
            }
            if let Some(body) = after.trim_start().strip_prefix('{') {
                if !body.trim_start().starts_with("..") {
                    sites.push(format!("{name}:{line}"));
                }
            }
            offset = abs + "Action::Close".len();
            rest = text.get(offset..).unwrap_or("");
        }
        // `use Action::*` / `use crate::model::Action::{Close, ..}` forms.
        if text.contains("Action::*") || text.contains("Action::{") {
            imports.push(format!("{name}: glob or brace import of Action"));
        }
    }
    assert_eq!(sites.len(), 1, "construction sites: {sites:?}");
    assert!(sites[0].starts_with("guard.rs"), "{sites:?}");
    assert!(
        imports.is_empty(),
        "imports that hide a construction: {imports:?}"
    );
}

/// S2's guard half, driven through `evaluate()` and `decide()`: every single
/// conjunct's input made Unknown or stale — one at a time — leaves the verdict
/// not True and the decision not a Close.
#[test]
fn an_unknown_or_stale_conjunct_never_clears() {
    let u = || Unk::new("probe", "forced");
    let old = Timestamp(NOW.0 - 30 * MIN);
    type Mut = Box<dyn Fn(&mut SessionState)>;
    let cases: Vec<(&str, GuardId, Mut)> = vec![
        (
            "coord unknown",
            GuardId::G1,
            Box::new(move |s| s.coord_finished = Observed::new(Tri::Unknown(u()), NOW)),
        ),
        (
            "coord stale",
            GuardId::G9,
            Box::new(move |s| s.coord_finished = Observed::new(Tri::True, old)),
        ),
        (
            "W unknown",
            GuardId::G2,
            Box::new(move |s| s.work = Observed::new(Declared::Unknown(u()), NOW)),
        ),
        (
            "W stale",
            GuardId::G9,
            Box::new(move |s| s.work = Observed::new(s.work.value.clone(), old)),
        ),
        (
            "R unknown",
            GuardId::G2,
            Box::new(move |s| s.revived = Observed::new(Tri::Unknown(u()), NOW)),
        ),
        (
            "R stale",
            GuardId::G9,
            Box::new(move |s| s.revived = Observed::new(Tri::False, old)),
        ),
        (
            "activity unknown",
            GuardId::G3,
            Box::new(move |s| s.activity = Observed::new(Activity::Unknown(u()), NOW)),
        ),
        (
            "activity stale",
            GuardId::G9,
            Box::new(move |s| s.activity = Observed::new(s.activity.value.clone(), old)),
        ),
        (
            "S unknown",
            GuardId::G4,
            Box::new(move |s| s.selfcont = Observed::new(SelfConts::Unknown(u()), NOW)),
        ),
        (
            "S stale",
            GuardId::G9,
            Box::new(move |s| s.selfcont = Observed::new(s.selfcont.value.clone(), old)),
        ),
        (
            "P unknown",
            GuardId::G5,
            Box::new(move |s| s.parked = Observed::new(Parked::Unknown(u()), NOW)),
        ),
        (
            "P stale",
            GuardId::G9,
            Box::new(move |s| s.parked = Observed::new(Parked::None, old)),
        ),
        (
            "I unknown",
            GuardId::G6,
            Box::new(move |s| s.input = Observed::new(Input::Unknown(u()), NOW)),
        ),
        (
            "I stale",
            GuardId::G9,
            Box::new(move |s| s.input = Observed::new(Input::Clear, old)),
        ),
        (
            "K unknown",
            GuardId::G7,
            Box::new(move |s| s.custody = Observed::new(CustodyAxis::Unknown(u()), NOW)),
        ),
        (
            "K stale",
            GuardId::G9,
            Box::new(move |s| s.custody = Observed::new(s.custody.value.clone(), old)),
        ),
        (
            "O unknown",
            GuardId::G8,
            Box::new(move |s| s.owned = Observed::new(OwnedAxis::Unknown(u()), NOW)),
        ),
        (
            "O stale",
            GuardId::G9,
            Box::new(move |s| {
                s.owned = Observed::new(OwnedAxis::Known, Timestamp(NOW.0 - 20 * MIN))
            }),
        ),
        (
            "H stale",
            GuardId::G9,
            Box::new(move |s| s.hosting = Observed::new(s.hosting.value.clone(), old)),
        ),
        (
            "schema skew",
            GuardId::G9,
            Box::new(move |s| s.schema_skew = Some(u())),
        ),
        (
            "pane kind unreadable",
            GuardId::G3,
            Box::new(|s| s.pane_kind = PaneKind::Unreadable),
        ),
    ];
    for (name, id, m) in &cases {
        let mut s = closable();
        m(&mut s);
        let r = report(&s);
        assert!(!r.verdict().is_true(), "{name}: the verdict stayed True");
        assert!(
            r.guards.iter().any(|g| g.id == *id && !g.value.is_true()),
            "{name}: expected {id:?} to refuse: {:?}",
            r.guards
        );
        assert!(r.close_proof().is_none(), "{name}");
        let dec = crate::decide::decide(&s, &[done("w")], &dials(), X, NOW);
        assert!(!dec.action.is_close(), "{name}: {dec:?}");
    }
    // Control: the unmutated state closes, so every refusal above is the mutation's.
    assert!(
        crate::decide::decide(&closable(), &[done("w")], &dials(), X, NOW)
            .action
            .is_close()
    );
}

#[test]
fn a_non_closable_pane_in_its_grace_is_not_grace_pending() {
    let mut s = closable();
    s.activity = Observed::new(
        Activity::Idle {
            since_turn_end: NOW,
            grace_from: Timestamp(NOW.0 - 5 * MIN),
        },
        NOW,
    );
    s.pane_kind = PaneKind::Steward;
    let r = report(&s);
    assert!(!r.cleared_but_for_grace());
    let dec = crate::decide::decide(&s, &[done("w")], &dials(), X, NOW);
    assert!(!matches!(
        dec.action,
        Action::Observe {
            why: crate::model::ObserveWhy::GraceRunning { .. }
        }
    ));
}
