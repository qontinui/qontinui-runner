//! Route-level acceptance for plan Phase 2 (criteria 2-5) and the Phase 4
//! edge-timing read. Every assertion is on the SERIALIZED body the route
//! answers with, never on an intermediate.

use super::*;
use crate::journey::read::TimedEdge;
use crate::journey::test_fixtures::{edge, frontier, key, timeline, writing};
use chrono::TimeZone;
use qontinui_types::journey::{FrontierEntry, LedgerState};

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 24, 12, 0, 0).unwrap()
}

fn ctx(query: &'static str) -> AnswerCtx<'static> {
    AnswerCtx {
        query,
        app_id: "app",
        gaps: ReadGaps::default(),
        evaluated_at: at(),
    }
}

fn status(v: &Value) -> &str {
    v["status"].as_str().expect("status is always present")
}

fn code(v: &Value) -> &str {
    v["unknown"]["code"].as_str().unwrap_or("<none>")
}

fn ledger_state_wire(s: LedgerState) -> Value {
    serde_json::to_value(s).unwrap()
}

/// A -> B by an affordance; B has an OPEN frontier entry; C is observed only
/// through a typed URL from X. Is C reachable from A?
fn frontier_open_ledger() -> (Vec<JourneyEdgeObservation>, Vec<FrontierEntry>) {
    (
        vec![
            edge(1, "a", Some("b"), "affordance", "changed", "fp-a1"),
            edge(2, "x", Some("c"), "push", "changed", "fp-x1"),
        ],
        vec![frontier("b", "fp-b-open")],
    )
}

#[test]
fn acceptance_2_a_frontier_open_component_is_unknown_not_not_reachable() {
    let (edges, front) = frontier_open_ledger();
    let g = build_graph(&edges, &front, &writing(), &JourneyFilter::default());
    let v = answer_steps(&ctx("steps"), &g, &key("a"), &key("c"));
    assert_eq!(status(&v), "unknown", "{v:#}");
    assert_eq!(code(&v), "frontier_open");
    // The answer names B's affordance — what would have to be explored.
    let body = v.to_string();
    assert!(body.contains("fp-b-open"), "{v:#}");
    let unmeasured = v["provenance"]["coverage"]["unmeasured"]
        .as_array()
        .unwrap();
    assert!(
        unmeasured.iter().any(|d| d["code"] == "frontier_open"),
        "{unmeasured:?}"
    );

    // Exploring B (its frontier closes, the edge B -> C is observed) flips
    // it to a measured answer.
    let mut explored = edges.clone();
    explored.push(edge(
        3,
        "b",
        Some("c"),
        "affordance",
        "changed",
        "fp-b-open",
    ));
    let g = build_graph(&explored, &[], &writing(), &JourneyFilter::default());
    let v = answer_steps(&ctx("steps"), &g, &key("a"), &key("c"));
    assert_eq!(status(&v), "measured", "{v:#}");
    assert_eq!(v["value"]["steps"], 2);
}

#[test]
fn a_settled_negative_is_absent_with_full_coverage() {
    // A -> B, nothing open anywhere: "B reaches A" is a real negative.
    let edges = vec![edge(1, "a", Some("b"), "affordance", "changed", "fp-a1")];
    let g = build_graph(&edges, &[], &writing(), &JourneyFilter::default());
    let v = answer_steps(&ctx("steps"), &g, &key("b"), &key("a"));
    assert_eq!(status(&v), "absent", "{v:#}");
    assert!(v["provenance"]["coverage"]["unmeasured"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(v.get("value").is_none());
    assert!(v["provenance"]["source"]["result"]["frontier"].is_object());
    // And it parses back through the canonical envelope.
    serde_json::from_value::<Observation<Value>>(v).expect("canonical parse");
}

#[test]
fn acceptance_3_an_empty_ledger_is_edges_observed_zero_and_unknown() {
    let g = build_graph(&[], &[], &writing(), &JourneyFilter::default());
    let answers = [
        answer_graph(&ctx("graph"), &g),
        answer_reachable(&ctx("reachable"), &g, &key("a")),
        answer_steps(&ctx("steps"), &g, &key("a"), &key("b")),
        answer_dead_ends(&ctx("dead-ends"), &g),
        answer_prior_knowledge(&ctx("prior-knowledge"), &g),
        answer_frontier(&ctx("frontier"), &g, None),
        answer_frontier(&ctx("frontier"), &g, Some(&key("a"))),
    ];
    for v in answers {
        assert_eq!(status(&v), "unknown", "{v:#}");
        assert_eq!(code(&v), "no_observations", "{v:#}");
        assert_eq!(v["provenance"]["coverage"]["edgesObserved"], 0, "{v:#}");
        assert!(v.get("value").is_none(), "never [] alone: {v:#}");
    }
}

#[test]
fn acceptance_4_schema_absent_propagates_into_every_response() {
    let absent = JourneyLedgerHealth {
        state: LedgerState::SchemaAbsent,
        detail: "embedded DB predates the journey tables".into(),
    };
    let wire = ledger_state_wire(LedgerState::SchemaAbsent);
    // The read path: the tables do not exist.
    for q in [
        "graph",
        "reachable",
        "steps",
        "dead-ends",
        "prior-knowledge",
        "frontier",
        "edge-timing",
    ] {
        let v = answer_unavailable(
            q,
            "app",
            &JourneyFilter::default(),
            &absent,
            &LedgerUnavailable::SchemaAbsent,
            at(),
        );
        assert_eq!(status(&v), "unknown");
        assert_eq!(code(&v), "ledger_schema_absent");
        assert_eq!(v["provenance"]["coverage"]["ledger"]["state"], wire, "{q}");
        assert_eq!(v["provenance"]["source"]["ledger"]["state"], wire, "{q}");
    }
    // A graph computed while the health says schema_absent (rows read before
    // the probe flipped): every answer repeats the state, and no negative is
    // settled over it.
    let (edges, _) = frontier_open_ledger();
    let g = build_graph(&edges, &[], &absent, &JourneyFilter::default());
    for v in [
        answer_graph(&ctx("graph"), &g),
        answer_reachable(&ctx("reachable"), &g, &key("b")),
        answer_steps(&ctx("steps"), &g, &key("b"), &key("a")),
        answer_dead_ends(&ctx("dead-ends"), &g),
        answer_prior_knowledge(&ctx("prior-knowledge"), &g),
        answer_frontier(&ctx("frontier"), &g, Some(&key("b"))),
    ] {
        assert_eq!(
            v["provenance"]["coverage"]["ledger"]["state"], wire,
            "{v:#}"
        );
        if status(&v) != "measured" {
            assert_eq!(code(&v), "ledger_schema_absent", "{v:#}");
        } else {
            let dims = v["provenance"]["coverage"]["unmeasured"]
                .as_array()
                .unwrap();
            assert!(dims.iter().any(|d| d["code"] == "ledger_schema_absent"));
        }
    }
}

#[test]
fn a_read_failure_is_unknown_and_carries_no_counts() {
    let v = answer_unavailable(
        "graph",
        "app",
        &JourneyFilter::default(),
        &writing(),
        &LedgerUnavailable::Failed("PG pool error: timed out".into()),
        at(),
    );
    assert_eq!(status(&v), "unknown");
    assert_eq!(code(&v), "producer_failed");
    assert!(
        v["provenance"]["coverage"].get("edgesObserved").is_none(),
        "a failed read has no edge count, not zero: {v:#}"
    );
    assert!(v.get("value").is_none());
}

#[test]
fn a_truncated_read_never_settles_a_negative() {
    let edges = vec![edge(1, "a", Some("b"), "affordance", "changed", "fp-a1")];
    let g = build_graph(&edges, &[], &writing(), &JourneyFilter::default());
    let c = AnswerCtx {
        gaps: ReadGaps {
            edges_truncated: true,
            ..ReadGaps::default()
        },
        ..ctx("steps")
    };
    let v = answer_steps(&c, &g, &key("b"), &key("a"));
    assert_eq!(status(&v), "unknown", "{v:#}");
    assert_eq!(code(&v), "input_missing");
    let c = AnswerCtx {
        gaps: ReadGaps {
            frontier_truncated: true,
            ..ReadGaps::default()
        },
        ..ctx("steps")
    };
    let v = answer_steps(&c, &g, &key("b"), &key("a"));
    assert_eq!(code(&v), "frontier_open", "an unread frontier row is open");
}

#[test]
fn acceptance_5_same_ledger_same_filter_is_byte_identical_across_10_runs() {
    let (mut edges, mut front) = frontier_open_ledger();
    edges.push(edge(3, "b", Some("a"), "pop", "changed", "fp-back"));
    edges.push(edge(
        4,
        "a",
        None,
        "affordance",
        "to_node_unobserved",
        "fp-a2",
    ));
    front.push(frontier("a", "fp-a3"));
    let filter = JourneyFilter {
        since: None,
        app_version: Some("1.0.0".into()),
        run_kind: Some(RunKind::AgentAction),
    };
    let render = |edges: &[JourneyEdgeObservation], front: &[FrontierEntry]| -> String {
        let g = build_graph(edges, front, &writing(), &filter);
        let all = [
            answer_graph(&ctx("graph"), &g),
            answer_reachable(&ctx("reachable"), &g, &key("a")),
            answer_steps(&ctx("steps"), &g, &key("a"), &key("c")),
            answer_dead_ends(&ctx("dead-ends"), &g),
            answer_prior_knowledge(&ctx("prior-knowledge"), &g),
            answer_frontier(&ctx("frontier"), &g, None),
        ];
        serde_json::to_string(&all).unwrap()
    };
    let first = render(&edges, &front);
    for i in 0..10 {
        // Rotate the row order every run: order-independence is part of it.
        edges.rotate_left(1);
        if i % 2 == 0 {
            front.reverse();
        }
        assert_eq!(render(&edges, &front), first, "run {i} differed");
    }
    assert!(first.contains("\"filterEcho\""));
}

#[test]
fn every_answer_parses_through_the_canonical_envelope() {
    let (edges, front) = frontier_open_ledger();
    let g = build_graph(&edges, &front, &writing(), &JourneyFilter::default());
    for v in [
        answer_graph(&ctx("graph"), &g),
        answer_reachable(&ctx("reachable"), &g, &key("a")),
        answer_steps(&ctx("steps"), &g, &key("a"), &key("c")),
        answer_dead_ends(&ctx("dead-ends"), &g),
        answer_prior_knowledge(&ctx("prior-knowledge"), &g),
        answer_frontier(&ctx("frontier"), &g, None),
        answer_frontier(&ctx("frontier"), &g, Some(&key("b"))),
    ] {
        serde_json::from_value::<Observation<Value>>(v.clone())
            .unwrap_or_else(|e| panic!("{e}: {v:#}"));
        assert_eq!(v["provenance"]["source"]["appId"], "app");
        assert!(
            v["provenance"]["coverage"]["nodesObserved"].is_u64(),
            "{v:#}"
        );
    }
}

#[test]
fn a_positive_answer_is_measured_and_degraded_by_the_open_frontier() {
    let (edges, front) = frontier_open_ledger();
    let g = build_graph(&edges, &front, &writing(), &JourneyFilter::default());
    let v = answer_reachable(&ctx("reachable"), &g, &key("a"));
    assert_eq!(status(&v), "measured", "{v:#}");
    let reached: Vec<&str> = v["value"]["reachable"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["nodeKey"].as_str().unwrap())
        .collect();
    assert!(reached.contains(&key("b").as_str()));
    assert!(!v["provenance"]["coverage"]["unmeasured"]
        .as_array()
        .unwrap()
        .is_empty());
    let f = answer_frontier(&ctx("frontier"), &g, Some(&key("b")));
    assert_eq!(status(&f), "measured");
    assert_eq!(f["value"]["entries"].as_array().unwrap().len(), 1);
    let none = answer_frontier(&ctx("frontier"), &g, Some(&key("never")));
    assert_eq!(code(&none), "no_observations");
    // C has an observed edge (a typed URL) and nothing open: nothing on its
    // frontier is a real negative.
    let c = answer_frontier(&ctx("frontier"), &g, Some(&key("c")));
    assert_eq!(status(&c), "absent", "{c:#}");
}

fn params(q: &str) -> Result<JourneyParams, String> {
    let uri: axum::http::Uri = format!("http://x/?{q}").parse().unwrap();
    Query::<JourneyParams>::try_from_uri(&uri)
        .map(|Query(p)| p)
        .map_err(|e| e.to_string())
}

#[test]
fn parameters_are_closed_and_echoed() {
    assert!(
        params("sinse=2026-01-01T00:00:00Z").is_err(),
        "a misspelt filter is refused"
    );
    let p = params("since=2026-10-01T00:00:00Z&app_version=1.0.0&run_kind=explorer").unwrap();
    let f = p.filter().unwrap();
    assert_eq!(f.run_kind, Some(RunKind::Explorer));
    assert_eq!(f.app_version.as_deref(), Some("1.0.0"));
    assert!(f.since.is_some());
    assert!(params("run_kind=robot").unwrap().filter().is_err());
    assert!(params("since=yesterday").unwrap().filter().is_err());
    assert!(params("to=x").unwrap().only(&[]).is_err());
    assert!(params("from=x").unwrap().only(&["from"]).is_ok());
    let p = params("").unwrap();
    assert!(p.required("from", &p.from).is_err());

    // The filter is echoed into the answer.
    let g = build_graph(&[], &[], &writing(), &f);
    let v = answer_graph(&ctx("graph"), &g);
    assert_eq!(
        v["provenance"]["source"]["result"]["filterEcho"]["runKind"],
        "explorer"
    );
}

#[test]
fn every_returned_code_is_a_member_of_the_closed_enum() {
    for c in [
        "frontier_open",
        "ledger_schema_absent",
        "ledger_write_failing",
        "no_observations",
        "to_node_unobserved",
    ] {
        assert_eq!(unknown_code(c).as_str(), c);
    }
}

// -- Phase 4: edge timing ---------------------------------------------------

fn timed(id: u32, from: &str, to: &str, settle: Option<u64>) -> TimedEdge {
    TimedEdge {
        edge: edge(id, from, Some(to), "affordance", "changed", "fp"),
        timeline: settle.map(|ms| timeline(vec![], ms).for_ledger()),
        timeline_unparseable: false,
    }
}

#[test]
fn edge_timing_is_a_distribution_with_its_sample_size() {
    let rows = vec![
        timed(1, "a", "b", Some(100)),
        timed(2, "a", "b", Some(300)),
        timed(3, "a", "b", Some(200)),
        timed(4, "a", "b", None),
        timed(5, "a", "c", Some(9999)),
    ];
    let v = answer_edge_timing(
        &ctx("edge-timing"),
        &rows,
        &key("a"),
        &key("b"),
        &JourneyFilter::default(),
        &writing(),
    );
    assert_eq!(status(&v), "measured", "{v:#}");
    let t = &v["value"]["timing"];
    assert_eq!(t["n"], 4);
    assert_eq!(t["withoutTimeline"], 1);
    assert_eq!(t["settleMs"]["measured"], 3);
    assert_eq!(t["settleMs"]["min"], 100.0);
    assert_eq!(t["settleMs"]["max"], 300.0);
    assert_eq!(t["observations"].as_array().unwrap().len(), 4);
    let dims = v["provenance"]["coverage"]["unmeasured"]
        .as_array()
        .unwrap();
    assert!(dims
        .iter()
        .any(|d| d["dimension"] == "timeline" && d["count"] == 1));
}

#[test]
fn edge_timing_for_an_unobserved_edge_is_unknown() {
    let rows = vec![timed(1, "a", "b", Some(100))];
    let v = answer_edge_timing(
        &ctx("edge-timing"),
        &rows,
        &key("b"),
        &key("a"),
        &JourneyFilter::default(),
        &writing(),
    );
    assert_eq!(status(&v), "unknown");
    assert_eq!(code(&v), "no_observations");
    assert_eq!(v["provenance"]["source"]["result"]["timing"]["n"], 0);
    // Filtered out is counted, not hidden.
    let v = answer_edge_timing(
        &ctx("edge-timing"),
        &rows,
        &key("a"),
        &key("b"),
        &JourneyFilter {
            app_version: Some("2.0.0".into()),
            ..JourneyFilter::default()
        },
        &writing(),
    );
    assert_eq!(code(&v), "no_observations");
    assert_eq!(v["provenance"]["source"]["result"]["edgesFilteredOut"], 1);
}
