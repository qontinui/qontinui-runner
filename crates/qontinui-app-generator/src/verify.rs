//! Phase 3 — the **verify adapter**: real per-state observability, not asserted coverage.
//!
//! Phases 1-2 proved the IR -> instrumentation mapping is matcher-correct, but they ran the
//! matcher over ONE snapshot carrying every screen's elements flattened together. That
//! flattened set cannot see a *missing element on the right screen*: an element belonging to
//! `pairing-error` satisfies a `pairing-confirm` assertion just as well. Phase 3 closes that
//! by snapshotting **each state in isolation** ([`state_snapshot`]) and running the real
//! [`qontinui_spec_check::evaluate`] once per state ([`spec_check_per_state`]).
//!
//! The result is then the ONLY thing that decides coverage:
//!
//! - [`coverage_evidence_from_app`] marks `uiStates.<id>` covered **iff** the matcher scored
//!   that state `Green` under [`ThresholdConfig::default`]; a below-threshold state becomes a
//!   [`GapReason::BehaviorMismatch`] carrying the matcher's own miss diagnostic as `detail`.
//!   `navigation.<id>` is covered iff its trigger resolved AND its source screen is itself
//!   confirmed. `operations.<name>` is covered iff the generator actually emitted the
//!   matching [`crate::app::ServiceMethod`].
//! - [`verdict_with_spec_check`] feeds that evidence to the **unchanged**
//!   [`evaluate_completeness`] and then sets
//!   [`CompletenessVerdict::ui_states_spec_check`] on the returned verdict.
//!
//! `evaluate_completeness` hardcodes `ui_states_spec_check: None` **by design** (it takes
//! only `spec` / `evidence` / `evaluated_at` and is a frozen shipped seam). The adapter sets
//! the field afterwards rather than editing the rubric — one rubric, reused, not rebuilt.
//!
//! ## Snapshot source (recorded per Phase 1's done-criterion)
//!
//! Every snapshot in this module is the **deterministic projection** of the generator's own
//! instrumentation manifest through [`crate::snapshot_element_from`] — the encoding of the
//! UI Bridge native SDK's registry contract that Phases 1-2 already proved matcher-correct.
//! The **on-device** snapshot (render the emitted `.tsx` under a real bridge) remains the
//! honestly-deferred leg; see `tests/runtime_render_deferred.rs`.

use std::collections::{BTreeMap, BTreeSet};

use qontinui_types::completeness_eval::{evaluate_completeness, CoverageEvidence, GapEvidence};
use qontinui_types::completeness_verdict::{CompletenessVerdict, GapReason};
use qontinui_types::functional_spec::FunctionalSpec;
use qontinui_types::ir::IrPageSpec;
use qontinui_types::spec_check::{
    AssertionOutcome, AssertionSeverityCounts, ClassificationStatus, MatchOutcome, MissReason,
    SpecCheckResult, SpecCheckSummary, StateMatchResult, ThresholdConfig,
};
use qontinui_types::ui_bridge::UIBridgeSnapshot;

use qontinui_spec_check::confidence::recommend_state;
use qontinui_spec_check::evaluate;

use crate::app::GeneratedApp;
use crate::screen::ScreenArtifact;
use crate::snapshot_element_from;

/// `IrPageSpec.version` the generator emits — the only value the IR schema defines.
const IR_PAGE_VERSION: &str = "1.0";

/// Fallback page id when the spec's `target.source_url` carries no usable path segment.
const FALLBACK_PAGE_ID: &str = "generated-app";

// ===========================================================================
// Per-state snapshots — the Phase-3 delta over the flattened Phase-2 snapshot
// ===========================================================================

/// Build the [`UIBridgeSnapshot`] a device would report **while that one screen is
/// mounted**: only that screen's instrumented elements, routed at that screen.
///
/// This is the Phase-3 delta. `tests/golden_full_app.rs` flattens every screen's elements
/// into a single snapshot, which means a state's assertion can be satisfied by an element
/// that lives on a *different* screen. Per-state isolation makes a missing element on the
/// right screen fail, which is the whole point of an observability seam.
pub fn state_snapshot(screen: &ScreenArtifact) -> UIBridgeSnapshot {
    snapshot_for(
        &screen.state_id,
        screen.elements.iter().map(snapshot_element_from),
    )
}

/// The snapshot for a state the generator produced **no** screen for: routed at the state,
/// carrying nothing. Every assertion on that state misses — which is the honest reading of
/// "nothing was generated here".
fn empty_snapshot(state_id: &str) -> UIBridgeSnapshot {
    snapshot_for(state_id, std::iter::empty())
}

fn snapshot_for(
    state_id: &str,
    elements: impl Iterator<Item = qontinui_types::ui_bridge::UIBridgeElement>,
) -> UIBridgeSnapshot {
    UIBridgeSnapshot {
        timestamp: 0,
        elements: elements.collect(),
        components: Vec::new(),
        workflows: Vec::new(),
        modal_stack: None,
        toasts: None,
        undo_redo: None,
        current_route: Some(format!("/{state_id}")),
        segments: vec![state_id.to_string()],
    }
}

/// Project a [`FunctionalSpec`] into the [`IrPageSpec`] the matcher evaluates against.
///
/// `ui_states` / `navigation` are a *literal superset* of the IR (they ARE `IrState` /
/// `IrTransition` values), so this is a re-wrap, not a conversion — the matcher sees exactly
/// the states the generator inverted. The page `id`/`name` are derived from the spec's
/// observed source URL so the derivation is deterministic and carries no hardcoded fixture.
pub fn page_spec_from(spec: &FunctionalSpec) -> IrPageSpec {
    let id = page_id_from_source_url(&spec.target.source_url);
    IrPageSpec {
        version: IR_PAGE_VERSION.to_string(),
        id: id.clone(),
        name: id,
        description: None,
        metadata: None,
        provenance: None,
        states: spec.ui_states.clone(),
        transitions: spec.navigation.clone(),
        synthesized_groups: None,
        initial_state: spec
            .ui_states
            .iter()
            .find(|s| s.is_initial.unwrap_or(false))
            .map(|s| s.id.clone()),
        api_assertions: None,
    }
}

/// Last non-empty path segment of the observed source URL (`.../connect-runner` ->
/// `connect-runner`), falling back to [`FALLBACK_PAGE_ID`].
fn page_id_from_source_url(source_url: &str) -> String {
    source_url
        .split(['?', '#'])
        .next()
        .unwrap_or(source_url)
        .rsplit('/')
        .find(|seg| !seg.is_empty() && !seg.contains(':'))
        .unwrap_or(FALLBACK_PAGE_ID)
        .to_string()
}

// ===========================================================================
// The per-state matcher run + the fold
// ===========================================================================

/// Run the **real** matcher once per state, each against that state's own isolated snapshot,
/// and fold the runs into one aggregate [`SpecCheckResult`].
///
/// Each run evaluates the whole page against one state's snapshot (the matcher has no
/// single-state entry point), so only that run's entry **for its own state** is meaningful —
/// the other entries score against a snapshot that was never claiming to be them. The fold
/// therefore takes exactly one `StateMatchResult` per spec state, in spec order, and
/// recomputes `summary` / `classification` over the folded set with the same rules
/// `evaluate` uses (mean per-state rate; `FullMatch` iff every assertion passed; severity
/// counts summed over failures; `recommend_state` with the spec's own `is_initial` lookup).
pub fn spec_check_per_state(app: &GeneratedApp, spec: &FunctionalSpec) -> SpecCheckResult {
    let page = page_spec_from(spec);

    let mut template: Option<SpecCheckResult> = None;
    let mut state_results: Vec<StateMatchResult> = Vec::with_capacity(spec.ui_states.len());

    for state in &spec.ui_states {
        let snapshot = match app.screens.iter().find(|s| s.state_id == state.id) {
            Some(screen) => state_snapshot(screen),
            None => empty_snapshot(&state.id),
        };
        let run = evaluate(&snapshot, &page);
        if let Some(sr) = run.state_results.iter().find(|s| s.state_id == state.id) {
            state_results.push(sr.clone());
        }
        if template.is_none() {
            template = Some(run);
        }
    }

    // A spec with no `ui_states` still deserves a well-formed result: one run over an empty
    // snapshot yields the same scaffolding (schema version, spec hash, thresholds) with an
    // empty `state_results`.
    let mut aggregate = template.unwrap_or_else(|| evaluate(&empty_snapshot(""), &page));

    let summary = fold_summary(spec, &state_results);
    aggregate.classification = aggregate
        .thresholds_used
        .classify_match_rate(summary.overall_match_rate);
    aggregate.summary = summary;
    aggregate.state_results = state_results;
    // The aggregate spans N per-state snapshots, so the honest element count is their union.
    aggregate.bridge_fingerprint.element_count =
        app.screens.iter().map(|s| s.elements.len()).sum::<usize>() as u32;
    aggregate
}

/// Recompute the aggregate summary over the folded per-state results, mirroring
/// `qontinui_spec_check`'s own `build_summary` with `validation == None` (the arm
/// [`evaluate`] itself takes).
fn fold_summary(spec: &FunctionalSpec, state_results: &[StateMatchResult]) -> SpecCheckSummary {
    let overall_match_rate = if state_results.is_empty() {
        0.0
    } else {
        state_results.iter().map(|s| s.match_rate).sum::<f32>() / state_results.len() as f32
    };

    let mut total = 0usize;
    let mut passed = 0usize;
    let mut severity_counts = AssertionSeverityCounts::default();
    for s in state_results {
        for a in &s.assertions {
            total += 1;
            match &a.outcome {
                AssertionOutcome::Pass { .. } => passed += 1,
                AssertionOutcome::Fail { .. } => match a.severity.to_ascii_lowercase().as_str() {
                    "critical" => severity_counts.critical += 1,
                    "warning" | "warn" => severity_counts.warning += 1,
                    "info" | "informational" | "notice" => severity_counts.info += 1,
                    // Unknown severities route to `error`, matching the matcher's
                    // "least restrictive" bucketing.
                    _ => severity_counts.error += 1,
                },
            }
        }
    }

    let match_outcome = if state_results.is_empty() || total == 0 {
        MatchOutcome::NoMatch
    } else if passed == total {
        MatchOutcome::FullMatch
    } else if passed == 0 {
        MatchOutcome::NoMatch
    } else {
        MatchOutcome::PartialMatch
    };

    let recommended_state = recommend_state(state_results, |sid| {
        spec.ui_states
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
        recommendation_reason: None,
    }
}

// ===========================================================================
// Evidence — derived from the matcher run, never from the generator's claim
// ===========================================================================

/// Derive the [`CoverageEvidence`] for node #1's half of the union from what the matcher
/// **observed**, not from what the generator claims it produced.
///
/// Refs use the exact [`qontinui_types::completeness_eval::enumerate_nodes`] dotted
/// convention (`uiStates.<id>` / `navigation.<id>` / `operations.<name>`), cross-checked
/// against [`GeneratedApp::produced_refs`], which is built from the same convention.
///
/// Rules:
///
/// - **`uiStates.<id>`** — covered iff that state's [`StateMatchResult`] classifies
///   [`ClassificationStatus::Green`] under [`ThresholdConfig::default`] (0.5 / 0.8).
///   Below threshold -> [`GapReason::BehaviorMismatch`] whose `detail` is the matcher's own
///   miss diagnostic (the failing assertion ids + their [`MissReason`]s). No result at all
///   -> [`GapReason::Unverifiable`] (the verifier could not look, which is not the same as
///   the generator not producing).
/// - **`navigation.<id>`** — covered iff the [`crate::app::NavEdge`] resolved a
///   `trigger_element_id` **and** its `from_state` screen is itself covered. A dangling
///   trigger is [`GapReason::NotGenerated`]; a resolved trigger on a below-threshold source
///   screen is [`GapReason::BehaviorMismatch`] naming that source state — the button the
///   transition needs was not confirmed observable.
/// - **`operations.<name>`** — covered iff a matching [`crate::app::ServiceMethod`] exists;
///   otherwise [`GapReason::NotGenerated`].
/// - **`backend_refs`** — node #2's half of the union (coordinator §2: `entities.*`,
///   `auth.*`, the server operation aspects), added to `covered` verbatim. A ref covered by
///   #2 wins over a gap #1 recorded for it, which is what "union" means.
/// - **`filled_assumed`** stays empty: this node fills no `Assumed` node (`.effect` is #2's).
pub fn coverage_evidence_from_app(
    app: &GeneratedApp,
    result: &SpecCheckResult,
    backend_refs: &[String],
) -> CoverageEvidence {
    let thresholds = ThresholdConfig::default();
    let mut covered: BTreeSet<String> = BTreeSet::new();
    let mut gaps: BTreeMap<String, GapEvidence> = BTreeMap::new();

    // --- uiStates.<id> — the matcher, and only the matcher, decides. ---
    for screen in &app.screens {
        let node_ref = format!("uiStates.{}", screen.state_id);
        match result
            .state_results
            .iter()
            .find(|s| s.state_id == screen.state_id)
        {
            Some(sr)
                if thresholds.classify_match_rate(sr.match_rate) == ClassificationStatus::Green =>
            {
                covered.insert(node_ref);
            }
            Some(sr) => {
                gaps.insert(
                    node_ref,
                    GapEvidence {
                        reason: GapReason::BehaviorMismatch,
                        detail: Some(miss_detail(sr, &thresholds)),
                    },
                );
            }
            None => {
                gaps.insert(
                    node_ref,
                    GapEvidence {
                        reason: GapReason::Unverifiable,
                        detail: Some(format!(
                            "spec_check returned no state result for `{}`",
                            screen.state_id
                        )),
                    },
                );
            }
        }
    }

    // --- navigation.<id> — a transition is only real if its button was observed. ---
    for edge in &app.nav_edges {
        let node_ref = format!("navigation.{}", edge.transition_id);
        let source_ref = format!("uiStates.{}", edge.from_state);
        match &edge.trigger_element_id {
            None => {
                gaps.insert(
                    node_ref,
                    GapEvidence {
                        reason: GapReason::NotGenerated,
                        detail: Some(format!(
                            "transition `{}` resolved no trigger element on source screen `{}`",
                            edge.transition_id, edge.from_state
                        )),
                    },
                );
            }
            Some(_) if covered.contains(&source_ref) => {
                covered.insert(node_ref);
            }
            Some(trigger) => {
                let source_detail = gaps
                    .get(&source_ref)
                    .and_then(|g| g.detail.clone())
                    .unwrap_or_else(|| {
                        format!("source screen `{}` was not confirmed", edge.from_state)
                    });
                gaps.insert(
                    node_ref,
                    GapEvidence {
                        reason: GapReason::BehaviorMismatch,
                        detail: Some(format!(
                            "trigger element `{trigger}` is on source screen `{}`, which the matcher did not confirm: {source_detail}",
                            edge.from_state
                        )),
                    },
                );
            }
        }
    }

    // --- operations.<name> — gated on the data-layer method actually being emitted. ---
    for node_ref in &app.produced_refs {
        let Some(op_name) = node_ref.strip_prefix("operations.") else {
            continue;
        };
        if app.services.iter().any(|m| m.operation == op_name) {
            covered.insert(node_ref.clone());
        } else {
            gaps.insert(
                node_ref.clone(),
                GapEvidence {
                    reason: GapReason::NotGenerated,
                    detail: Some(format!(
                        "no data-layer ServiceMethod emitted for operation `{op_name}`"
                    )),
                },
            );
        }
    }

    // --- #2's half of the union, verbatim. Covered wins over a gap #1 recorded. ---
    covered.extend(backend_refs.iter().cloned());
    gaps.retain(|k, _| !covered.contains(k));

    CoverageEvidence {
        covered,
        gaps,
        filled_assumed: BTreeSet::new(),
    }
}

/// Render a below-threshold state's miss compactly: which assertions failed and why, plus
/// the rate and the threshold it fell under. This string becomes the `CoverageGap.detail`
/// a reconciler reads, so it must name the *observation*, never the assertion's intent.
fn miss_detail(sr: &StateMatchResult, thresholds: &ThresholdConfig) -> String {
    let misses: Vec<String> = sr
        .assertions
        .iter()
        .filter_map(|a| match &a.outcome {
            AssertionOutcome::Fail { miss } => Some(format!(
                "{} ({})",
                a.assertion_id,
                miss_reason_str(miss.reason)
            )),
            AssertionOutcome::Pass { .. } => None,
        })
        .collect();
    let failing = if misses.is_empty() {
        "none (the state has no enabled assertions to match)".to_string()
    } else {
        misses.join(", ")
    };
    format!(
        "spec_check: state `{}` match_rate {:.2} classified {} (green requires >= {:.2}); failing assertions: {}",
        sr.state_id,
        sr.match_rate,
        thresholds.classify_match_rate(sr.match_rate),
        thresholds.yellow_threshold,
        failing
    )
}

/// Stable snake_case rendering of a [`MissReason`], matching its wire form.
fn miss_reason_str(reason: MissReason) -> &'static str {
    match reason {
        MissReason::NoCandidates => "no_candidates",
        MissReason::RoleMismatch => "role_mismatch",
        MissReason::TextMismatch => "text_mismatch",
        MissReason::VisibilityMismatch => "visibility_mismatch",
        MissReason::AttributeMismatch => "attribute_mismatch",
        MissReason::MultipleMatches => "multiple_matches",
    }
}

// ===========================================================================
// The verdict
// ===========================================================================

/// Score the spec with the **unchanged** [`evaluate_completeness`] rubric, then attach the
/// per-state matcher run as [`CompletenessVerdict::ui_states_spec_check`].
///
/// `evaluate_completeness` takes only `(spec, evidence, evaluated_at)` and hardcodes
/// `ui_states_spec_check: None` by design — it is a frozen shipped seam. Setting the field
/// here keeps the one rubric reused rather than rebuilt.
pub fn verdict_with_spec_check(
    spec: &FunctionalSpec,
    evidence: &CoverageEvidence,
    result: &SpecCheckResult,
    evaluated_at: &str,
) -> CompletenessVerdict {
    let mut verdict = evaluate_completeness(spec, evidence, evaluated_at);
    verdict.ui_states_spec_check = Some(result.clone());
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::InstrumentedElement;

    fn el(id: &str, role: Option<&str>, text: Option<&str>) -> InstrumentedElement {
        InstrumentedElement {
            element_id: id.into(),
            element_type: "button".into(),
            accessibility_role: role.map(str::to_string),
            label: text.map(str::to_string),
            visible_text: text.map(str::to_string),
            needs_press: role == Some("button"),
        }
    }

    #[test]
    fn state_snapshot_carries_only_that_screens_elements_and_routes_to_it() {
        let screen = ScreenArtifact {
            state_id: "pairing-confirm".into(),
            file_path: "app/pairing-confirm.tsx".into(),
            tsx: String::new(),
            elements: vec![el("a", Some("button"), Some("Connect"))],
        };
        let snap = state_snapshot(&screen);
        assert_eq!(snap.elements.len(), 1);
        assert_eq!(snap.current_route.as_deref(), Some("/pairing-confirm"));
        assert_eq!(snap.segments, vec!["pairing-confirm".to_string()]);
    }

    #[test]
    fn page_id_is_derived_from_the_observed_source_url() {
        assert_eq!(
            page_id_from_source_url("https://app.qontinui.io/connect-runner"),
            "connect-runner"
        );
        assert_eq!(
            page_id_from_source_url("https://app.qontinui.io/connect-runner?x=1"),
            "connect-runner"
        );
        // No usable segment (scheme-only) -> the declared fallback, never a panic.
        assert_eq!(page_id_from_source_url("https://"), FALLBACK_PAGE_ID);
    }

    #[test]
    fn miss_reason_rendering_is_the_wire_spelling() {
        assert_eq!(miss_reason_str(MissReason::RoleMismatch), "role_mismatch");
        assert_eq!(miss_reason_str(MissReason::NoCandidates), "no_candidates");
    }
}
