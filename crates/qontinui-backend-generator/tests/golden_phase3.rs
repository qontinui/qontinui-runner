//! Phase 3 golden tests — deterministic checks on the emitted quality artifacts
//! (Alembic migration, validated+auth handlers, generated pytest suite, coverage gate).
//! No Docker, no Python execution — those live in `tests/runtime_phase3.rs` (`--ignored`).

use std::path::PathBuf;

use qontinui_backend_generator::{generate_phase3, min_coverage_pct, run_enforcement, BreakMode};
use qontinui_types::functional_spec::FunctionalSpec;
use qontinui_types::priorities_profile::Profile;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../qontinui-schemas/rust/tests/fixtures/functional_spec")
}

fn load_spec() -> FunctionalSpec {
    serde_json::from_str(
        &std::fs::read_to_string(fixtures_dir().join("connect-runner.functional_spec.json"))
            .unwrap(),
    )
    .unwrap()
}

fn load_profile() -> Profile {
    serde_json::from_str(
        &std::fs::read_to_string(fixtures_dir().join("connect-runner.profile.json")).unwrap(),
    )
    .unwrap()
}

#[test]
fn emits_alembic_migration_creating_devices_table() {
    let backend = generate_phase3(&load_spec(), &load_profile(), BreakMode::None);
    let mig = backend
        .files
        .get("alembic/versions/0001_initial.py")
        .expect("initial migration emitted");
    assert!(
        mig.contains("op.create_table("),
        "migration creates tables:\n{mig}"
    );
    assert!(mig.contains("\"devices\""), "creates the devices table");
    for col in ["device_name", "device_id", "state", "callback"] {
        assert!(
            mig.contains(&format!("\"{col}\"")),
            "migration has column {col}"
        );
    }
    // The session no longer uses create_all (Alembic owns the schema).
    let session = backend.files.get("app/db/session.py").unwrap();
    assert!(
        !session.contains("create_all"),
        "session must NOT create_all (Alembic owns schema):\n{session}"
    );
    // alembic.ini + env.py present.
    assert!(backend.files.contains_key("alembic.ini"));
    assert!(backend.files.contains_key("alembic/env.py"));
}

#[test]
fn handler_has_validation_auth_and_persistence() {
    let backend = generate_phase3(&load_spec(), &load_profile(), BreakMode::None);
    let route = backend.files.get("app/api/pair_confirm.py").unwrap();
    // Auth dependency wired.
    assert!(
        route.contains("Depends(require_auth)"),
        "auth dependency wired:\n{route}"
    );
    // Validation gate (422) on the https-allowlisted callback.
    assert!(
        route.contains("is_https_allowlisted(payload.callback)"),
        "callback validated"
    );
    assert!(
        route.contains("HTTP_422_UNPROCESSABLE_ENTITY"),
        "422 on invalid input"
    );
    // Real persistence.
    assert!(
        route.contains("db.add(row)") && route.contains("db.commit()"),
        "persists"
    );
    assert!(route.contains("secrets.token_urlsafe"), "returns a token");
    // Auth module + validators emitted.
    assert!(backend.files.contains_key("app/auth.py"));
    assert!(backend.files.contains_key("app/validation.py"));
}

#[test]
fn emits_generated_pytest_suite_and_coverage_gate() {
    let profile = load_profile();
    let backend = generate_phase3(&load_spec(), &profile, BreakMode::None);
    // Per-operation test + auth + health + model tests.
    assert!(backend.files.contains_key("tests/test_pair_confirm.py"));
    assert!(backend.files.contains_key("tests/test_auth.py"));
    assert!(backend.files.contains_key("tests/test_health.py"));
    assert!(backend.files.contains_key("tests/test_model_device.py"));
    assert!(backend.files.contains_key("tests/conftest.py"));
    assert!(backend.files.contains_key(".coveragerc"));

    // The op test exercises happy path (201), validation (422), and auth (401).
    let optest = backend.files.get("tests/test_pair_confirm.py").unwrap();
    assert!(optest.contains("== 201"), "happy path 201:\n{optest}");
    assert!(optest.contains("== 422"), "validation 422");
    assert!(optest.contains("== 401"), "auth 401");

    // The coverage gate reflects the profile's min_coverage_pct (80).
    assert_eq!(min_coverage_pct(&profile), 80);
    let runner = backend.files.get("run_tests.sh").unwrap();
    assert!(
        runner.contains("--cov-fail-under=80"),
        "coverage gate must be the profile's 80%:\n{runner}"
    );
    assert!(
        runner.contains("alembic upgrade head"),
        "runner applies migrations first"
    );
}

#[test]
fn enforcement_gate_passes_on_clean_generated_tree() {
    // The generated tree must not trip the built-in security static scan.
    let backend = generate_phase3(&load_spec(), &load_profile(), BreakMode::None);
    let report = run_enforcement(&load_profile(), &backend);
    assert!(
        report.passed(),
        "clean generated tree must pass enforcement; blocking={:?}",
        report.blocking
    );
    // `code-reviewer` is structurally-wired but has no in-crate adapter → deferred,
    // honestly reported (NOT faked as a pass).
    assert!(
        report.deferred_gates.contains(&"code-reviewer".to_string()),
        "code-reviewer must be reported deferred, not faked: {:?}",
        report.deferred_gates
    );
}

#[test]
fn drop_column_fault_removes_column_from_migration() {
    let backend = generate_phase3(
        &load_spec(),
        &load_profile(),
        BreakMode::DropColumn {
            entity: "Device".to_string(),
            column: "callback".to_string(),
        },
    );
    let mig = backend
        .files
        .get("alembic/versions/0001_initial.py")
        .unwrap();
    assert!(
        !mig.contains("\"callback\""),
        "dropped column must be absent from the migration:\n{mig}"
    );
}
