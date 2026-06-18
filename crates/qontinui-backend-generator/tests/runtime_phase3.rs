//! Phase 3 runtime tier (Fork B) — **REAL, but `#[ignore]`d for CI (no Docker daemon)**.
//!
//! This is the headline-verifiable deliverable: the GENERATED backend's GENERATED test
//! suite actually runs in Docker and passes at the coverage gate. The test:
//!
//! 1. materializes the Phase-3 backend (Alembic migrations + validated/auth handlers +
//!    generated pytest suite) into `run/generated`,
//! 2. brings it up under docker-compose — the app container runs `alembic upgrade head`
//!    (the schema is owned by Alembic, NOT create_all) before uvicorn, and the test
//!    introspects the live postgres schema to confirm the migration ran,
//! 3. runs the generated `pytest --cov=app --cov-fail-under=<min>` **inside the
//!    container** against the real postgres, captures the real pytest summary + coverage
//!    %, and asserts the gate (≥ `min_coverage_pct` = 80) is genuinely met,
//! 4. assembles a [`CoverageEvidence`] from those observations — a below-bar coverage
//!    surfaces `operations.pairConfirm.effect` as a [`CoverageGap`] (re-dispatch signal),
//!    never a silent pass — and runs the real `evaluate_completeness`.
//!
//! Run with:
//! ```sh
//! cargo test -p qontinui-backend-generator --test runtime_phase3 -- --ignored
//! ```
//!
//! Ports: app on 8012, postgres on 55434 (distinct from the Phase-2 test's 8011/55433).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use qontinui_backend_generator::{generate_phase3, min_coverage_pct, BreakMode};
use qontinui_types::completeness_eval::{evaluate_completeness, CoverageEvidence, GapEvidence};
use qontinui_types::completeness_verdict::GapReason;
use qontinui_types::functional_spec::FunctionalSpec;
use qontinui_types::priorities_profile::Profile;

const APP_HOST_PORT: &str = "8012";
const DB_HOST_PORT: &str = "55434";
const PROJECT: &str = "backgen3_itest";

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures_dir() -> PathBuf {
    crate_dir().join("../../../qontinui-schemas/rust/tests/fixtures/functional_spec")
}

fn load_spec() -> FunctionalSpec {
    serde_json::from_str(
        &std::fs::read_to_string(fixtures_dir().join("connect-runner.functional_spec.json"))
            .expect("read spec fixture"),
    )
    .expect("parse spec")
}

fn load_profile() -> Profile {
    serde_json::from_str(
        &std::fs::read_to_string(fixtures_dir().join("connect-runner.profile.json"))
            .expect("read profile fixture"),
    )
    .expect("parse profile")
}

fn compose(args: &[&str]) -> std::process::Output {
    let run_dir = crate_dir().join("run");
    Command::new("docker")
        .current_dir(&run_dir)
        .env("APP_HOST_PORT", APP_HOST_PORT)
        .env("DB_HOST_PORT", DB_HOST_PORT)
        .args(["compose", "-p", PROJECT, "-f", "docker-compose.yml"])
        .args(args)
        .output()
        .expect("spawn docker compose")
}

fn compose_down() {
    let out = compose(&["down", "-v"]);
    eprintln!(
        "[compose down] status={:?}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn materialize(break_mode: BreakMode) {
    let spec = load_spec();
    let profile = load_profile();
    let backend = generate_phase3(&spec, &profile, break_mode);
    let out = crate_dir().join("run/generated");
    let _ = std::fs::remove_dir_all(&out);
    backend
        .materialize_to(&out)
        .expect("materialize generated backend");
}

fn multi_fixtures_dir() -> PathBuf {
    crate_dir().join("tests/fixtures")
}

fn materialize_multi() -> FunctionalSpec {
    let spec: FunctionalSpec = serde_json::from_str(
        &std::fs::read_to_string(multi_fixtures_dir().join("multi-entity.functional_spec.json"))
            .expect("read multi spec"),
    )
    .expect("parse multi spec");
    let profile: Profile = serde_json::from_str(
        &std::fs::read_to_string(multi_fixtures_dir().join("multi-entity.profile.json"))
            .expect("read multi profile"),
    )
    .expect("parse multi profile");
    let backend = generate_phase3(&spec, &profile, BreakMode::None);
    let out = crate_dir().join("run/generated");
    let _ = std::fs::remove_dir_all(&out);
    backend
        .materialize_to(&out)
        .expect("materialize multi backend");
    spec
}

fn up_and_wait_healthy() {
    let up = compose(&["up", "--build", "-d"]);
    assert!(
        up.status.success(),
        "docker compose up failed:\n{}\n{}",
        String::from_utf8_lossy(&up.stdout),
        String::from_utf8_lossy(&up.stderr)
    );
    let container = format!("{PROJECT}-app-1");
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let out = Command::new("docker")
            .args(["inspect", "-f", "{{.State.Health.Status}}", &container])
            .output()
            .expect("docker inspect");
        let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if status == "healthy" {
            return;
        }
        if status == "unhealthy" || Instant::now() > deadline {
            let logs = Command::new("docker")
                .args(["logs", &container])
                .output()
                .expect("docker logs");
            panic!(
                "app never became healthy (last status={status:?}); logs:\n{}\n{}",
                String::from_utf8_lossy(&logs.stdout),
                String::from_utf8_lossy(&logs.stderr)
            );
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Introspect the live `devices` table columns (proves the Alembic migration ran).
fn live_device_columns() -> BTreeSet<String> {
    let db = format!("{PROJECT}-db-1");
    let out = Command::new("docker")
        .args([
            "exec",
            &db,
            "psql",
            "-U",
            "postgres",
            "-d",
            "app",
            "-t",
            "-A",
            "-c",
            "select column_name from information_schema.columns where table_name='devices';",
        ])
        .output()
        .expect("psql introspect");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Confirm the Alembic version table exists + is stamped (the migration really applied,
/// not create_all).
fn alembic_version() -> String {
    let db = format!("{PROJECT}-db-1");
    let out = Command::new("docker")
        .args([
            "exec",
            &db,
            "psql",
            "-U",
            "postgres",
            "-d",
            "app",
            "-t",
            "-A",
            "-c",
            "select version_num from alembic_version;",
        ])
        .output()
        .expect("psql alembic_version");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Run the generated `pytest --cov` INSIDE the app container against the real postgres.
/// Returns `(success, total_coverage_pct, full_output)`.
fn run_generated_tests_in_container() -> (bool, f64, String) {
    let container = format!("{PROJECT}-app-1");
    let out = Command::new("docker")
        .args([
            "exec",
            "-w",
            "/srv",
            &container,
            "pytest",
            "--cov=app",
            "--cov-report=term-missing",
            "--cov-fail-under=80",
        ])
        .output()
        .expect("docker exec pytest");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}");
    let pct = parse_total_coverage(&combined);
    (out.status.success(), pct, combined)
}

/// Parse the `TOTAL ... NN%` (or `Total coverage: NN.NN%`) line coverage.py prints.
fn parse_total_coverage(output: &str) -> f64 {
    // Prefer the explicit "Total coverage: NN.NN%" line (cov-fail-under summary).
    for line in output.lines() {
        if let Some(idx) = line.find("Total coverage:") {
            let rest = &line[idx + "Total coverage:".len()..];
            if let Ok(pct) = rest.trim().trim_end_matches('%').trim().parse::<f64>() {
                return pct;
            }
        }
    }
    // Fallback: the TOTAL row's last %-suffixed token.
    for line in output.lines() {
        if line.trim_start().starts_with("TOTAL") {
            for tok in line.split_whitespace().rev() {
                if let Some(num) = tok.strip_suffix('%') {
                    if let Ok(v) = num.parse::<f64>() {
                        return v;
                    }
                }
            }
        }
    }
    -1.0
}

/// Build a [`CoverageEvidence`] from the live observations + the in-container test run.
/// The coverage gate is part of the evidence: below-bar coverage demotes the assumed
/// effect from `filled` to a `CoverageGap` (the re-dispatch signal).
fn evidence_from_run(
    live_columns: &BTreeSet<String>,
    tests_passed: bool,
    coverage_pct: f64,
    min_cov: u32,
) -> CoverageEvidence {
    let mut covered: BTreeSet<String> = BTreeSet::new();
    let mut gaps: BTreeMap<String, GapEvidence> = BTreeMap::new();
    let mut filled_assumed: BTreeSet<String> = BTreeSet::new();

    if !live_columns.is_empty() {
        covered.insert("entities.Device".to_string());
    } else {
        gaps.insert(
            "entities.Device".to_string(),
            GapEvidence {
                reason: GapReason::NotGenerated,
                detail: Some("devices table absent after alembic upgrade".to_string()),
            },
        );
    }
    for (field, col) in [
        ("deviceName", "device_name"),
        ("deviceId", "device_id"),
        ("state", "state"),
        ("callback", "callback"),
    ] {
        let rf = format!("entities.Device.fields.{field}");
        if live_columns.contains(col) {
            covered.insert(rf);
        } else {
            gaps.insert(
                rf,
                GapEvidence {
                    reason: GapReason::NotGenerated,
                    detail: Some(format!("column `{col}` absent in migrated schema")),
                },
            );
        }
    }

    // operations.pairConfirm + its validation are covered iff the generated suite passes.
    let gate_met = tests_passed && coverage_pct >= f64::from(min_cov);
    if gate_met {
        covered.insert("operations.pairConfirm".to_string());
        covered.insert("operations.pairConfirm.inputs.callback.validation".to_string());
        covered.insert("auth".to_string());
        // The assumed effect is FILLED only when the quality gate is genuinely met.
        filled_assumed.insert("operations.pairConfirm.effect".to_string());
    } else {
        // Below-bar / failing suite → the under-tested node surfaces as a gap, not a pass.
        gaps.insert(
            "operations.pairConfirm".to_string(),
            GapEvidence {
                reason: GapReason::BehaviorMismatch,
                detail: Some(format!(
                    "generated suite did not meet the coverage gate: passed={tests_passed} coverage={coverage_pct:.2}% < {min_cov}%"
                )),
            },
        );
    }

    CoverageEvidence {
        covered,
        gaps,
        filled_assumed,
    }
}

#[test]
#[ignore = "runtime tier: needs a Docker daemon (CI has none); run with --ignored"]
fn phase3_generated_suite_passes_coverage_gate_in_docker() {
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            compose_down();
        }
    }
    compose_down();
    let _guard = Guard;

    let spec = load_spec();
    let profile = load_profile();
    let min_cov = min_coverage_pct(&profile);

    materialize(BreakMode::None);
    up_and_wait_healthy();

    // (a) Alembic really applied: version table stamped + devices columns present.
    let version = alembic_version();
    eprintln!("[alembic_version] {version:?}");
    assert_eq!(
        version, "0001_initial",
        "alembic must be stamped to the initial revision"
    );

    let columns = live_device_columns();
    eprintln!("[live schema] devices columns = {columns:?}");
    for col in ["device_name", "device_id", "state", "callback"] {
        assert!(
            columns.contains(col),
            "migrated schema must have `{col}`; got {columns:?}"
        );
    }

    // (b) THE headline: run the generated pytest --cov inside the container.
    let (passed, coverage, output) = run_generated_tests_in_container();
    eprintln!("================ GENERATED pytest --cov (in Docker) ================\n{output}\n===================================================================");
    assert!(
        passed,
        "generated suite must pass in-container at the coverage gate"
    );
    assert!(
        coverage >= f64::from(min_cov),
        "real line coverage {coverage:.2}% must be >= {min_cov}%"
    );
    eprintln!("[coverage] {coverage:.2}% >= {min_cov}% gate MET");

    // (c) Evidence assembled from the run → completeness verdict.
    let evidence = evidence_from_run(&columns, passed, coverage, min_cov);
    let verdict = evaluate_completeness(&spec, &evidence, "2026-06-18T00:00:00Z");
    assert!(
        evidence
            .filled_assumed
            .contains("operations.pairConfirm.effect"),
        "effect filled only when the quality gate is met"
    );
    let bad: Vec<_> = verdict
        .gaps
        .iter()
        .filter(|g| {
            g.r#ref.starts_with("operations.pairConfirm") || g.r#ref.starts_with("entities.Device")
        })
        .map(|g| g.r#ref.clone())
        .collect();
    assert!(
        bad.is_empty(),
        "no Device/pairConfirm gaps when the gate is met; got {bad:?}"
    );
    eprintln!(
        "[verdict] coverage={:.3} assumed_fill_rate={:.3} gaps={}",
        verdict.coverage,
        verdict.assumed_fill_rate,
        verdict.gaps.len()
    );

    // (d) Negative control: a below-bar run demotes the effect to a gap (re-dispatch).
    let below = evidence_from_run(&columns, false, 12.0, min_cov);
    let below_verdict = evaluate_completeness(&spec, &below, "2026-06-18T00:00:00Z");
    assert!(
        below_verdict
            .gaps
            .iter()
            .any(|g| g.r#ref == "operations.pairConfirm"),
        "a below-bar suite MUST surface pairConfirm as a CoverageGap, not a silent pass"
    );
    assert!(
        !below
            .filled_assumed
            .contains("operations.pairConfirm.effect"),
        "below-bar must NOT fill the assumed effect"
    );

    // Teardown via the guard.
}

/// Multi-entity / all-operations walk (deliverable 4): a 2-entity / 2-op spec generates,
/// runs under Docker (alembic creates BOTH tables), and the generated suite passes the
/// coverage gate — proving the generator's N-entity × M-operation loop end-to-end.
#[test]
#[ignore = "runtime tier: needs a Docker daemon (CI has none); run with --ignored"]
fn phase3_multi_entity_generates_runs_and_passes_in_docker() {
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            compose_down();
        }
    }
    compose_down();
    let _guard = Guard;

    let spec = materialize_multi();
    up_and_wait_healthy();

    // Both tables exist after `alembic upgrade head`.
    for table in ["projects", "members"] {
        let cols = live_table_columns(table);
        eprintln!("[live schema] {table} columns = {cols:?}");
        assert!(
            !cols.is_empty(),
            "alembic must create the `{table}` table; got {cols:?}"
        );
    }

    let (passed, coverage, output) = run_generated_tests_in_container();
    eprintln!("============ MULTI-ENTITY generated pytest --cov (Docker) ============\n{output}\n=====================================================================");
    assert!(
        passed,
        "multi-entity generated suite must pass in-container"
    );
    assert!(
        coverage >= 80.0,
        "multi-entity coverage {coverage:.2}% must be >= 80%"
    );

    // Both operations + both entities are covered from the live run.
    let pcols = live_table_columns("projects");
    let mcols = live_table_columns("members");
    let mut covered: BTreeSet<String> = BTreeSet::new();
    if !pcols.is_empty() {
        covered.insert("entities.Project".to_string());
    }
    if !mcols.is_empty() {
        covered.insert("entities.Member".to_string());
    }
    if passed && coverage >= 80.0 {
        covered.insert("operations.createProject".to_string());
        covered.insert("operations.inviteMember".to_string());
    }
    let evidence = CoverageEvidence {
        covered,
        gaps: BTreeMap::new(),
        filled_assumed: BTreeSet::new(),
    };
    let verdict = evaluate_completeness(&spec, &evidence, "2026-06-18T00:00:00Z");
    eprintln!(
        "[multi verdict] coverage={:.3} gaps={}",
        verdict.coverage,
        verdict.gaps.len()
    );
    assert!(
        evidence.covered.contains("operations.createProject")
            && evidence.covered.contains("operations.inviteMember"),
        "both operations covered from the live multi-entity run"
    );

    // Teardown via the guard.
}

/// Introspect any table's columns from the running postgres container.
fn live_table_columns(table: &str) -> BTreeSet<String> {
    let db = format!("{PROJECT}-db-1");
    let out = Command::new("docker")
        .args([
            "exec",
            &db,
            "psql",
            "-U",
            "postgres",
            "-d",
            "app",
            "-t",
            "-A",
            "-c",
            &format!(
                "select column_name from information_schema.columns where table_name='{table}';"
            ),
        ])
        .output()
        .expect("psql introspect");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}
