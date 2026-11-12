//! Shared test fixtures for the journey routes and the timeline consumers.

use chrono::{TimeZone, Utc};
use qontinui_types::journey::{
    build_graph, FrontierEntry, JourneyEdgeObservation, JourneyFilter, JourneyGraph,
    JourneyLedgerHealth, JourneyNode, LedgerState,
};
use qontinui_vision_core::ChangeTimeline;
use serde_json::{json, Value};

use super::routes::{
    answer_dead_ends, answer_graph, answer_prior_knowledge, answer_reachable, answer_steps,
    AnswerCtx, ReadGaps,
};

pub(crate) fn writing() -> JourneyLedgerHealth {
    JourneyLedgerHealth {
        state: LedgerState::Writing,
        detail: "journey tables present".into(),
    }
}

pub(crate) fn node(spec: &str) -> JourneyNode {
    JourneyNode::new(Some(spec.to_string()), ["s".to_string()], None, None)
}

/// `<spec>#s`.
pub(crate) fn key(spec: &str) -> String {
    node(spec).key()
}

/// An edge `from -> to` (to = None: unobserved), triggered as `nav`
/// (`affordance`, `push`, …), closing as `outcome`.
pub(crate) fn edge(
    id: u32,
    from: &str,
    to: Option<&str>,
    nav: &str,
    outcome: &str,
    fp: &str,
) -> JourneyEdgeObservation {
    let affordance = nav == "affordance";
    serde_json::from_value(json!({
        "id": format!("00000000-0000-4000-8000-{id:012}"),
        "observedAt": format!("2026-10-01T10:{:02}:00.000000Z", id % 60),
        "appId": "app",
        "appVersion": "1.0.0",
        "runnerBuildId": "b",
        "runnerInstance": "primary",
        "runId": "run-1",
        "runKind": "agent_action",
        "fromNode": node(from),
        "toNode": to.map(node),
        "trigger": {
            "actionType": "click",
            // A navigation names no affordance (the contract refuses one).
            "targetFingerprint": if affordance { Some(fp) } else { None },
            "targetRole": if affordance { Some("button") } else { None },
            "declaredEffect": null,
            "navigationTrigger": nav,
            // A non-affordance trigger is a navigation, never an element action.
            "chokePoint": if affordance { "element_action" } else { "navigation" }
        },
        "outcome": outcome,
    }))
    .expect("fixture edge parses")
}

pub(crate) fn frontier(at: &str, fp: &str) -> FrontierEntry {
    serde_json::from_value(json!({
        "appId": "app",
        "nodeKey": key(at),
        "node": node(at),
        "affordanceFingerprint": fp,
        "affordanceRole": "link",
        "declaredEffect": "read",
        "reason": "not_yet_activated",
        "firstSeenAt": "2026-10-01T10:00:00.000000Z",
        "lastSeenAt": "2026-10-01T10:00:00.000000Z",
        "lastSeenRunId": null,
    }))
    .expect("fixture frontier parses")
}

fn prov() -> Value {
    json!({
        "producer": { "id": "sdk/timeline/events", "version": "test" },
        "observedAt": "2026-10-20T00:00:00Z",
        "evaluatedAt": "2026-10-20T00:00:00Z",
        "coverage": { "considered": 1, "measured": 1, "unmeasured": [] },
        "confidence": null,
        "cache": null,
        "source": null
    })
}

fn measured(v: Value) -> Value {
    json!({ "status": "measured", "value": v, "provenance": prov() })
}

/// The JSON of an SDK timeline: an action, then `events`, settled at
/// `settle_ms`. Every summary field measured.
pub(crate) fn timeline_json(events: Vec<Value>, settle_ms: u64) -> Value {
    let mut all = vec![json!({ "type": "action", "offsetMs": 0, "summary": "clicked Save" })];
    all.extend(events);
    json!({
        "events": all,
        "recordedMs": 1000,
        "settled": true,
        "observedSources": [
            "snapshot-poll", "layout-shift", "long-task", "animation-events",
            "get-animations", "states"
        ],
        "summary": {
            "firstResponseMs": measured(json!(10)),
            "settleMs": measured(json!(settle_ms)),
            "layoutShiftTotal": measured(json!(0)),
            "layoutShiftCount": measured(json!(0)),
            "layoutShiftAfterFirstResponseMs": measured(json!(0)),
            "animationsUnsettledAtEnd": measured(json!([])),
            "longTaskTotalMs": measured(json!(0)),
            "expectedStates": []
        }
    })
}

pub(crate) fn timeline(events: Vec<Value>, settle_ms: u64) -> ChangeTimeline {
    serde_json::from_value(timeline_json(events, settle_ms)).expect("fixture timeline parses")
}

pub(crate) fn anim(ty: &str, at: u64, name: &str) -> Value {
    let mut v = json!({
        "type": ty, "offsetMs": at, "summary": format!("{ty} {name}"),
        "animation": { "name": name, "targetElement": null, "isInfinite": false }
    });
    if ty == "animation-ended" {
        v["endReason"] = json!("finished");
    }
    v
}

/// A `page-changed` event carrying a URL with user input in its path.
pub(crate) fn page_changed(at: u64, url: &str) -> Value {
    json!({ "type": "page-changed", "offsetMs": at, "summary": format!("navigated to {url}"), "url": url })
}

// -- A grouped read concludes what its rows would -------------------------

/// The context every answer here is computed in.
fn answer_ctx() -> AnswerCtx<'static> {
    AnswerCtx {
        query: "q",
        app_id: "app",
        gaps: ReadGaps::default(),
        evaluated_at: Utc.with_ymd_and_hms(2026, 10, 24, 12, 0, 0).unwrap(),
    }
}

/// An answer's `status`, which every envelope carries.
pub(crate) fn status(v: &Value) -> &str {
    v["status"].as_str().expect("status is always present")
}

/// What an answer concludes: everything but the size of the counts.
///
/// Each conclusion is read by its identity, so two answers that list the same
/// NUMBER of things but different things never agree:
///
/// - a listed node by its key — `nodeKey` on every listing but the graph's own
///   nodes (`JourneyGraphNode`), which carry it as `key` — and `backOnly`;
/// - a graph edge by `(fromKey, toKey, trigger, outcome)`, never by how often or
///   when it was observed;
/// - a node `reachable` lists, with its distance;
/// - the `steps` answer's distance, and its path as `(fromKey, toKey)` steps.
pub(crate) fn verdict(v: &Value) -> Value {
    let result = if status(v) == "measured" {
        &v["value"]
    } else {
        &v["provenance"]["source"]["result"]
    };
    let node = |n: &Value| {
        let key = if n["nodeKey"].is_null() {
            &n["key"]
        } else {
            &n["nodeKey"]
        };
        json!([key, n["backOnly"]])
    };
    let dimensions: Vec<Value> = v["provenance"]["coverage"]["unmeasured"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| json!([d["dimension"], d["code"]]))
        .collect();
    json!({
        "status": v["status"],
        "code": v["unknown"]["code"],
        "deadEnds": each(result, "deadEnds", node),
        "unexplored": each(result, "unexplored", node),
        // The graph answer's nodes; the prior-knowledge answer's settled ones.
        "nodes": each(result, "nodes", node),
        "unsettled": each(result, "unsettled", node),
        "edges": each(result, "edges", |e| {
            json!([e["fromKey"], e["toKey"], e["trigger"], e["outcome"]])
        }),
        "reachable": each(result, "reachable", |n| json!([n["nodeKey"], n["steps"]])),
        "steps": result["steps"],
        "path": each(result, "path", |e| json!([e["fromKey"], e["toKey"]])),
        "dimensions": dimensions,
        "unknownDestinations": result["unknownDestinationArrivals"].as_u64().unwrap_or(0) > 0,
    })
}

/// Every entry of the list `result[field]` as `identity` reads it; none when the
/// answer has no such list.
fn each(result: &Value, field: &str, identity: impl Fn(&Value) -> Value) -> Vec<Value> {
    result[field]
        .as_array()
        .map(|entries| entries.iter().map(identity).collect())
        .unwrap_or_default()
}

/// Every graph answer reaches the same [`verdict`] over `whole` —
/// `build_graph` over every row of a ledger — and over `grouped` — the same
/// ledger read with its rows before `since` grouped
/// (`LedgerRead::build_graph`). Only the size of the counts may differ: a
/// grouped read counts each group once in every count but the three row totals
/// it corrects (`journey::read`'s module doc).
///
/// `reachable` is asked from `from`, and `steps` from `steps_from` to
/// `steps_to`; `what` names the ledger in a failure.
pub(crate) fn assert_same_conclusions(
    what: &str,
    whole: &JourneyGraph,
    grouped: &JourneyGraph,
    from: &str,
    steps_from: &str,
    steps_to: &str,
) {
    let c = answer_ctx();
    let pairs = [
        ("graph", answer_graph(&c, whole), answer_graph(&c, grouped)),
        (
            "dead-ends",
            answer_dead_ends(&c, whole),
            answer_dead_ends(&c, grouped),
        ),
        (
            "prior-knowledge",
            answer_prior_knowledge(&c, whole),
            answer_prior_knowledge(&c, grouped),
        ),
        (
            "reachable",
            answer_reachable(&c, whole, from),
            answer_reachable(&c, grouped, from),
        ),
        (
            "steps",
            answer_steps(&c, whole, steps_from, steps_to),
            answer_steps(&c, grouped, steps_from, steps_to),
        ),
    ];
    for (name, a, b) in pairs {
        assert_eq!(
            verdict(&a),
            verdict(&b),
            "{what}: the {name} answer concludes differently over grouped rows \
             than over every row\nrows {a:#}\ngroups {b:#}"
        );
    }
}

// -- The verdict itself -----------------------------------------------------

/// A graph over `edges` and `open` frontier entries, every row kept.
fn graph_of(edges: Vec<JourneyEdgeObservation>, open: Vec<FrontierEntry>) -> JourneyGraph {
    build_graph(&edges, &open, &writing(), &JourneyFilter::default())
}

/// The verdicts of `a` and `b` differ in each of `fields` and agree on
/// everything else, so `fields` alone tell the two answers apart.
fn differ_only_in(what: &str, fields: &[&str], a: &Value, b: &Value) {
    let (mut one, mut other) = (verdict(a), verdict(b));
    for &field in fields {
        assert_ne!(
            one[field], other[field],
            "{what}: the verdict reads no difference in `{field}`\n{a:#}\n{b:#}"
        );
        for v in [&mut one, &mut other] {
            v.as_object_mut()
                .expect("a verdict is an object")
                .remove(field);
        }
    }
    assert_eq!(
        one, other,
        "{what}: the answers should differ in {fields:?} only"
    );
}

/// Each conclusion is compared by identity: two answers listing as MANY
/// nodes, edges, reached nodes or steps, but different ones, never agree.
#[test]
fn the_verdict_tells_apart_answers_that_list_as_many_different_things() {
    let c = answer_ctx();
    let to = |from: &str, to: &str| edge(1, from, Some(to), "affordance", "changed", "fp");
    differ_only_in(
        "graph nodes: an open affordance at X, or at Y",
        &["nodes"],
        &answer_graph(
            &c,
            &graph_of(vec![to("a", "b")], vec![frontier("x", "fp-x")]),
        ),
        &answer_graph(
            &c,
            &graph_of(vec![to("a", "b")], vec![frontier("y", "fp-x")]),
        ),
    );
    differ_only_in(
        "graph edges: A -> B by one affordance, or by another",
        &["edges"],
        &answer_graph(&c, &graph_of(vec![to("a", "b")], vec![])),
        &answer_graph(
            &c,
            &graph_of(
                vec![edge(1, "a", Some("b"), "affordance", "changed", "fp-2")],
                vec![],
            ),
        ),
    );
    differ_only_in(
        "reachable: A reaches B, or C",
        &["reachable"],
        &answer_reachable(&c, &graph_of(vec![to("a", "b")], vec![]), &key("a")),
        &answer_reachable(&c, &graph_of(vec![to("a", "c")], vec![]), &key("a")),
    );
    differ_only_in(
        "reachable: C one step from A, or two",
        &["reachable"],
        &answer_reachable(
            &c,
            &graph_of(vec![to("a", "b"), to("a", "c")], vec![]),
            &key("a"),
        ),
        &answer_reachable(
            &c,
            &graph_of(vec![to("a", "b"), to("b", "c")], vec![]),
            &key("a"),
        ),
    );
    differ_only_in(
        "steps: A to D in one step, or two",
        &["steps", "path"],
        &answer_steps(
            &c,
            &graph_of(vec![to("a", "d")], vec![]),
            &key("a"),
            &key("d"),
        ),
        &answer_steps(
            &c,
            &graph_of(vec![to("a", "b"), to("b", "d")], vec![]),
            &key("a"),
            &key("d"),
        ),
    );
    differ_only_in(
        "steps: A to D in two steps, through B or through C",
        &["path"],
        &answer_steps(
            &c,
            &graph_of(vec![to("a", "b"), to("b", "d")], vec![]),
            &key("a"),
            &key("d"),
        ),
        &answer_steps(
            &c,
            &graph_of(vec![to("a", "c"), to("c", "d")], vec![]),
            &key("a"),
            &key("d"),
        ),
    );
}

/// How often, and when, an edge was observed is a count, not a conclusion:
/// the oracle's whole licence to compare a grouped read with its rows.
#[test]
fn the_verdict_reads_no_count() {
    let a_b = |id| edge(id, "a", Some("b"), "affordance", "changed", "fp");
    let once = graph_of(vec![a_b(1)], vec![]);
    let twice = graph_of(vec![a_b(1), a_b(2)], vec![]);
    assert_ne!(
        once.edges, twice.edges,
        "the fixture must differ in the edge's observation count and times"
    );
    assert_same_conclusions(
        "one observation of A -> B, or two",
        &once,
        &twice,
        &key("a"),
        &key("a"),
        &key("b"),
    );
}
