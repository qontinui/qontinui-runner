//! Helper (run with `--ignored`) that materializes the Phase-3 backend to `run/generated`
//! so the python suite can be inspected/run locally during development.

use std::path::PathBuf;

use qontinui_backend_generator::{generate_phase3, BreakMode};
use qontinui_types::functional_spec::FunctionalSpec;
use qontinui_types::priorities_profile::Profile;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../qontinui-schemas/rust/tests/fixtures/functional_spec")
}

#[test]
#[ignore = "helper: materializes the phase-3 tree to run/generated for local inspection"]
fn materialize_connect_runner() {
    let spec: FunctionalSpec = serde_json::from_str(
        &std::fs::read_to_string(fixtures_dir().join("connect-runner.functional_spec.json"))
            .unwrap(),
    )
    .unwrap();
    let profile: Profile = serde_json::from_str(
        &std::fs::read_to_string(fixtures_dir().join("connect-runner.profile.json")).unwrap(),
    )
    .unwrap();
    let backend = generate_phase3(&spec, &profile, BreakMode::None);
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("run/generated");
    let _ = std::fs::remove_dir_all(&out);
    backend.materialize_to(&out).unwrap();
    eprintln!(
        "materialized {} files to {}",
        backend.files.len(),
        out.display()
    );
}

#[test]
#[ignore = "helper: materializes the multi-entity phase-3 tree to run/generated-multi"]
fn materialize_multi_entity() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let spec: FunctionalSpec = serde_json::from_str(
        &std::fs::read_to_string(dir.join("multi-entity.functional_spec.json")).unwrap(),
    )
    .unwrap();
    let profile: Profile = serde_json::from_str(
        &std::fs::read_to_string(dir.join("multi-entity.profile.json")).unwrap(),
    )
    .unwrap();
    let backend = generate_phase3(&spec, &profile, BreakMode::None);
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("run/generated-multi");
    let _ = std::fs::remove_dir_all(&out);
    backend.materialize_to(&out).unwrap();
    eprintln!(
        "materialized {} files to {}",
        backend.files.len(),
        out.display()
    );
}
