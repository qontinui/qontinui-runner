//! Shared fixtures for the guard and decide tests: a session that is, in every
//! axis, a clean finished one the guard clears.

use crate::model::{Dials, HostCaps, ItemKind, ItemState, Owner, WorkItemView};
use crate::state::{
    Activity, Custody, CustodyAxis, Declared, FinishKind, Hosting, Input, Instance, Mode, Observed,
    OwnedAxis, PaneKind, PaneShape, Parked, SelfConts, SessionState, Timestamp, Tri,
};

pub const NOW: Timestamp = Timestamp(100_000_000);
pub const MIN: u64 = 60_000;

pub fn instance() -> Instance {
    Instance {
        pid: 11,
        proc_start: 111,
    }
}

/// A finished, quiet, clean, unattended session whose grace has long run out.
pub fn closable() -> SessionState {
    let mut s = SessionState::unread("sess", NOW);
    s.hosting = Observed::new(
        Hosting::Live {
            instance: instance(),
            config_dir: "/c".into(),
            cc_version: "2.1.0".into(),
            pane_shape: PaneShape::Holder,
            registry_seen: true,
        },
        NOW,
    );
    s.activity = Observed::new(
        Activity::Idle {
            since_turn_end: Timestamp(NOW.0 - 30 * MIN),
            grace_from: Timestamp(NOW.0 - 30 * MIN),
        },
        NOW,
    );
    s.selfcont = Observed::new(SelfConts::Known(vec![]), NOW);
    s.parked = Observed::new(Parked::None, NOW);
    s.input = Observed::new(Input::Clear, NOW);
    s.work = Observed::new(
        Declared::Finished {
            at: Timestamp(NOW.0 - 40 * MIN),
            kind: FinishKind::Done,
            items: vec![],
        },
        NOW,
    );
    s.owned = Observed::new(OwnedAxis::Known, NOW);
    s.custody = Observed::new(
        CustodyAxis::Known(vec![(
            "/w".into(),
            Custody::Clean {
                evidence: "e".into(),
            },
        )]),
        NOW,
    );
    s.mode = Mode::Unattended;
    s.pane_kind = PaneKind::Terminal;
    s.revived = Tri::False;
    s.coord_finished = Tri::True;
    s
}

pub fn item(id: &str, kind: ItemKind, state: ItemState) -> WorkItemView {
    WorkItemView {
        id: id.into(),
        kind,
        state,
        inside_first_verdict_hold: false,
        fact: format!("{id} is owed"),
    }
}

pub fn owed(id: &str) -> WorkItemView {
    item(
        id,
        ItemKind::CiRed,
        ItemState::Open {
            owner: Owner::AuthorSession,
            deadline: Timestamp(NOW.0 + 60 * MIN),
        },
    )
}

pub fn done(id: &str) -> WorkItemView {
    item(id, ItemKind::PrWatch, ItemState::Done)
}

pub fn dials() -> Dials {
    Dials::permissive()
}

pub const X: HostCaps = HostCaps::BUILD_X;
pub const INTERIM: HostCaps = HostCaps::INTERIM;
