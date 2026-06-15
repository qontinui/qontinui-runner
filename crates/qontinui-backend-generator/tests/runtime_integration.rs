//! Runtime-verification integration tier (Fork B) — **DEFERRED, `#[ignore]`d**.
//!
//! This is the test that proves the plan's load-bearing premise: that a generated
//! backend can be brought up, hit, and have coverage *observed from the running server*.
//! It is `#[ignore]`d in Phase 1 and does NOT assert against a live server, because the
//! deterministic scaffold emits a stub handler and no alembic migrations — so there is
//! no real pairing endpoint to exercise yet (see `run/README.md`). Hand-writing those
//! just to make this pass would be faking runtime verification, which Phase 1 forbids.
//!
//! What it WILL do once the Phase-2 LLM-filled handler + migrations land:
//!   1. `generate_phase1(spec, profile)` then `materialize_to(run/generated)`.
//!   2. `docker compose -f run/docker-compose.yml up --build -d` (throwaway postgres),
//!      `alembic upgrade head`.
//!   3. `POST /api/v1/devices/pair-confirm` with a valid pairing → assert 201 + a
//!      token-shaped body; introspect the live schema for the `devices` table + its
//!      four columns; confirm the session middleware gates the route.
//!   4. Assemble a `CoverageEvidence { covered, gaps, filled_assumed }` from those LIVE
//!      observations and assert `evaluate_completeness` reports `entities.Device*` +
//!      `operations.pairConfirm` covered, `operations.pairConfirm.effect` filled.
//!   5. A deliberately broken variant (500 / dropped column) must surface the matching
//!      ref as a `CoverageGap` — proving evidence reflects the running server.
//!
//! Run (manually, after Phase 2): `cargo test -p qontinui-backend-generator --
//! --ignored runtime_pair_confirm_observed_from_live_server`.

use std::path::PathBuf;

use qontinui_backend_generator::generate_phase1;
use qontinui_types::functional_spec::FunctionalSpec;
use qontinui_types::priorities_profile::Profile;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../qontinui-schemas/rust/tests/fixtures/functional_spec")
}

#[test]
#[ignore = "runtime tier deferred: needs Phase-2 LLM-filled handler + alembic migrations; \
            Phase 1 must not fake runtime verification (see run/README.md)"]
fn runtime_pair_confirm_observed_from_live_server() {
    // Deterministic prerequisite that DOES hold today: the generator materializes a
    // tree containing app/main.py + the Device model + the pair-confirm route. This is
    // the only part this stub exercises; the live bring-up + HTTP probe + CoverageEvidence
    // assembly is intentionally NOT implemented until the Phase-2 bodies land.
    let spec: FunctionalSpec = serde_json::from_str(
        &std::fs::read_to_string(fixtures_dir().join("connect-runner.functional_spec.json"))
            .expect("read spec fixture"),
    )
    .expect("parse spec");
    let profile: Profile = serde_json::from_str(
        &std::fs::read_to_string(fixtures_dir().join("connect-runner.profile.json"))
            .expect("read profile fixture"),
    )
    .expect("parse profile");

    let backend = generate_phase1(&spec, &profile);
    assert!(backend.files.contains_key("app/main.py"));
    assert!(backend.files.contains_key("app/models/device.py"));
    assert!(backend.files.contains_key("app/api/pair_confirm.py"));

    // DEFERRED: docker compose up + alembic upgrade + live POST + CoverageEvidence.
    // See run/README.md. Do not implement until the real handler body + migrations exist.
}
