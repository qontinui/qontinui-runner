//! Acceptance for plan Phase 3's explorer that does not need a live app: the
//! 422s, D4's predicate and its ordering, stop-reason selection (unreachable
//! vs exhausted above all), the accounting, and the row's CHECKs.
//!
//! The loop is driven against [`SimApp`], a simulated app whose producer
//! mirrors Phase 1's rules with the REAL affordance extraction and D4 reason
//! (`node::extract_affordances`, `frontier::d4_reason`), so what the explorer
//! is tested against is what the live producer would have written.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use qontinui_types::journey::{
    FrontierEntry, FrontierReason, JourneyEdgeObservation, JourneyNode, JourneyTrigger,
};
use serde_json::{json, Value};

use super::*;
use crate::journey::capture::{BarrierMiss, BARRIER_TIMEOUT};
use crate::journey::frontier::{d4_reason, d4_reason_is_observation, effect_str};
use crate::journey::node::{extract_affordances, is_navigation_affordance, unknown_node};
use crate::journey::test_fixtures::writing;

// ---------------------------------------------------------------------------
// Elements
// ---------------------------------------------------------------------------

fn link(id: &str) -> Value {
    json!({ "id": id, "type": "link", "role": "link", "label": id, "actions": ["click"] })
}

fn tab(id: &str) -> Value {
    json!({ "id": id, "type": "button", "role": "tab", "label": id, "actions": ["click"] })
}

/// The D4 trap: a "Delete" BUTTON rendered inside a sidebar. ui-bridge's
/// `isNavigationElement` calls it navigation (parent context `sidebar`,
/// semantic type containing `nav`); D4 does not.
fn sidebar_delete() -> Value {
    json!({
        "id": "delete", "type": "button", "role": "button", "label": "Delete",
        "actions": ["click"], "parentContext": "sidebar", "semanticType": "nav-item"
    })
}

/// A declared-`write` affordance.
fn save() -> Value {
    json!({
        "id": "save", "type": "button", "role": "button", "label": "Save",
        "customActions": [{ "id": "save", "effect": "write" }]
    })
}

/// A LINK that declares itself destructive: a declared effect overrides
/// navigation-ness (D4 rule 1 before rule 3).
fn destructive_link() -> Value {
    json!({
        "id": "purge", "type": "link", "role": "link", "label": "Purge",
        "actions": ["click"], "customActions": [{ "id": "purge", "effect": "destructive" }]
    })
}

/// A declared-`read` affordance (custom actions only, all `read`).
fn refresh() -> Value {
    json!({
        "id": "refresh", "type": "button", "role": "button", "label": "Refresh",
        "customActions": [{ "id": "refresh", "effect": "read" }]
    })
}

// ---------------------------------------------------------------------------
// The simulated app
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Page {
    node: JourneyNode,
    elements: Vec<Value>,
}

struct Pending {
    from: JourneyNode,
    fingerprint: Option<String>,
    role: Option<String>,
    effect: Option<qontinui_types::ir::IrEffect>,
    action: String,
    navigation: bool,
}

/// A simulated app plus a producer that follows Phase 1's rules.
struct SimApp {
    pages: BTreeMap<String, Page>,
    /// `(page, element id) -> page`.
    transitions: BTreeMap<(String, String), String>,
    /// `url -> page`.
    urls: BTreeMap<String, String>,
    current: Option<String>,
    /// The app stops answering after this many snapshots.
    alive_snapshots: Option<usize>,
    snapshots: usize,
    /// Every activation, in order: `(element id, action)`.
    activations: Vec<(String, String)>,
    navigations: usize,
    /// Simulated time per action.
    tick: Duration,
    clock: Duration,
    edges: Vec<JourneyEdgeObservation>,
    frontier: Vec<FrontierEntry>,
    pending: Option<Pending>,
    seq: u32,
    /// Snapshots the producer resolved (the barrier's monotonic count), and
    /// the node of the last one.
    resolved: u64,
    resolved_node: Option<JourneyNode>,
    /// From this many snapshots on, the producer never receives one (a
    /// dropped event): the barrier's node goes stale.
    drop_snapshot_events_from: Option<usize>,
    /// From this many snapshots on, a snapshot never answers.
    hang_snapshots_from: Option<usize>,
    /// Elements that vanish the moment they are clicked.
    vanish_on_click: BTreeSet<String>,
    /// Elements whose NEXT click the app fails (once): the element stays,
    /// and the producer keeps its row as `activation_failed`.
    fail_once: BTreeSet<String>,
    /// `id -> element`: what an element turns into the moment it is clicked
    /// (the same id, a different UI).
    change_on_click: BTreeMap<String, Value>,
    /// The ledger read hits its bound / has unparseable rows.
    truncated: bool,
    rows_unparseable: u64,
    /// Action events the producer's queue drops.
    drop_actions: bool,
    /// At this snapshot count, the app is suddenly on this page (a session
    /// that expired between two looks).
    jump_at_snapshot: Option<(usize, String)>,
    events: EdgeEvents,
    /// `page -> components` the page's snapshot carries.
    components: BTreeMap<String, Vec<Value>>,
    /// From this many snapshots on, the APP answers a snapshot with a
    /// failure (it is reachable; it gave no snapshot).
    snapshot_refused_from: Option<usize>,
    /// How long the producer takes to answer a barrier (tokio time). Longer
    /// than the barrier's deadline → it times out at the deadline.
    barrier_delay: Duration,
    /// When set, the run's elapsed time also counts tokio time since this
    /// instant (for tests where waits, not actions, spend the budget).
    tokio_clock: Option<tokio::time::Instant>,
    /// Pending edges closed so far (the producer's edge writes, in order).
    edges_closed: usize,
    /// From this many edge writes on, the write FAILS: no edge, and the
    /// frontier row it would have cleared stays.
    edge_writes_fail_from: Option<usize>,
    /// Whether a failed edge write is counted on the health counters (it
    /// is, live; `false` simulates a loss nothing counted).
    write_failures_counted: bool,
    /// The health counters' failed / not-attempted writes.
    write_failures: u64,
    /// Ledger reads so far, and from which read on the ledger reports this
    /// state instead of `writing`.
    reads: usize,
    ledger_state_from_read: Option<(usize, qontinui_types::journey::LedgerState)>,
    /// How long the ledger read with this index (0-based) takes (tokio time).
    ledger_delays: BTreeMap<usize, Duration>,
    /// The app's answer to the activation with this index (0-based), to the
    /// start-URL navigation, or to snapshots from this count on, cannot be
    /// read (`failure_origin: "unknown"`).
    activation_unknown_at: Option<usize>,
    navigation_unknown: bool,
    snapshot_unknown_from: Option<usize>,
    /// Every snapshot answer carries an authentication refusal.
    snapshot_auth_wall: bool,
    /// The producer's cursor position: the node of the last snapshot it
    /// resolved — what a NAVIGATION's edge runs from — until forgotten.
    cursor_last: Option<JourneyNode>,
    /// `forget_position` calls, and whether they fail (the worker is gone).
    forgets: usize,
    forget_fails: bool,
    /// Pending edges a forget closed as unobserved.
    unobserved_closes: usize,
    /// Nodes the app was snapshotted at (and the producer resolved), and
    /// every frontier row it exposed there, since [`run`] last cleared them.
    stood_on: BTreeSet<String>,
    exposed: BTreeSet<EntryKey>,
}

fn page_node(spec: &str) -> JourneyNode {
    JourneyNode::new(Some(spec.to_string()), ["s".to_string()], None, None)
}

impl SimApp {
    /// home --a--> A --home-from-a--> home
    /// home --b--> B --b-tab2--> B2
    /// home also shows the sidebar Delete (undeclared) and Save (write).
    fn site() -> Self {
        let mut app = SimApp::empty();
        app.page(
            "home",
            page_node("home"),
            vec![link("a"), link("b"), sidebar_delete(), save()],
        );
        app.page("A", page_node("A"), vec![link("home-from-a")]);
        app.page("B", page_node("B"), vec![tab("b-tab2")]);
        app.page("B2", page_node("B2"), vec![]);
        app.page("deleted", page_node("deleted"), vec![]);
        app.go("home", "a", "A");
        app.go("home", "b", "B");
        app.go("home", "delete", "deleted");
        app.go("home", "save", "home");
        app.go("A", "home-from-a", "home");
        app.go("B", "b-tab2", "B2");
        app.urls.insert("/home".into(), "home".into());
        app
    }

    fn empty() -> Self {
        SimApp {
            pages: BTreeMap::new(),
            transitions: BTreeMap::new(),
            urls: BTreeMap::new(),
            current: None,
            alive_snapshots: None,
            snapshots: 0,
            activations: Vec::new(),
            navigations: 0,
            tick: Duration::from_millis(10),
            clock: Duration::ZERO,
            edges: Vec::new(),
            frontier: Vec::new(),
            pending: None,
            seq: 0,
            resolved: 0,
            resolved_node: None,
            drop_snapshot_events_from: None,
            hang_snapshots_from: None,
            vanish_on_click: BTreeSet::new(),
            fail_once: BTreeSet::new(),
            change_on_click: BTreeMap::new(),
            truncated: false,
            rows_unparseable: 0,
            drop_actions: false,
            jump_at_snapshot: None,
            events: EdgeEvents::default(),
            components: BTreeMap::new(),
            snapshot_refused_from: None,
            barrier_delay: Duration::ZERO,
            tokio_clock: None,
            edges_closed: 0,
            edge_writes_fail_from: None,
            write_failures_counted: true,
            write_failures: 0,
            reads: 0,
            ledger_state_from_read: None,
            ledger_delays: BTreeMap::new(),
            activation_unknown_at: None,
            navigation_unknown: false,
            snapshot_unknown_from: None,
            snapshot_auth_wall: false,
            cursor_last: None,
            forgets: 0,
            forget_fails: false,
            unobserved_closes: 0,
            stood_on: BTreeSet::new(),
            exposed: BTreeSet::new(),
        }
    }

    /// Record every frontier row now open at a node the app was snapshotted
    /// at — what the app EXPOSED to the run.
    fn expose(&mut self) {
        for row in &self.frontier {
            if self.stood_on.contains(&row.node_key) {
                self.exposed
                    .insert((row.node_key.clone(), row.affordance_fingerprint.clone()));
            }
        }
    }

    /// Count tokio time toward the run's elapsed time from now on.
    fn on_tokio_clock(&mut self) {
        self.tokio_clock = Some(tokio::time::Instant::now());
    }

    fn page(&mut self, name: &str, node: JourneyNode, elements: Vec<Value>) {
        self.pages.insert(name.into(), Page { node, elements });
    }

    fn go(&mut self, from: &str, element: &str, to: &str) {
        self.transitions
            .insert((from.into(), element.into()), to.into());
    }

    /// Count one action event; `false` = the queue dropped it, so the
    /// producer never opens its pending edge and no edge is ever written.
    fn count_event(&mut self) -> bool {
        if self.drop_actions {
            self.events.dropped += 1;
            false
        } else {
            self.events.enqueued += 1;
            true
        }
    }

    fn alive(&self) -> bool {
        self.alive_snapshots.is_none_or(|n| self.snapshots < n)
    }

    /// The page as the app's snapshot carries it (the SDK's `data`).
    fn snapshot_json(&self) -> Value {
        let current = self.current.as_ref().expect("on a page");
        let page = &self.pages[current];
        let components = self.components.get(current).cloned().unwrap_or_default();
        json!({ "elements": page.elements, "components": components })
    }

    fn ts(&mut self) -> String {
        self.seq += 1;
        format!(
            "2026-10-28T10:{:02}:{:02}.000000Z",
            self.seq / 60,
            self.seq % 60
        )
    }

    /// The producer's snapshot side: close the pending edge, upsert the
    /// frontier (D4 reason; never re-adding an affordance explored from here).
    fn produce_snapshot(&mut self) {
        let page = self.pages[self.current.as_ref().unwrap()].clone();
        let node = page.node.clone();
        self.cursor_last = Some(node.clone());
        let closing = self.pending.take();
        let write_fails = closing.is_some()
            && self
                .edge_writes_fail_from
                .is_some_and(|n| self.edges_closed >= n);
        if closing.is_some() {
            self.edges_closed += 1;
        }
        if write_fails {
            // The edge write failed: no edge, and the row it would have
            // cleared stays open.
            if self.write_failures_counted {
                self.write_failures += 1;
            }
        }
        if let Some(p) = closing.filter(|_| !write_fails) {
            let outcome = if p.from.key() == node.key() {
                "no_change"
            } else {
                "changed"
            };
            let observed_at = self.ts();
            let id = format!("00000000-0000-4000-8000-{:012}", self.seq);
            let trigger = if p.navigation {
                json!({ "actionType": p.action, "targetFingerprint": null, "targetRole": null,
                        "declaredEffect": null, "navigationTrigger": "push", "chokePoint": "navigation" })
            } else {
                json!({ "actionType": p.action, "targetFingerprint": p.fingerprint,
                        "targetRole": p.role, "declaredEffect": p.effect.map(effect_str),
                        "navigationTrigger": "affordance", "chokePoint": "sdk_element_action" })
            };
            let edge: JourneyEdgeObservation = serde_json::from_value(json!({
                "id": id, "observedAt": observed_at, "appId": "app", "appVersion": null,
                "runnerBuildId": "b", "runnerInstance": "primary", "runId": "run",
                "runKind": "explorer", "fromNode": p.from, "toNode": node,
                "trigger": trigger, "outcome": outcome,
            }))
            .expect("sim edge parses");
            if let Some(fp) = &p.fingerprint {
                let from_key = p.from.key();
                self.frontier
                    .retain(|f| !(f.node_key == from_key && &f.affordance_fingerprint == fp));
            }
            self.edges.push(edge);
        }
        let index = extract_affordances(&self.snapshot_json());
        for a in index.distinct() {
            let explored = self.edges.iter().any(|e| {
                e.from_node.key() == node.key()
                    && e.trigger.target_fingerprint.as_deref() == Some(a.fingerprint.as_str())
            });
            if explored {
                continue;
            }
            let reason = d4_reason(a.declared_effect, a.navigation);
            if let Some(row) = self
                .frontier
                .iter_mut()
                .find(|f| f.node_key == node.key() && f.affordance_fingerprint == a.fingerprint)
            {
                // FRONTIER_UPSERT_SQL (N5): an observation moves the reason
                // only between the D4 observation reasons — never out of
                // `activation_failed` / `budget_exhausted`.
                if d4_reason_is_observation(row.reason) {
                    row.reason = reason;
                }
                continue;
            }
            let seen = self.ts();
            self.frontier.push(
                serde_json::from_value(json!({
                    "appId": "app", "nodeKey": node.key(), "node": node,
                    "affordanceFingerprint": a.fingerprint, "affordanceRole": a.role,
                    "declaredEffect": a.declared_effect.map(effect_str), "reason": reason.as_str(),
                    "firstSeenAt": seen, "lastSeenAt": seen, "lastSeenRunId": null,
                }))
                .expect("sim frontier parses"),
            );
        }
    }
}

#[async_trait::async_trait]
impl ExploreDriver for SimApp {
    async fn snapshot(&mut self) -> Result<Value, DriveFailure> {
        if !self.alive() {
            return Err(DriveFailure::Unreachable("connection refused".into()));
        }
        if self
            .hang_snapshots_from
            .is_some_and(|n| self.snapshots >= n)
        {
            std::future::pending::<()>().await;
        }
        if self.snapshot_auth_wall {
            return Err(DriveFailure::AuthWall("UNAUTHORIZED".into()));
        }
        if self
            .snapshot_unknown_from
            .is_some_and(|n| self.snapshots >= n)
        {
            return Err(DriveFailure::OutcomeUnknown("snapshot timed out".into()));
        }
        if self
            .snapshot_refused_from
            .is_some_and(|n| self.snapshots >= n)
        {
            return Err(DriveFailure::AppAnswered(
                "\"ELEMENT_REGISTRY_EMPTY\"".into(),
            ));
        }
        if let Some((n, page)) = &self.jump_at_snapshot {
            if *n == self.snapshots {
                self.current = Some(page.clone());
                self.pending = None;
            }
        }
        let dropped = self
            .drop_snapshot_events_from
            .is_some_and(|n| self.snapshots >= n);
        self.snapshots += 1;
        if !dropped {
            self.produce_snapshot();
            self.resolved += 1;
            self.resolved_node = self.current.as_ref().map(|c| self.pages[c].node.clone());
            if let Some(n) = &self.resolved_node {
                self.stood_on.insert(n.key());
            }
            self.expose();
        }
        // The live answer's shape: the SDK's `{success, data}` envelope, as
        // `handle_snapshot` returns it — so the explorer is tested reading the
        // layer it reads live.
        Ok(json!({ "success": true, "data": self.snapshot_json() }))
    }

    async fn activate(
        &mut self,
        element_id: &str,
        action: &str,
    ) -> Result<ActResult, DriveFailure> {
        if !self.alive() {
            return Err(DriveFailure::Unreachable("connection refused".into()));
        }
        self.clock += self.tick;
        if self.activation_unknown_at == Some(self.activations.len()) {
            // It may have acted; its answer could not be read. Here it did
            // not: the app stays put and no edge opens.
            self.activations.push((element_id.into(), action.into()));
            return Err(DriveFailure::OutcomeUnknown("action timed out".into()));
        }
        let queued = self.count_event();
        let page = self.current.clone().unwrap();
        if self.vanish_on_click.contains(element_id) {
            // The app fails the click; the producer keeps the row as
            // `activation_failed` (EDGE_WRITE_SQL's `error` branch).
            let before = extract_affordances(&self.snapshot_json());
            if let Some(el) = before.elements.get(element_id) {
                let (node_key, fp) = (
                    self.pages[&page].node.key(),
                    el.affordance.fingerprint.clone(),
                );
                for row in &mut self.frontier {
                    if row.node_key == node_key && row.affordance_fingerprint == fp {
                        row.reason = FrontierReason::ActivationFailed;
                    }
                }
            }
            self.pages
                .get_mut(&page)
                .unwrap()
                .elements
                .retain(|e| e["id"] != element_id);
        }
        if self.fail_once.remove(element_id) {
            // The app answered that it failed: the producer writes an `error`
            // edge and keeps the row as `activation_failed` (EDGE_WRITE_SQL)
            // — unless the action event never reached it (dropped).
            let before = extract_affordances(&self.snapshot_json());
            if let Some(el) = before.elements.get(element_id).filter(|_| queued) {
                let (node_key, fp) = (
                    self.pages[&page].node.key(),
                    el.affordance.fingerprint.clone(),
                );
                for row in &mut self.frontier {
                    if row.node_key == node_key && row.affordance_fingerprint == fp {
                        row.reason = FrontierReason::ActivationFailed;
                    }
                }
            }
            self.activations.push((element_id.into(), action.into()));
            return Ok(ActResult::Failed("the app failed the click".into()));
        }
        if let Some(changed) = self.change_on_click.get(element_id).cloned() {
            for e in &mut self.pages.get_mut(&page).unwrap().elements {
                if e["id"] == element_id {
                    *e = changed.clone();
                }
            }
        }
        let snapshot = self.snapshot_json();
        let index = extract_affordances(&snapshot);
        // The element is gone: the app answers that it failed, never panics.
        let Some(el) = index.elements.get(element_id) else {
            self.activations.push((element_id.into(), action.into()));
            return Ok(ActResult::Failed("no such element".into()));
        };
        if queued {
            self.pending = Some(Pending {
                from: self.pages[&page].node.clone(),
                fingerprint: Some(el.affordance.fingerprint.clone()),
                role: el.affordance.role.clone(),
                effect: el.affordance.declared_effect,
                action: action.to_string(),
                navigation: false,
            });
        }
        self.activations.push((element_id.into(), action.into()));
        if let Some(to) = self.transitions.get(&(page, element_id.to_string())) {
            self.current = Some(to.clone());
        }
        Ok(ActResult::Acted)
    }

    async fn navigate(&mut self, url: &str) -> Result<ActResult, DriveFailure> {
        if !self.alive() {
            return Err(DriveFailure::Unreachable("connection refused".into()));
        }
        self.clock += self.tick;
        if self.navigation_unknown {
            self.navigations += 1;
            return Err(DriveFailure::OutcomeUnknown("navigation timed out".into()));
        }
        let queued = self.count_event();
        self.navigations += 1;
        let Some(to) = self.urls.get(url).cloned() else {
            return Ok(ActResult::Failed("no such page".into()));
        };
        // As the producer: the edge runs from the cursor's last resolved node
        // (the unknown node when there is none), not from where the app is.
        self.pending = queued.then(|| Pending {
            from: self.cursor_last.clone().unwrap_or_else(unknown_node),
            fingerprint: None,
            role: None,
            effect: None,
            action: "navigate".into(),
            navigation: true,
        });
        self.current = Some(to);
        Ok(ActResult::Acted)
    }

    async fn current_node(&mut self, deadline: Duration) -> Result<NodeReading, BarrierMiss> {
        // As `capture::barrier`: the wait is capped at BARRIER_TIMEOUT.
        let wait = deadline.min(BARRIER_TIMEOUT);
        if self.barrier_delay > wait {
            tokio::time::sleep(wait).await;
            return Err(BarrierMiss::TimedOut);
        }
        if !self.barrier_delay.is_zero() {
            tokio::time::sleep(self.barrier_delay).await;
        }
        // What the producer last RESOLVED — stale when a snapshot was dropped.
        Ok(NodeReading {
            node: self.resolved_node.clone(),
            snapshots: self.resolved,
        })
    }

    async fn ledger_rows(&mut self) -> Result<LedgerRows, String> {
        self.expose();
        let read = self.reads;
        self.reads += 1;
        if let Some(delay) = self.ledger_delays.get(&read) {
            tokio::time::sleep(*delay).await;
        }
        let mut ledger = writing();
        if let Some((from, state)) = self.ledger_state_from_read {
            if read >= from {
                ledger.state = state;
                ledger.detail = "a journey write failed".into();
            }
        }
        Ok(LedgerRows {
            edges: self.edges.clone(),
            frontier: self.frontier.clone(),
            ledger,
            truncated: self.truncated,
            rows_unparseable: self.rows_unparseable,
        })
    }

    fn edge_events(&self) -> EdgeEvents {
        self.events
    }

    fn ledger_write_failures(&self) -> u64 {
        self.write_failures
    }

    async fn forget_position(&mut self) -> Result<(), BarrierMiss> {
        self.forgets += 1;
        if self.forget_fails {
            return Err(BarrierMiss::WorkerGone);
        }
        self.cursor_last = None;
        // As the producer: a pending edge closes as unobserved (no edge row
        // here — the simulated ledger keeps only observed edges).
        if self.pending.take().is_some() {
            self.unobserved_closes += 1;
        }
        Ok(())
    }

    fn elapsed(&self) -> Duration {
        self.clock + self.tokio_clock.map_or(Duration::ZERO, |t0| t0.elapsed())
    }
}

fn request(
    start: ExploreStart,
    max_actions: u32,
    max_depth: u32,
    max_wall_clock_s: u32,
) -> ExploreRequest {
    ExploreRequest {
        start,
        budget: ExploreBudget {
            max_actions,
            max_depth,
            max_wall_clock_s,
        },
    }
}

fn home() -> ExploreStart {
    ExploreStart::Url("/home".into())
}

/// One run; what the app exposed is measured afresh for it.
async fn run(app: &mut SimApp, req: &ExploreRequest) -> LoopOutcome {
    app.stood_on.clear();
    app.exposed.clear();
    explore(app, req).await.expect("no runner-side abort")
}

/// What an earned exhaustion's measures read: confirmed, nothing open, the
/// closing look completed, and no other flag.
fn earned() -> Unsettled {
    Unsettled {
        exhaustion_unconfirmed: Some(false),
        open_at_confirmation: Some(0),
        closing_look_failed: Some(false),
        ..Default::default()
    }
}

fn by_reason(o: &LoopOutcome) -> BTreeMap<String, u32> {
    o.accounting
        .remaining_by_reason(o.stopped_because, &o.budget_left, &o.attempts_finished)
}

fn remaining_total(o: &LoopOutcome) -> u32 {
    by_reason(o).values().sum()
}

/// Nothing vanishes, measured against the APP: every frontier row the
/// simulated app exposed at a node it was snapshotted at during the run is
/// accounted for — activated or remaining — and nothing else is; and
/// `affordancesActivated + Σ remaining = affordancesSeen`.
fn nothing_vanishes(o: &LoopOutcome, app: &SimApp) {
    let seen: BTreeSet<EntryKey> = o.accounting.seen.keys().cloned().collect();
    assert_eq!(
        seen, app.exposed,
        "the run's accounting must cover exactly the frontier the app exposed"
    );
    assert_eq!(
        o.accounting.activated_count() + remaining_total(o),
        o.accounting.seen.len() as u32
    );
}

/// The ids of every element the run activated (navigation excluded).
fn activated_ids(app: &SimApp) -> BTreeSet<String> {
    app.activations.iter().map(|(id, _)| id.clone()).collect()
}

// ---------------------------------------------------------------------------
// The request: every budget field is required
// ---------------------------------------------------------------------------

#[test]
fn a_request_missing_any_budget_field_is_a_422_naming_it() {
    let full = json!({
        "start": { "url": "/admin/coord" },
        "max_actions": 60, "max_depth": 3, "max_wall_clock_s": 120
    });
    assert!(parse_explore_request(full.to_string().as_bytes()).is_ok());
    for field in ["max_actions", "max_depth", "max_wall_clock_s", "start"] {
        let mut body = full.clone();
        body.as_object_mut().unwrap().remove(field);
        let Refusal(status, msg) = parse_explore_request(body.to_string().as_bytes())
            .expect_err("a missing field is refused");
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        assert!(msg.contains(field), "{field}: {msg}");
    }
}

#[test]
fn the_request_has_no_unset_or_out_of_range_dial() {
    let base = || {
        json!({ "start": { "node": "home#s" }, "max_actions": 5, "max_depth": 0,
                "max_wall_clock_s": 30 })
    };
    let req = parse_explore_request(base().to_string().as_bytes()).unwrap();
    assert_eq!(req.start, ExploreStart::Node("home#s".into()));
    assert_eq!(
        req.budget.max_depth, 0,
        "depth 0 = explore the start node only"
    );
    let refused = |patch: Value| {
        let mut body = base();
        for (k, v) in patch.as_object().unwrap() {
            body[k] = v.clone();
        }
        parse_explore_request(body.to_string().as_bytes())
            .unwrap_err()
            .0
    };
    for patch in [
        json!({ "max_actions": 0 }),
        json!({ "max_wall_clock_s": 0 }),
        json!({ "max_actions": null }),
        json!({ "max_depth": -1 }),
        json!({ "max_depth": 1.5 }),
        json!({ "max_actions": 3_000_000_000u64 }),
        json!({ "max_actions": "60" }),
        json!({ "start": { "url": "/a", "node": "b#s" } }),
        json!({ "start": { "url": "" } }),
        json!({ "start": "/admin" }),
        json!({ "budget": {} }),
    ] {
        assert_eq!(
            refused(patch.clone()),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{patch}"
        );
    }
    assert_eq!(
        parse_explore_request(b"not json").unwrap_err().0,
        StatusCode::BAD_REQUEST
    );
}

// ---------------------------------------------------------------------------
// D4
// ---------------------------------------------------------------------------

#[test]
fn d4_role_and_type_subset_is_exactly_the_plans() {
    for role in ["link", "menuitem", "tab"] {
        assert!(is_navigation_affordance(Some(role), None), "{role}");
    }
    for ty in ["link", "menuitem"] {
        assert!(is_navigation_affordance(None, Some(ty)), "{ty}");
    }
    // Not in the subset — including everything isNavigationElement keys on.
    for role in [
        "button",
        "navigation",
        "menu",
        "menubar",
        "tablist",
        "option",
        "treeitem",
    ] {
        assert!(!is_navigation_affordance(Some(role), None), "{role}");
    }
    for ty in ["tab", "button", "nav", "sidebar"] {
        assert!(!is_navigation_affordance(None, Some(ty)), "{ty}");
    }
}

#[test]
fn d4_is_evaluated_in_order_on_the_live_element() {
    let snapshot = json!({ "elements": [
        sidebar_delete(), save(), destructive_link(), refresh(), link("nav"), tab("t"),
        { "id": "mi", "type": "menuitem", "actions": ["click"] },
        { "id": "linkless", "type": "link", "role": "link", "customActions": [{ "id": "go" }] },
    ]});
    let idx = extract_affordances(&snapshot);
    let decide = |id: &str| live_activation(&idx, id);
    // 1. a declared write / destructive is never activated, whatever the role.
    assert_eq!(decide("save"), Err(FrontierReason::EffectWrite));
    assert_eq!(decide("purge"), Err(FrontierReason::EffectDestructive));
    // 2. a declared read is activated, through its read action.
    assert_eq!(decide("refresh"), Ok("refresh".into()));
    // 3. no declaration + explicit navigation role / type → click.
    assert_eq!(decide("nav"), Ok("click".into()));
    assert_eq!(decide("t"), Ok("click".into()));
    assert_eq!(decide("mi"), Ok("click".into()));
    // 4. anything else is effect_undeclared — the sidebar Delete above all.
    assert_eq!(decide("delete"), Err(FrontierReason::EffectUndeclared));
    // A navigation element that offers no click has nothing safe to fire.
    assert_eq!(decide("linkless"), Err(FrontierReason::EffectUndeclared));
    // An element not on the page is not activated.
    assert_eq!(decide("gone"), Err(FrontierReason::NotYetActivated));
}

/// An observed edge home → A taken by `run_kind`, through a trigger with
/// this declared effect and role.
fn observed(run_kind: &str, effect: Option<&str>, role: Option<&str>) -> JourneyEdgeObservation {
    let trigger: JourneyTrigger = serde_json::from_value(json!({
        "actionType": "click", "targetFingerprint": "fp", "targetRole": role,
        "declaredEffect": effect, "navigationTrigger": "affordance",
        "chokePoint": "element_action"
    }))
    .unwrap();
    serde_json::from_value(json!({
        "id": "00000000-0000-4000-8000-000000000001", "observedAt": "2026-10-28T10:00:00.000000Z",
        "appId": "app", "appVersion": null, "runnerBuildId": "b", "runnerInstance": "primary",
        "runId": "run", "runKind": run_kind, "fromNode": page_node("home"),
        "toNode": page_node("A"), "trigger": trigger, "outcome": "changed",
    }))
    .unwrap()
}

#[test]
fn an_observed_unsafe_edge_is_never_on_a_route() {
    for kind in ["agent_action", "passive_session", "explorer"] {
        assert!(replayable(&observed(kind, None, Some("link"))), "{kind}");
        assert!(
            replayable(&observed(kind, Some("read"), Some("button"))),
            "{kind}"
        );
        assert!(
            !replayable(&observed(kind, Some("destructive"), Some("link"))),
            "{kind}"
        );
        assert!(
            !replayable(&observed(kind, Some("write"), Some("tab"))),
            "{kind}"
        );
    }
    assert!(
        !replayable(&observed("agent_action", None, Some("button"))),
        "an agent's undeclared click"
    );
}

#[test]
fn replay_agrees_with_activation_on_a_type_only_link() {
    // `<a onClick>`: type `link`, no role. Activation passes D4 on its TYPE.
    let el = json!({ "id": "x", "type": "link", "label": "x", "actions": ["click"] });
    let snapshot = json!({ "elements": [el] });
    let index = extract_affordances(&snapshot);
    assert_eq!(index.elements["x"].affordance.role, None);
    assert!(live_activation(&index, "x").is_ok());
    // The edge the EXPLORER took through it records no role and no type, yet
    // the explorer passed D4 on the live element when it took it: routable.
    assert!(replayable(&observed("explorer", None, None)));
    // An agent's edge through an element of unrecorded type is not — the
    // record cannot show the type arm (fail-closed).
    assert!(!replayable(&observed("agent_action", None, None)));
    assert!(!replayable(&observed("passive_session", None, None)));
}

#[tokio::test]
async fn the_sidebar_delete_and_every_unsafe_affordance_are_never_activated() {
    let mut app = SimApp::site();
    app.pages
        .get_mut("A")
        .unwrap()
        .elements
        .extend([destructive_link(), refresh()]);
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierExhausted,
        "{}",
        o.stop_explanation
    );
    let ids = activated_ids(&app);
    for unsafe_id in ["delete", "save", "purge"] {
        assert!(
            !ids.contains(unsafe_id),
            "{unsafe_id} was activated: {ids:?}"
        );
    }
    assert!(
        ids.contains("refresh"),
        "a declared read IS activated: {ids:?}"
    );
    // And independently from the "ledger": no explorer edge carries an
    // unsafe trigger.
    for e in &app.edges {
        if let Some(fp) = e.trigger.target_fingerprint.as_deref() {
            assert!(replayable(e), "unsafe edge {fp}");
        }
    }
}

// ---------------------------------------------------------------------------
// Stop reasons and accounting
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_fully_explored_site_is_frontier_exhausted_and_nothing_vanishes() {
    let mut app = SimApp::site();
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierExhausted,
        "{}",
        o.stop_explanation
    );
    assert_eq!(
        activated_ids(&app),
        ["a", "b", "b-tab2", "home-from-a"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    );
    let edges_added = o.accounting.activated_count();
    assert_eq!(edges_added, 4);
    let by_reason =
        o.accounting
            .remaining_by_reason(o.stopped_because, &o.budget_left, &o.attempts_finished);
    assert_eq!(by_reason["effect_undeclared"], 1, "the sidebar Delete");
    assert_eq!(by_reason["effect_write"], 1, "Save");
    assert_eq!(by_reason["not_yet_activated"], 0);
    assert_eq!(
        by_reason.len(),
        FrontierReason::ALL.len(),
        "every reason, zeros included"
    );
    // Nothing vanishes: activated + remaining = every affordance seen.
    assert_eq!(
        edges_added + remaining_total(&o),
        o.accounting.seen.len() as u32
    );
    assert_eq!(o.accounting.seen.len(), 6);
    // Spend is real: the navigation and the replay steps are actions too.
    assert!(o.spent_actions > edges_added);
    assert_eq!(o.spent_depth, 1);
    // edgesAdded counts every action event the producer took — activations,
    // replays and navigations — never more than the actions spent.
    assert_eq!(
        o.edge_events.enqueued,
        (app.activations.len() + app.navigations) as u64
    );
    assert_eq!(o.edge_events.enqueued, u64::from(o.spent_actions));
    assert_eq!(o.unsettled, earned(), "an earned exhaustion");
    nothing_vanishes(&o, &app);
    assert!(o.stop_node.is_some());
}

#[tokio::test]
async fn max_depth_zero_explores_the_start_node_only_and_stops_budget_depth() {
    let mut app = SimApp::site();
    let o = run(&mut app, &request(home(), 60, 0, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::BudgetDepth,
        "{}",
        o.stop_explanation
    );
    assert_eq!(
        activated_ids(&app),
        ["a", "b"].iter().map(|s| s.to_string()).collect()
    );
    assert_eq!(o.spent_depth, 0);
    assert!(o.not_activated.beyond_depth > 0);
    let by_reason =
        o.accounting
            .remaining_by_reason(o.stopped_because, &o.budget_left, &o.attempts_finished);
    // The two deeper entries the budget left are budget_exhausted.
    assert_eq!(by_reason["budget_exhausted"], 2);
    assert_eq!(by_reason["not_yet_activated"], 0);
    assert_eq!(
        o.accounting.activated_count() + remaining_total(&o),
        o.accounting.seen.len() as u32
    );
}

#[tokio::test]
async fn the_action_budget_is_never_exceeded() {
    for max_actions in 1..=8 {
        let mut app = SimApp::site();
        let o = run(&mut app, &request(home(), max_actions, 5, 60)).await;
        assert!(
            o.spent_actions <= max_actions,
            "{max_actions}: {}",
            o.spent_actions
        );
        assert_eq!(
            o.spent_actions as usize,
            app.activations.len() + app.navigations,
            "every action the app saw is counted"
        );
        assert!(o.accounting.activated_count() <= o.spent_actions);
        if o.stopped_because != StopReason::FrontierExhausted {
            assert_eq!(
                o.stopped_because,
                StopReason::BudgetActions,
                "{max_actions}"
            );
        }
    }
    let mut app = SimApp::site();
    let o = run(&mut app, &request(home(), 2, 5, 60)).await;
    assert_eq!(o.stopped_because, StopReason::BudgetActions);
    assert_eq!(o.accounting.activated_count(), 1);
}

#[tokio::test]
async fn the_wall_clock_budget_stops_the_run() {
    let mut app = SimApp::site();
    app.tick = Duration::from_secs(1);
    let o = run(&mut app, &request(home(), 60, 5, 2)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::BudgetTime,
        "{}",
        o.stop_explanation
    );
    assert!(
        o.accounting
            .remaining_by_reason(o.stopped_because, &o.budget_left, &o.attempts_finished)
            ["budget_exhausted"]
            > 0
    );
}

#[tokio::test]
async fn an_app_killed_mid_run_is_app_unreachable_never_frontier_exhausted() {
    // How many snapshots does a full run take?
    let mut full = SimApp::site();
    let o = run(&mut full, &request(home(), 60, 5, 60)).await;
    assert_eq!(o.stopped_because, StopReason::FrontierExhausted);
    let total = full.snapshots;
    // Kill the app after every possible number of answered snapshots —
    // including the very last, the snapshot that CONFIRMS the exhausted
    // frontier. Every run must read app_unreachable.
    for alive in 0..total {
        let mut app = SimApp::site();
        app.alive_snapshots = Some(alive);
        let o = run(&mut app, &request(home(), 60, 5, 60)).await;
        assert_eq!(
            o.stopped_because,
            StopReason::AppUnreachable,
            "killed after {alive} of {total} snapshots: {}",
            o.stop_explanation
        );
        assert_eq!(
            o.accounting.activated_count() + remaining_total(&o),
            o.accounting.seen.len() as u32
        );
    }
}

#[tokio::test]
async fn a_login_page_is_an_auth_wall_with_its_node_recorded() {
    let login = JourneyNode::new(None, Vec::<String>::new(), Some("/login/".into()), None);
    let mut app = SimApp::site();
    app.page("login", login.clone(), vec![link("submit")]);
    app.pages.get_mut("home").unwrap().elements = vec![link("signout")];
    app.go("home", "signout", "login");
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(o.stopped_because, StopReason::AuthWall);
    assert_eq!(o.stop_node, Some(login));
    assert!(
        !activated_ids(&app).contains("submit"),
        "nothing is activated behind the wall"
    );

    // An authentication refusal from the app is an auth wall too.
    assert!(answer_is_auth_refusal(
        &json!({ "success": false, "code": "UNAUTHORIZED" })
    ));
    assert!(answer_is_auth_refusal(
        &json!({ "error": { "errorCode": "AUTH_REQUIRED" } })
    ));
    assert!(!answer_is_auth_refusal(
        &json!({ "success": false, "code": "TAB_STALE" })
    ));
    let labelled = JourneyNode::new(None, Vec::<String>::new(), None, Some("sign-in".into()));
    assert!(is_auth_wall(&labelled));
    assert!(!is_auth_wall(&page_node("loginsettings")));
    let near_miss = JourneyNode::new(None, Vec::<String>::new(), Some("/login/help".into()), None);
    assert!(!is_auth_wall(&near_miss), "the template set is closed");
}

#[tokio::test]
async fn an_app_dead_from_the_start_spends_one_action_and_names_no_node() {
    let mut app = SimApp::site();
    app.alive_snapshots = Some(0);
    let o = run(&mut app, &request(home(), 5, 5, 60)).await;
    assert_eq!(o.stopped_because, StopReason::AppUnreachable);
    assert_eq!(o.stop_node, None);
    assert_eq!(o.accounting.activated_count(), 0);
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

fn entry(node: &str, fp: &str, seen: &str, effect: Option<&str>) -> FrontierEntry {
    let mut e = crate::journey::test_fixtures::frontier(node, fp);
    e.first_seen_at = seen.into();
    e.declared_effect = effect.map(|s| serde_json::from_value(json!(s)).unwrap());
    e
}

#[test]
fn selection_takes_the_oldest_safe_entry_and_fails_closed_on_a_stale_reason() {
    let key = crate::journey::test_fixtures::key;
    let frontier = vec![
        // Oldest, but its row declares write while still reading
        // not_yet_activated: never selected.
        entry(
            "home",
            "fp-write",
            "2026-10-28T10:00:00.000000Z",
            Some("write"),
        ),
        entry(
            "home",
            "component:grid:delete",
            "2026-10-28T10:00:01.000000Z",
            Some("read"),
        ),
        entry(
            "home",
            "fp-new",
            "2026-10-28T10:00:03.000000Z",
            Some("read"),
        ),
        entry(
            "home",
            "fp-old",
            "2026-10-28T10:00:02.000000Z",
            Some("read"),
        ),
    ];
    let graph = routing_graph(&[], &frontier, &writing());
    let none = BTreeSet::new();
    let pick = select_next(
        &graph,
        &key("home"),
        false,
        &key("home"),
        0,
        10,
        &BTreeSet::new(),
        &none,
    );
    let Choice::Next {
        entry: chosen,
        depth,
        route,
    } = &pick.choice
    else {
        panic!("{pick:?}")
    };
    assert_eq!(chosen.affordance_fingerprint, "fp-old");
    assert_eq!(*depth, 0);
    assert_eq!(route.cost(), 1);
    // What only the budget could keep from the run: the two read entries —
    // never the write row or the component action.
    assert_eq!(
        pick.budget_left,
        ["fp-new", "fp-old"]
            .iter()
            .map(|fp| (key("home"), fp.to_string()))
            .collect::<BTreeSet<EntryKey>>()
    );

    // Attempted entries are not retried; with only unsafe / component rows
    // left nothing is selectable, and the component action is counted.
    let attempted: BTreeSet<EntryKey> = ["fp-old", "fp-new"]
        .iter()
        .map(|fp| (key("home"), fp.to_string()))
        .collect();
    let stop = select_next(
        &graph,
        &key("home"),
        false,
        &key("home"),
        0,
        10,
        &attempted,
        &none,
    );
    assert_eq!(stop.choice, Choice::Stop(StopReason::FrontierExhausted));
    assert_eq!(stop.skipped.component_actions, 1);
    // ...which `settle_stop` accepts only when nothing is unsettled — and an
    // undriven D4-safe component action IS unsettled: activatable frontier
    // the explorer leaves by construction.
    assert_eq!(
        settle_stop(StopReason::FrontierExhausted, &Unsettled::default()),
        (StopReason::FrontierExhausted, None)
    );
    for unsettled in [
        Unsettled {
            attempts_failed: 1,
            ..Default::default()
        },
        Unsettled {
            component_actions: stop.skipped.component_actions,
            ..Default::default()
        },
        Unsettled {
            open_at_confirmation: Some(1),
            ..Default::default()
        },
        Unsettled {
            edge_events_dropped: 1,
            ..Default::default()
        },
        Unsettled {
            snapshot_refused_by_app: true,
            ..Default::default()
        },
    ] {
        let (reason, why) = settle_stop(StopReason::FrontierExhausted, &unsettled);
        assert_eq!(reason, StopReason::FrontierUnsettled, "{unsettled:?}");
        assert!(why.unwrap().contains("NOT exhausted"));
    }

    // No action left: budget_actions, not exhausted.
    let stop = select_next(
        &graph,
        &key("home"),
        false,
        &key("home"),
        0,
        0,
        &BTreeSet::new(),
        &none,
    );
    assert_eq!(stop.choice, Choice::Stop(StopReason::BudgetActions));
}

#[test]
fn an_entry_outside_the_start_component_counts_only_where_the_run_stood() {
    let key = crate::journey::test_fixtures::key;
    // "far" has no observed edge from home: outside the start's component.
    let frontier = vec![
        entry("far", "fp-far", "2026-10-28T10:00:00.000000Z", Some("read")),
        entry(
            "far",
            "component:grid:refresh",
            "2026-10-28T10:00:01.000000Z",
            Some("read"),
        ),
    ];
    let graph = routing_graph(&[], &frontier, &writing());
    let pick = |visited: &BTreeSet<String>| {
        select_next(
            &graph,
            &key("home"),
            true,
            &key("home"),
            5,
            10,
            &BTreeSet::new(),
            visited,
        )
    };
    // Never stood there: not this run's frontier — uncounted.
    let unseen = pick(&BTreeSet::new());
    assert_eq!(unseen.choice, Choice::Stop(StopReason::FrontierExhausted));
    assert_eq!(unseen.skipped, NotActivated::default());
    // Stood there: activatable work it cannot route to, counted.
    let stood = pick(&[key("far")].into_iter().collect::<BTreeSet<String>>());
    assert_eq!(stood.skipped.unroutable, 1);
    assert_eq!(stood.skipped.component_actions, 1);
    assert!(
        stood.budget_left.is_empty(),
        "never labelled budget_exhausted"
    );
}

// ---------------------------------------------------------------------------
// The record and its CHECKs
// ---------------------------------------------------------------------------

fn record(stopped: StopReason) -> ExplorationResult {
    let req = request(home(), 10, 2, 30);
    let started = chrono::DateTime::parse_from_rfc3339("2026-10-28T10:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let mut accounting = Accounting::default();
    accounting.activated(("a#s".into(), "fp1".into()));
    accounting.remaining(("a#s".into(), "fp2".into()), FrontierReason::EffectWrite);
    assemble(
        "run-1".into(),
        "app".into(),
        &req,
        started,
        Duration::from_millis(1500),
        LoopOutcome {
            stopped_because: stopped,
            stop_explanation: String::new(),
            stop_node: Some(page_node("a")),
            spent_actions: 3,
            spent_depth: 1,
            accounting,
            not_activated: NotActivated::default(),
            // An unsettled run always names why (check_record holds it).
            unsettled: if stopped == StopReason::FrontierUnsettled {
                Unsettled {
                    attempts_failed: 1,
                    ..Default::default()
                }
            } else {
                Unsettled::default()
            },
            budget_left: BTreeSet::new(),
            attempts_finished: BTreeSet::new(),
            edge_events: EdgeEvents {
                enqueued: 1,
                dropped: 0,
            },
        },
    )
}

#[test]
fn an_unsettled_record_that_names_no_reason_is_refused() {
    let mut r = record(StopReason::FrontierUnsettled);
    assert!(check_record(&r).is_ok());
    r.unsettled = Unsettled::default();
    let err = check_record(&r).expect_err("frontier_unsettled with nothing named");
    assert!(err.contains("names no reason"), "{err}");
}

#[test]
fn confirmation_measures_are_null_when_no_confirmation_ran() {
    // A budget stop confirms no exhaustion: those two fields are not
    // measured, and say so as JSON null — never 0 / false.
    let r = record(StopReason::BudgetActions);
    let wire = serde_json::to_value(&r).unwrap();
    assert!(wire["unsettled"]["openAtConfirmation"].is_null(), "{wire}");
    assert!(
        wire["unsettled"]["exhaustionUnconfirmed"].is_null(),
        "{wire}"
    );
    let mut r = record(StopReason::FrontierUnsettled);
    r.unsettled.exhaustion_unconfirmed = None;
    let detail = stop_detail_column(&r).unwrap();
    assert!(detail["exhaustionUnconfirmed"].is_null(), "{detail}");
    assert!(detail["openAtConfirmation"].is_null(), "{detail}");
}

#[test]
fn the_schema_probe_requires_the_stop_detail_column() {
    // A database at journey_02 has the table but would refuse every INSERT
    // naming stop_detail: the probe reads it as schema_absent.
    assert!(EXPLORATION_SCHEMA_PROBE_SQL.contains("to_regclass('project.journey_explorations')"));
    assert!(EXPLORATION_SCHEMA_PROBE_SQL.contains("column_name = 'stop_detail'"));
    assert!(EXPLORATION_SCHEMA_PROBE_SQL.contains("table_name = 'journey_explorations'"));
}

#[test]
fn the_assembled_record_satisfies_every_check() {
    for reason in StopReason::ALL {
        let r = record(reason);
        check_record(&r).unwrap_or_else(|e| panic!("{reason:?}: {e}"));
        assert_eq!(r.edges_added, 1);
        assert_eq!(r.spent.wall_clock_ms, 1500);
        assert_eq!(r.start, json!({ "url": "/home" }));
        assert!(r.finished_at > r.started_at, "same clock, never before");
        let wire = serde_json::to_value(&r).unwrap();
        assert_eq!(wire["stoppedBecause"], reason.as_str());
        for f in [
            "runId",
            "budget",
            "spent",
            "stopNode",
            "edgesAdded",
            "frontierRemainingByReason",
            "start",
            "startedAt",
            "finishedAt",
            "runnerBuildId",
            "runnerInstance",
        ] {
            assert!(wire.get(f).is_some(), "missing {f}");
        }
    }
}

#[test]
fn a_record_the_database_would_refuse_is_refused_here_with_the_constraint_named() {
    let cases: Vec<(&str, Box<dyn Fn(&mut ExplorationResult)>)> = vec![
        ("spent_within_budget", Box::new(|r| r.spent.actions = 11)),
        ("spent_within_budget", Box::new(|r| r.spent.depth = 3)),
        ("edges_added", Box::new(|r| r.edges_added = 4)),
        (
            "finished_after_started",
            Box::new(|r| r.finished_at = "2026-10-28T09:59:59Z".into()),
        ),
        (
            "auth_wall_names_node",
            Box::new(|r| {
                r.stopped_because = StopReason::AuthWall;
                r.stop_node = None;
            }),
        ),
        ("start_object", Box::new(|r| r.start = json!("/home"))),
        ("budget_positive", Box::new(|r| r.budget.max_actions = 0)),
    ];
    for (constraint, mutate) in cases {
        let mut r = record(StopReason::BudgetActions);
        mutate(&mut r);
        let err = check_record(&r).expect_err(constraint);
        assert!(err.contains(constraint), "{constraint}: {err}");
    }
    // A non-auth-wall stop may name no node (app_unreachable before any).
    let mut r = record(StopReason::AppUnreachable);
    r.stop_node = None;
    assert!(check_record(&r).is_ok());
}

#[test]
fn every_mirrored_check_exists_in_the_vendored_embedded_schema() {
    // LIMIT: this compares against the runner's vendored embedded schema, not
    // the qontinui-web migration text — that file lives in another repo and
    // is not reachable from a unit test offline. The vendored block was
    // produced by applying `journey_02_explorations`'s DDL (web 56f88b78),
    // `journey_03_explorations_stop_unsettled`'s (the seventh word and the
    // `stop_detail` column) and `journey_04_explorations_stop_detail_any_stop`'s
    // (`stop_detail` on any stop; its CHECK line re-vendored from
    // `pg_get_constraintdef`, read from an ephemeral PostgreSQL 16.15
    // database — `ephemeral-db.sh start pgvector`, 2026-10-29 — after alembic
    // applied journey_04 to it) and dumping the table, so it is the migrations' text as Postgres renders
    // it; a later web change to the table must be re-vendored for this test
    // to see it.
    let schema = include_str!("../../schema.pg.sql.generated");
    assert!(schema.contains("CREATE TABLE project.journey_explorations ("));
    let table = schema
        .split("CREATE TABLE project.journey_explorations (")
        .nth(1)
        .and_then(|t| t.split("\n);").next())
        .unwrap_or_default();
    assert!(
        table.contains("\n    stop_detail jsonb,"),
        "stop_detail is nullable jsonb"
    );
    for c in [
        "ck_journey_explorations_auth_wall_names_node",
        "ck_journey_explorations_budget_positive",
        "ck_journey_explorations_edges_added",
        "ck_journey_explorations_finished_after_started",
        "ck_journey_explorations_frontier_remaining_object",
        "ck_journey_explorations_spent_non_negative",
        "ck_journey_explorations_spent_within_budget",
        "ck_journey_explorations_start_object",
        "ck_journey_explorations_stop_detail",
        "ck_journey_explorations_stop_node_object",
        "ck_journey_explorations_stopped_because",
    ] {
        assert!(
            schema.contains(c),
            "{c} missing from schema.pg.sql.generated"
        );
    }
    // The stop vocabulary is EXACTLY that one CHECK's word set — read out of
    // its own definition, since other CHECKs name some of the same words.
    let check = table
        .lines()
        .find(|l| l.contains("CONSTRAINT ck_journey_explorations_stopped_because CHECK"))
        .expect("the stopped_because CHECK");
    let words: BTreeSet<&str> = check.split('\'').skip(1).step_by(2).collect();
    let want: BTreeSet<&str> = StopReason::ALL.iter().map(|r| r.as_str()).collect();
    assert_eq!(words, want, "{check}");
    // A fresh embedded database is at journey_04: no CHECK ties stop_detail
    // to frontier_unsettled, so the probe reads it AnyStop.
    let detail_check = table
        .lines()
        .find(|l| l.contains("CONSTRAINT ck_journey_explorations_stop_detail CHECK"))
        .expect("the stop_detail CHECK");
    assert!(!check_restricts_stop_detail(detail_check), "{detail_check}");
    assert!(
        !table
            .lines()
            .filter(|l| l.contains("CHECK"))
            .any(check_restricts_stop_detail),
        "no CHECK on the vendored table restricts stop_detail"
    );
}

#[test]
fn the_insert_binds_match_the_sql_and_name_every_column() {
    let max = (1..=40)
        .rev()
        .find(|n| EXPLORATION_INSERT_SQL.contains(&format!("${n}")))
        .unwrap_or(0);
    assert_eq!(max, EXPLORATION_INSERT_BINDS);
    for col in [
        "run_id",
        "app_id",
        "runner_build_id",
        "runner_instance",
        "started_at",
        "finished_at",
        "start",
        "budget_max_actions",
        "budget_max_depth",
        "budget_max_wall_clock_s",
        "spent_actions",
        "spent_depth",
        "spent_wall_clock_ms",
        "stopped_because",
        "stop_node",
        "edges_added",
        "frontier_remaining_by_reason",
        "stop_detail",
    ] {
        assert!(EXPLORATION_INSERT_SQL.contains(col), "{col}");
    }
    assert!(
        EXPLORATION_INSERT_SQL.contains("$18::jsonb"),
        "stop_detail is jsonb"
    );
    assert!(
        EXPLORATION_INSERT_SQL.contains("$6::text::timestamptz"),
        "finished_at is sent"
    );
}

// ---------------------------------------------------------------------------
// Attribution: the explorer's actions are `run_kind = explorer`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_action_inside_an_explorer_run_is_attributed_to_it() {
    use crate::journey::capture::{as_explorer, attribute_action};
    use crate::journey::cursor::Provenance;
    use qontinui_types::journey::RunKind;
    let inside = as_explorer(crate::journey::capture::ExplorerRun::new("run-x"), async {
        attribute_action(Provenance {
            app_version: Some("1.0".into()),
            run_id: Some("task-7".into()),
            ..Default::default()
        })
    })
    .await;
    assert_eq!(inside.run_kind, RunKind::Explorer);
    assert_eq!(inside.run_id.as_deref(), Some("run-x"));
    assert_eq!(inside.app_version.as_deref(), Some("1.0"));
    let outside = attribute_action(Provenance::default());
    assert_eq!(outside.run_kind, RunKind::AgentAction);
    assert_eq!(outside.run_id, None);
}

// ---------------------------------------------------------------------------
// Review round 1: the click lands only on the named app
// ---------------------------------------------------------------------------

#[test]
fn the_runner_app_is_refused_with_a_422_naming_why() {
    let runner = crate::spec_api::storage::RUNNER_APP_ID;
    // In any case: refused by name.
    for spelled in [runner.to_string(), runner.to_ascii_uppercase()] {
        let Refusal(status, why) = explore_app_id(&spelled)
            .expect_err("the runner's own webview is not D4-safe to explore");
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{spelled:?}");
        assert!(why.contains("no declared effect"), "{spelled:?}: {why}");
    }
    // Padded (a decoded `%20`): not an identifier, refused — never trimmed
    // into an id the path did not give (as the observe route checks it).
    for padded in [
        format!("{runner} "),
        format!(" {runner}\t"),
        " qontinui-web ".to_string(),
    ] {
        let Refusal(status, why) = explore_app_id(&padded).expect_err("padded");
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{padded:?}");
        assert!(why.contains("identifier"), "{padded:?}: {why}");
    }
    assert_eq!(
        explore_app_id("qontinui-web").map_err(|r| r.1),
        Ok("qontinui-web".to_string())
    );
    for bad in ["", "a b", "../etc", "qontinui-web/x"] {
        let Refusal(status, why) = explore_app_id(bad).expect_err(bad);
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad:?}");
        assert!(why.contains("identifier"), "{bad:?}: {why}");
    }
}

#[test]
fn a_refused_webview_fallback_is_app_unreachable_and_records_no_edge() {
    use crate::mcp::sdk_client::no_fallback_refusal;
    // What a named-app dispatch that cannot reach its app (or any webview
    // fallback inside an explorer run) answers: a TRANSPORT refusal.
    let answer = no_fallback_refusal("element action", "app qontinui-web not connected");
    assert!(
        crate::journey::capture::sdk_refusal(&answer),
        "the journey ledger records nothing for it — no UI was acted on"
    );
    assert!(matches!(
        classify_answer(answer),
        Err(DriveFailure::Unreachable(_))
    ));
    // An app-answered failure is a failed activation; success acts.
    assert_eq!(
        classify_answer(json!({ "success": false, "error": "disabled", "failure_origin": "app" })),
        Ok(ActResult::Failed("\"disabled\"".into()))
    );
    assert_eq!(
        classify_answer(json!({ "success": true })),
        Ok(ActResult::Acted)
    );
}

#[test]
fn the_sdk_action_query_names_its_app() {
    use crate::mcp::sdk_client::SdkActionQueryParams;
    for body in [
        r#"{"app_id":"qontinui-web"}"#,
        r#"{"appId":"qontinui-web"}"#,
    ] {
        let q: SdkActionQueryParams = serde_json::from_str(body).unwrap();
        assert_eq!(q.app_id.as_deref(), Some("qontinui-web"));
    }
    let q: SdkActionQueryParams = serde_json::from_str("{}").unwrap();
    assert_eq!(
        q.app_id, None,
        "an agent call without it keeps the active-app behaviour"
    );
}

// `only_an_explorer_scope_refuses_the_webview_fallback` lives with the
// routes it guards (`mcp::sdk_client` tests), where it drives the gate and
// checks both acting routes ask it before falling back.

// ---------------------------------------------------------------------------
// Review round 1: frontier_exhausted is earned
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_truncated_or_unreadable_ledger_read_never_reads_exhausted() {
    for (truncated, unparseable) in [(true, 0), (false, 2)] {
        let mut app = SimApp::site();
        app.truncated = truncated;
        app.rows_unparseable = unparseable;
        let o = run(&mut app, &request(home(), 60, 5, 60)).await;
        assert_eq!(
            o.stopped_because,
            StopReason::FrontierUnsettled,
            "{}",
            o.stop_explanation
        );
        assert!(
            o.stop_explanation.contains("NOT exhausted"),
            "{}",
            o.stop_explanation
        );
        assert_eq!(o.unsettled.ledger_read_truncated, truncated);
        assert_eq!(o.unsettled.rows_unreadable, unparseable);
        nothing_vanishes(&o, &app);
    }
}

#[tokio::test]
async fn an_unroutable_entry_never_reads_exhausted_and_is_not_budget_exhausted() {
    // Start from a NODE (no URL to return through): after following `a` to
    // A, home's `b` has no observed safe route back.
    let mut app = SimApp::site();
    app.pages.get_mut("A").unwrap().elements.clear();
    app.current = Some("home".into());
    let o = run(
        &mut app,
        &request(ExploreStart::Node(page_node("home").key()), 60, 5, 60),
    )
    .await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierUnsettled,
        "{}",
        o.stop_explanation
    );
    assert!(o.unsettled.unroutable > 0, "{:?}", o.unsettled);
    let reasons = by_reason(&o);
    assert!(
        reasons["not_yet_activated"] > 0,
        "unroutable keeps its reason"
    );
    assert_eq!(reasons["budget_exhausted"], 0, "only what the budget left");
    nothing_vanishes(&o, &app);
}

#[tokio::test]
async fn a_vanished_element_is_a_failed_attempt_never_a_panic_nor_exhaustion() {
    let mut app = SimApp::site();
    app.vanish_on_click.insert("b".into());
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(o.stopped_because, StopReason::FrontierUnsettled);
    assert!(o.unsettled.attempts_failed > 0);
    // Unknown is not dressed as a measured reason: the action budget was
    // NOT spent, and nothing is labelled budget_exhausted.
    assert!(o.spent_actions < 60);
    assert_eq!(by_reason(&o)["budget_exhausted"], 0);
    assert_eq!(by_reason(&o)["activation_failed"], 1);
    nothing_vanishes(&o, &app);

    // An element that turns into something else under the same id is
    // clicked as found (the live D4 check was on the snapshot before the
    // click) — the run completes and still accounts for everything.
    let mut app = SimApp::site();
    app.change_on_click.insert("a".into(), sidebar_delete());
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    nothing_vanishes(&o, &app);
}

#[tokio::test]
async fn a_budget_stop_labels_only_what_the_budget_left() {
    let mut app = SimApp::site();
    let o = run(&mut app, &request(home(), 2, 5, 60)).await;
    assert_eq!(o.stopped_because, StopReason::BudgetActions);
    let reasons = by_reason(&o);
    assert_eq!(
        reasons["budget_exhausted"] as usize,
        o.budget_left
            .iter()
            .filter(|k| o.accounting.seen.get(*k)
                == Some(&Disposition::Remaining(FrontierReason::NotYetActivated)))
            .count()
    );
    assert_eq!(
        reasons["effect_undeclared"], 1,
        "the Delete keeps its reason"
    );
    nothing_vanishes(&o, &app);
}

// ---------------------------------------------------------------------------
// Review round 1: time, stale nodes, honest reasons, counts
// ---------------------------------------------------------------------------

#[test]
fn the_wall_clock_budget_is_capped_at_an_hour() {
    let body = |s: u64| {
        json!({ "start": { "url": "/" }, "max_actions": 5, "max_depth": 1,
                "max_wall_clock_s": s })
        .to_string()
    };
    assert!(parse_explore_request(body(u64::from(MAX_WALL_CLOCK_S)).as_bytes()).is_ok());
    let Refusal(status, why) =
        parse_explore_request(body(u64::from(MAX_WALL_CLOCK_S) + 1).as_bytes()).unwrap_err();
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(why.contains("max_wall_clock_s"), "{why}");
}

#[tokio::test(start_paused = true)]
async fn the_work_after_a_stop_is_bounded() {
    // A budget stop whose confirming snapshot never answers still returns,
    // within POST_STOP_GRACE, as the budget stop.
    let mut app = SimApp::site();
    let mut probe = SimApp::site();
    let _ = run(&mut probe, &request(home(), 2, 5, 60)).await;
    app.hang_snapshots_from = Some(probe.snapshots - 1);
    let started = tokio::time::Instant::now();
    let o = run(&mut app, &request(home(), 2, 5, 60)).await;
    assert_eq!(o.stopped_because, StopReason::BudgetActions);
    assert!(started.elapsed() <= POST_STOP_GRACE + Duration::from_millis(1));
    assert_eq!(
        o.unsettled.closing_look_failed,
        Some(true),
        "the grace ran out: recorded, not dropped"
    );

    // An exhaustion it could not confirm is not claimed.
    let mut full = SimApp::site();
    let _ = run(&mut full, &request(home(), 60, 5, 60)).await;
    let mut app = SimApp::site();
    app.hang_snapshots_from = Some(full.snapshots - 1);
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierUnsettled,
        "{}",
        o.stop_explanation
    );
    assert_eq!(o.unsettled.exhaustion_unconfirmed, Some(true));
}

#[tokio::test]
async fn a_dropped_snapshot_event_is_an_unknown_node_never_a_match() {
    let mut app = SimApp::site();
    app.drop_snapshot_events_from = Some(2);
    let no_row = explore(&mut app, &request(home(), 60, 5, 60))
        .await
        .expect_err("a stale node must not be explored against");
    assert_eq!(no_row.kind, NoRowKind::Internal);
    assert!(no_row.detail.contains("stale"), "{}", no_row.detail);
}

#[tokio::test]
async fn a_start_url_the_app_refuses_is_no_row_not_app_unreachable() {
    let mut app = SimApp::site();
    let no_row = explore(
        &mut app,
        &request(ExploreStart::Url("/nope".into()), 5, 1, 60),
    )
    .await
    .expect_err("the app answered: not unreachable");
    assert_eq!(no_row.kind, NoRowKind::StartRefused);
    assert_eq!(no_row.spent_actions, 1);
}

#[tokio::test]
async fn a_login_page_at_the_confirming_snapshot_is_an_auth_wall() {
    // The frontier looks exhausted, then the session expires: the CONFIRMING
    // look lands on a login page. That is an auth wall, not an exhaustion.
    let mut probe = SimApp::site();
    let o = run(&mut probe, &request(home(), 60, 5, 60)).await;
    assert_eq!(o.stopped_because, StopReason::FrontierExhausted);
    let login = JourneyNode::new(None, Vec::<String>::new(), Some("/login".into()), None);
    let mut app = SimApp::site();
    app.page("login", login.clone(), vec![]);
    app.jump_at_snapshot = Some((probe.snapshots - 1, "login".into()));
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::AuthWall,
        "{}",
        o.stop_explanation
    );
    assert_eq!(o.stop_node, Some(login));
}

#[tokio::test]
async fn edges_added_counts_what_the_queue_took_and_drops_are_reported() {
    // The producer's queue drops every action event, so the simulated
    // producer writes no edge for any of them (and moves no frontier row).
    let mut app = SimApp::site();
    app.drop_actions = true;
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert!(app.edges.is_empty(), "a dropped event writes no edge");
    assert_eq!(o.edge_events.enqueued, 0, "nothing reached the producer");
    assert_eq!(o.edge_events.dropped, u64::from(o.spent_actions));
    // The ledger the run read is not the app's: never an exhaustion.
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierUnsettled,
        "{}",
        o.stop_explanation
    );
    assert_eq!(o.unsettled.edge_events_dropped, o.edge_events.dropped);
    let r = assemble(
        "run".into(),
        "app".into(),
        &request(home(), 60, 5, 60),
        chrono::Utc::now(),
        Duration::from_millis(5),
        o,
    );
    assert_eq!(r.edges_added, 0);
    assert!(r.unsettled.edge_events_dropped > 0);
    // Reported ONCE, in the block the row keeps — not again at the top level.
    let wire = serde_json::to_value(&r).unwrap();
    assert!(wire.get("edgeEventsDropped").is_none(), "{wire}");
    assert_eq!(
        wire["unsettled"]["edgeEventsDropped"],
        json!(r.unsettled.edge_events_dropped)
    );
    assert_eq!(
        stop_detail_column(&r).unwrap()["edgeEventsDropped"],
        json!(r.unsettled.edge_events_dropped),
        "and the stored row keeps it"
    );
    assert!(r.affordances_activated > 0, "the app still acted");
    assert_eq!(
        r.affordances_activated + r.frontier_remaining_by_reason.values().sum::<u32>(),
        r.affordances_seen
    );
}

#[test]
fn a_second_run_on_the_same_app_is_refused_until_the_first_ends() {
    let first = RunGuard::claim("guard-test-app").expect("free");
    assert!(
        RunGuard::claim("guard-test-app").is_none(),
        "a 409 for the second"
    );
    assert!(RunGuard::claim("guard-test-other").is_some(), "per app");
    drop(first);
    assert!(
        RunGuard::claim("guard-test-app").is_some(),
        "released on drop"
    );
}

// ---------------------------------------------------------------------------
// Review round 2: frontier_exhausted only when nothing activatable is left
// ---------------------------------------------------------------------------

/// A type-only link (`<a onClick>`: type `link`, no role).
fn type_only_link(id: &str) -> Value {
    json!({ "id": id, "type": "link", "label": id, "actions": ["click"] })
}

#[tokio::test]
async fn an_edge_the_explorer_took_through_a_type_only_link_is_replayed() {
    // home --a (type-only)--> A, which has two exits with no way back: one of
    // them can only be reached again by replaying `a` from the start URL.
    let mut app = SimApp::empty();
    app.page(
        "home",
        page_node("home"),
        vec![type_only_link("a"), link("b")],
    );
    app.page("A", page_node("A"), vec![link("x"), link("y")]);
    for p in ["B", "AX", "AY"] {
        app.page(p, page_node(p), vec![]);
    }
    app.go("home", "a", "A");
    app.go("home", "b", "B");
    app.go("A", "x", "AX");
    app.go("A", "y", "AY");
    app.urls.insert("/home".into(), "home".into());
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierExhausted,
        "{}",
        o.stop_explanation
    );
    assert_eq!(
        activated_ids(&app),
        ["a", "b", "x", "y"].iter().map(|s| s.to_string()).collect()
    );
    assert_eq!(o.unsettled, earned());
    nothing_vanishes(&o, &app);
}

/// The site plus a page `J` no observed edge leads to, whose link `j1`
/// stays activatable.
fn site_with_unrouted_page() -> SimApp {
    let mut app = SimApp::site();
    app.page("J", page_node("J"), vec![link("j1")]);
    app.go("J", "j1", "B2");
    app
}

#[tokio::test]
async fn an_entry_where_the_run_stood_but_cannot_route_is_unroutable_never_exhausted() {
    // Mid-run the app is suddenly on J (no edge recorded leads there).
    let mut app = site_with_unrouted_page();
    app.jump_at_snapshot = Some((2, "J".into()));
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierUnsettled,
        "{}",
        o.stop_explanation
    );
    assert!(o.unsettled.unroutable >= 1, "{:?}", o.unsettled);
    assert!(!activated_ids(&app).contains("j1"));
    assert_eq!(by_reason(&o)["budget_exhausted"], 0);
    nothing_vanishes(&o, &app);
}

#[tokio::test]
async fn activatable_frontier_at_the_confirming_read_is_never_exhausted() {
    let mut probe = site_with_unrouted_page();
    let o = run(&mut probe, &request(home(), 60, 5, 60)).await;
    assert_eq!(o.stopped_because, StopReason::FrontierExhausted);
    // The CONFIRMING look lands on J, whose `j1` is open: the selection
    // re-run on that ledger finds it.
    let mut app = site_with_unrouted_page();
    app.jump_at_snapshot = Some((probe.snapshots - 1, "J".into()));
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierUnsettled,
        "{}",
        o.stop_explanation
    );
    assert!(
        o.unsettled.open_at_confirmation.unwrap_or(0) >= 1,
        "{:?}",
        o.unsettled
    );
    assert_eq!(
        o.unsettled.exhaustion_unconfirmed,
        Some(false),
        "it WAS confirmed — open"
    );
    nothing_vanishes(&o, &app);
}

#[tokio::test]
async fn an_undriven_safe_component_action_is_never_exhausted() {
    let mut app = SimApp::site();
    app.components.insert(
        "home".into(),
        vec![json!({ "id": "grid", "actions": [
            { "id": "refresh", "effect": "read" },
            { "id": "purge", "effect": "write" },
        ]})],
    );
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierUnsettled,
        "{}",
        o.stop_explanation
    );
    // The read one is the activatable work left; the write one is not.
    assert_eq!(o.unsettled.component_actions, 1, "{:?}", o.unsettled);
    assert_eq!(o.not_activated.component_actions, 1);
    nothing_vanishes(&o, &app);
}

// ---------------------------------------------------------------------------
// Review round 2: a barrier timeout against the time budget
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_barrier_the_time_budget_cut_short_is_budget_time_and_a_look_shares_one_deadline() {
    // 6 s of budget; the producer takes 4 s per barrier. The first look's
    // baseline barrier takes 4 s, leaving 2 s of the look's ONE deadline for
    // the barrier after the snapshot — which times out at the budget's end.
    let mut app = SimApp::site();
    app.on_tokio_clock();
    app.barrier_delay = Duration::from_secs(4);
    let started = tokio::time::Instant::now();
    let o = explore(&mut app, &request(home(), 60, 5, 6))
        .await
        .expect("the budget ran out: a row, not a runner failure");
    assert_eq!(
        o.stopped_because,
        StopReason::BudgetTime,
        "{}",
        o.stop_explanation
    );
    // Never past the budget by a second full wait: the budget, plus the
    // bounded post-stop work.
    assert!(
        started.elapsed() <= Duration::from_secs(6) + POST_STOP_GRACE,
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn a_barrier_that_times_out_with_budget_left_is_a_runner_failure_without_a_row() {
    // A stuck producer: no answer within BARRIER_TIMEOUT while the budget
    // still had more than that. No stop word is true: no row.
    let mut app = SimApp::site();
    app.on_tokio_clock();
    app.barrier_delay = BARRIER_TIMEOUT + Duration::from_secs(1);
    let no_row = explore(&mut app, &request(home(), 60, 5, 120))
        .await
        .expect_err("a stuck producer is not a budget stop");
    assert_eq!(no_row.kind, NoRowKind::Internal);
    assert!(
        no_row.detail.contains("did not answer"),
        "{}",
        no_row.detail
    );
}

// ---------------------------------------------------------------------------
// Review round 2: an app that answered is never app_unreachable
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_snapshot_the_app_answered_with_a_failure_is_never_app_unreachable() {
    // Mid-run: the run ends with frontier it could not read.
    let mut app = SimApp::site();
    app.snapshot_refused_from = Some(3);
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierUnsettled,
        "{}",
        o.stop_explanation
    );
    assert!(o.unsettled.snapshot_refused_by_app);
    nothing_vanishes(&o, &app);

    // At the confirming look: the exhaustion is unconfirmed.
    let mut probe = SimApp::site();
    let _ = run(&mut probe, &request(home(), 60, 5, 60)).await;
    let mut app = SimApp::site();
    app.snapshot_refused_from = Some(probe.snapshots - 1);
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierUnsettled,
        "{}",
        o.stop_explanation
    );
    assert_eq!(o.unsettled.exhaustion_unconfirmed, Some(true));

    // At the very first look nothing was explored: no row, a 422.
    let mut app = SimApp::site();
    app.snapshot_refused_from = Some(0);
    let no_row = explore(&mut app, &request(home(), 60, 5, 60))
        .await
        .expect_err("no stop word is true of a run that never began");
    assert_eq!(no_row.kind, NoRowKind::StartRefused);
}

#[test]
fn the_live_snapshot_answer_tells_the_app_from_the_transport() {
    let ok = json!({ "success": true, "data": { "elements": [] } });
    assert_eq!(classify_snapshot(StatusCode::OK, ok.clone()), Ok(ok));
    // The app answered: reachable.
    for (status, body) in [
        (
            StatusCode::OK,
            json!({ "success": false, "error": "no registry" }),
        ),
        (
            StatusCode::BAD_GATEWAY,
            json!({ "success": false, "error": "x", "failure_origin": "app" }),
        ),
    ] {
        assert!(
            matches!(
                classify_snapshot(status, body.clone()),
                Err(DriveFailure::AppAnswered(_))
            ),
            "{status} {body}"
        );
    }
    // It never reached the app: only a failure that SAYS so.
    assert!(matches!(
        classify_snapshot(
            StatusCode::BAD_GATEWAY,
            json!({ "success": false, "error": "x", "failure_origin": "transport" })
        ),
        Err(DriveFailure::Unreachable(_))
    ));
    // A failure that names no origin (or `unknown`) does not say whether it
    // reached the app: an unknown outcome, never unreachable.
    for body in [
        json!({ "success": false, "error": "x" }),
        json!({ "success": false, "error": "x", "failure_origin": "unknown" }),
    ] {
        assert!(
            matches!(
                classify_snapshot(StatusCode::BAD_GATEWAY, body.clone()),
                Err(DriveFailure::OutcomeUnknown(_))
            ),
            "{body}"
        );
    }
    assert!(matches!(
        classify_snapshot(
            StatusCode::OK,
            json!({ "success": false, "code": "UNAUTHORIZED" })
        ),
        Err(DriveFailure::AuthWall(_))
    ));
}

// ---------------------------------------------------------------------------
// Review round 2: the explorer's own cursor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_explorer_has_its_own_cursor_no_click_or_snapshot_can_share() {
    use crate::journey::capture::{as_explorer, explorer_cursor, ExplorerRun};
    use crate::journey::cursor::CursorKey;
    let shared = CursorKey::new("qontinui-web", None);
    assert_ne!(
        CursorKey::for_explorer("qontinui-web"),
        shared,
        "a human's relayed click with no tabId, or an agent's unnamed snapshot"
    );
    assert_eq!(explorer_cursor(shared.clone()), shared, "outside a run");
    let inside = as_explorer(ExplorerRun::new("r"), async {
        explorer_cursor(CursorKey::new("qontinui-web", Some("tab-1")))
    })
    .await;
    assert_eq!(inside, CursorKey::for_explorer("qontinui-web"));
}

// ---------------------------------------------------------------------------
// Review round 3: one snapshot layer; nothing undrivable or failed is dropped
// ---------------------------------------------------------------------------

#[test]
fn the_click_check_reads_the_layer_the_index_was_built_from() {
    // The live snapshot answer is the SDK envelope; the explorer indexes its
    // `data`. Whether `click` is offered is read off that same layer.
    let wrapped = json!({ "success": true, "data": { "elements": [link("nav")] } });
    let idx = extract_affordances(crate::journey::capture::snapshot_view(&wrapped));
    assert!(idx.elements["nav"].offers_click);
    assert_eq!(live_activation(&idx, "nav"), Ok("click".into()));
}

#[tokio::test]
async fn a_live_shaped_run_activates_its_navigation_links() {
    // SimApp answers in the live `{success, data}` envelope: the links are
    // activated, and an exhausted frontier is claimed with none left.
    let mut app = SimApp::site();
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierExhausted,
        "{}",
        o.stop_explanation
    );
    let ids = activated_ids(&app);
    for id in ["a", "b", "home-from-a", "b-tab2"] {
        assert!(ids.contains(id), "{id} activated: {ids:?}");
    }
    assert_eq!(by_reason(&o)["not_yet_activated"], 0, "{:?}", by_reason(&o));
    nothing_vanishes(&o, &app);
}

/// A navigation element offering no `click` — role `link`, one undeclared
/// custom action — is `not_yet_activated` on the ledger, yet nothing on it is
/// safe to fire.
fn linkless() -> Value {
    json!({ "id": "linkless", "type": "link", "role": "link", "label": "linkless",
            "customActions": [{ "id": "go" }] })
}

#[tokio::test]
async fn an_undrivable_navigation_element_never_reads_exhausted() {
    let mut app = SimApp::site();
    app.pages.get_mut("A").unwrap().elements.push(linkless());
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierUnsettled,
        "{}",
        o.stop_explanation
    );
    assert_eq!(o.unsettled.undrivable, 1, "{:?}", o.unsettled);
    assert!(
        !activated_ids(&app).contains("linkless"),
        "never fired: it offers no click"
    );
    // It keeps the ledger's reason, never reclassified by the run.
    assert_eq!(by_reason(&o)["not_yet_activated"], 1, "{:?}", by_reason(&o));
    assert_eq!(
        by_reason(&o)["effect_undeclared"],
        1,
        "only the sidebar Delete"
    );
    nothing_vanishes(&o, &app);
}

#[test]
fn an_open_activation_failed_row_is_counted_whether_or_not_it_was_retried() {
    let key = crate::journey::test_fixtures::key;
    let mut failed = entry("home", "fp-x", "2026-10-28T10:00:00.000000Z", None);
    failed.reason = FrontierReason::ActivationFailed;
    let frontier = vec![
        failed,
        entry("home", "fp-y", "2026-10-28T10:00:01.000000Z", Some("read")),
    ];
    let graph = routing_graph(&[], &frontier, &writing());
    let none: BTreeSet<EntryKey> = BTreeSet::new();
    let nowhere: BTreeSet<String> = BTreeSet::new();
    // Fresh entries first: the retry waits behind fp-y although it is older.
    let pick = select_next(
        &graph,
        &key("home"),
        false,
        &key("home"),
        0,
        10,
        &none,
        &nowhere,
    );
    let Choice::Next { entry: chosen, .. } = &pick.choice else {
        panic!("{pick:?}")
    };
    assert_eq!(chosen.affordance_fingerprint, "fp-y");
    assert_eq!(pick.failed_open, 1);
    // Both attempted: the selection finds nothing — and still counts the
    // open activation_failed row, which unsettles the exhaustion.
    let attempted: BTreeSet<EntryKey> = ["fp-x", "fp-y"]
        .iter()
        .map(|fp| (key("home"), fp.to_string()))
        .collect();
    let stop = select_next(
        &graph,
        &key("home"),
        false,
        &key("home"),
        0,
        10,
        &attempted,
        &nowhere,
    );
    assert_eq!(stop.choice, Choice::Stop(StopReason::FrontierExhausted));
    assert_eq!(stop.failed_open, 1);
    let (reason, why) = settle_stop(
        StopReason::FrontierExhausted,
        &Unsettled {
            activation_failed_open: stop.failed_open,
            ..Default::default()
        },
    );
    assert_eq!(reason, StopReason::FrontierUnsettled);
    assert!(why.unwrap().contains("1 activation_failed row(s) open"));
    // Outside the start's component and never stood on: not this run's.
    let far = select_next(
        &graph,
        &key("elsewhere"),
        false,
        &key("elsewhere"),
        0,
        10,
        &none,
        &nowhere,
    );
    assert_eq!(far.failed_open, 0);
}

#[tokio::test]
async fn a_later_run_never_claims_exhaustion_over_an_earlier_runs_failure() {
    // Run 1: the app fails `b` and it vanishes; its row is activation_failed.
    let mut app = SimApp::site();
    app.vanish_on_click.insert("b".into());
    let first = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(first.stopped_because, StopReason::FrontierUnsettled);
    // Run 2, a fresh run on the same ledger: nothing it did failed, yet the
    // row is still activation_failed — so the frontier is not exhausted.
    let second = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        second.stopped_because,
        StopReason::FrontierUnsettled,
        "{}",
        second.stop_explanation
    );
    assert_eq!(
        second.unsettled.activation_failed_open, 1,
        "{:?}",
        second.unsettled
    );
    assert_eq!(by_reason(&second)["activation_failed"], 1);
    nothing_vanishes(&second, &app);
}

#[tokio::test]
async fn a_failed_activation_is_retried_by_the_next_run_and_can_settle() {
    // Run 1: the app fails `a` once. Its row is activation_failed, so run 1
    // is unsettled and A is never reached.
    let mut app = SimApp::site();
    app.fail_once.insert("a".into());
    let first = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(first.stopped_because, StopReason::FrontierUnsettled);
    assert_eq!(
        first.unsettled.activation_failed_open, 1,
        "{:?}",
        first.unsettled
    );
    // Run 2 retries it within budget; this time the app acts, the edge closes
    // the row, A's link is explored, and the frontier IS exhausted.
    let second = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        second.stopped_because,
        StopReason::FrontierExhausted,
        "{}",
        second.stop_explanation
    );
    assert_eq!(second.unsettled, earned());
    assert!(activated_ids(&app).contains("home-from-a"));
    nothing_vanishes(&second, &app);
}

#[test]
fn the_stored_row_says_why_a_run_was_unsettled() {
    let mut r = record(StopReason::FrontierUnsettled);
    r.unsettled.undrivable = 2;
    r.unsettled.activation_failed_open = 1;
    let detail = stop_detail_column(&r).expect("an unsettled run carries its breakdown");
    assert_eq!(detail, serde_json::to_value(r.unsettled).unwrap());
    assert_eq!(detail["undrivable"], 2);
    assert_eq!(detail["activationFailedOpen"], 1);
    check_record(&r).unwrap();
    // Any other stop whose block records nothing: SQL NULL.
    for reason in StopReason::ALL {
        if reason != StopReason::FrontierUnsettled {
            assert_eq!(
                stop_detail_column(&record(reason)),
                None,
                "{reason:?}: SQL NULL"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Review round 4: the run's own writes, unreadable answers, the start check
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_failed_edge_write_mid_run_never_reads_exhausted() {
    // From the second edge write on, the producer's writes fail (a pool or
    // probe blip): the edges of acted activations never reach the ledger.
    let mut app = SimApp::site();
    app.edge_writes_fail_from = Some(1);
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierUnsettled,
        "{}",
        o.stop_explanation
    );
    assert!(o.unsettled.ledger_write_failures > 0, "{:?}", o.unsettled);
    nothing_vanishes(&o, &app);
}

#[tokio::test]
async fn an_activated_entry_whose_edge_never_landed_is_open_at_confirmation() {
    // The LAST edge write is lost with nothing counting it: no health
    // counter moves and the ledger still reads `writing`. The run activated
    // the entry, so the re-run selection skips it — yet its row is still
    // open, and the confirmation counts it.
    let mut probe = SimApp::site();
    let p = run(&mut probe, &request(home(), 60, 5, 60)).await;
    assert_eq!(p.stopped_because, StopReason::FrontierExhausted);
    let mut app = SimApp::site();
    app.write_failures_counted = false;
    app.edge_writes_fail_from = Some(probe.edges_closed - 1);
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierUnsettled,
        "{}",
        o.stop_explanation
    );
    assert_eq!(o.unsettled.ledger_write_failures, 0);
    assert!(!o.unsettled.ledger_not_writing);
    assert_eq!(
        o.unsettled.open_at_confirmation,
        Some(1),
        "{:?}",
        o.unsettled
    );
    assert_eq!(o.unsettled.exhaustion_unconfirmed, Some(false));
    nothing_vanishes(&o, &app);
}

#[tokio::test]
async fn a_ledger_that_stops_writing_mid_run_never_reads_exhausted() {
    let mut app = SimApp::site();
    app.ledger_state_from_read = Some((2, qontinui_types::journey::LedgerState::WriteFailing));
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierUnsettled,
        "{}",
        o.stop_explanation
    );
    assert!(o.unsettled.ledger_not_writing);
    assert!(
        o.stop_explanation.contains("ledger not writing: true"),
        "{}",
        o.stop_explanation
    );
    nothing_vanishes(&o, &app);
}

#[tokio::test]
async fn write_failures_from_before_the_run_do_not_unsettle_it() {
    let mut app = SimApp::site();
    app.write_failures = 5;
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierExhausted,
        "{}",
        o.stop_explanation
    );
    assert_eq!(o.unsettled, earned());
}

#[tokio::test]
async fn an_action_whose_answer_could_not_be_read_is_unsettled_never_unreachable() {
    let mut app = SimApp::site();
    app.activation_unknown_at = Some(0);
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierUnsettled,
        "{}",
        o.stop_explanation
    );
    assert!(o.unsettled.action_outcome_unknown, "{:?}", o.unsettled);
    assert!(
        o.stop_explanation.contains("could not be read"),
        "{}",
        o.stop_explanation
    );
    nothing_vanishes(&o, &app);
}

#[tokio::test]
async fn a_snapshot_whose_answer_could_not_be_read_is_unsettled_or_no_row() {
    // Mid-run: frontier_unsettled, with the snapshot flag.
    let mut app = SimApp::site();
    app.snapshot_unknown_from = Some(3);
    let o = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::FrontierUnsettled,
        "{}",
        o.stop_explanation
    );
    assert!(o.unsettled.snapshot_outcome_unknown, "{:?}", o.unsettled);
    nothing_vanishes(&o, &app);
    // At the first look: nothing explored, no row (502).
    let mut app = SimApp::site();
    app.snapshot_unknown_from = Some(0);
    let no_row = explore(&mut app, &request(home(), 60, 5, 60))
        .await
        .expect_err("no stop word is true of a run that never began");
    assert_eq!(no_row.kind, NoRowKind::StartUnknown);
}

#[tokio::test]
async fn a_start_url_whose_answer_could_not_be_read_is_no_row() {
    let mut app = SimApp::site();
    app.navigation_unknown = true;
    let no_row = explore(&mut app, &request(home(), 60, 5, 60))
        .await
        .expect_err("nothing explored, outcome unknown");
    assert_eq!(no_row.kind, NoRowKind::StartUnknown);
    assert!(
        no_row.detail.contains("could not be read"),
        "{}",
        no_row.detail
    );
}

#[test]
fn an_unknown_origin_answer_is_an_unknown_outcome_never_unreachable() {
    let body = json!({ "success": false, "error": "timed out", "failure_origin": "unknown" });
    assert!(matches!(
        classify_answer(body.clone()),
        Err(DriveFailure::OutcomeUnknown(_))
    ));
    assert!(matches!(
        classify_snapshot(StatusCode::BAD_GATEWAY, body),
        Err(DriveFailure::OutcomeUnknown(_))
    ));
    // Never delivered stays unreachable.
    let never = json!({ "success": false, "error": "x", "failure_origin": "transport" });
    assert!(matches!(
        classify_answer(never),
        Err(DriveFailure::Unreachable(_))
    ));
}

#[tokio::test]
async fn the_node_start_check_names_an_auth_wall_as_one() {
    let mut app = SimApp::site();
    app.current = Some("home".into());
    app.snapshot_auth_wall = true;
    let Refusal(status, msg) = check_node_start(&mut app, &page_node("home").key())
        .await
        .expect_err("an auth wall is not a reachable start");
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(msg.contains("authentication wall"), "{msg}");
}

#[tokio::test]
async fn the_node_start_check_never_names_a_stale_node() {
    // A previous look resolved `home`; the app is now on A and the check's
    // own snapshot never reaches the producer. The node it would report is
    // home — stale — so the check refuses rather than name it.
    let mut app = SimApp::site();
    app.current = Some("home".into());
    app.snapshot().await.unwrap();
    app.current = Some("A".into());
    app.drop_snapshot_events_from = Some(app.snapshots);
    let Refusal(status, msg) = check_node_start(&mut app, &page_node("home").key())
        .await
        .expect_err("a stale node is unknown");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(msg.contains("stale"), "{msg}");
    // A fresh look at the start node passes.
    let mut app = SimApp::site();
    app.current = Some("home".into());
    assert!(check_node_start(&mut app, &page_node("home").key())
        .await
        .is_ok());
}

// ---------------------------------------------------------------------------
// Review round 5: where a run starts from; the insert binds; runner failures
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_url_start_never_runs_its_first_edge_from_the_previous_runs_node() {
    let mut app = SimApp::site();
    let _ = run(&mut app, &request(home(), 60, 5, 60)).await;
    let last_of_run_one = app.cursor_last.clone().expect("run one ended on a node");
    assert_ne!(last_of_run_one.key(), unknown_node().key());
    let before = app.edges.len();
    let _ = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(app.forgets, 2, "every run forgets its cursor first");
    let start_nav = app.edges[before..]
        .iter()
        .find(|e| e.trigger.action_type == "navigate")
        .expect("run two navigates to its start URL");
    assert_eq!(
        start_nav.from_node.key(),
        unknown_node().key(),
        "the start navigation runs from the unknown node, not from {}",
        last_of_run_one.key()
    );
}

#[tokio::test]
async fn a_producer_that_cannot_forget_the_position_is_a_runner_failure_without_a_row() {
    let mut app = SimApp::site();
    app.forget_fails = true;
    let no_row = explore(&mut app, &request(home(), 60, 5, 60))
        .await
        .expect_err("no action is taken from a stale position");
    assert_eq!(no_row.kind, NoRowKind::Internal);
    assert_eq!(app.navigations, 0, "nothing was acted on");
}

#[tokio::test]
async fn a_runner_side_failure_is_no_row_never_app_unreachable() {
    struct BrokenRunner(SimApp);
    #[async_trait::async_trait]
    impl ExploreDriver for BrokenRunner {
        async fn snapshot(&mut self) -> Result<Value, DriveFailure> {
            Err(DriveFailure::Runner(
                "the snapshot route's body is not JSON".into(),
            ))
        }
        async fn activate(&mut self, e: &str, a: &str) -> Result<ActResult, DriveFailure> {
            self.0.activate(e, a).await
        }
        async fn navigate(&mut self, url: &str) -> Result<ActResult, DriveFailure> {
            self.0.navigate(url).await
        }
        async fn current_node(&mut self, d: Duration) -> Result<NodeReading, BarrierMiss> {
            self.0.current_node(d).await
        }
        async fn ledger_rows(&mut self) -> Result<LedgerRows, String> {
            self.0.ledger_rows().await
        }
        async fn forget_position(&mut self) -> Result<(), BarrierMiss> {
            self.0.forget_position().await
        }
        fn edge_events(&self) -> EdgeEvents {
            self.0.edge_events()
        }
        fn ledger_write_failures(&self) -> u64 {
            self.0.ledger_write_failures()
        }
        fn elapsed(&self) -> Duration {
            self.0.elapsed()
        }
    }
    let mut d = BrokenRunner(SimApp::site());
    let no_row = explore(&mut d, &request(home(), 60, 5, 60))
        .await
        .expect_err("a runner-side failure says nothing about the app");
    assert_eq!(no_row.kind, NoRowKind::Internal);
    assert!(no_row.detail.contains("not JSON"), "{}", no_row.detail);
}

#[test]
fn the_insert_binds_each_column_its_own_value_in_order() {
    // The INSERT's column list, in order, parsed out of the SQL.
    let sql = EXPLORATION_INSERT_SQL;
    let cols: Vec<&str> = sql
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(list, _)| list.split(',').map(str::trim).collect())
        .unwrap();
    let mut r = record(StopReason::FrontierUnsettled);
    // Distinct integers, so any two swapped binds differ.
    r.spent.actions = 7;
    r.edges_added = 5;
    let params = exploration_insert_params(&r, StopDetailScope::AnyStop);
    let names: Vec<&str> = params.iter().map(|(c, _)| *c).collect();
    assert_eq!(names, cols, "one bind per column, in the SQL's order");
    assert_eq!(params.len(), EXPLORATION_INSERT_BINDS);
    // Every column binds ITS value: distinct values, so a swap shows.
    let dbg = |col: &str| {
        let (_, v) = params.iter().find(|(c, _)| *c == col).unwrap();
        format!("{v:?}")
    };
    let want = |v: &dyn std::fmt::Debug| format!("{v:?}");
    assert_eq!(dbg("run_id"), want(&r.run_id));
    assert_eq!(dbg("app_id"), want(&r.app_id));
    assert_eq!(dbg("runner_build_id"), want(&r.runner_build_id));
    assert_eq!(dbg("runner_instance"), want(&r.runner_instance));
    assert_eq!(dbg("started_at"), want(&r.started_at));
    assert_eq!(dbg("finished_at"), want(&r.finished_at));
    assert_eq!(dbg("start"), want(&r.start));
    assert_eq!(dbg("budget_max_actions"), want(&10i32));
    assert_eq!(dbg("budget_max_depth"), want(&2i32));
    assert_eq!(dbg("budget_max_wall_clock_s"), want(&30i32));
    assert_eq!(dbg("spent_actions"), want(&7i32));
    assert_eq!(dbg("spent_depth"), want(&1i32));
    assert_eq!(dbg("spent_wall_clock_ms"), want(&1500i64));
    assert_eq!(dbg("stopped_because"), want(&"frontier_unsettled"));
    assert_eq!(
        dbg("stop_node"),
        want(&Some(
            serde_json::to_value(r.stop_node.as_ref().unwrap()).unwrap()
        ))
    );
    assert_eq!(dbg("edges_added"), want(&5i32));
    assert_eq!(
        dbg("frontier_remaining_by_reason"),
        want(&serde_json::to_value(&r.frontier_remaining_by_reason).unwrap())
    );
    assert_eq!(dbg("stop_detail"), want(&stop_detail_column(&r)));
}

#[tokio::test]
async fn a_node_start_check_never_closes_the_previous_runs_pending_edge() {
    // Run one acts, then every snapshot is refused: it stops with its last
    // action's edge still pending.
    let mut app = SimApp::site();
    app.snapshot_refused_from = Some(2);
    let first = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(first.stopped_because, StopReason::FrontierUnsettled);
    assert!(app.pending.is_some(), "run one left an edge pending");
    // Run two starts from a NODE: its check snapshots the app first. The run
    // forgets the cursor ONCE, before that check, so the check's snapshot
    // cannot close run one's edge with where the app is now.
    app.snapshot_refused_from = None;
    app.forgets = 0;
    let here = app.pages[app.current.as_ref().unwrap()].node.key();
    let _ = run(&mut app, &request(ExploreStart::Node(here), 60, 5, 60)).await;
    assert_eq!(app.forgets, 1, "a node start forgets exactly once");
    // The forget found run one's edge still pending — so the check's
    // snapshot, taken after it, closed nothing of run one's.
    assert_eq!(
        app.unobserved_closes, 1,
        "closed as unobserved by the forget"
    );
}

#[tokio::test]
async fn a_node_start_check_takes_no_action_when_the_producer_cannot_forget() {
    let mut app = SimApp::site();
    app.current = Some("home".into());
    app.forget_fails = true;
    let no_row = explore(
        &mut app,
        &request(ExploreStart::Node(page_node("home").key()), 60, 5, 60),
    )
    .await
    .expect_err("no snapshot is taken from a stale position");
    assert_eq!(no_row.kind, NoRowKind::Internal);
    assert!(
        no_row.detail.contains("unknown position"),
        "{}",
        no_row.detail
    );
    assert_eq!(app.snapshots, 0, "nothing was asked of the app");
    assert_eq!(app.forgets, 1);
}

#[tokio::test]
async fn a_node_start_is_checked_inside_the_run_after_its_one_forget() {
    // A start node the app has never been observed at: the run refuses it
    // (no row, a 422) before any action — the check runs inside `explore`,
    // so no caller can reach the loop with an unchecked node start.
    let mut app = SimApp::site();
    app.current = Some("home".into());
    let no_row = explore(
        &mut app,
        &request(ExploreStart::Node(page_node("nowhere").key()), 60, 5, 60),
    )
    .await
    .expect_err("an unobserved start node is not a reachable start");
    assert_eq!(
        no_row.kind,
        NoRowKind::StartCheck(StatusCode::UNPROCESSABLE_ENTITY)
    );
    assert!(
        no_row.detail.contains("never been observed"),
        "{}",
        no_row.detail
    );
    assert!(
        app.activations.is_empty() && app.navigations == 0,
        "nothing acted on"
    );
    assert_eq!(no_row.spent_actions, 0);
    assert_eq!(
        app.forgets, 1,
        "forgotten once, before the check's snapshot"
    );

    // An auth wall at the check is a 422 naming it; an unreadable answer a 502.
    let mut app = SimApp::site();
    app.current = Some("home".into());
    app.snapshot_auth_wall = true;
    let no_row = explore(
        &mut app,
        &request(ExploreStart::Node(page_node("home").key()), 60, 5, 60),
    )
    .await
    .unwrap_err();
    assert_eq!(
        no_row.kind,
        NoRowKind::StartCheck(StatusCode::UNPROCESSABLE_ENTITY)
    );
    assert!(
        no_row.detail.contains("authentication wall"),
        "{}",
        no_row.detail
    );

    // A URL start still forgets (once), and takes no check.
    let mut app = SimApp::site();
    let _ = run(&mut app, &request(home(), 60, 5, 60)).await;
    assert_eq!(app.forgets, 1);
}

#[test]
fn a_failed_attempt_is_never_relabelled_budget_exhausted() {
    // Two entries the last selection left to the budget; the run's attempt
    // at one FAILED (its replayed route did not hold). Under a budget stop
    // only the other is `budget_exhausted`.
    let mut accounting = Accounting::default();
    let failed: EntryKey = ("a#s".into(), "fp-failed".into());
    let left: EntryKey = ("a#s".into(), "fp-left".into());
    accounting.remaining(failed.clone(), FrontierReason::NotYetActivated);
    accounting.remaining(left.clone(), FrontierReason::NotYetActivated);
    let budget_left: BTreeSet<EntryKey> = [failed.clone(), left].into_iter().collect();
    let finished: BTreeSet<EntryKey> = [failed].into_iter().collect();
    let by = accounting.remaining_by_reason(StopReason::BudgetTime, &budget_left, &finished);
    assert_eq!(by["budget_exhausted"], 1, "{by:?}");
    assert_eq!(
        by["not_yet_activated"], 1,
        "the failed attempt keeps its reason"
    );
    // Not a budget stop: nothing is relabelled.
    let by = accounting.remaining_by_reason(StopReason::FrontierUnsettled, &budget_left, &finished);
    assert_eq!(by["budget_exhausted"], 0);
}

#[tokio::test]
async fn an_attempt_the_time_budget_cuts_short_is_budget_exhausted() {
    // One second per action and a three-second budget: the start URL, one
    // activation, then the next entry's route back through the start URL —
    // whose look finds the time spent. That entry was ATTEMPTED, but only the
    // budget kept it from the run, so it reads budget_exhausted.
    let mut app = SimApp::site();
    app.tick = Duration::from_secs(1);
    let o = run(&mut app, &request(home(), 60, 5, 3)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::BudgetTime,
        "{}",
        o.stop_explanation
    );
    assert_eq!(app.navigations, 2, "the second entry's route had begun");
    assert_eq!(app.activations.len(), 1);
    assert_eq!(
        o.attempts_finished.len(),
        1,
        "only the first activation finished; the second was cut short"
    );
    let by = by_reason(&o);
    assert_eq!(by["not_yet_activated"], 0, "{by:?}");
    // The cut-short entry and the other entry the last selection left to the
    // budget (the one waiting at the node the activation reached).
    assert_eq!(by["budget_exhausted"], 2, "{by:?}");
    nothing_vanishes(&o, &app);
}

#[tokio::test(start_paused = true)]
async fn an_undrivable_entry_reached_before_the_budget_ran_out_is_never_budget_exhausted() {
    // Home offers only `linkless` (a link with no click). The run reaches it
    // (it is at the start node), finds nothing safe to fire, and goes back to
    // the loop top, where the NEXT ledger read outlasts the time budget. Its
    // attempt FINISHED — undrivable — so the budget did not keep it from the
    // run: it keeps not_yet_activated, never budget_exhausted.
    let mut app = SimApp::empty();
    app.page("home", page_node("home"), vec![linkless()]);
    app.urls.insert("/home".into(), "home".into());
    app.tick = Duration::ZERO;
    app.on_tokio_clock();
    app.ledger_delays.insert(1, Duration::from_secs(10));
    let o = run(&mut app, &request(home(), 60, 5, 5)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::BudgetTime,
        "{}",
        o.stop_explanation
    );
    assert_eq!(o.unsettled.undrivable, 1, "{:?}", o.unsettled);
    assert_eq!(o.attempts_finished.len(), 1);
    let by = by_reason(&o);
    assert_eq!(by["budget_exhausted"], 0, "{by:?}");
    assert_eq!(by["not_yet_activated"], 1, "{by:?}");
}

#[tokio::test(start_paused = true)]
async fn a_failed_action_whose_event_was_dropped_is_never_budget_exhausted() {
    // `x` fails, and its action event is dropped, so its row stays
    // not_yet_activated in the ledger. The next ledger read outlasts the time
    // budget; the post-stop read then shows the row not_yet_activated again.
    // The attempt FINISHED (failed): never budget_exhausted.
    let mut app = SimApp::empty();
    app.page("home", page_node("home"), vec![link("x")]);
    app.urls.insert("/home".into(), "home".into());
    app.fail_once.insert("x".into());
    app.drop_actions = true;
    app.tick = Duration::ZERO;
    app.on_tokio_clock();
    app.ledger_delays.insert(1, Duration::from_secs(10));
    let o = run(&mut app, &request(home(), 60, 5, 5)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::BudgetTime,
        "{}",
        o.stop_explanation
    );
    assert_eq!(app.activations.len(), 1, "x was attempted, and failed");
    assert_eq!(o.attempts_finished.len(), 1);
    let by = by_reason(&o);
    assert_eq!(by["budget_exhausted"], 0, "{by:?}");
    assert_eq!(by["not_yet_activated"], 1, "{by:?}");
}

// ---------------------------------------------------------------------------
// Follow-ups to #2438: budget-stop unknowns, the stored row, runner origins
// ---------------------------------------------------------------------------

/// `journey_03`'s `ck_journey_explorations_stop_detail`, as PostgreSQL renders
/// it: `pg_get_constraintdef`, read from an ephemeral PostgreSQL 16.15 database
/// (`ephemeral-db.sh start pgvector`, 2026-10-29) after alembic applied
/// journey_03 to it.
const JOURNEY_03_STOP_DETAIL_CHECK: &str = "CHECK (((stop_detail IS NULL) OR ((stopped_because = \
     'frontier_unsettled'::text) AND (jsonb_typeof(stop_detail) = 'object'::text))))";
/// `journey_04`'s, read the same way from the same database after alembic
/// applied journey_04.
const JOURNEY_04_STOP_DETAIL_CHECK: &str =
    "CHECK (((stop_detail IS NULL) OR (jsonb_typeof(stop_detail) = 'object'::text)))";

#[tokio::test]
async fn a_budget_stops_failed_closing_look_is_kept_in_the_answer_and_the_row() {
    // The same budget run, clean: its block records nothing, so no detail.
    let mut clean = SimApp::site();
    let o = run(&mut clean, &request(home(), 2, 5, 60)).await;
    assert_eq!(o.stopped_because, StopReason::BudgetActions);
    assert!(!o.unsettled.any(), "{:?}", o.unsettled);
    assert_eq!(o.unsettled.closing_look_failed, Some(false));
    // Now the closing look's snapshot never reaches the producer (a stale
    // barrier: the runner cannot tie it to a node). Mid-run that is a 503
    // with no row; after an exhausted frontier it unconfirms the
    // exhaustion; after a BUDGET stop it used to vanish. It is kept.
    let mut app = SimApp::site();
    app.drop_snapshot_events_from = Some(clean.snapshots - 1);
    let o = run(&mut app, &request(home(), 2, 5, 60)).await;
    assert_eq!(
        o.stopped_because,
        StopReason::BudgetActions,
        "a budget stop stays one"
    );
    assert!(
        o.unsettled.any(),
        "the unknown is recorded: {:?}",
        o.unsettled
    );
    assert_eq!(o.unsettled.closing_look_failed, Some(true));
    assert_eq!(
        o.unsettled.exhaustion_unconfirmed, None,
        "no exhaustion was confirmed"
    );
    assert!(
        o.stop_explanation.contains("closing look"),
        "{}",
        o.stop_explanation
    );
    let r = assemble(
        "run".into(),
        "app".into(),
        &request(home(), 2, 5, 60),
        chrono::Utc::now(),
        Duration::from_millis(5),
        o,
    );
    check_record(&r).expect("a budget row may carry its unknowns (journey_04)");
    let detail = stop_detail_column(&r).expect("the stored row keeps the unknown");
    assert_eq!(detail["closingLookFailed"], true, "{detail}");
    let wire = serde_json::to_value(&r).unwrap();
    assert_eq!(wire["unsettled"]["closingLookFailed"], true, "{wire}");
}

#[test]
fn stop_detail_is_written_on_any_stop_that_records_an_unknown_and_withheld_on_journey_03() {
    let mut budget = record(StopReason::BudgetTime);
    assert_eq!(stop_detail_column(&budget), None, "nothing recorded: NULL");
    budget.unsettled.edge_events_dropped = 2;
    budget.unsettled.ledger_write_failures = 1;
    let detail = stop_detail_column(&budget).expect("a budget stop with unknowns");
    assert_eq!(detail, serde_json::to_value(budget.unsettled).unwrap());
    assert_eq!(detail["edgeEventsDropped"], 2);
    check_record(&budget).unwrap();
    // A passed or untaken confirmation records nothing on its own.
    let mut confirmed = record(StopReason::FrontierExhausted);
    confirmed.unsettled = earned();
    confirmed.unsettled.closing_look_failed = Some(false);
    assert_eq!(stop_detail_column(&confirmed), None);

    // journey_04 (AnyStop): bound as is.
    assert_eq!(
        stop_detail_bind(&budget, StopDetailScope::AnyStop),
        (Some(detail.clone()), false)
    );
    // journey_03 (UnsettledOnly): withheld from a budget row — never sent
    // where the CHECK would refuse the insert — and said so.
    assert_eq!(
        stop_detail_bind(&budget, StopDetailScope::UnsettledOnly),
        (None, true)
    );
    let bound = |r: &ExplorationResult, scope| {
        let params = exploration_insert_params(r, scope);
        let (_, v) = params.iter().find(|(c, _)| *c == "stop_detail").unwrap();
        format!("{v:?}")
    };
    assert_eq!(
        bound(&budget, StopDetailScope::UnsettledOnly),
        format!("{:?}", None::<Value>)
    );
    assert_eq!(
        bound(&budget, StopDetailScope::AnyStop),
        format!("{:?}", Some(&detail))
    );
    // A frontier_unsettled row carries it under either shape.
    let unsettled = record(StopReason::FrontierUnsettled);
    for scope in [StopDetailScope::AnyStop, StopDetailScope::UnsettledOnly] {
        let (v, withheld) = stop_detail_bind(&unsettled, scope);
        assert!(v.is_some() && !withheld, "{scope:?}");
    }
    // A row with nothing to withhold is not reported withheld.
    assert_eq!(
        stop_detail_bind(
            &record(StopReason::BudgetActions),
            StopDetailScope::UnsettledOnly
        ),
        (None, false)
    );
}

#[test]
fn the_schema_probe_tells_journey_03_from_journey_04() {
    // The SQL's second column is `check_restricts_stop_detail` over every
    // CHECK on the table, evaluated by Postgres.
    for pattern in [
        "pg_get_constraintdef(c.oid) LIKE '%stop_detail%'",
        "pg_get_constraintdef(c.oid) LIKE '%frontier_unsettled%'",
        "c.conrelid = to_regclass('project.journey_explorations')",
        "c.contype = 'c'",
    ] {
        assert!(EXPLORATION_SCHEMA_PROBE_SQL.contains(pattern), "{pattern}");
    }
    assert!(check_restricts_stop_detail(JOURNEY_03_STOP_DETAIL_CHECK));
    assert!(!check_restricts_stop_detail(JOURNEY_04_STOP_DETAIL_CHECK));
    // The stopped_because CHECK names frontier_unsettled but not stop_detail.
    assert!(!check_restricts_stop_detail(
        "CHECK ((stopped_because = ANY (ARRAY['frontier_unsettled'::text])))"
    ));
}

#[test]
fn a_runner_rejection_is_a_runner_failure_never_app_unreachable() {
    let rejected = json!({
        "success": false,
        "error": "page/navigate: not navigable",
        "failure_origin": crate::journey::capture::RUNNER_REJECTION_ORIGIN,
    });
    assert!(matches!(
        classify_answer(rejected.clone()),
        Err(DriveFailure::Runner(_))
    ));
    // A runner rejection is the runner's, WHATEVER the status: page/navigate
    // sends its rejection with a 200 and `success: false`. Both readers
    // agree, and neither reads it as the app's answer.
    for status in [
        StatusCode::OK,
        StatusCode::BAD_REQUEST,
        StatusCode::UNPROCESSABLE_ENTITY,
        StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        assert!(
            matches!(
                classify_snapshot(status, rejected.clone()),
                Err(DriveFailure::Runner(_))
            ),
            "{status}"
        );
    }
    // Checked before the auth reading, too: the runner refused before any UI
    // could have answered with an authentication code.
    let mut with_code = rejected.clone();
    with_code["code"] = json!("UNAUTHORIZED");
    assert!(matches!(
        classify_answer(with_code.clone()),
        Err(DriveFailure::Runner(_))
    ));
    assert!(matches!(
        classify_snapshot(StatusCode::OK, with_code),
        Err(DriveFailure::Runner(_))
    ));
    // An untagged 2xx success:false is still the app answering.
    assert!(matches!(
        classify_snapshot(StatusCode::OK, json!({ "success": false, "error": "x" })),
        Err(DriveFailure::AppAnswered(_))
    ));
    // Only `transport` reads as unreachable.
    let never = json!({ "success": false, "error": "x", "failure_origin": "transport" });
    assert!(matches!(
        classify_snapshot(StatusCode::BAD_GATEWAY, never),
        Err(DriveFailure::Unreachable(_))
    ));
}

#[tokio::test]
async fn a_runner_rejected_action_ends_the_run_without_a_row_never_app_unreachable() {
    struct RejectingRunner(SimApp);
    #[async_trait::async_trait]
    impl ExploreDriver for RejectingRunner {
        async fn snapshot(&mut self) -> Result<Value, DriveFailure> {
            self.0.snapshot().await
        }
        async fn activate(&mut self, _e: &str, _a: &str) -> Result<ActResult, DriveFailure> {
            classify_answer(json!({
                "success": false,
                "error": "rejected before any UI",
                "failure_origin": "runner",
            }))
        }
        async fn navigate(&mut self, url: &str) -> Result<ActResult, DriveFailure> {
            self.0.navigate(url).await
        }
        async fn current_node(&mut self, d: Duration) -> Result<NodeReading, BarrierMiss> {
            self.0.current_node(d).await
        }
        async fn ledger_rows(&mut self) -> Result<LedgerRows, String> {
            self.0.ledger_rows().await
        }
        async fn forget_position(&mut self) -> Result<(), BarrierMiss> {
            self.0.forget_position().await
        }
        fn edge_events(&self) -> EdgeEvents {
            self.0.edge_events()
        }
        fn ledger_write_failures(&self) -> u64 {
            self.0.ledger_write_failures()
        }
        fn elapsed(&self) -> Duration {
            self.0.elapsed()
        }
    }
    let mut d = RejectingRunner(SimApp::site());
    let no_row = explore(&mut d, &request(home(), 60, 5, 60))
        .await
        .expect_err("a runner rejection says nothing about the app");
    assert_eq!(no_row.kind, NoRowKind::Internal);
    assert!(no_row.detail.contains("rejected"), "{}", no_row.detail);
}
