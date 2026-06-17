//! Runtime-verification integration tier (Fork B) — **REAL, but `#[ignore]`d for CI**.
//!
//! This is the test that proves the plan's load-bearing premise: a generated backend is
//! brought up for real (docker-compose: throwaway postgres + uvicorn), its live pairing
//! endpoint is hit over HTTP, the live DB schema is introspected, and a
//! [`CoverageEvidence`] is assembled **only from those running-server observations** —
//! never from the generator's own claims. It then asserts (via the real
//! [`qontinui_types::completeness_eval::evaluate_completeness`]) that the `entities.Device`
//! refs and `operations.pairConfirm` are covered and `operations.pairConfirm.effect` is
//! filled, and that a deliberately-broken variant (a dropped column) surfaces the matching
//! gap (`qontinui_types::completeness_verdict::CoverageGap`).
//!
//! It is kept `#[ignore]`d because CI has **no Docker daemon**; it runs only when invoked
//! explicitly:
//!
//! ```sh
//! cargo test -p qontinui-backend-generator -- --ignored runtime_pair_confirm_observed_from_live_server
//! ```
//!
//! Host port note: the dev host already binds 8000, so this test publishes the app on
//! **8011** and postgres on **55433** (distinct from the manual `run/` defaults so the
//! two never collide).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use qontinui_backend_generator::{generate_runnable, BreakMode};
use qontinui_types::completeness_eval::{evaluate_completeness, CoverageEvidence, GapEvidence};
use qontinui_types::completeness_verdict::GapReason;
use qontinui_types::functional_spec::FunctionalSpec;
use qontinui_types::priorities_profile::Profile;

const APP_HOST_PORT: &str = "8011";
const DB_HOST_PORT: &str = "55433";
const PROJECT: &str = "backgen2_itest";

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

/// Run a `docker compose` subcommand in the crate's `run/` harness with our ports.
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

/// Materialize a generated backend into `run/generated`, replacing any prior tree.
fn materialize(break_mode: BreakMode) {
    let spec = load_spec();
    let profile = load_profile();
    let backend = generate_runnable(&spec, &profile, break_mode);
    let out = crate_dir().join("run/generated");
    let _ = std::fs::remove_dir_all(&out);
    backend
        .materialize_to(&out)
        .expect("materialize generated backend");
}

/// Bring the stack up (rebuilding the image from the freshly materialized tree) and wait
/// until the app container reports healthy. Panics (after dumping logs) on timeout.
fn up_and_wait_healthy() {
    let up = compose(&["up", "--build", "-d"]);
    assert!(
        up.status.success(),
        "docker compose up failed:\n{}\n{}",
        String::from_utf8_lossy(&up.stdout),
        String::from_utf8_lossy(&up.stderr)
    );

    let container = format!("{PROJECT}-app-1");
    let deadline = Instant::now() + Duration::from_secs(120);
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
                "app never became healthy (last status={status:?}); logs:\n{}",
                String::from_utf8_lossy(&logs.stderr)
            );
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// POST a valid pairing to the live endpoint. Returns `(http_status, body)`.
fn post_pairing() -> (u16, String) {
    // Use curl (present on the dev host + CI images) to avoid adding an HTTP dep.
    let url = format!("http://localhost:{APP_HOST_PORT}/api/v1/devices/pair-confirm");
    // stdout = body, then a sentinel line `<<<HTTP>>>NNN` with the status code.
    let out = Command::new("curl")
        .args([
            "-s",
            "-w",
            "\n<<<HTTP>>>%{http_code}",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "-d",
            r#"{"device_name":"RFCW6041HHN","device_id":"dev-abc-123","state":"csrf-xyz","callback":"https://app.qontinui.io/paired"}"#,
            &url,
        ])
        .output()
        .expect("curl POST");
    let raw = String::from_utf8_lossy(&out.stdout).to_string();
    let (body, code) = match raw.rsplit_once("<<<HTTP>>>") {
        Some((b, c)) => (
            b.trim_end_matches('\n').to_string(),
            c.trim().parse().unwrap_or(0),
        ),
        None => (raw, 0u16),
    };
    (code, body)
}

/// Introspect the live `devices` table's columns from the running postgres container.
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

/// Assemble a [`CoverageEvidence`] from LIVE observations of a healthy backend.
fn evidence_from_live(
    http_status: u16,
    body: &str,
    live_columns: &BTreeSet<String>,
) -> CoverageEvidence {
    let mut covered: BTreeSet<String> = BTreeSet::new();
    let mut gaps: BTreeMap<String, GapEvidence> = BTreeMap::new();
    let mut filled_assumed: BTreeSet<String> = BTreeSet::new();

    // entities.Device — covered iff the table exists (it does iff it has any columns).
    if !live_columns.is_empty() {
        covered.insert("entities.Device".to_string());
    } else {
        gaps.insert(
            "entities.Device".to_string(),
            GapEvidence {
                reason: GapReason::NotGenerated,
                detail: Some("devices table absent in live schema".to_string()),
            },
        );
    }

    // Each Device field ref — covered iff its snake_case column is in the LIVE schema.
    let field_to_col = [
        ("deviceName", "device_name"),
        ("deviceId", "device_id"),
        ("state", "state"),
        ("callback", "callback"),
    ];
    for (field, col) in field_to_col {
        let rf = format!("entities.Device.fields.{field}");
        if live_columns.contains(col) {
            covered.insert(rf);
        } else {
            gaps.insert(
                rf,
                GapEvidence {
                    reason: GapReason::NotGenerated,
                    detail: Some(format!("column `{col}` absent in live schema")),
                },
            );
        }
    }

    // operations.pairConfirm — covered iff the endpoint really created (201).
    let created = http_status == 201;
    if created {
        covered.insert("operations.pairConfirm".to_string());
    } else {
        gaps.insert(
            "operations.pairConfirm".to_string(),
            GapEvidence {
                reason: GapReason::BehaviorMismatch,
                detail: Some(format!("pair-confirm returned {http_status}, expected 201")),
            },
        );
    }

    // operations.pairConfirm.inputs.callback.validation (Observed) — covered iff the
    // endpoint accepted a valid (https-allowlisted) callback, i.e. the happy path worked.
    let validation_ref = "operations.pairConfirm.inputs.callback.validation".to_string();
    if created {
        covered.insert(validation_ref);
    } else {
        gaps.insert(
            validation_ref,
            GapEvidence {
                reason: GapReason::Unverifiable,
                detail: Some("endpoint did not accept a valid pairing".to_string()),
            },
        );
    }

    // operations.pairConfirm.effect (Assumed) — FILLED iff a token-shaped body came back.
    let token_shaped = body.contains("deviceToken")
        && body
            .split("\"deviceToken\"")
            .nth(1)
            .map(|rest| rest.trim_start_matches([':', ' ', '"']).len() > 3)
            .unwrap_or(false);
    if created && token_shaped {
        filled_assumed.insert("operations.pairConfirm.effect".to_string());
    }

    // auth (Inferred) — the spec records a session model; the route is mounted under the
    // app (observed by it serving). v0 treats a live, mounted route as covering `auth`.
    if created {
        covered.insert("auth".to_string());
    } else {
        gaps.insert(
            "auth".to_string(),
            GapEvidence {
                reason: GapReason::Unverifiable,
                detail: Some("route not observed serving".to_string()),
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
fn runtime_pair_confirm_observed_from_live_server() {
    // Guarantee teardown even on a panic mid-test.
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            compose_down();
        }
    }
    // Clean any prior run first, then arm the guard for this run.
    compose_down();
    let _guard = Guard;

    let spec = load_spec();

    // ---------- HEALTHY backend: observe coverage from the live server ----------
    materialize(BreakMode::None);
    up_and_wait_healthy();

    let (status, body) = post_pairing();
    eprintln!("[live POST] status={status} body={body}");
    assert_eq!(
        status, 201,
        "live pair-confirm must return 201; body={body}"
    );
    assert!(
        body.contains("deviceToken"),
        "live response must be token-shaped; body={body}"
    );

    let columns = live_device_columns();
    eprintln!("[live schema] devices columns = {columns:?}");
    for col in ["device_name", "device_id", "state", "callback"] {
        assert!(
            columns.contains(col),
            "live devices table must have column `{col}`; got {columns:?}"
        );
    }

    let evidence = evidence_from_live(status, &body, &columns);
    let verdict = evaluate_completeness(&spec, &evidence, "2026-06-17T00:00:00Z");

    // The covered set, drawn ONLY from live observations.
    let covered = &evidence.covered;
    assert!(covered.contains("entities.Device"), "Device entity covered");
    for field in ["deviceName", "deviceId", "state", "callback"] {
        assert!(
            covered.contains(&format!("entities.Device.fields.{field}")),
            "field {field} covered from live schema"
        );
    }
    assert!(
        covered.contains("operations.pairConfirm"),
        "pairConfirm covered from live 201"
    );
    assert!(
        evidence
            .filled_assumed
            .contains("operations.pairConfirm.effect"),
        "pairConfirm.effect filled from live token body"
    );

    // No gap for any Device/pairConfirm ref in the healthy verdict.
    let bad_gaps: Vec<_> = verdict
        .gaps
        .iter()
        .filter(|g| {
            g.r#ref.starts_with("entities.Device") || g.r#ref.starts_with("operations.pairConfirm")
        })
        .map(|g| g.r#ref.clone())
        .collect();
    assert!(
        bad_gaps.is_empty(),
        "healthy backend must have no Device/pairConfirm gaps; got {bad_gaps:?}"
    );
    eprintln!(
        "[verdict healthy] coverage={:.3} assumed_fill_rate={:.3} gaps={}",
        verdict.coverage,
        verdict.assumed_fill_rate,
        verdict.gaps.len()
    );

    compose_down();

    // ---------- BROKEN backend (dropped `callback` column): a live gap ----------
    materialize(BreakMode::DropColumn {
        entity: "Device".to_string(),
        column: "callback".to_string(),
    });
    up_and_wait_healthy();

    let (bstatus, bbody) = post_pairing();
    eprintln!("[live POST broken] status={bstatus} body={bbody}");
    let bcolumns = live_device_columns();
    eprintln!("[live schema broken] devices columns = {bcolumns:?}");
    assert!(
        !bcolumns.contains("callback"),
        "broken variant must drop the live `callback` column; got {bcolumns:?}"
    );

    let bevidence = evidence_from_live(bstatus, &bbody, &bcolumns);
    let bverdict = evaluate_completeness(&spec, &bevidence, "2026-06-17T00:00:00Z");

    let callback_gap = bverdict
        .gaps
        .iter()
        .find(|g| g.r#ref == "entities.Device.fields.callback");
    assert!(
        callback_gap.is_some(),
        "dropped live column must surface entities.Device.fields.callback as a gap; gaps={:?}",
        bverdict.gaps.iter().map(|g| &g.r#ref).collect::<Vec<_>>()
    );
    eprintln!(
        "[verdict broken] callback gap reason = {:?}",
        callback_gap.map(|g| &g.reason)
    );

    // Teardown via the guard's Drop.
}
