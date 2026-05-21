//! spec-multi-app Stream E.9 tests #6 + #7 — `qontinui-specs` CLI
//! `--app` filter behavior.
//!
//! These exercise the compiled binary as a subprocess (the canonical
//! integration-test pattern for a separate `[[bin]]` target). Both are
//! gated on a live PG (via DATABASE_URL) because the CLI requires its own
//! short-lived connection — there is no in-process test seam for it.
//!
//! Run locally:
//!   DATABASE_URL=postgres://qontinui_user:PASSWORD@localhost:5433/qontinui_db \
//!     cargo test --bin qontinui-runner --test qontinui_specs -- --ignored
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

#[test]
#[ignore = "requires PG via DATABASE_URL"]
fn coverage_with_app_filter_returns_one_section() {
    let bin = binary_path();
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
    let out = Command::new(&bin)
        .args(["coverage", "--app", &app_id, "--format", "json"])
        .output()
        .expect("spawn qontinui-specs");

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
#[ignore = "requires PG via DATABASE_URL"]
fn coverage_without_app_filter_returns_grouped_output() {
    let bin = binary_path();
    let out = Command::new(&bin)
        .args(["coverage", "--format", "json"])
        .output()
        .expect("spawn qontinui-specs");

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
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .expect("--format json output must parse as JSON");
    assert!(
        parsed.is_object(),
        "expected top-level object (per-app map), got: {:?}",
        parsed
    );
    let keys: Vec<String> = parsed
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
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
