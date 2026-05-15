//! Two-phase evaluator orchestration. Step 4 of Plan 02.
//!
//! Composes the per-state, per-assertion two-phase loop:
//!
//! - **Phase 1** (`criteria::narrow_candidates` + `criteria::matches_all`) —
//!   binary pass/fail via the indexed snapshot. First Phase-1 winner wins
//!   in natural element-array order (determinism baked in by
//!   `narrow_candidates`).
//! - **Phase 2** (`diff::compute_miss`) — selectivity-weighted top-N
//!   candidate diagnostics for failed assertions.
//!
//! Internal rayon parallelism across states (and across specs in
//! [`evaluate_batch`]) — caller sync at the boundary. No tokio, no axum,
//! no Tauri types.

use std::cmp::Ordering;
use std::time::Instant;

use rayon::prelude::*;

use qontinui_types::ir::{IrAssertion, IrElementCriteria, IrPageSpec, IrState};

use crate::confidence::recommend_state;
use crate::criteria::{matches_all, narrow_candidates};
use crate::diff::compute_miss;
use crate::fetch::{compute_content_sha256, millis_to_iso8601};
use crate::snapshot::{ElementIdx, IndexedSnapshot};
use crate::{
    AssertionMiss, AssertionOutcome, AssertionResult, AssertionSeverityCounts, BridgeFingerprint,
    MatchOutcome, MatchedElement, SpecCheckResult, SpecCheckSummary, StateMatchResult,
    UIBridgeSnapshot,
};

// ===========================================================================
// Constants
// ===========================================================================

/// `result_schema_version` v1.
pub const RESULT_SCHEMA_VERSION: u32 = 1;

/// Sentinel `snapshot_id` used when the evaluator runs directly on a
/// pre-parsed snapshot (i.e. the caller didn't go through
/// [`crate::wrap_snapshot`]). HTTP / MCP adapters in Stream C re-mint a
/// `scs_<UUIDv7>` before persisting; in-process callers don't need the
/// telemetry handle.
const SENTINEL_SNAPSHOT_ID: &str = "scs_internal_eval";

/// Synthetic `app_id` used in the [`BridgeFingerprint`] when the evaluator
/// is called without a real fingerprint (no wrap_snapshot upstream).
const SENTINEL_APP_ID: &str = "internal-evaluator";

// ===========================================================================
// Public API
// ===========================================================================

/// Single-spec evaluation. Builds the [`IndexedSnapshot`] once and forces
/// the [`crate::diff::SelectivityIndex`] init on the main thread before any
/// rayon fan-out (so worker threads never race on the OnceCell).
#[tracing::instrument(
    name = "spec_check.evaluate",
    skip(snapshot, spec),
    fields(
        page_id = %spec.id,
        snapshot_id = tracing::field::Empty,
        spec_version = %spec.version,
        match_outcome = tracing::field::Empty,
        evaluation_duration_ms = tracing::field::Empty,
    ),
)]
pub fn evaluate(snapshot: &UIBridgeSnapshot, spec: &IrPageSpec) -> SpecCheckResult {
    let started = Instant::now();
    let indexed = IndexedSnapshot::new(snapshot);
    let _ = indexed.selectivity();
    let result = evaluate_with_indexed(&indexed, spec);
    let span = tracing::Span::current();
    span.record("snapshot_id", tracing::field::display(&result.snapshot_id));
    span.record(
        "match_outcome",
        tracing::field::debug(&result.summary.match_outcome),
    );
    span.record(
        "evaluation_duration_ms",
        started.elapsed().as_millis() as u64,
    );
    result
}

/// Multi-spec evaluation against a single snapshot. Builds [`IndexedSnapshot`]
/// once — shared read-only across all specs — then fan-outs across `specs`
/// via rayon. Load-bearing for the §5.14 batch endpoint.
#[tracing::instrument(
    name = "spec_check.evaluate_batch",
    skip(snapshot, specs),
    fields(
        page_count = specs.len(),
        snapshot_id = tracing::field::Empty,
        total_duration_ms = tracing::field::Empty,
    ),
)]
pub fn evaluate_batch(snapshot: &UIBridgeSnapshot, specs: &[IrPageSpec]) -> Vec<SpecCheckResult> {
    let started = Instant::now();
    let indexed = IndexedSnapshot::new(snapshot);
    let _ = indexed.selectivity();
    let results: Vec<SpecCheckResult> = specs
        .par_iter()
        .map(|spec| evaluate_with_indexed(&indexed, spec))
        .collect();
    let span = tracing::Span::current();
    if let Some(first) = results.first() {
        span.record("snapshot_id", tracing::field::display(&first.snapshot_id));
    }
    span.record("total_duration_ms", started.elapsed().as_millis() as u64);
    results
}

// ===========================================================================
// Core orchestration
// ===========================================================================

fn evaluate_with_indexed(indexed: &IndexedSnapshot, spec: &IrPageSpec) -> SpecCheckResult {
    let state_results: Vec<StateMatchResult> = spec
        .states
        .par_iter()
        .map(|s| evaluate_state(indexed, s))
        .collect();

    let summary = build_summary(spec, &state_results);
    let bridge_fingerprint = build_internal_fingerprint(indexed);
    let evaluated_at = now_iso8601();
    let spec_content_hash = hash_spec(spec);

    SpecCheckResult {
        result_schema_version: RESULT_SCHEMA_VERSION,
        snapshot_id: SENTINEL_SNAPSHOT_ID.to_string(),
        spec_content_hash,
        spec_version: spec.version.clone(),
        page_id: spec.id.clone(),
        state_results,
        summary,
        bridge_fingerprint,
        evaluated_at,
        warnings: Vec::new(),
    }
}

fn evaluate_state(indexed: &IndexedSnapshot, state: &IrState) -> StateMatchResult {
    // Preserve declaration order across enabled assertions. rayon's
    // `par_iter().collect()` preserves source order, so this is
    // deterministic without explicit indexing.
    let assertions: Vec<AssertionResult> = state
        .assertions
        .par_iter()
        .filter(|a| a.enabled)
        .map(|a| evaluate_assertion(indexed, a))
        .collect();

    let total = assertions.len();
    let passed = assertions
        .iter()
        .filter(|a| matches!(a.outcome, AssertionOutcome::Pass { .. }))
        .count();

    // Per Plan §Step 4 acceptance: empty enabled-assertion set → 0.0
    // (vacuous satisfaction is the §5.12 degenerate-spec bug B is partly
    // here to expose). Do NOT default to 1.0.
    let match_rate = if total == 0 {
        0.0
    } else {
        passed as f32 / total as f32
    };

    StateMatchResult {
        state_id: state.id.clone(),
        state_name: state.name.clone(),
        match_rate,
        assertions,
    }
}

fn evaluate_assertion(indexed: &IndexedSnapshot, assertion: &IrAssertion) -> AssertionResult {
    let crit = parse_criteria(&assertion.target.criteria);
    let outcome = match crit {
        Ok(crit) => run_two_phase_match(indexed, &crit),
        Err(_) => AssertionOutcome::Fail {
            miss: AssertionMiss {
                reason: crate::MissReason::NoCandidates,
                candidates: Vec::new(),
            },
        },
    };

    AssertionResult {
        assertion_id: assertion.id.clone(),
        description: assertion.description.clone(),
        severity: assertion.severity.clone(),
        category: assertion.category.clone(),
        outcome,
    }
}

/// Parse `IrAssertionTarget.criteria` (loosely typed `serde_json::Value`)
/// into a strongly-typed [`IrElementCriteria`]. The IR keeps the criteria
/// loose at the schema layer so synthesis can emit partial criteria
/// without tripping `deny_unknown_fields`; the matcher takes the strict
/// view. Unparseable criteria → error → assertion fails with
/// `MissReason::NoCandidates`.
fn parse_criteria(value: &serde_json::Value) -> Result<IrElementCriteria, serde_json::Error> {
    // `null` or omitted criteria deserialize to the default (every field
    // None), which `matches_all` treats as "any element matches".
    if value.is_null() {
        return Ok(IrElementCriteria::default());
    }
    serde_json::from_value(value.clone())
}

/// Two-phase matcher core:
/// - Phase 1: `narrow_candidates` → first `matches_all` winner → Pass.
/// - Phase 2: `compute_miss` → Fail with structured AssertionMiss.
fn run_two_phase_match(indexed: &IndexedSnapshot, crit: &IrElementCriteria) -> AssertionOutcome {
    for idx in narrow_candidates(indexed, crit) {
        if matches_all(indexed, crit, idx) {
            return AssertionOutcome::Pass {
                matched: matched_from(indexed, idx),
            };
        }
    }
    AssertionOutcome::Fail {
        miss: compute_miss(indexed, crit),
    }
}

fn matched_from(indexed: &IndexedSnapshot, idx: ElementIdx) -> MatchedElement {
    let el = indexed.element(idx);
    MatchedElement {
        element_id: Some(el.id.clone()).filter(|s| !s.is_empty()),
        role: el.role.clone().filter(|s| !s.is_empty()),
        text: el.text.clone().filter(|s| !s.is_empty()),
        path: el.identifier.selector.clone(),
    }
}

// ===========================================================================
// Summary aggregation
// ===========================================================================

fn build_summary(spec: &IrPageSpec, state_results: &[StateMatchResult]) -> SpecCheckSummary {
    let overall_match_rate = mean_match_rate(state_results);
    let match_outcome = match_outcome_for(state_results);
    let severity_counts = aggregate_severity_counts(state_results);

    // recommend_state needs an is_initial lookup over the IR.
    let recommended_state = recommend_state(state_results, |sid| {
        spec.states
            .iter()
            .find(|s| s.id == sid)
            .and_then(|s| s.is_initial)
            .unwrap_or(false)
    });

    SpecCheckSummary {
        match_outcome,
        overall_match_rate,
        severity_counts,
        recommended_state,
    }
}

fn mean_match_rate(state_results: &[StateMatchResult]) -> f32 {
    if state_results.is_empty() {
        return 0.0;
    }
    let sum: f32 = state_results.iter().map(|s| s.match_rate).sum();
    sum / state_results.len() as f32
}

/// `FullMatch` iff every assertion across every state passed; `NoMatch` iff
/// zero assertions passed; `PartialMatch` otherwise. Empty state set →
/// `NoMatch` (degenerate spec).
fn match_outcome_for(state_results: &[StateMatchResult]) -> MatchOutcome {
    if state_results.is_empty() {
        return MatchOutcome::NoMatch;
    }
    let mut total = 0usize;
    let mut passed = 0usize;
    for s in state_results {
        for a in &s.assertions {
            total += 1;
            if matches!(a.outcome, AssertionOutcome::Pass { .. }) {
                passed += 1;
            }
        }
    }
    if total == 0 {
        // Every state has zero enabled assertions → spec is degenerate.
        // Match what `evaluate_state` does for an empty set (rate = 0.0)
        // by emitting NoMatch.
        return MatchOutcome::NoMatch;
    }
    if passed == total {
        MatchOutcome::FullMatch
    } else if passed == 0 {
        MatchOutcome::NoMatch
    } else {
        MatchOutcome::PartialMatch
    }
}

/// Aggregate failure counts across every state. Unknown severity strings
/// route to the `error` bucket (the "least restrictive" choice — visible to
/// policy gates that fail on `error` and above, but doesn't escalate to
/// `critical`).
fn aggregate_severity_counts(state_results: &[StateMatchResult]) -> AssertionSeverityCounts {
    let mut counts = AssertionSeverityCounts::default();
    for s in state_results {
        for a in &s.assertions {
            if !matches!(a.outcome, AssertionOutcome::Fail { .. }) {
                continue;
            }
            // Severity matching is case-insensitive — wire is free-form but
            // the four canonical buckets are the AI debugger's surface.
            match a.severity.to_ascii_lowercase().as_str() {
                "critical" => counts.critical += 1,
                "error" => counts.error += 1,
                "warning" | "warn" => counts.warning += 1,
                "info" | "informational" | "notice" => counts.info += 1,
                // Unknown → default to `error` bucket per design decision
                // (load-bearing for `SpecCheckStepConfig.fail_on` policies
                // that don't enumerate every custom severity).
                _ => counts.error += 1,
            }
        }
    }
    counts
}

// ===========================================================================
// Internal fingerprint + content hash
// ===========================================================================

fn build_internal_fingerprint(indexed: &IndexedSnapshot) -> BridgeFingerprint {
    let snapshot_timestamp =
        millis_to_iso8601(indexed.raw.timestamp).unwrap_or_else(|_| now_iso8601());
    BridgeFingerprint {
        app_id: SENTINEL_APP_ID.to_string(),
        app_version: None,
        route: None,
        bridge_version: None,
        snapshot_timestamp,
        element_count: indexed.raw.elements.len() as u32,
    }
}

/// JCS + SHA-256 of the IR spec — `sha256-<hex>`. Routes through the
/// fetch-module helper to keep the canonicalization rules in one place.
/// On serialization failure (shouldn't happen for a well-typed IR), emits
/// a fixed sentinel hash so callers can still observe the result.
fn hash_spec(spec: &IrPageSpec) -> String {
    match serde_json::to_value(spec) {
        Ok(value) => compute_content_sha256(&value)
            .unwrap_or_else(|_| "sha256-".to_string()),
        Err(_) => "sha256-".to_string(),
    }
}

fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

// ===========================================================================
// Ordering helper (kept module-internal for clarity even though we don't
// expose sort comparators on the public API)
// ===========================================================================

#[allow(dead_code)]
fn f32_desc(a: f32, b: f32) -> Ordering {
    b.partial_cmp(&a).unwrap_or(Ordering::Equal)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_types::ir::{IrAssertion, IrAssertionTarget};
    use qontinui_types::ui_bridge::{
        ElementIdentifier, ElementRect, ElementState, UIBridgeElement, UIBridgeSnapshot,
    };
    use serde_json::json;

    // ----- helpers -----

    fn state_block(visible: bool) -> ElementState {
        ElementState {
            visible,
            enabled: true,
            focused: false,
            rect: ElementRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
                left: 0.0,
            },
            value: None,
            checked: None,
            selected_options: None,
            text_content: None,
        }
    }

    fn ident(selector: &str) -> ElementIdentifier {
        ElementIdentifier {
            ui_id: None,
            test_id: None,
            awas_id: None,
            html_id: None,
            xpath: String::new(),
            selector: selector.to_string(),
        }
    }

    fn elem(
        id: &str,
        selector: &str,
        role: Option<&str>,
        text: Option<&str>,
        visible: bool,
    ) -> UIBridgeElement {
        UIBridgeElement {
            id: id.to_string(),
            element_type: "div".to_string(),
            label: None,
            actions: Vec::new(),
            custom_actions: None,
            identifier: ident(selector),
            state: state_block(visible),
            registered_at: 0,
            mounted: true,
            bbox: None,
            visible: Some(visible),
            role: role.map(String::from),
            tag_name: None,
            aria_label: None,
            accessible_name: None,
            text: text.map(String::from),
        }
    }

    fn snap(elements: Vec<UIBridgeElement>) -> UIBridgeSnapshot {
        UIBridgeSnapshot {
            timestamp: 1_700_000_000_000,
            elements,
            components: Vec::new(),
            workflows: Vec::new(),
            modal_stack: None,
            toasts: None,
            undo_redo: None,
            current_route: None,
            segments: Vec::new(),
        }
    }

    fn assertion(
        id: &str,
        severity: &str,
        crit: serde_json::Value,
        enabled: bool,
    ) -> IrAssertion {
        IrAssertion {
            id: id.to_string(),
            description: format!("desc-{id}"),
            category: "a11y".to_string(),
            severity: severity.to_string(),
            assertion_type: "exists".to_string(),
            target: IrAssertionTarget {
                kind: "search".to_string(),
                criteria: crit,
                label: format!("label-{id}"),
            },
            source: "test".to_string(),
            reviewed: false,
            enabled,
            precondition: None,
        }
    }

    fn state(id: &str, name: &str, is_initial: Option<bool>, assertions: Vec<IrAssertion>) -> IrState {
        IrState {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            assertions,
            excluded_elements: None,
            conditions: None,
            is_initial,
            is_terminal: None,
            blocking: None,
            group: None,
            path_cost: None,
            precondition: None,
            element_ids: None,
            incoming_transitions: None,
            metadata: None,
            provenance: None,
            cross_refs: None,
        }
    }

    fn page_spec(id: &str, states: Vec<IrState>) -> IrPageSpec {
        IrPageSpec {
            version: "1.0".to_string(),
            id: id.to_string(),
            name: format!("{id}-name"),
            description: None,
            metadata: None,
            provenance: None,
            states,
            transitions: Vec::new(),
            synthesized_groups: None,
            initial_state: None,
        }
    }

    // ----- evaluate (happy path) -----

    #[test]
    fn evaluate_full_match_when_every_assertion_passes() {
        let s = snap(vec![elem(
            "btn-save",
            "#btn-save",
            Some("button"),
            Some("Save"),
            true,
        )]);

        let a = assertion(
            "a1",
            "critical",
            json!({ "role": "button", "text": "Save" }),
            true,
        );
        let st = state("s1", "Loaded", Some(true), vec![a]);
        let spec = page_spec("page-1", vec![st]);

        let result = evaluate(&s, &spec);
        assert_eq!(result.result_schema_version, 1);
        assert_eq!(result.spec_version, "1.0");
        assert_eq!(result.page_id, "page-1");
        assert_eq!(result.state_results.len(), 1);
        assert_eq!(result.state_results[0].match_rate, 1.0);
        assert_eq!(result.summary.match_outcome, MatchOutcome::FullMatch);
        assert_eq!(result.summary.overall_match_rate, 1.0);
        assert!(result.summary.recommended_state.is_some());
        let rec = result.summary.recommended_state.unwrap();
        assert_eq!(rec.state_id, "s1");

        // Bridge fingerprint sentinel.
        assert_eq!(result.bridge_fingerprint.app_id, "internal-evaluator");
        assert_eq!(result.bridge_fingerprint.element_count, 1);

        // spec_content_hash format.
        assert!(result.spec_content_hash.starts_with("sha256-"));

        // evaluated_at format (ISO-8601 with .sssZ suffix).
        assert!(result.evaluated_at.ends_with('Z'));
        assert!(result.evaluated_at.contains('T'));
    }

    #[test]
    fn evaluate_no_match_when_nothing_passes() {
        let s = snap(vec![elem(
            "e0", "#e0", Some("link"), Some("Cancel"), true,
        )]);
        let a = assertion("a1", "critical", json!({ "role": "button", "text": "Save" }), true);
        let st = state("s1", "Loaded", Some(true), vec![a]);
        let spec = page_spec("page-1", vec![st]);

        let result = evaluate(&s, &spec);
        assert_eq!(result.state_results[0].match_rate, 0.0);
        assert_eq!(result.summary.match_outcome, MatchOutcome::NoMatch);

        // The failing assertion is critical → severity_counts.critical = 1.
        assert_eq!(result.summary.severity_counts.critical, 1);
        assert_eq!(result.summary.severity_counts.error, 0);
    }

    #[test]
    fn evaluate_partial_match_mixed_outcomes() {
        let s = snap(vec![
            elem("ok", "#ok", Some("button"), Some("Save"), true),
            elem("nope", "#nope", Some("link"), Some("Other"), true),
        ]);
        let a1 = assertion("a1", "critical", json!({ "role": "button", "text": "Save" }), true);
        let a2 = assertion("a2", "error", json!({ "role": "checkbox" }), true);
        let st = state("s1", "Loaded", Some(true), vec![a1, a2]);
        let spec = page_spec("page-1", vec![st]);

        let result = evaluate(&s, &spec);
        assert_eq!(result.state_results[0].match_rate, 0.5);
        assert_eq!(result.summary.match_outcome, MatchOutcome::PartialMatch);
        assert_eq!(result.summary.severity_counts.error, 1);
    }

    #[test]
    fn evaluate_empty_assertions_yields_zero_match_rate() {
        // Plan §Step 4 acceptance: vacuous satisfaction is the §5.12
        // degenerate-spec bug; empty enabled-assertion set must NOT default
        // to 1.0.
        let s = snap(vec![]);
        let st = state("s1", "Loaded", Some(true), Vec::new());
        let spec = page_spec("page-1", vec![st]);

        let result = evaluate(&s, &spec);
        assert_eq!(result.state_results[0].match_rate, 0.0);
        assert!(result.state_results[0].assertions.is_empty());
        assert_eq!(result.summary.match_outcome, MatchOutcome::NoMatch);
        // recommend_state returns None when no state has a band.
        assert!(result.summary.recommended_state.is_none());
    }

    #[test]
    fn evaluate_skips_disabled_assertions() {
        let s = snap(vec![elem(
            "ok", "#ok", Some("button"), Some("Save"), true,
        )]);
        let a_pass = assertion("pass", "info", json!({ "role": "button" }), true);
        let a_skip = assertion("skip", "critical", json!({ "role": "nope" }), false);
        let st = state("s1", "Loaded", Some(true), vec![a_skip, a_pass]);
        let spec = page_spec("page-1", vec![st]);

        let result = evaluate(&s, &spec);
        // Only the enabled assertion shows up.
        assert_eq!(result.state_results[0].assertions.len(), 1);
        assert_eq!(result.state_results[0].assertions[0].assertion_id, "pass");
        assert_eq!(result.state_results[0].match_rate, 1.0);
    }

    #[test]
    fn evaluate_preserves_declaration_order_across_reruns() {
        // Plan §Step 4 determinism rule: per-state assertion order in
        // `state_results[*].assertions` matches the IR's authoring order.
        let s = snap(vec![elem(
            "ok", "#ok", Some("button"), Some("Save"), true,
        )]);
        let a1 = assertion("a1", "info", json!({ "role": "button" }), true);
        let a2 = assertion("a2", "info", json!({ "role": "button" }), true);
        let a3 = assertion("a3", "info", json!({ "role": "button" }), true);
        let a4 = assertion("a4", "info", json!({ "role": "button" }), true);
        let st = state("s1", "Loaded", Some(true), vec![a1, a2, a3, a4]);
        let spec = page_spec("page-1", vec![st]);

        for _ in 0..50 {
            let result = evaluate(&s, &spec);
            let ids: Vec<&str> = result.state_results[0]
                .assertions
                .iter()
                .map(|a| a.assertion_id.as_str())
                .collect();
            assert_eq!(ids, vec!["a1", "a2", "a3", "a4"]);
        }
    }

    #[test]
    fn evaluate_batch_returns_results_for_each_spec_in_order() {
        let s = snap(vec![elem(
            "ok", "#ok", Some("button"), Some("Save"), true,
        )]);
        let a1 = assertion("a", "info", json!({ "role": "button" }), true);
        let st1 = state("s1", "Loaded", Some(true), vec![a1]);
        let spec1 = page_spec("page-1", vec![st1]);

        let a2 = assertion("b", "info", json!({ "role": "link" }), true); // miss
        let st2 = state("s1", "Loaded", Some(true), vec![a2]);
        let spec2 = page_spec("page-2", vec![st2]);

        let specs = vec![spec1, spec2];
        let results = evaluate_batch(&s, &specs);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].page_id, "page-1");
        assert_eq!(results[0].summary.match_outcome, MatchOutcome::FullMatch);
        assert_eq!(results[1].page_id, "page-2");
        assert_eq!(results[1].summary.match_outcome, MatchOutcome::NoMatch);
    }

    #[test]
    fn evaluate_recommended_state_prefers_highest_match_rate() {
        let s = snap(vec![elem(
            "ok", "#ok", Some("button"), Some("Save"), true,
        )]);
        // s1 will fail; s2 will pass.
        let a1 = assertion("a1", "info", json!({ "role": "nope" }), true);
        let st1 = state("s1", "Idle", None, vec![a1]);

        let a2 = assertion("a2", "info", json!({ "role": "button" }), true);
        let st2 = state("s2", "Loaded", Some(true), vec![a2]);

        let spec = page_spec("page-1", vec![st1, st2]);
        let result = evaluate(&s, &spec);

        let rec = result.summary.recommended_state.expect("must recommend");
        assert_eq!(rec.state_id, "s2");
    }

    #[test]
    fn evaluate_full_match_with_no_states_is_no_match() {
        // Degenerate spec with zero states should not pass.
        let s = snap(vec![]);
        let spec = page_spec("page-1", vec![]);
        let result = evaluate(&s, &spec);
        assert_eq!(result.summary.match_outcome, MatchOutcome::NoMatch);
        assert_eq!(result.summary.overall_match_rate, 0.0);
        assert!(result.state_results.is_empty());
    }

    // ----- Severity routing -----

    #[test]
    fn severity_counts_unknown_severity_routes_to_error() {
        let s = snap(vec![]);
        let a = assertion("a1", "blocker", json!({ "role": "button" }), true);
        let st = state("s1", "Loaded", Some(true), vec![a]);
        let spec = page_spec("page-1", vec![st]);

        let result = evaluate(&s, &spec);
        // The assertion fails (no elements). "blocker" is unknown → error.
        assert_eq!(result.summary.severity_counts.error, 1);
        assert_eq!(result.summary.severity_counts.critical, 0);
        assert_eq!(result.summary.severity_counts.warning, 0);
    }

    #[test]
    fn severity_counts_warn_alias_for_warning() {
        let s = snap(vec![]);
        let a = assertion("a1", "WARN", json!({ "role": "button" }), true);
        let st = state("s1", "Loaded", Some(true), vec![a]);
        let spec = page_spec("page-1", vec![st]);

        let result = evaluate(&s, &spec);
        assert_eq!(result.summary.severity_counts.warning, 1);
    }

    #[test]
    fn severity_counts_passing_assertions_dont_increment() {
        let s = snap(vec![elem("ok", "#ok", Some("button"), None, true)]);
        let a = assertion("a1", "critical", json!({ "role": "button" }), true);
        let st = state("s1", "Loaded", Some(true), vec![a]);
        let spec = page_spec("page-1", vec![st]);

        let result = evaluate(&s, &spec);
        let counts = &result.summary.severity_counts;
        assert_eq!(counts.critical, 0);
        assert_eq!(counts.error, 0);
        assert_eq!(counts.warning, 0);
        assert_eq!(counts.info, 0);
    }

    // ----- Determinism -----

    #[test]
    fn evaluate_byte_identical_across_100_runs() {
        // Plan §Step 4 acceptance: byte-identical across 100 reps. Compare
        // serialized JSON to factor out evaluated_at (timestamps drift).
        let s = snap(vec![
            elem("a", "#a", Some("button"), Some("Save"), true),
            elem("b", "#b", Some("link"), Some("Cancel"), true),
        ]);
        let a1 = assertion("a1", "critical", json!({ "role": "button" }), true);
        let a2 = assertion("a2", "error", json!({ "role": "nope" }), true);
        let st = state("s1", "Loaded", Some(true), vec![a1, a2]);
        let spec = page_spec("page-1", vec![st]);

        // Canonicalize each result by zeroing out the timestamp fields, then
        // compare JCS-canonical bytes.
        let normalize = |r: &SpecCheckResult| -> Vec<u8> {
            let mut v: serde_json::Value = serde_json::to_value(r).unwrap();
            if let Some(obj) = v.as_object_mut() {
                obj.insert("evaluatedAt".to_string(), json!("FIXED"));
                if let Some(fp) = obj.get_mut("bridgeFingerprint").and_then(|x| x.as_object_mut()) {
                    fp.insert("snapshotTimestamp".to_string(), json!("FIXED"));
                }
            }
            serde_jcs::to_vec(&v).unwrap()
        };

        let baseline = normalize(&evaluate(&s, &spec));
        for _ in 0..100 {
            let r = evaluate(&s, &spec);
            assert_eq!(normalize(&r), baseline);
        }
    }

    // ----- Property: evaluate() is deterministic across 50 reps for any
    // adversarial input drawn from the small dictionaries in
    // `proptest_strategies`. -----

    /// Pin the timestamp fields (`evaluatedAt`,
    /// `bridgeFingerprint.snapshotTimestamp`) to a fixed value so equality
    /// reflects evaluator determinism rather than clock drift.
    fn canonicalize_pinning_timestamps(r: &SpecCheckResult) -> String {
        let mut v: serde_json::Value = serde_json::to_value(r).unwrap();
        if let Some(obj) = v.as_object_mut() {
            obj.insert("evaluatedAt".to_string(), json!("FIXED"));
            if let Some(fp) = obj.get_mut("bridgeFingerprint").and_then(|x| x.as_object_mut()) {
                fp.insert("snapshotTimestamp".to_string(), json!("FIXED"));
            }
        }
        serde_jcs::to_string(&v).unwrap()
    }

    use proptest::prelude::*;

    proptest! {
        // Default ProptestConfig.cases is 256 — that's the Plan 02 gate.
        #[test]
        fn prop_evaluate_is_deterministic(
            snapshot in crate::proptest_strategies::arbitrary_snapshot(),
            spec in crate::proptest_strategies::arbitrary_spec(),
        ) {
            let baseline = evaluate(&snapshot, &spec);
            let baseline_canonical = canonicalize_pinning_timestamps(&baseline);
            for _ in 0..49 {
                let result = evaluate(&snapshot, &spec);
                let result_canonical = canonicalize_pinning_timestamps(&result);
                prop_assert_eq!(&result_canonical, &baseline_canonical);
            }
        }
    }
}
