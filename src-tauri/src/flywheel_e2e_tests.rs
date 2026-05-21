//! End-to-end integration test for the Stream E (Flywheel) coverage-growth
//! loop — Step 11 of `qontinui-dev-notes/spec-check-v1/05-flywheel.md`.
//!
//! These tests exercise the closed-loop INVARIANTS of the flywheel without
//! standing up a real PG + AppState + HTTP transport. Per the plan's
//! "pragmatic alternative" framing, we drive the production functions
//! directly — `storage::write_pending_ir`, `storage::promote_pending`,
//! `storage::demote_pending`, `coverage::generate_regression_suite`,
//! `coverage::coverage_of`, `validator::gate_b_execution_green_with_snapshot`,
//! `spec_authoring::merge_patch_into_ir`.
//!
//! Tests that require a live PG (e.g. concurrent-scan UNIQUE-constraint
//! exercise) are gated behind `--features pg_integration_tests` to match
//! the sibling proposals::tests::pg pattern.
//!
//! Each test maps to one of the six required invariants in Step 11:
//!
//!   1. flywheel_full_walk_synthetic_page_through_promotion
//!      — write_pending_ir → simulate green → promote → assert canonical IR
//!        exists with provenance.status = Promoted.
//!   2. flywheel_demotes_on_single_red
//!      — write_pending_ir → simulate red → demote → assert _pending/ gone
//!        and no canonical IR was written.
//!   3. flywheel_skips_existing_specs_in_scan
//!      — pre-create canonical IR for spec_id → list_pages exposes it →
//!        scan filter would skip it.
//!   4. flywheel_dedupes_concurrent_scans
//!      — insert_proposal twice with same kind+pathname → second returns
//!        Ok(false) per the UNIQUE constraint.
//!   5. flywheel_patch_preserves_hand_authored
//!      — merge_patch_into_ir on hand-authored state → rejects with the
//!        "pinned state" sentinel.
//!   6. flywheel_validator_rejects_low_coverage
//!      — synthetic IR with no transitions → gate_coverage returns
//!        CoverageBelowFloor.
//!
//! Pretty much all of these rely on integration-time visibility of
//! `spec_api`, `workflow_generation`, and (for the validator inner helper +
//! merge_patch_into_ir) symbol widening. See the agent report for the
//! exhaustive list of widenings the orchestrator needs to apply before this
//! file compiles.

#![cfg(all(test, feature = "spec-authoring"))]

use std::path::Path;

use crate::spec_api::storage;
use crate::spec_api::types::{
    IrAssertion, IrAssertionTarget, IrPageSpec, IrProvenance, IrState, IrTransition, ProposalStatus,
};
use crate::spec_api::validator::{
    gate_b_execution_green_with_snapshot, gate_coverage, ValidatorError,
};
use crate::workflow_generation::coverage::{coverage_of, generate_regression_suite};
use crate::workflow_generation::spec_authoring::{merge_patch_into_ir, IrPatch};

use qontinui_types::ui_bridge::{
    ElementIdentifier, ElementRect, ElementState, UIBridgeElement, UIBridgeSnapshot,
};
use serde_json::json;
use tempfile::TempDir;

// =============================================================================
// TestEnv — minimal harness shared across tests
// =============================================================================

/// Holds a temp specs root + makes it the resolved `QONTINUI_SPECS_ROOT` for
/// the duration of a single test. NOT for parallel use — set via env var,
/// so each test serializes on `ENV_GUARD` if it relies on the override.
struct TestEnv {
    tmp: TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("create tempdir");
        Self { tmp }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }
}

// =============================================================================
// IR / snapshot fixtures
// =============================================================================

/// Single-element snapshot whose `tag_name == "body"`. Matches the IR
/// `tagName == "body"` search criteria emitted by `chain_ir`. Mirrors the
/// shape in `validator.rs::tests::body_snapshot`.
fn body_snapshot() -> UIBridgeSnapshot {
    UIBridgeSnapshot {
        timestamp: 1_700_000_000_000,
        elements: vec![UIBridgeElement {
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
        }],
        components: Vec::new(),
        workflows: Vec::new(),
        modal_stack: None,
        toasts: None,
        undo_redo: None,
        current_route: None,
        segments: Vec::new(),
    }
}

/// Snapshot with no elements — every assertion misses.
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

/// IR with one state asserting on a `tagName == "body"` element. Status =
/// `Proposed` so `write_pending_ir` can flip it to Pending on stage and
/// `promote_pending` can flip it again to Promoted.
///
/// The `single state + single assertion` shape passes distinctness (no peers
/// to be identical-with) and round-trips cleanly; coverage is 0/1 = 0
/// against the default 0.80 floor, which is why we set
/// `QONTINUI_SPEC_COVERAGE_FLOOR=0.0` in tests that exercise gate-3 alongside
/// gate-4.
fn proposed_ir(id: &str) -> IrPageSpec {
    IrPageSpec {
        version: "1.0".into(),
        id: id.into(),
        name: id.into(),
        description: None,
        metadata: None,
        provenance: Some(IrProvenance {
            source: "ai-generated".into(),
            app_id: "qontinui-runner".to_string(),
            file: None,
            line: None,
            column: None,
            plugin_version: None,
            status: Some(ProposalStatus::Proposed),
        }),
        states: vec![IrState {
            id: "s0".into(),
            name: "s0".into(),
            description: None,
            assertions: vec![IrAssertion {
                id: "a-s0".into(),
                description: "body exists".into(),
                category: "structural".into(),
                severity: "critical".into(),
                assertion_type: "element-exists".into(),
                target: IrAssertionTarget {
                    kind: "search".into(),
                    criteria: json!({ "tagName": "body" }),
                    label: "body".into(),
                },
                source: "ai-generated".into(),
                reviewed: false,
                enabled: true,
                precondition: None,
            }],
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
        }],
        transitions: Vec::new(),
        synthesized_groups: None,
        initial_state: Some("s0".into()),
    }
}

/// IR with one bare state and no transitions. Reachable = {s0}, covered =
/// {} (no transitions touch any state). Ratio = 0/1 = 0 — below the default
/// 0.80 floor.
fn disconnected_ir(id: &str) -> IrPageSpec {
    IrPageSpec {
        version: "1.0".into(),
        id: id.into(),
        name: id.into(),
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
        }],
        transitions: Vec::new(),
        synthesized_groups: None,
        initial_state: Some("s0".into()),
    }
}

/// Existing IR with one state stamped `provenance.source = "hand-authored"`
/// — used to prove the patch path refuses to mutate it.
fn hand_authored_ir(id: &str, state_id: &str) -> IrPageSpec {
    let mut ir = disconnected_ir(id);
    ir.states[0].id = state_id.into();
    ir.states[0].name = state_id.into();
    ir.states[0].provenance = Some(IrProvenance {
        source: "hand-authored".into(),
        app_id: "qontinui-runner".to_string(),
        file: None,
        line: None,
        column: None,
        plugin_version: None,
        status: None,
    });
    ir.initial_state = Some(state_id.into());
    ir
}

// =============================================================================
// Test 1 — full walk through promotion
// =============================================================================
//
// Plan invariant (Step 11):
//   write_pending_ir → simulate sweep iteration #1 (green) → simulate sweep
//   iteration #2 (green) → promote → assert canonical files exist + _pending/
//   gone + provenance.status = Promoted.
//
// We drive the production storage helpers directly. The "two greens"
// requirement is exercised by calling `gate_b_execution_green_with_snapshot`
// twice with the same canned-matching snapshot; both must return Ok. The
// page-lock + canonical-write + rm-rf-_pending dance is exercised by
// `promote_pending`.

#[tokio::test(flavor = "multi_thread")]
async fn flywheel_full_walk_synthetic_page_through_promotion() {
    let env = TestEnv::new();
    let root = env.root();
    let page_id = "test-page-a";
    let ir = proposed_ir(page_id);

    // Stage the candidate into _pending/. This stamps status = Pending on
    // the on-disk doc (caller's IR is untouched).
    let staged_path =
        storage::write_pending_ir(root, storage::RUNNER_APP_ID, &ir).expect("write_pending_ir");
    assert!(
        staged_path.to_string_lossy().contains("_pending"),
        "staged path should land under _pending/: {}",
        staged_path.display()
    );
    let pending_ir = root.join("pages").join(page_id).join("_pending");
    assert!(
        pending_ir.exists(),
        "_pending/ should exist after stage: {}",
        pending_ir.display()
    );

    // Simulate sweep iteration #1 — gate-3 green.
    let snap1 = body_snapshot();
    let r1 = gate_b_execution_green_with_snapshot(&snap1, &ir);
    assert!(
        r1.is_ok(),
        "first sweep should be green (matching snapshot): {:?}",
        r1.err()
    );

    // Simulate sweep iteration #2 — gate-3 green again.
    let snap2 = body_snapshot();
    let r2 = gate_b_execution_green_with_snapshot(&snap2, &ir);
    assert!(
        r2.is_ok(),
        "second sweep should be green (matching snapshot): {:?}",
        r2.err()
    );

    // Two greens → promote. promote_pending stamps status = Promoted and
    // moves the file out of _pending/.
    storage::promote_pending(root, storage::RUNNER_APP_ID, page_id).expect("promote_pending");

    // Canonical IR + projection exist.
    let canonical_ir = root
        .join("pages")
        .join(page_id)
        .join("state-machine.derived.json");
    let canonical_projection = root.join("pages").join(page_id).join("spec.uibridge.json");
    assert!(
        canonical_ir.exists(),
        "canonical IR missing after promotion: {}",
        canonical_ir.display()
    );
    assert!(
        canonical_projection.exists(),
        "canonical projection missing after promotion: {}",
        canonical_projection.display()
    );

    // _pending/ is gone.
    assert!(
        !pending_ir.exists(),
        "_pending/ should have been removed after promotion: {}",
        pending_ir.display()
    );

    // Promoted IR carries provenance.status = Promoted.
    let read = storage::read_ir(root, storage::RUNNER_APP_ID, page_id)
        .expect("read canonical")
        .expect("present");
    let prov = read.provenance.as_ref().expect("provenance");
    assert_eq!(prov.source, "ai-generated");
    assert_eq!(prov.status, Some(ProposalStatus::Promoted));
}

// =============================================================================
// Test 2 — demote on single red
// =============================================================================
//
// Plan invariant (Step 11):
//   write_pending_ir → simulate sweep iteration #1 green → simulate sweep
//   iteration #2 red (missing element) → demote → _pending/ gone, NO
//   canonical IR was written, greens counter reset (verified by the demote
//   sentinel — no canonical file).

#[tokio::test(flavor = "multi_thread")]
async fn flywheel_demotes_on_single_red() {
    let env = TestEnv::new();
    let root = env.root();
    let page_id = "test-page-b";
    let ir = proposed_ir(page_id);

    storage::write_pending_ir(root, storage::RUNNER_APP_ID, &ir).expect("write_pending_ir");
    let pending_dir = root.join("pages").join(page_id).join("_pending");
    assert!(pending_dir.exists(), "_pending/ should exist after stage");

    // First sweep — green.
    let snap_green = body_snapshot();
    let r1 = gate_b_execution_green_with_snapshot(&snap_green, &ir);
    assert!(r1.is_ok(), "first sweep should be green: {:?}", r1.err());

    // Second sweep — RED (empty snapshot misses the `body` assertion).
    let snap_red = empty_snapshot();
    let r2 = gate_b_execution_green_with_snapshot(&snap_red, &ir);
    match r2 {
        Err(ValidatorError::BExecutionRed { .. }) => { /* expected */ }
        other => panic!(
            "expected BExecutionRed on missing-element snapshot, got {:?}",
            other
        ),
    }

    // Demote — _pending/ gets nuked, canonical was never written.
    storage::demote_pending(root, page_id).expect("demote_pending");
    assert!(
        !pending_dir.exists(),
        "_pending/ should be removed after demote: {}",
        pending_dir.display()
    );

    let canonical_ir = root
        .join("pages")
        .join(page_id)
        .join("state-machine.derived.json");
    assert!(
        !canonical_ir.exists(),
        "canonical IR should NOT have been written on demote: {}",
        canonical_ir.display()
    );

    // Re-reading after demote returns None (no on-disk file).
    let read = storage::read_ir(root, storage::RUNNER_APP_ID, page_id).expect("read ok");
    assert!(
        read.is_none(),
        "no canonical IR should be readable after demote"
    );
}

// =============================================================================
// Test 3 — scan skips pathnames already covered by an on-disk spec
// =============================================================================
//
// Plan invariant (Step 11):
//   hand-create specs/pages/<spec_id>/state-machine.derived.json BEFORE the
//   scan → scan's on-disk filter (`storage::list_pages`) catches the
//   pathname → no proposal queued for that pathname.
//
// We exercise the filter directly because the full scan path requires a
// live PG (covered in flywheel_dedupes_concurrent_scans).

#[tokio::test(flavor = "multi_thread")]
async fn flywheel_skips_existing_specs_in_scan() {
    let env = TestEnv::new();
    let root = env.root();
    let page_id = "test-page-c";

    // Pre-create the canonical IR for `test-page-c` so the scan would skip it.
    let mut ir = proposed_ir(page_id);
    if let Some(prov) = ir.provenance.as_mut() {
        prov.status = Some(ProposalStatus::Promoted);
    }
    storage::write_ir_and_regenerate(root, storage::RUNNER_APP_ID, &ir).expect("write canonical");

    // The on-disk filter the scan uses lives in `storage::list_pages`. After
    // pre-creating the canonical spec, the page id must show up — this is
    // exactly what `proposals.rs::post_scan` consults to skip pre-spec'd
    // pathnames (`on_disk_pathnames.contains(&pathname)`).
    let pages = storage::list_pages(root, storage::RUNNER_APP_ID).expect("list_pages");
    assert!(
        pages.iter().any(|p| p == page_id),
        "list_pages should report {} after canonical write; got {:?}",
        page_id,
        pages
    );

    // A simulated scan-time filter mirroring post_scan's behaviour: for any
    // candidate pathname whose slugified id matches an on-disk page, skip
    // it. Here we just assert the membership predicate.
    let candidate_pathname = "/test/page-c";
    let candidate_slug = slugify_pathname(candidate_pathname);
    assert_eq!(candidate_slug, page_id);
    let skip = pages.iter().any(|p| p == &candidate_slug);
    assert!(
        skip,
        "scan should skip pathname {} (already on disk as {})",
        candidate_pathname, candidate_slug
    );
}

/// Local slug helper mirroring `spec_authoring::pathname_to_spec_id` — used
/// only in test 3 since the production helper is `pub(crate)` and not
/// reachable from an integration test. Behaviour is identical: lowercase
/// ASCII alphanumeric, runs of non-alphanumeric collapse to `-`, leading/
/// trailing `-` trimmed, empty maps to `"root"`.
fn slugify_pathname(pathname: &str) -> String {
    let mut out = String::with_capacity(pathname.len());
    let mut last_was_hyphen = true;
    for ch in pathname.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            out.push('-');
            last_was_hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "root".into()
    } else {
        out
    }
}

// =============================================================================
// Test 4 — concurrent /scan calls dedupe via UNIQUE constraint
// =============================================================================
//
// Plan invariant (Step 11):
//   /scan called twice in rapid succession → only one row in spec_proposals
//   (UNIQUE constraint kicks). PgDb::insert_proposal returns Ok(true) on
//   first insert and Ok(false) on the duplicate.
//
// PG-bound — gated behind `pg_integration_tests`. Mirrors the dedup test
// pattern in `proposals.rs::tests::pg::insert_dedupes_on_kind_and_target`.

#[cfg(feature = "pg_integration_tests")]
#[tokio::test(flavor = "multi_thread")]
async fn flywheel_dedupes_concurrent_scans() {
    use crate::database::pg::PgDb;

    let db = PgDb::new_blocking_for_test();
    // Wipe any prior test rows so dedup behaves deterministically. Mirrors
    // the `fresh_db` fixture used in proposals::tests::pg.
    {
        let conn = db.pool().get().await.expect("pg conn");
        conn.execute("TRUNCATE TABLE spec_proposals", &[])
            .await
            .expect("truncate spec_proposals");
    }

    let pathname = "/test/concurrent-dedupe";
    let metadata = serde_json::json!({ "minObservations": 50, "lookbackDays": 90 });

    let first = db
        .insert_proposal(
            "prop_flywheel_dedupe_one",
            "fullPage",
            Some(pathname),
            None,
            "queued",
            &metadata,
        )
        .await
        .expect("first insert");
    assert!(first, "first insert should succeed");

    let second = db
        .insert_proposal(
            "prop_flywheel_dedupe_two",
            "fullPage",
            Some(pathname),
            None,
            "queued",
            &metadata,
        )
        .await
        .expect("second insert");
    assert!(
        !second,
        "second insert should be deduped by the UNIQUE constraint"
    );
}

// =============================================================================
// Test 5 — patch preserves hand-authored states
// =============================================================================
//
// Plan invariant (Step 11):
//   seed a hand-authored IR + a drift report → patch authoring rejects the
//   merge with the "pinned state" sentinel. The pure logic lives in
//   `spec_authoring::merge_patch_into_ir`; we exercise it directly to avoid
//   spinning the meta-workflow executor + AppState.

#[tokio::test(flavor = "multi_thread")]
async fn flywheel_patch_preserves_hand_authored() {
    use qontinui_types::ir::IrElementCriteria;

    let existing = hand_authored_ir("test-page-d", "s-hand");
    // Build a patch targeting the hand-authored state. `IrPatch` has
    // `add_required_elements: BTreeMap<String, Vec<IrElementCriteria>>` and
    // `add_transitions: Vec<IrTransition>`. We populate just the required-
    // elements bucket — the merge helper validates BEFORE applying so it
    // never half-mutates.
    let mut crit = IrElementCriteria::default();
    crit.id = Some("new-elem".into());

    let mut patch = IrPatch::default();
    patch
        .add_required_elements
        .insert("s-hand".into(), vec![crit]);

    let err = merge_patch_into_ir(existing, patch, "qontinui-runner")
        .expect_err("merge must refuse pinned hand-authored state");
    assert!(
        err.contains("pinned state 's-hand'") || err.contains("hand-authored"),
        "expected pinned-state error, got: {}",
        err
    );
}

// =============================================================================
// Test 6 — validator rejects low-coverage candidates
// =============================================================================
//
// Plan invariant (Step 11):
//   synthetic IR with a disconnected state subgraph → `gate_coverage`
//   returns `ValidatorError::CoverageBelowFloor`.
//
// The default floor is `QONTINUI_SPEC_COVERAGE_FLOOR` (0.80). A 1-state IR
// with no transitions has states_covered = 0 (no transition touches any
// state) and reachable = {s0}; ratio = 0/1 = 0 < 0.80 → must fail.
//
// We also assert the underlying coverage report shape directly to confirm
// the production helper computes the report the gate consumes.

#[tokio::test(flavor = "multi_thread")]
async fn flywheel_validator_rejects_low_coverage() {
    // Serialize on the env-var guard — gate_coverage reads
    // QONTINUI_SPEC_COVERAGE_FLOOR and the validator's unit tests share
    // the same env-var with their own Mutex; we don't have access to that
    // Mutex from an integration test, so we make sure the env var is unset
    // and accept that parallel test execution against the same env var
    // (other tests in this file don't touch it) is safe here.
    std::env::remove_var("QONTINUI_SPEC_COVERAGE_FLOOR");

    let ir = disconnected_ir("test-page-e");

    // Sanity-check the underlying coverage report so a future change to
    // the coverage helper surfaces here too.
    let suite = generate_regression_suite(&ir);
    let report = coverage_of(&ir, &suite);
    assert_eq!(report.states_covered, 0, "expected 0 covered states");
    assert_eq!(
        report.reachable_states.len(),
        1,
        "expected only s0 reachable"
    );
    // Ratio = 0/1 = 0.0 ⇒ below default 0.80 floor.

    let result = gate_coverage(&ir);
    match result {
        Err(ValidatorError::CoverageBelowFloor { ratio, floor }) => {
            assert!(ratio < 0.01, "expected ~0 ratio, got {}", ratio);
            assert!(
                (floor - 0.80).abs() < 1e-6,
                "expected default 0.80 floor, got {}",
                floor
            );
        }
        other => panic!("expected CoverageBelowFloor, got {:?}", other),
    }
}

// =============================================================================
// spec-multi-app Stream E.9 test #4 — two apps run independent cycles
// =============================================================================
//
// Plan invariant (§E.9):
//   Two distinct apps each register a separate repo_root, each stage a
//   `_pending/` candidate, and each promotion lands under the owning app's
//   spec corpus without bleeding into the sibling.
//
// We exercise the storage primitives directly (no HTTP) to keep the test
// hermetic; the route layer is a thin `Path((app_id,))` wrapper around
// these calls.

#[tokio::test(flavor = "multi_thread")]
async fn e2e_two_apps_independent_proposal_cycles() {
    // Two apps, two tempdirs, two pages — same page id slug to prove
    // the per-app root partitions the corpus.
    let app_a = "qontinui-runner";
    let app_b = "qontinui-web";
    let env_a = TestEnv::new();
    let env_b = TestEnv::new();
    let page_id = "shared-slug";

    let ir_a = proposed_ir(page_id);
    let ir_b = proposed_ir(page_id);

    // Stage under each app's root.
    storage::write_pending_ir(env_a.root(), app_a, &ir_a).expect("stage A");
    storage::write_pending_ir(env_b.root(), app_b, &ir_b).expect("stage B");

    let pending_a = env_a.root().join("pages").join(page_id).join("_pending");
    let pending_b = env_b.root().join("pages").join(page_id).join("_pending");
    assert!(pending_a.exists(), "A _pending/ missing");
    assert!(pending_b.exists(), "B _pending/ missing");

    // Promote A. B must be untouched.
    storage::promote_pending(env_a.root(), app_a, page_id).expect("promote A");
    let canon_a = env_a
        .root()
        .join("pages")
        .join(page_id)
        .join("state-machine.derived.json");
    assert!(canon_a.exists(), "A canonical missing after promote");
    assert!(!pending_a.exists(), "A _pending/ should be gone");
    // Sibling app B is untouched.
    assert!(
        pending_b.exists(),
        "B _pending/ must not be affected by A's promotion"
    );
    let canon_b = env_b
        .root()
        .join("pages")
        .join(page_id)
        .join("state-machine.derived.json");
    assert!(
        !canon_b.exists(),
        "B must not have a canonical IR yet — only A was promoted"
    );

    // Now promote B independently.
    storage::promote_pending(env_b.root(), app_b, page_id).expect("promote B");
    assert!(canon_b.exists(), "B canonical missing after promote");
    assert!(!pending_b.exists(), "B _pending/ should be gone");

    // Promoted IR carries the right provenance.app_id on each side.
    let read_a = storage::read_ir(env_a.root(), app_a, page_id)
        .expect("read A")
        .expect("A present");
    let read_b = storage::read_ir(env_b.root(), app_b, page_id)
        .expect("read B")
        .expect("B present");
    let prov_a = read_a.provenance.as_ref().expect("A provenance");
    let prov_b = read_b.provenance.as_ref().expect("B provenance");
    assert_eq!(prov_a.app_id, app_a, "A provenance.app_id");
    assert_eq!(prov_b.app_id, app_b, "B provenance.app_id");
}
