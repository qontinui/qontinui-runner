//! Shared test fixtures for the journey routes and the timeline consumers.

use qontinui_types::journey::{
    FrontierEntry, JourneyEdgeObservation, JourneyLedgerHealth, JourneyNode, LedgerState,
};
use qontinui_vision_core::ChangeTimeline;
use serde_json::{json, Value};

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
            "targetFingerprint": fp,
            "targetRole": "button",
            "declaredEffect": null,
            "navigationTrigger": nav,
            "chokePoint": "element_action"
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
