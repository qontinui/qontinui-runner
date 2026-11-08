//! Route-level acceptance for plan Phase 2 (criteria 2-5) and the Phase 4
//! edge-timing read. Every assertion is on the SERIALIZED body the route
//! answers with, never on an intermediate.

use super::*;
use crate::journey::read::{OlderEdgeGroup, TimedEdge};
use crate::journey::test_fixtures::{edge, frontier, key, timeline, writing};
use chrono::TimeZone;
use qontinui_types::journey::{FrontierEntry, JourneyEdgeObservation, LedgerState};
use serde_json::json;

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
        rows: 1,
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

// -- Review fixes: a listing is never measured over a gap -------------------

fn gappy(query: &'static str) -> AnswerCtx<'static> {
    AnswerCtx {
        gaps: ReadGaps {
            edges_truncated: true,
            ..ReadGaps::default()
        },
        ..ctx(query)
    }
}

#[test]
fn a_truncated_read_never_lists_a_dead_end_as_measured() {
    // A -> B and nothing open: B is a dead end over a COMPLETE read...
    let edges = vec![edge(1, "a", Some("b"), "affordance", "changed", "fp-a1")];
    let g = build_graph(&edges, &[], &writing(), &JourneyFilter::default());
    let v = answer_dead_ends(&ctx("dead-ends"), &g);
    assert_eq!(status(&v), "measured", "{v:#}");
    // ...but B's exit could be one of the edges beyond the read bound, and
    // its frontier row already deleted: over a truncated read it is unknown,
    // with the listing kept in the source.
    let v = answer_dead_ends(&gappy("dead-ends"), &g);
    assert_eq!(status(&v), "unknown", "{v:#}");
    assert_eq!(code(&v), "input_missing");
    assert!(
        !v["provenance"]["source"]["result"]["deadEnds"]
            .as_array()
            .unwrap()
            .is_empty(),
        "the listing is kept for the reader: {v:#}"
    );
    let p = answer_prior_knowledge(&gappy("prior-knowledge"), &g);
    assert_ne!(status(&p), "measured", "{p:#}");
    let unparseable = AnswerCtx {
        gaps: ReadGaps {
            rows_unparseable: 2,
            ..ReadGaps::default()
        },
        ..ctx("dead-ends")
    };
    let v = answer_dead_ends(&unparseable, &g);
    assert_eq!(code(&v), "producer_failed", "{v:#}");
}

#[test]
fn prior_knowledge_is_never_measured_beside_an_open_frontier() {
    // C arrives only by a typed URL; A -> B is an affordance edge.
    let edges = vec![
        edge(1, "a", Some("b"), "affordance", "changed", "fp-a1"),
        edge(2, "x", Some("c"), "push", "changed", "fp-x1"),
    ];
    // An open frontier anywhere keeps the gate open: unknown, never measured.
    let g = build_graph(
        &edges,
        &[frontier("b", "fp-open")],
        &writing(),
        &JourneyFilter::default(),
    );
    let gate = requires_prior_knowledge(&g)
        .frontier
        .unsettled_because(&g.coverage, true);
    assert!(gate.is_some(), "fixture must leave the gate open");
    let v = answer_prior_knowledge(&ctx("prior-knowledge"), &g);
    assert_eq!(status(&v), "unknown", "{v:#}");
    assert_eq!(code(&v), gate.unwrap());
    assert!(v["provenance"]["source"]["result"]["nodes"].is_array());
}

// -- Review fixes: the detail names the dimension; filters never claim the bound

#[test]
fn a_run_kind_query_over_a_small_ledger_does_not_claim_the_read_bound() {
    let mut explorer = edge(2, "b", Some("c"), "affordance", "changed", "fp-b1");
    explorer.run_kind = RunKind::Explorer;
    let rows = vec![
        serde_json::to_value(edge(1, "a", Some("b"), "affordance", "changed", "fp-a1")).unwrap(),
        serde_json::to_value(explorer).unwrap(),
    ];
    // The bounded read of a two-row ledger, exactly as `read_ledger` splits it.
    let (rows, truncated) =
        crate::journey::read::split_bound(rows, crate::journey::read::EDGE_READ_LIMIT);
    let (edges, bad) = crate::journey::read::parse_rows::<JourneyEdgeObservation>(rows);
    let read = LedgerRead {
        edges,
        older_groups: Vec::new(),
        frontier: Vec::new(),
        edges_truncated: truncated,
        older_groups_truncated: false,
        frontier_truncated: false,
        rows_unparseable: bad,
    };
    let filter = JourneyFilter {
        run_kind: Some(RunKind::AgentAction),
        ..JourneyFilter::default()
    };
    let g = build_graph(&read.edges, &read.frontier, &writing(), &filter);
    let c = AnswerCtx {
        gaps: ReadGaps::of(&read),
        ..ctx("steps")
    };
    // From B, the only departure is the explorer edge the filter removed.
    let v = answer_steps(&c, &g, &key("b"), &key("a"));
    assert_eq!(status(&v), "unknown", "{v:#}");
    let body = v.to_string();
    assert!(
        !body.contains("read bound") && !body.contains("edges_beyond_read_bound"),
        "a filter is not a truncated read: {v:#}"
    );
    let detail = v["unknown"]["detail"].as_str().unwrap();
    let dims: Vec<&str> = v["provenance"]["coverage"]["unmeasured"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["dimension"].as_str().unwrap())
        .collect();
    let named = dims
        .iter()
        .find(|d| detail.contains(**d))
        .unwrap_or_else(|| panic!("detail {detail:?} names none of {dims:?}"));
    assert_ne!(*named, "edges_beyond_read_bound");
}

#[test]
fn a_truncated_read_detail_names_the_read_bound() {
    let edges = vec![edge(1, "a", Some("b"), "affordance", "changed", "fp-a1")];
    let g = build_graph(&edges, &[], &writing(), &JourneyFilter::default());
    let v = answer_steps(&gappy("steps"), &g, &key("b"), &key("a"));
    assert!(
        v["unknown"]["detail"]
            .as_str()
            .unwrap()
            .contains("read bound"),
        "{v:#}"
    );
}

// -- Review fixes: edge-timing handles bad rows as the graph does -----------

#[test]
fn edge_timing_rejects_contract_violations_rather_than_sampling_or_filtering() {
    let mut bad_time = timed(2, "a", "b", Some(200));
    bad_time.edge.observed_at = "yesterday".into();
    // `no_change` between different keys fails validate().
    let invalid = TimedEdge {
        edge: edge(3, "a", Some("b"), "affordance", "no_change", "fp"),
        timeline: None,
        timeline_unparseable: false,
        rows: 1,
    };
    assert!(invalid.edge.validate().is_err(), "fixture must be invalid");
    // An untrusted row whose keys name ANOTHER pair: matching on keys before
    // the trust check would never see it.
    let mut elsewhere = timed(4, "c", "d", Some(300));
    elsewhere.edge.observed_at = "yesterday".into();
    // A withdrawn untrusted row: trust comes before withdrawal.
    let mut withdrawn_bad = timed(5, "a", "b", Some(400));
    withdrawn_bad.edge.observed_at = "yesterday".into();
    withdrawn_bad.edge.invalidated_at = Some("2026-10-02T00:00:00Z".into());
    let rows = vec![
        timed(1, "a", "b", Some(100)),
        bad_time,
        invalid,
        elsewhere,
        withdrawn_bad,
    ];
    // A `since` filter must not swallow the unparseable timestamp either.
    let filter = JourneyFilter {
        since: Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
        ..JourneyFilter::default()
    };
    let v = answer_edge_timing(
        &ctx("edge-timing"),
        &rows,
        &key("a"),
        &key("b"),
        &filter,
        &writing(),
    );
    // Every untrusted row in the read is rejected — including c -> d.
    assert_eq!(v["value"]["rowsRejected"], 4, "{v:#}");
    let dims = v["provenance"]["coverage"]["unmeasured"]
        .as_array()
        .unwrap();
    assert!(
        dims.iter()
            .any(|d| d["dimension"] == "unplaced_rejected_rows" && d["count"] == 4),
        "{dims:?}"
    );
    // ...and no untrusted row is ever a sample, filtered out or withdrawn.
    assert_eq!(status(&v), "measured", "{v:#}");
    assert_eq!(v["value"]["timing"]["n"], 1);
    let observed: Vec<&str> = v["value"]["timing"]["observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["observationId"].as_str().unwrap())
        .collect();
    assert_eq!(
        observed,
        vec![edge(1, "a", Some("b"), "affordance", "changed", "fp")
            .id
            .as_str()]
    );
    assert_eq!(v["value"]["edgesFilteredOut"], 0);
    assert_eq!(v["value"]["edgesInvalidated"], 0);
    assert_eq!(v["value"]["samplesByOutcome"]["changed"], 1);
    assert_eq!(v["value"]["samplesByTrigger"]["affordance"], 1);
}

#[test]
fn edge_timing_uses_the_shared_ledger_code() {
    let failing = JourneyLedgerHealth {
        state: LedgerState::WriteFailing,
        detail: "x".into(),
    };
    let v = answer_edge_timing(
        &ctx("edge-timing"),
        &[],
        &key("a"),
        &key("b"),
        &JourneyFilter::default(),
        &failing,
    );
    assert_eq!(code(&v), "ledger_write_failing");
}

// -- Review fix 2 on the shared per-node split ------------------------------

#[test]
fn prior_knowledge_lists_only_settled_nodes_as_measured() {
    // C is reached only by a typed URL; nothing is open anywhere.
    let settled = vec![
        edge(1, "a", Some("b"), "affordance", "changed", "fp-a1"),
        edge(2, "x", Some("c"), "push", "changed", "fp-x1"),
    ];
    let g = build_graph(&settled, &[], &writing(), &JourneyFilter::default());
    let p = requires_prior_knowledge(&g);
    assert!(p.nodes.iter().any(|n| n.node_key == key("c")), "{p:?}");
    if p.frontier.unsettled_because(&g.coverage, true).is_none() {
        let v = answer_prior_knowledge(&ctx("prior-knowledge"), &g);
        assert_eq!(status(&v), "measured", "{v:#}");
    }

    // A withdrawn click into C makes C doubtful: it may never be listed as a
    // measured finding.
    let mut withdrawn = edge(3, "a", Some("c"), "affordance", "changed", "fp-a2");
    withdrawn.invalidated_at = Some("2026-10-02T00:00:00Z".into());
    let mut rows = settled.clone();
    rows.push(withdrawn);
    let g = build_graph(&rows, &[], &writing(), &JourneyFilter::default());
    let p = requires_prior_knowledge(&g);
    assert!(
        p.unsettled.iter().any(|n| n.node_key == key("c")),
        "fixture must make C unsettled: {p:?}"
    );
    let v = answer_prior_knowledge(&ctx("prior-knowledge"), &g);
    if status(&v) == "measured" {
        let listed: Vec<&str> = v["value"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["nodeKey"].as_str().unwrap())
            .collect();
        assert!(!listed.contains(&key("c").as_str()), "{v:#}");
        let dims = v["provenance"]["coverage"]["unmeasured"]
            .as_array()
            .unwrap();
        assert!(
            dims.iter()
                .any(|d| d["dimension"] == "unsettled_prior_knowledge_nodes"),
            "a measured list with an unsettled remainder is degraded: {v:#}"
        );
    } else {
        assert_eq!(status(&v), "unknown", "never absent over a doubt: {v:#}");
        assert!(v["provenance"]["source"]["result"]["unsettled"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["nodeKey"] == key("c")));
    }
}

#[test]
fn edge_timing_applies_trust_before_withdrawal() {
    // An untrusted row is rejected even when it is also withdrawn, exactly
    // as build_graph counts it.
    let mut bad = timed(2, "a", "b", Some(200));
    bad.edge.observed_at = "yesterday".into();
    bad.edge.invalidated_at = Some("2026-10-02T00:00:00Z".into());
    let rows = vec![timed(1, "a", "b", Some(100)), bad];
    let v = answer_edge_timing(
        &ctx("edge-timing"),
        &rows,
        &key("a"),
        &key("b"),
        &JourneyFilter::default(),
        &writing(),
    );
    assert_eq!(v["value"]["rowsRejected"], 1, "{v:#}");
    assert_eq!(v["value"]["edgesInvalidated"], 0, "{v:#}");
}

#[test]
fn prior_knowledge_is_measured_for_settled_nodes_despite_an_unrelated_doubt() {
    // C arrives only by a typed URL. An explorer click A -> B is filtered out
    // by ?run_kind=agent_action: a doubt about B (and an open graph-wide
    // settle gate), not about C.
    let mut filtered = edge(3, "a", Some("b"), "affordance", "changed", "fp-a2");
    filtered.run_kind = RunKind::Explorer;
    let edges = vec![
        edge(1, "a", Some("b"), "affordance", "changed", "fp-a1"),
        edge(2, "x", Some("c"), "push", "changed", "fp-x1"),
        filtered,
    ];
    let filter = JourneyFilter {
        run_kind: Some(RunKind::AgentAction),
        ..JourneyFilter::default()
    };
    let g = build_graph(&edges, &[], &writing(), &filter);
    let p = requires_prior_knowledge(&g);
    assert!(
        p.nodes.iter().any(|n| n.node_key == key("c")),
        "the shared crate settles C: {p:?}"
    );
    assert!(
        p.frontier.unsettled_because(&g.coverage, true).is_some(),
        "the whole-graph gate is open, which must not void C"
    );
    let v = answer_prior_knowledge(&ctx("prior-knowledge"), &g);
    assert_eq!(status(&v), "measured", "{v:#}");
    // Over a read gap the same list is unknown.
    let v = answer_prior_knowledge(&gappy("prior-knowledge"), &g);
    assert_eq!(status(&v), "unknown", "{v:#}");
}

// -- Re-review 1: `since` is a build_graph filter, never a SQL cut ----------

#[test]
fn a_pre_since_exit_makes_the_node_unexplored_not_a_dead_end() {
    // B's only exit (B -> C) happened before `since`. The read hands
    // build_graph EVERY row; the filter then excludes the exit and the
    // shared crate records it as doubt about B.
    let mut old_exit = edge(1, "b", Some("c"), "affordance", "changed", "fp-b1");
    old_exit.observed_at = "2026-09-01T00:00:00.000000Z".into();
    let rows = vec![
        edge(2, "a", Some("b"), "affordance", "changed", "fp-a1"),
        old_exit,
    ];
    let filter = JourneyFilter {
        since: Some(Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap()),
        ..JourneyFilter::default()
    };
    let g = build_graph(&rows, &[], &writing(), &filter);
    let v = answer_dead_ends(&ctx("dead-ends"), &g);
    let listed = |field: &str| -> Vec<String> {
        let result = if status(&v) == "measured" {
            &v["value"]
        } else {
            &v["provenance"]["source"]["result"]
        };
        result[field]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["nodeKey"].as_str().unwrap().to_string())
            .collect()
    };
    assert!(!listed("deadEnds").contains(&key("b")), "{v:#}");
    assert!(listed("unexplored").contains(&key("b")), "{v:#}");
    assert!(
        !v["provenance"]["coverage"]["unmeasured"]
            .as_array()
            .unwrap()
            .is_empty(),
        "never a complete answer over a filtered exit: {v:#}"
    );
    assert_ne!(status(&v), "absent", "{v:#}");
    assert!(
        !v.to_string().contains("?since="),
        "no detail may offer ?since= as a way to narrow the read: {v:#}"
    );

    // The read-bound detail itself, produced by a truncated read over an
    // otherwise settled graph, names the bound and the lever that narrows it.
    let settled = vec![edge(2, "a", Some("b"), "affordance", "changed", "fp-a1")];
    let g = build_graph(&settled, &[], &writing(), &JourneyFilter::default());
    let v = answer_dead_ends(&gappy("dead-ends"), &g);
    let detail = v["unknown"]["detail"].as_str().expect("an unknown detail");
    assert!(detail.contains("read bound"), "{v:#}");
    assert!(detail.contains("`since`"), "{v:#}");
}

// -- A `since` read: rows before it arrive as groups ------------------------

/// The instant the fixtures below partition at.
fn cutoff() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap()
}

fn since_cutoff() -> JourneyFilter {
    JourneyFilter {
        since: Some(cutoff()),
        ..JourneyFilter::default()
    }
}

/// `row` as observed before [`cutoff`], with a distinct id per `n`.
fn before_cutoff(mut row: JourneyEdgeObservation, n: u32) -> JourneyEdgeObservation {
    row.id = format!("10000000-0000-4000-8000-{n:012}");
    row.observed_at = format!("2026-09-01T00:{:02}:00.000000Z", n % 60);
    row
}

/// `(template, withdrawn)`: one older transition of each kind `build_graph`
/// treats differently, all of them trusted.
fn trusted_templates() -> Vec<(JourneyEdgeObservation, bool)> {
    vec![
        // B's only exit: an excluded departure, so B is never a dead end.
        (
            edge(0, "b", Some("c"), "affordance", "changed", "fp-b"),
            false,
        ),
        // A withdrawn departure from D, so D is never a dead end either.
        (
            edge(0, "d", Some("e"), "affordance", "changed", "fp-d"),
            true,
        ),
        // An older click INTO U, whose kept arrival is a URL: never settled
        // as prior knowledge.
        (
            edge(0, "x", Some("u"), "affordance", "changed", "fp-x"),
            false,
        ),
        // A URL arrival, a back, and a dead click (a genuine negative).
        (edge(0, "f", Some("g"), "push", "changed", "fp-f"), false),
        (edge(0, "m", Some("n"), "pop", "changed", "fp-m"), false),
        (
            edge(0, "z", Some("z"), "affordance", "no_change", "fp-z"),
            false,
        ),
        // Closes whose destination is unknown.
        (
            edge(0, "p", None, "affordance", "to_node_unobserved", "fp-p"),
            false,
        ),
        (
            edge(0, "q", Some("r"), "affordance", "settle_timeout", "fp-q"),
            false,
        ),
        (
            edge(0, "s", Some("t"), "affordance", "error", "fp-s"),
            false,
        ),
    ]
}

/// Untrusted templates: `no_change` across two keys fails validate(). One live
/// and one withdrawn, which `build_graph` counts differently.
fn untrusted_templates() -> Vec<(JourneyEdgeObservation, bool)> {
    vec![
        (
            edge(0, "j", Some("k"), "affordance", "no_change", "fp-j"),
            false,
        ),
        (
            edge(0, "v", Some("w"), "affordance", "no_change", "fp-v"),
            true,
        ),
    ]
}

/// The same ledger two ways: every row, and full rows plus older groups whose
/// representatives are aggregates, as the SQL's are: a mix of columns that no
/// real row has, which the `since` filter must exclude before reading.
fn both_reads(
    templates: Vec<(JourneyEdgeObservation, bool)>,
    rows_each: u32,
) -> (Vec<JourneyEdgeObservation>, LedgerRead) {
    let recent = vec![
        edge(1, "a", Some("b"), "affordance", "changed", "fp-a"),
        edge(2, "a", Some("d"), "affordance", "changed", "fp-a2"),
        // A true dead end: seen, with no exit in any row.
        edge(3, "a", Some("end"), "affordance", "changed", "fp-a3"),
        // U is only ever reached by URL among the kept rows.
        edge(4, "a", Some("u"), "push", "changed", "fp-a4"),
        // Y is too, with nothing older arriving: settled prior knowledge.
        edge(5, "a", Some("y"), "push", "changed", "fp-a5"),
        // Z exists among the kept rows, so its older dead click is judged.
        edge(6, "a", Some("z"), "affordance", "changed", "fp-a6"),
    ];
    let mut every_row = recent.clone();
    let mut older_groups = Vec::new();
    for (t, (template, withdrawn)) in templates.into_iter().enumerate() {
        let t = u32::try_from(t).unwrap();
        let copies: Vec<JourneyEdgeObservation> = (0..rows_each)
            .map(|i| {
                let mut row = before_cutoff(template.clone(), t * 1000 + i);
                if withdrawn {
                    row.invalidated_at = Some("2026-09-02T00:00:00.000000Z".into());
                }
                row
            })
            .collect();
        every_row.extend(copies.iter().cloned());
        let mut representative = copies[0].clone();
        representative.app_version = Some("aggregate-of-many".into());
        representative.run_id = None;
        representative.runner_instance = "aggregate".into();
        older_groups.push(OlderEdgeGroup {
            edge: representative,
            rows: u64::from(rows_each),
        });
    }
    let read = LedgerRead {
        edges: recent,
        older_groups,
        ..LedgerRead::default()
    };
    (every_row, read)
}

/// What an answer concludes: everything but the size of the counts.
fn verdict(v: &Value) -> Value {
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

/// The `nodeKey`s an answer lists under `field`, compared whole: a substring
/// test would read `"end#s"` as listing `"d#s"`.
fn listed(v: &Value, field: &str) -> Vec<String> {
    let result = if status(v) == "measured" {
        &v["value"]
    } else {
        &v["provenance"]["source"]["result"]
    };
    result[field]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|n| n["nodeKey"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// [`trusted_templates`] without the closes whose destination is unknown,
/// which unsettle every prior-knowledge candidate on their own and so would
/// hide whether an older group did.
fn trusted_templates_with_known_destinations() -> Vec<(JourneyEdgeObservation, bool)> {
    trusted_templates()
        .into_iter()
        .filter(|(e, _)| {
            let from = e.from_node.key();
            e.to_node.is_some() && from != key("q") && from != key("s")
        })
        .collect()
}

/// Every graph answer agrees between the two reads.
fn assert_same_conclusions(templates: Vec<(JourneyEdgeObservation, bool)>) {
    let (every_row, read) = both_reads(templates, 5);
    let filter = since_cutoff();
    let whole = build_graph(&every_row, &[], &writing(), &filter);
    let grouped = read.build_graph(&writing(), &filter);
    let c = ctx("q");
    let pairs = [
        (
            "graph",
            answer_graph(&c, &whole),
            answer_graph(&c, &grouped),
        ),
        (
            "dead-ends",
            answer_dead_ends(&c, &whole),
            answer_dead_ends(&c, &grouped),
        ),
        (
            "prior-knowledge",
            answer_prior_knowledge(&c, &whole),
            answer_prior_knowledge(&c, &grouped),
        ),
        (
            "reachable",
            answer_reachable(&c, &whole, &key("a")),
            answer_reachable(&c, &grouped, &key("a")),
        ),
        (
            "steps",
            answer_steps(&c, &whole, &key("b"), &key("c")),
            answer_steps(&c, &grouped, &key("b"), &key("c")),
        ),
    ];
    for (name, a, b) in pairs {
        assert_eq!(verdict(&a), verdict(&b), "{name}\nrows {a:#}\ngroups {b:#}");
    }
}

/// The whole point of grouping: summarized rows change no conclusion. Only the
/// size of the counts differs, so a dropped or misjudged group shows here.
#[test]
fn older_groups_conclude_exactly_what_their_rows_would() {
    assert_same_conclusions(trusted_templates());
    assert_same_conclusions(trusted_templates_with_known_destinations());
    let mut with_untrusted = trusted_templates();
    with_untrusted.extend(untrusted_templates());
    assert_same_conclusions(with_untrusted);
}

/// The contract's core scenario over a grouped read, with nothing else
/// unsettling the graph: an exit before `since` keeps a node out of the dead
/// ends, and a node with no exit in any row is still listed as one.
#[test]
fn an_older_grouped_exit_keeps_a_node_from_being_a_dead_end() {
    let (_, read) = both_reads(trusted_templates(), 5);
    let g = read.build_graph(&writing(), &since_cutoff());
    let v = answer_dead_ends(&ctx("dead-ends"), &g);
    let dead = listed(&v, "deadEnds");
    let unexplored = listed(&v, "unexplored");
    assert!(dead.contains(&key("end")), "{v:#}");
    assert!(!dead.contains(&key("b")), "{v:#}");
    assert!(unexplored.contains(&key("b")), "{v:#}");
    assert!(!dead.contains(&key("d")), "withdrawn exit: {v:#}");
    assert!(unexplored.contains(&key("d")), "withdrawn exit: {v:#}");
    assert_ne!(status(&v), "absent", "{v:#}");

    // Removing just B's older exit makes B a settled dead end, so the
    // assertion above is really about that group.
    let mut without = read.clone();
    without
        .older_groups
        .retain(|group| group.edge.from_node.key() != key("b"));
    let g = without.build_graph(&writing(), &since_cutoff());
    let v = answer_dead_ends(&ctx("dead-ends"), &g);
    assert!(listed(&v, "deadEnds").contains(&key("b")), "{v:#}");
}

/// An older click into a node whose kept arrivals are all URLs means the node
/// may not need prior knowledge at all.
#[test]
fn an_older_grouped_click_unsettles_prior_knowledge() {
    let (_, read) = both_reads(trusted_templates_with_known_destinations(), 5);
    let g = read.build_graph(&writing(), &since_cutoff());
    let v = answer_prior_knowledge(&ctx("prior-knowledge"), &g);
    assert!(!listed(&v, "nodes").contains(&key("u")), "{v:#}");
    assert!(listed(&v, "unsettled").contains(&key("u")), "{v:#}");
    // Y has no older click into it, so it stays settled: the doubt on U is
    // the older group's, not the graph's.
    assert!(listed(&v, "nodes").contains(&key("y")), "{v:#}");

    // Without just the older click into U, U is settled too.
    let mut without = read.clone();
    without
        .older_groups
        .retain(|group| group.edge.to_node.as_ref().map(|n| n.key()) != Some(key("u")));
    let g = without.build_graph(&writing(), &since_cutoff());
    let v = answer_prior_knowledge(&ctx("prior-knowledge"), &g);
    assert!(listed(&v, "nodes").contains(&key("u")), "{v:#}");
}

/// The three row totals count ROWS however they were read, or `filteredOut 8`
/// would be reported over a thousand excluded rows.
#[test]
fn older_groups_keep_the_row_totals_of_their_rows() {
    let mut templates = trusted_templates();
    templates.extend(untrusted_templates());
    let (every_row, read) = both_reads(templates, 5);
    let filter = since_cutoff();
    let whole = build_graph(&every_row, &[], &writing(), &filter);
    let grouped = read.build_graph(&writing(), &filter);
    assert_eq!(
        grouped.coverage.edges_filtered_out, 40,
        "8 live trusted x 5"
    );
    assert_eq!(grouped.coverage.edges_invalidated, 10, "2 withdrawn x 5");
    assert_eq!(grouped.unplaced_rejected_rows, 10, "2 untrusted x 5");
    assert_eq!(
        grouped.coverage.edges_filtered_out,
        whole.coverage.edges_filtered_out
    );
    assert_eq!(
        grouped.coverage.edges_invalidated,
        whole.coverage.edges_invalidated
    );
    assert_eq!(grouped.unplaced_rejected_rows, whole.unplaced_rejected_rows);
    assert_eq!(
        grouped.coverage.edges_observed,
        whole.coverage.edges_observed
    );
    // The shared crate's invariant: edge entries' `rows` sum to the count.
    let entries: u64 = grouped.rejected.iter().map(|r| r.rows).sum();
    assert_eq!(entries, grouped.unplaced_rejected_rows);
    assert_eq!(read.older_rows(), 55);
}

/// Too many older groups is its own gap, with advice that works: a later
/// `since` would only push more rows into the groups.
#[test]
fn truncated_older_groups_are_their_own_gap() {
    let edges = vec![edge(1, "a", Some("b"), "affordance", "changed", "fp-a1")];
    let g = build_graph(&edges, &[], &writing(), &JourneyFilter::default());
    let c = AnswerCtx {
        gaps: ReadGaps {
            older_groups_truncated: true,
            ..ReadGaps::default()
        },
        ..ctx("dead-ends")
    };
    assert!(c.gaps.any());
    let v = answer_dead_ends(&c, &g);
    assert_eq!(status(&v), "unknown", "{v:#}");
    let detail = v["unknown"]["detail"].as_str().expect("an unknown detail");
    assert!(detail.contains("earlier `since`"), "{v:#}");
    assert!(!detail.contains("later `since`"), "{v:#}");
    assert!(
        v.to_string()
            .contains("older_groups_beyond_read_bound_20000"),
        "{v:#}"
    );
}

/// A group of one is simply a row, so no correction may be applied to it.
#[test]
fn an_older_group_of_one_is_a_row() {
    let (every_row, read) = both_reads(trusted_templates(), 1);
    let filter = since_cutoff();
    assert_eq!(
        read.build_graph(&writing(), &filter),
        build_graph(&every_row, &[], &writing(), &filter)
    );
}

/// With nothing older, the read is exactly `build_graph` over its rows.
#[test]
fn a_read_with_no_older_groups_is_plain_build_graph() {
    let rows = vec![edge(1, "a", Some("b"), "affordance", "changed", "fp-a")];
    let read = LedgerRead {
        edges: rows.clone(),
        ..LedgerRead::default()
    };
    let filter = JourneyFilter::default();
    assert_eq!(
        read.build_graph(&writing(), &filter),
        build_graph(&rows, &[], &writing(), &filter)
    );
    assert_eq!(read.older_rows(), 0);
}

/// Summarized rows are a disclosed resolution, not a gap: a settled listing
/// over them stays settled, and an unsummarized answer's bytes do not change.
#[test]
fn summarized_rows_are_disclosed_and_are_not_a_gap() {
    let gaps = ReadGaps {
        summarized_rows: 45,
        summarized_groups: 9,
        ..ReadGaps::default()
    };
    assert!(!gaps.any());
    assert!(gaps.dimensions().is_empty());
    let (_, read) = both_reads(trusted_templates(), 5);
    let of_read = ReadGaps::of(&read);
    assert_eq!(of_read.summarized_rows, 45);
    assert_eq!(of_read.summarized_groups, 9);

    let filter = since_cutoff();
    let g = read.build_graph(&writing(), &filter);
    let mut v = answer_dead_ends(&ctx("dead-ends"), &g);
    let untouched = v.clone();
    disclose_summarized(&mut v, &ReadGaps::default());
    assert_eq!(v, untouched, "nothing summarized, nothing added");
    disclose_summarized(&mut v, &of_read);
    assert_eq!(
        v["provenance"]["coverage"]["edgesSummarized"],
        json!({ "rows": 45, "groups": 9 })
    );
    assert!(serde_json::from_value::<Observation<Value>>(v).is_ok());
}

/// Edge-timing reports every count in rows: a group is weighed by its count,
/// and it is never a sample.
#[test]
fn edge_timing_weighs_an_older_group_by_its_rows() {
    let group = |id: u32, outcome: &str, rows: u64| TimedEdge {
        edge: before_cutoff(edge(id, "a", Some("b"), "affordance", outcome, "fp"), id),
        timeline: None,
        timeline_unparseable: false,
        rows,
    };
    let live = group(1, "changed", 7);
    let mut withdrawn = group(2, "changed", 3);
    withdrawn.edge.invalidated_at = Some("2026-09-02T00:00:00.000000Z".into());
    let untrusted = group(3, "no_change", 2);
    assert!(
        untrusted.edge.validate().is_err(),
        "fixture must be invalid"
    );
    let other_pair = TimedEdge {
        edge: before_cutoff(edge(4, "c", Some("d"), "affordance", "changed", "fp"), 4),
        timeline: None,
        timeline_unparseable: false,
        rows: 11,
    };
    let rows = [
        timed(9, "a", "b", Some(100)),
        live,
        withdrawn,
        untrusted,
        other_pair,
    ];
    let v = answer_edge_timing(
        &ctx("edge-timing"),
        &rows,
        &key("a"),
        &key("b"),
        &since_cutoff(),
        &writing(),
    );
    assert_eq!(status(&v), "measured", "{v:#}");
    assert_eq!(v["value"]["edgesFilteredOut"], 7, "{v:#}");
    assert_eq!(v["value"]["edgesInvalidated"], 3, "{v:#}");
    assert_eq!(v["value"]["rowsRejected"], 2, "{v:#}");
    assert_eq!(v["value"]["timing"]["n"], 1, "only the kept row: {v:#}");
}

// -- Re-review 2: an unexplored node always degrades the dead-end listing ---

#[test]
fn a_push_error_arrival_keeps_dead_ends_from_reading_absent_or_complete() {
    // B -> X is a typed URL that errored: X is observed only through an
    // arrival whose destination is unconfirmed, so X is unexplored.
    let unexplored_dim = |v: &Value| {
        v["provenance"]["coverage"]["unmeasured"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["dimension"] == "unexplored_nodes" && d["count"] == 1)
    };

    // With a dead end listed (B), the listing is degraded, never complete.
    let rows = vec![
        edge(1, "a", Some("b"), "affordance", "changed", "fp-a1"),
        edge(2, "b", Some("x"), "push", "error", "fp-unused"),
    ];
    let g = build_graph(&rows, &[], &writing(), &JourneyFilter::default());
    let d = dead_ends(&g);
    assert!(
        d.unexplored.iter().any(|n| n.node_key == key("x")),
        "fixture must leave X unexplored: {d:?}"
    );
    let v = answer_dead_ends(&ctx("dead-ends"), &g);
    assert!(unexplored_dim(&v), "{v:#}");

    // With no dead end at all (A <-> B), the empty listing is never absent.
    let rows = vec![
        edge(1, "a", Some("b"), "affordance", "changed", "fp-a1"),
        edge(2, "b", Some("a"), "affordance", "changed", "fp-b1"),
        edge(3, "b", Some("x"), "push", "error", "fp-unused"),
    ];
    let g = build_graph(&rows, &[], &writing(), &JourneyFilter::default());
    assert!(dead_ends(&g).dead_ends.is_empty());
    let v = answer_dead_ends(&ctx("dead-ends"), &g);
    assert_ne!(status(&v), "absent", "{v:#}");
    assert!(unexplored_dim(&v), "{v:#}");
}

// -- Round 4, L4: a gap never reads as full measurement --------------------

#[test]
fn edge_timing_coverage_never_claims_full_measurement_beside_a_gap() {
    // Every sample has a timeline, but the read holds an untrusted row: the
    // answer is degraded, so `measured` must be below `considered`.
    let mut bad = timed(2, "c", "d", Some(200));
    bad.edge.observed_at = "yesterday".into();
    let rows = vec![timed(1, "a", "b", Some(100)), bad];
    let v = answer_edge_timing(
        &ctx("edge-timing"),
        &rows,
        &key("a"),
        &key("b"),
        &JourneyFilter::default(),
        &writing(),
    );
    assert_eq!(status(&v), "measured", "{v:#}");
    let cov = &v["provenance"]["coverage"];
    assert!(!cov["unmeasured"].as_array().unwrap().is_empty(), "{v:#}");
    assert_eq!(cov["considered"], 1);
    assert!(
        cov["measured"].as_u64().unwrap() < cov["considered"].as_u64().unwrap(),
        "measured == considered beside a gap: {v:#}"
    );
}
