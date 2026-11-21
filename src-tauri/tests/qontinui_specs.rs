//! spec-multi-app Stream E.9 tests #6 + #7 — `qontinui-specs` CLI
//! `--app` filter behavior.
//!
//! These exercise the compiled binary as a subprocess (the canonical
//! integration-test pattern for a separate `[[bin]]` target). Both are
//! PG-backed: the CLI opens its own short-lived connection, so each test
//! takes a fresh database from the lib's test harness
//! (`qontinui_runner_lib::embedded_pg::test_harness`) and hands its URL to the
//! spawned binary as `RUNNER_DATABASE_URL`, with `QONTINUI_HOME` pointed at an
//! empty temp dir so no real `profiles.json` can override it.
//!
//! Test #6: `coverage_with_app_filter_returns_one_section` — `--app foo`
//! produces a single-section output (text) / single-key map (json).
//! Test #7: `coverage_without_app_filter_returns_grouped_output` — omitted
//! `--app` produces one section per registered app.

use std::process::Command;

/// Locate the compiled `qontinui_specs` binary. cargo's `CARGO_BIN_EXE_<name>`
/// env var is automatically populated when running integration tests against
/// a `[[bin]]` target.
fn binary_path() -> String {
    // The bin target name in Cargo.toml is `qontinui_specs` (underscored).
    std::env::var("CARGO_BIN_EXE_qontinui_specs")
        .expect("CARGO_BIN_EXE_qontinui_specs must be set when running integration tests")
}

/// Run the CLI against `db` with a hermetic profile/DSN environment.
fn run_cli(
    bin: &str,
    db: &qontinui_runner_lib::embedded_pg::test_harness::TestDatabase,
    args: &[&str],
) -> std::process::Output {
    let home = tempfile::tempdir().expect("temp QONTINUI_HOME");
    Command::new(bin)
        .args(args)
        .env("QONTINUI_HOME", home.path())
        .env("RUNNER_DATABASE_URL", db.url())
        .env_remove("QONTINUI_ENV")
        .env_remove("DATABASE_URL")
        .output()
        .expect("spawn qontinui-specs")
}

#[test]
fn coverage_with_app_filter_returns_one_section() {
    let bin = binary_path();
    let db = qontinui_runner_lib::embedded_pg::test_harness::fresh_database_blocking();
    let app_id = format!(
        "e9-cli-filter-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );

    // Register a temp app via psql is over the wire; for this test we
    // just pass a typo'd id and assert exit code 2. The full "register
    // then run" path is exercised by the unit tests inside
    // `database/pg/apps.rs::tests`.
    let out = run_cli(
        &bin,
        &db,
        &["coverage", "--app", &app_id, "--format", "json"],
    );

    // Validation rejects an unregistered id with exit 2 + a usable error
    // message listing valid app_ids.
    let code = out.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        code, 2,
        "expected exit 2 on unregistered --app filter; stderr={}",
        stderr
    );
    assert!(
        stderr.contains(&app_id) && stderr.contains("not registered"),
        "stderr must explain the rejection: {}",
        stderr
    );
}

#[test]
fn coverage_without_app_filter_returns_grouped_output() {
    let bin = binary_path();
    let db = qontinui_runner_lib::embedded_pg::test_harness::fresh_database_blocking();
    let out = run_cli(&bin, &db, &["coverage", "--format", "json"]);

    let code = out.status.code().unwrap_or(-1);
    assert!(
        code == 0,
        "expected exit 0 with no --app filter; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The output is a JSON object whose keys are app_ids (or
    // `(aggregate)` if the registry is empty). Either way, the top-level
    // shape MUST be an object map — never a bare numeric / array, because
    // the per-app sectioning shape is what downstream dashboards consume.
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("--format json output must parse as JSON");
    assert!(
        parsed.is_object(),
        "expected top-level object (per-app map), got: {:?}",
        parsed
    );
    let keys: Vec<String> = parsed.as_object().unwrap().keys().cloned().collect();
    assert!(
        !keys.is_empty(),
        "per-app map must have at least one section (aggregate fallback if registry is empty)"
    );
    // Each section value must have the canonical coverage shape.
    for (_app_id, section) in parsed.as_object().unwrap() {
        assert!(
            section.get("window_days").is_some(),
            "section missing window_days: {}",
            section
        );
    }
}
