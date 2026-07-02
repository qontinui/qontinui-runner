//! Stream E (Flywheel) — the three-gate validator.
//!
//! Decides whether a proposed `IrPageSpec` candidate is allowed to land in
//! `_pending/`. Composes four sequential gates per
//! `qontinui-dev-notes/spec-check-v1/05-flywheel.md` § Step 8:
//!
//!   1. **Distinctness** — `spec_api::distinctness::validate(&candidate)`.
//!      Same gate as `POST /spec/author` so the flywheel path can't bypass
//!      Plan 04's empty-criteria / identical-states / subset-domination
//!      checks.
//!   2. **Round-trip** — `serde_json::to_vec` → `from_slice` → equality.
//!      Catches IR documents whose Rust-side struct shape doesn't survive
//!      JSON serialization (e.g. NaN floats, non-string map keys).
//!   3. **Coverage floor** — `coverage_of(ir, generate_regression_suite(ir))`
//!      ratio of `states_covered / max(reachable_states.len(), 1)` must be
//!      ≥ the env-configurable `QONTINUI_SPEC_COVERAGE_FLOOR` (default 0.80).
//!   4. **B-execution green** — fetch a live UI Bridge snapshot, run
//!      `spec_check::evaluate(snapshot, candidate)` + `apply_policy(..,
//!      factories::strict_all_pass())`. Anything other than
//!      `PolicyStatus::Pass` (Fail AND Indeterminate) rejects the candidate.
//!
//! Distinctness sits in front because (a) it's the cheapest, (b) the other
//! three gates run against the same IR and so still see the violation if we
//! skipped it. Round-trip is gate 2 because a candidate that doesn't survive
//! serialization can't be staged to `_pending/` anyway. Coverage and B-green
//! follow per the plan's specified order.
//!
//! Tests against a canned `UIBridgeSnapshot` go through
//! `gate_b_execution_green_with_snapshot` — the production path's
//! `gate_b_execution_green` wraps it with the WS dispatch helper.

use std::sync::Arc;

use qontinui_spec_check as spec_check;
use qontinui_types::spec_check::{
    PolicyEvaluation, PolicyStatus, SpecCheckResult, SpecCheckSummary,
};
use qontinui_types::ui_bridge::UIBridgeSnapshot;
use serde_json::json;
use tracing::{debug, warn};

use crate::spec_api::distinctness::{self, DistinctnessReport};
use crate::spec_api::types::IrPageSpec;
use spec_check::SnapshotFetchError;

// =============================================================================
// Public surface
// =============================================================================

/// Validator failure taxonomy. Every variant maps to a structured response
/// reason in `proposals.rs::post_execute` and a row in
/// `spec_proposals.last_error`.
#[derive(Debug)]
pub enum ValidatorError {
    /// Gate 1 — distinctness violation (empty criteria, identical states,
    /// or subset domination). Same shape as `POST /spec/author`'s 422.
    DistinctnessFailed(DistinctnessReport),
    /// Gate 2 — serde-side error during the round-trip serialize step.
    SerializeFailed(serde_json::Error),
    /// Gate 2 — serde-side error during the round-trip deserialize step.
    DeserializeFailed(serde_json::Error),
    /// Gate 2 — the round-tripped IR was not byte-identical to the input.
    RoundTripMismatch,
    /// Gate 3 — coverage ratio below the configured floor.
    CoverageBelowFloor { ratio: f64, floor: f64 },
    /// Gate 4 — snapshot fetch failed before B could evaluate.
    SnapshotFetchFailed(SnapshotFetchError),
    /// Gate 4 — `apply_policy` returned `Fail` or `Indeterminate`.
    BExecutionRed {
        result_summary: SpecCheckSummary,
        policy_eval: PolicyEvaluation,
        /// The snapshot id of the result that produced the red. Populated
        /// so downstream emit sites (Plan 06 `FlywheelProposalDemoted`) can
        /// join against tracing spans / `workflow_verification_phase_results`.
        snapshot_id: String,
        /// First failing assertion's `(state_id, assertion_id)`. None when
        /// the red came from policy `Indeterminate` over a fully-passing
        /// result set (e.g. empty in-scope assertions); the demote emit
        /// substitutes an empty string in that case.
        failing_assertion: Option<(String, String)>,
    },
}

/// Short, stable reason code for each variant. Used as the `reason` field
/// of the `/spec/proposals/{id}/execute` validator-rejected response shape.
pub fn validator_error_reason(e: &ValidatorError) -> &'static str {
    match e {
        ValidatorError::DistinctnessFailed(_) => "distinctness-failed",
        ValidatorError::SerializeFailed(_) => "serialize-failed",
        ValidatorError::DeserializeFailed(_) => "deserialize-failed",
        ValidatorError::RoundTripMismatch => "round-trip-mismatch",
        ValidatorError::CoverageBelowFloor { .. } => "coverage-below-floor",
        ValidatorError::SnapshotFetchFailed(_) => "snapshot-fetch-failed",
        ValidatorError::BExecutionRed { .. } => "b-execution-red",
    }
}

/// Human-readable detail for the `detail` field of the response + the
/// `spec_proposals.last_error` column.
pub fn validator_error_detail(e: &ValidatorError) -> String {
    match e {
        ValidatorError::DistinctnessFailed(r) => {
            format!("distinctness: {} violation(s)", r.violations.len())
        }
        ValidatorError::SerializeFailed(err) => format!("serialize: {err}"),
        ValidatorError::DeserializeFailed(err) => format!("deserialize: {err}"),
        ValidatorError::RoundTripMismatch => {
            "round-trip mismatch: IR did not survive JSON serialization unchanged".to_string()
        }
        ValidatorError::CoverageBelowFloor { ratio, floor } => {
            format!("coverage {:.4} below floor {:.4}", ratio, floor)
        }
        ValidatorError::SnapshotFetchFailed(err) => format!("snapshot fetch: {err}"),
        ValidatorError::BExecutionRed {
            result_summary,
            policy_eval,
            ..
        } => {
            let sc = &result_summary.severity_counts;
            let misses = sc.critical + sc.error + sc.warning + sc.info;
            format!(
                "B-execution red: overall_status={:?}, miss_count={}",
                policy_eval.overall_status, misses
            )
        }
    }
}

/// Run the four gates in order: distinctness → round-trip → coverage →
/// b-green. Returns the `SpecCheckResult` from gate 4 on success.
///
/// `app_state` is required by gate 4 (live snapshot fetch). `pathname` is
/// the page the candidate targets (used to navigate / locate the snapshot
/// for that route). `_distinct_root` is reserved for a future
/// `_pending/`-vs-disk-distinctness pass; gate 1 currently checks only
/// internal distinctness of the candidate.
pub async fn validate_candidate(
    app_state: Arc<crate::commands::AppState>,
    candidate: &IrPageSpec,
    pathname: &str,
    _distinct_root: &std::path::Path,
) -> Result<SpecCheckResult, ValidatorError> {
    // Gate 1 — distinctness.
    if let Err(report) = distinctness::validate(candidate) {
        debug!(
            "validate_candidate: gate-1 distinctness failed ({} violations)",
            report.violations.len()
        );
        return Err(ValidatorError::DistinctnessFailed(report));
    }

    // Gate 2 — round-trip.
    gate_round_trip(candidate)?;

    // Gate 3 — coverage floor.
    gate_coverage(candidate)?;

    // Gate 4 — B-green.
    gate_b_execution_green(app_state, candidate, pathname).await
}

/// Standalone entry point for the gate-4 B-green check. Exposed so Step 9's
/// sweep handler (re-running gate 4 nightly on `_pending/` candidates) can
/// call it directly without re-running gates 1-3 (those are pure properties
/// of the candidate IR and don't change between sweeps).
pub async fn gate_b_execution_green(
    app_state: Arc<crate::commands::AppState>,
    candidate: &IrPageSpec,
    pathname: &str,
) -> Result<SpecCheckResult, ValidatorError> {
    let snapshot = fetch_live_snapshot(app_state, pathname)
        .await
        .map_err(ValidatorError::SnapshotFetchFailed)?;
    gate_b_execution_green_with_snapshot(&snapshot, candidate)
}

// =============================================================================
// Private gate impls
// =============================================================================

/// Gate 2: serde round-trip. Returns the candidate-side bytes only on
/// success; the bytes are not propagated (callers re-serialize at the
/// staging boundary).
fn gate_round_trip(ir: &IrPageSpec) -> Result<(), ValidatorError> {
    let bytes = serde_json::to_vec(ir).map_err(ValidatorError::SerializeFailed)?;
    let reparsed: IrPageSpec =
        serde_json::from_slice(&bytes).map_err(ValidatorError::DeserializeFailed)?;
    if &reparsed != ir {
        return Err(ValidatorError::RoundTripMismatch);
    }
    Ok(())
}

/// Gate 3: coverage floor. Floor is configurable via
/// `QONTINUI_SPEC_COVERAGE_FLOOR` (default `0.80`). Denominator floored at
/// 1 so a degenerate IR (no reachable states) doesn't divide by zero.
pub(crate) fn gate_coverage(ir: &IrPageSpec) -> Result<f64, ValidatorError> {
    use crate::workflow_generation::coverage::{coverage_of, generate_regression_suite};
    let suite = generate_regression_suite(ir);
    let report = coverage_of(ir, &suite);
    let floor: f64 = std::env::var("QONTINUI_SPEC_COVERAGE_FLOOR")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.80_f64);
    let reachable_count = report.reachable_states.len().max(1);
    let ratio = report.states_covered as f64 / reachable_count as f64;
    if ratio < floor {
        debug!(
            "validate_candidate: gate-3 coverage {} below floor {}",
            ratio, floor
        );
        return Err(ValidatorError::CoverageBelowFloor { ratio, floor });
    }
    Ok(ratio)
}

/// Gate 4 inner — given an already-fetched snapshot, run B's evaluator +
/// the strict-all-pass policy. Split out so tests can construct canned
/// snapshots without going through `try_ws_dispatch`.
pub(crate) fn gate_b_execution_green_with_snapshot(
    snapshot: &UIBridgeSnapshot,
    candidate: &IrPageSpec,
) -> Result<SpecCheckResult, ValidatorError> {
    let result = spec_check::evaluate(snapshot, candidate);
    let policy = spec_check::policy::factories::strict_all_pass();
    let eval = spec_check::apply_policy(&result, &policy);

    // Treat anything other than `Pass` as red — `Fail` and `Indeterminate`
    // both reject. Per the plan's "could not evaluate is not a green signal"
    // rule (see § Step 8 + the vet-pass correction at the head of the doc).
    if !matches!(eval.overall_status, PolicyStatus::Pass) {
        let failing_assertion = first_failing_assertion(&result);
        return Err(ValidatorError::BExecutionRed {
            result_summary: result.summary.clone(),
            policy_eval: eval,
            snapshot_id: result.snapshot_id.clone(),
            failing_assertion,
        });
    }
    Ok(result)
}

/// Locate the first assertion with a `Fail` outcome in result-iteration
/// order. Returns `Some((state_id, assertion_id))` or `None` if every
/// assertion passed (the red came from policy `Indeterminate` rather than
/// a structural failure).
fn first_failing_assertion(result: &SpecCheckResult) -> Option<(String, String)> {
    use qontinui_types::spec_check::AssertionOutcome;
    for state in &result.state_results {
        for assertion in &state.assertions {
            if matches!(assertion.outcome, AssertionOutcome::Fail { .. }) {
                return Some((state.state_id.clone(), assertion.assertion_id.clone()));
            }
        }
    }
    None
}

/// Pull a fresh UI Bridge snapshot for the active app via the WS dispatch
/// helper, then wrap it into a typed `UIBridgeSnapshot` via
/// `spec_check::wrap_snapshot`.
///
/// `pathname` is forwarded to the SDK as the `route` field on
/// `BridgeFingerprint` so downstream `SpecCheckResult.fingerprint` carries
/// it; we do NOT navigate the connected app here. The flywheel cron either
/// (a) trusts the user has the app on the right page, or (b) extends this
/// helper later to pre-dispatch a `/ui-bridge/sdk/navigate` call. v1.0 ships
/// (a) per the plan's "optional" framing in § Step 8 step 2.
///
/// Phase 3: Adds a 3-second grace period retry loop for headless browser
/// launches. If no SDK connection is active, polls up to 30 times (100ms
/// backoff) before failing with `NotConnected`. This allows the spawn-headless
/// flow to complete registration while validator gate 4 is running.
async fn fetch_live_snapshot(
    app_state: Arc<crate::commands::AppState>,
    pathname: &str,
) -> Result<UIBridgeSnapshot, SnapshotFetchError> {
    use axum::http::Method;

    // 1. Resolve the active app id from the SDK connection.
    // Phase 3: If no connection initially, wait up to 3s in case headless is launching.
    let active_app_id = {
        let guard = app_state.sdk_connection.lock().await;
        guard.active_connection().map(|c| c.app_info.app_id.clone())
    };

    let app_id = match active_app_id {
        Some(id) => {
            debug!("fetch_live_snapshot: found active app_id={}", id);
            id
        }
        None => {
            // Phase 3: Headless launch grace period — wait for SDK to register
            debug!("fetch_live_snapshot: no active connection, waiting for headless launch grace period");
            let mut app_id_found: Option<String> = None;
            for retry in 0..30 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let guard = app_state.sdk_connection.lock().await;
                if let Some(conn) = guard.active_connection() {
                    app_id_found = Some(conn.app_info.app_id.clone());
                    debug!(
                        "fetch_live_snapshot: SDK connected on retry {} for app_id={}",
                        retry,
                        app_id_found.as_ref().unwrap()
                    );
                    break;
                }
            }
            match app_id_found {
                Some(id) => id,
                None => {
                    debug!("fetch_live_snapshot: timeout after headless grace period (no active connection)");
                    return Err(SnapshotFetchError::NotConnected);
                }
            }
        }
    };

    // 2. Look up the registry entry (also confirms the app is registered).
    //    `AppState::app_registry` / `app_dispatcher` are wrapped in
    //    `OnceCell`, populated during runner startup. A `None` here means
    //    the MCP plumbing hasn't initialized yet — treat as
    //    `NotConnected` for the flywheel's purposes.
    let registry = match app_state.app_registry.get() {
        Some(r) => r.clone(),
        None => return Err(SnapshotFetchError::NotConnected),
    };
    let dispatcher = match app_state.app_dispatcher.get() {
        Some(d) => d.clone(),
        None => return Err(SnapshotFetchError::NotConnected),
    };
    let entry = match registry.get(&app_id).await {
        Some(e) => e,
        None => return Err(SnapshotFetchError::NotConnected),
    };
    let app_version: Option<String> = entry.app.version.clone();

    // 3. Dispatch `getControlSnapshot`. For WS-transport apps the dispatcher
    //    sends the action over the active socket; for HTTP-transport apps
    //    it issues a GET to `/control/snapshot`. The flywheel cron is
    //    indifferent — either way we get back a raw JSON snapshot value.
    let payload = json!({});
    let raw = match dispatcher
        .dispatch(
            &app_id,
            "getControlSnapshot",
            Method::GET,
            "/control/snapshot",
            payload,
        )
        .await
    {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "fetch_live_snapshot: dispatch getControlSnapshot failed for app_id={}: {:?}",
                app_id, e
            );
            return Err(SnapshotFetchError::Network(format!("{e:?}")));
        }
    };

    // 4. Unwrap the typical `{ success, data: { ... } }` envelope so the
    //    inner snapshot shape is what `wrap_snapshot` deserializes.
    let inner: serde_json::Value = match raw.get("data") {
        Some(d) if !d.is_null() => d.clone(),
        _ => raw,
    };

    // 5. Wrap into a typed `UIBridgeSnapshot` (+ fingerprint, snapshot_id,
    //    content_sha256 — the latter three are discarded here; the
    //    flywheel cron records them at a higher layer when it persists
    //    sweep results).
    let (snapshot, _fp, _id, _hash) = spec_check::wrap_snapshot(
        inner,
        app_id,
        app_version,
        Some(pathname.to_string()),
        None, // bridge_version is not carried on the registry entry today
    )?;
    Ok(snapshot)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_api::types::{
        IrAssertion, IrAssertionTarget, IrPageSpec, IrState, IrTransition,
    };
    use qontinui_types::ui_bridge::UIBridgeSnapshot;

    // -------- Helpers ----------------------------------------------------

    /// IR with `n_states` states and a chain `s0→s1→...→s{n-1}` (when
    /// `transitions_link_all` is true) or no transitions otherwise. Each
    /// state has a single `assertion` whose criteria match the body
    /// element by `tagName == "body"` — small enough to evaluate cheaply
    /// against the test snapshots. Note: IRs with 2+ states will FAIL
    /// distinctness (identical state keys) — tests that need to pass
    /// distinctness use `n_states == 1`.
    fn chain_ir(id: &str, n_states: u32, transitions_link_all: bool) -> IrPageSpec {
        let mut states: Vec<IrState> = Vec::new();
        for i in 0..n_states {
            let sid = format!("s{i}");
            states.push(IrState {
                id: sid.clone(),
                name: sid,
                description: None,
                // Every state asserts that an element with `tag_name ==
                // "body"` exists. The body element in `body_snapshot()`
                // matches; the empty snapshot misses. Distinctness flags
                // identical state keys when 2+ states share criteria; the
                // smoke test below works around that by using a 1-state
                // IR (chain_ir(_, 1, false)). All other tests bypass
                // gate 1 by calling later gates directly.
                assertions: vec![IrAssertion {
                    id: format!("a-s{i}"),
                    description: format!("a-s{i}"),
                    category: "structural".into(),
                    severity: "critical".into(),
                    assertion_type: "element-exists".into(),
                    target: IrAssertionTarget {
                        kind: "search".into(),
                        criteria: json!({ "tagName": "body" }),
                        label: format!("body-s{i}"),
                    },
                    source: "build-plugin".into(),
                    reviewed: false,
                    enabled: true,
                    precondition: None,
                }],
                excluded_elements: None,
                conditions: None,
                is_initial: if i == 0 { Some(true) } else { None },
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
                api_assertions: None,
            });
        }

        let mut transitions: Vec<IrTransition> = Vec::new();
        if transitions_link_all && n_states >= 2 {
            for i in 0..(n_states - 1) {
                let from = format!("s{i}");
                let to = format!("s{}", i + 1);
                transitions.push(IrTransition {
                    id: format!("t{i}"),
                    name: format!("t{i}"),
                    description: None,
                    from_states: vec![from],
                    activate_states: vec![to],
                    exit_states: None,
                    actions: Vec::new(),
                    path_cost: None,
                    bidirectional: None,
                    effect: None,
                    metadata: None,
                    provenance: None,
                    cross_refs: None,
                });
            }
        }

        IrPageSpec {
            version: "1.0".into(),
            id: id.into(),
            name: id.into(),
            description: None,
            metadata: None,
            provenance: None,
            states,
            transitions,
            synthesized_groups: None,
            initial_state: Some("s0".into()),
        }
    }

    /// Build a minimal `UIBridgeElement` with `tag_name == "body"`. Used to
    /// produce a snapshot that matches the chain IRs' search criteria.
    fn body_element() -> qontinui_types::ui_bridge::UIBridgeElement {
        use qontinui_types::ui_bridge::{
            ElementIdentifier, ElementRect, ElementState, UIBridgeElement,
        };
        UIBridgeElement {
            id: "body-1".into(),
            element_type: "body".into(),
            label: None,
            actions: Vec::new(),
            custom_actions: None,
            identifier: ElementIdentifier {
                ui_id: None,
                test_id: None,
                awas_id: None,
                html_id: None,
                xpath: String::new(),
                selector: "body".into(),
            },
            state: ElementState {
                visible: true,
                enabled: true,
                focused: false,
                rect: ElementRect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                    top: 0.0,
                    right: 100.0,
                    bottom: 100.0,
                    left: 0.0,
                },
                value: None,
                checked: None,
                selected_options: None,
                text_content: None,
            },
            registered_at: 0,
            mounted: true,
            bbox: None,
            visible: Some(true),
            role: Some("document".into()),
            tag_name: Some("body".into()),
            aria_label: None,
            accessible_name: None,
            text: None,
        }
    }

    /// Tiny snapshot with one `<body>` element that matches the chain IRs'
    /// `tagName == "body"` criteria.
    fn body_snapshot() -> UIBridgeSnapshot {
        UIBridgeSnapshot {
            timestamp: 1_700_000_000_000,
            elements: vec![body_element()],
            components: Vec::new(),
            workflows: Vec::new(),
            modal_stack: None,
            toasts: None,
            undo_redo: None,
            current_route: None,
            segments: Vec::new(),
        }
    }

    /// Snapshot with NO elements — every search assertion will miss, so
    /// `apply_policy` will return Fail.
    fn empty_snapshot() -> UIBridgeSnapshot {
        UIBridgeSnapshot {
            timestamp: 1_700_000_000_000,
            elements: Vec::new(),
            components: Vec::new(),
            workflows: Vec::new(),
            modal_stack: None,
            toasts: None,
            undo_redo: None,
            current_route: None,
            segments: Vec::new(),
        }
    }

    // -------- Gate 2: round-trip ----------------------------------------

    #[test]
    fn gate_round_trip_passes_on_well_formed_ir() {
        let ir = chain_ir("rt-ok", 2, true);
        gate_round_trip(&ir).expect("round-trip passes");
    }

    // -------- Gate 3: coverage ------------------------------------------

    /// Serializes env-var tests so they don't race when cargo runs tests
    /// in parallel. Every gate_coverage_* test mutates or reads
    /// `QONTINUI_SPEC_COVERAGE_FLOOR`; they all take this lock.
    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn gate_coverage_passes_at_full_coverage() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        std::env::remove_var("QONTINUI_SPEC_COVERAGE_FLOOR");
        // 3-state chain s0→s1→s2 — every state touched, every state
        // reachable from s0. Ratio = 1.0 ≥ default 0.80 floor.
        let ir = chain_ir("cov-ok", 3, true);
        let ratio = gate_coverage(&ir).expect("coverage passes");
        assert!(ratio >= 0.99, "expected near-1.0 ratio, got {}", ratio);
    }

    #[test]
    fn gate_coverage_rejects_when_below_floor() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        // 5 states, chain transitions linking ALL states — coverage = 1.0.
        // Set floor to >1.0 to guarantee failure regardless of inputs.
        let ir = chain_ir("cov-fail", 5, true);
        std::env::set_var("QONTINUI_SPEC_COVERAGE_FLOOR", "1.5");
        let result = gate_coverage(&ir);
        std::env::remove_var("QONTINUI_SPEC_COVERAGE_FLOOR");
        match result {
            Err(ValidatorError::CoverageBelowFloor { ratio, floor }) => {
                assert!(ratio <= 1.0001);
                assert!((floor - 1.5).abs() < 1e-9);
            }
            other => panic!("expected CoverageBelowFloor, got {:?}", other),
        }
    }

    #[test]
    fn gate_coverage_disconnected_subgraph_below_default_floor() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        // Ensure the env var is not set from a prior test that may not have
        // cleaned up if it panicked mid-run.
        std::env::remove_var("QONTINUI_SPEC_COVERAGE_FLOOR");
        // 5 states; only 2 are reachable (s0→s1). states_covered touches
        // only the 2 wired states. Ratio against reachable = 2/2 = 1.0 —
        // the BFS denominator handles disconnected states by reporting
        // them as unreachable, so the ratio stays high.
        //
        // To exercise a true coverage shortfall, we'd need transitions
        // that touch only a subset of reachable states — that's a wider
        // shape that the coverage_of unit tests already cover. Here we
        // just sanity-check the gate doesn't false-pass at ratio=0.
        let ir = chain_ir("cov-disc", 5, false); // NO transitions at all
                                                 // No transitions, no `from_states` / `activate_states` touched.
                                                 // states_covered = 0. reachable_states = {s0} (initial only).
                                                 // ratio = 0/1 = 0.0 < 0.80 (default floor) → fail.
        let result = gate_coverage(&ir);
        match result {
            Err(ValidatorError::CoverageBelowFloor { ratio, .. }) => {
                assert!(ratio < 0.01, "expected ~0 ratio, got {}", ratio);
            }
            other => panic!("expected CoverageBelowFloor, got {:?}", other),
        }
    }

    // -------- Gate 4: B-green --------------------------------------------

    #[test]
    fn gate_b_execution_green_passes_on_matching_snapshot() {
        let ir = chain_ir("b-ok", 2, true);
        let snapshot = body_snapshot();
        let result = gate_b_execution_green_with_snapshot(&snapshot, &ir)
            .expect("b-green passes on matching snapshot");
        // Sanity: the spec-check evaluator should not have flagged any
        // critical/error/warning/info misses against our trivially-matching
        // `<body>` snapshot.
        let sc = &result.summary.severity_counts;
        assert_eq!(sc.critical + sc.error + sc.warning + sc.info, 0);
    }

    #[test]
    fn gate_b_execution_red_on_failing_assertions() {
        let ir = chain_ir("b-fail", 2, true);
        let snapshot = empty_snapshot();
        let result = gate_b_execution_green_with_snapshot(&snapshot, &ir);
        match result {
            Err(ValidatorError::BExecutionRed {
                result_summary,
                policy_eval,
                ..
            }) => {
                // strict_all_pass treats every miss as a Fail.
                assert!(
                    !matches!(policy_eval.overall_status, PolicyStatus::Pass),
                    "expected non-Pass status, got {:?}",
                    policy_eval.overall_status
                );
                let sc = &result_summary.severity_counts;
                assert!(
                    sc.critical + sc.error + sc.warning + sc.info >= 1,
                    "expected at least one miss tallied"
                );
            }
            other => panic!("expected BExecutionRed, got {:?}", other),
        }
    }

    #[test]
    fn gate_b_execution_red_on_indeterminate_treats_as_red() {
        // An IR with NO assertions across all states would yield an
        // Indeterminate evaluation under `strict_all_pass` (empty conjunct
        // scope). Build a minimal IR with one bare state, no assertions.
        let ir = IrPageSpec {
            version: "1.0".into(),
            id: "b-indet".into(),
            name: "b-indet".into(),
            description: None,
            metadata: None,
            provenance: None,
            states: vec![IrState {
                id: "s0".into(),
                name: "s0".into(),
                description: None,
                assertions: Vec::new(),
                excluded_elements: None,
                conditions: None,
                is_initial: Some(true),
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
                api_assertions: None,
            }],
            transitions: Vec::new(),
            synthesized_groups: None,
            initial_state: Some("s0".into()),
        };
        let snapshot = empty_snapshot();
        // strict_all_pass with an empty conjunct scope (no assertions)
        // returns Indeterminate (see `policy.rs:176-181`). The flywheel
        // treats anything other than `Pass` as red — Indeterminate must
        // surface as `BExecutionRed`.
        let outcome = gate_b_execution_green_with_snapshot(&snapshot, &ir);
        match outcome {
            Err(ValidatorError::BExecutionRed { policy_eval, .. }) => {
                assert!(
                    matches!(
                        policy_eval.overall_status,
                        PolicyStatus::Fail | PolicyStatus::Indeterminate
                    ),
                    "expected non-Pass status, got {:?}",
                    policy_eval.overall_status
                );
            }
            Ok(_) => {
                panic!("expected Indeterminate→BExecutionRed for empty-assertion IR");
            }
            Err(other) => panic!("unexpected variant: {:?}", other),
        }
    }

    // -------- All-gates-pass smoke --------------------------------------

    #[test]
    fn all_gates_pass_smoke() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        std::env::remove_var("QONTINUI_SPEC_COVERAGE_FLOOR");
        // Single-state IR with one assertion + no transitions. Distinctness
        // passes (no peers to be identical-with); gate-2 round-trips
        // cleanly; gate-3 coverage = states_covered=0 / reachable=1 = 0
        // which is BELOW the default 0.80 floor — so we set a 0.0 floor
        // for this smoke test (every gate runs but coverage is gated
        // permissively). gate-4 matches the body snapshot.
        let ir = chain_ir("all-ok", 1, false);
        std::env::set_var("QONTINUI_SPEC_COVERAGE_FLOOR", "0.0");
        let r1 = distinctness::validate(&ir);
        let r2 = gate_round_trip(&ir);
        let r3 = gate_coverage(&ir);
        std::env::remove_var("QONTINUI_SPEC_COVERAGE_FLOOR");
        r1.expect("distinctness");
        r2.expect("round-trip");
        let _ = r3.expect("coverage");
        let snapshot = body_snapshot();
        let _ = gate_b_execution_green_with_snapshot(&snapshot, &ir).expect("b-green");
    }

    // -------- Error-reason / detail mappings ----------------------------

    #[test]
    fn error_reasons_are_stable_short_strings() {
        let cases: Vec<(ValidatorError, &str)> = vec![
            (
                ValidatorError::DistinctnessFailed(DistinctnessReport {
                    ok: false,
                    violations: Vec::new(),
                }),
                "distinctness-failed",
            ),
            (ValidatorError::RoundTripMismatch, "round-trip-mismatch"),
            (
                ValidatorError::CoverageBelowFloor {
                    ratio: 0.5,
                    floor: 0.8,
                },
                "coverage-below-floor",
            ),
            (
                ValidatorError::SnapshotFetchFailed(SnapshotFetchError::NotConnected),
                "snapshot-fetch-failed",
            ),
        ];
        for (err, expected_reason) in &cases {
            assert_eq!(validator_error_reason(err), *expected_reason);
            // detail is never empty.
            assert!(!validator_error_detail(err).is_empty());
        }
    }
}
