//! Shared test fixtures for the journey routes and the timeline consumers.

use chrono::{TimeZone, Utc};
use qontinui_types::journey::{
    FrontierEntry, JourneyEdgeObservation, JourneyGraph, JourneyLedgerHealth, JourneyNode,
    LedgerState,
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

/// An answer's `status`, which every envelope carries.
pub(crate) fn status(v: &Value) -> &str {
    v["status"].as_str().expect("status is always present")
}

/// What an answer concludes: everything but the size of the counts.
pub(crate) fn verdict(v: &Value) -> Value {
    let result = if status(v) == "measured" {
        &v["value"]
    } else {
        &v["provenance"]["source"]["result"]
    };
    let keys = |field: &str| -> Vec<Value> {
        result[field]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|n| json!([n["nodeKey"], n["backOnly"]]))
                    .collect()
            })
            .unwrap_or_default()
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
        "deadEnds": keys("deadEnds"),
        "unexplored": keys("unexplored"),
        "settled": keys("nodes"),
        "unsettled": keys("unsettled"),
        "dimensions": dimensions,
        "unknownDestinations": result["unknownDestinationArrivals"].as_u64().unwrap_or(0) > 0,
    })
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
    let c = AnswerCtx {
        query: "q",
        app_id: "app",
        gaps: ReadGaps::default(),
        evaluated_at: Utc.with_ymd_and_hms(2026, 10, 24, 12, 0, 0).unwrap(),
    };
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
