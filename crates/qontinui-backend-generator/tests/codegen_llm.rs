//! Fork A LLM-handler verification (`#[ignore]`d — needs the `claude` CLI + Docker).
//!
//! Proves the optional LLM body path end-to-end WITHOUT trusting it: it asks the
//! subscription `claude` CLI (prose mode, `claude -p`) to author the `pairConfirm`
//! handler body, splices that body into the generated handler (keeping the deterministic
//! signature + imports), materializes the tree, and runs the GENERATED pytest suite in
//! Docker. The LLM body is accepted ONLY if the coverage-gated suite still passes;
//! otherwise the deterministic body stands. Verify, don't trust.
//!
//! Run with:
//! ```sh
//! cargo test -p qontinui-backend-generator --test codegen_llm -- --ignored
//! ```

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use qontinui_backend_generator::{generate_phase3, llm_author_handler, BreakMode, LlmOutcome};
use qontinui_types::functional_spec::FunctionalSpec;
use qontinui_types::priorities_profile::Profile;

const APP_HOST_PORT: &str = "8013";
const DB_HOST_PORT: &str = "55435";
const PROJECT: &str = "backgen3_llm_itest";

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures_dir() -> PathBuf {
    crate_dir().join("../../../qontinui-schemas/rust/tests/fixtures/functional_spec")
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

fn compose(args: &[&str]) -> std::process::Output {
    Command::new("docker")
        .current_dir(crate_dir().join("run"))
        .env("APP_HOST_PORT", APP_HOST_PORT)
        .env("DB_HOST_PORT", DB_HOST_PORT)
        .args(["compose", "-p", PROJECT, "-f", "docker-compose.yml"])
        .args(args)
        .output()
        .expect("spawn docker compose")
}

fn compose_down() {
    let _ = compose(&["down", "-v"]);
}

/// Re-build a complete handler file with the LLM-authored BODY spliced into the
/// deterministic signature + imports (so unknown LLM imports can't break the module).
fn handler_with_llm_body(body: &str) -> String {
    // Normalize: ensure each statement is indented at least 4 spaces (the CLI usually
    // already does this). Re-indent defensively.
    let reindented: String = body
        .lines()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else if l.starts_with("    ") {
                l.to_string()
            } else {
                format!("    {}", l.trim_start())
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    [
        "\"\"\"FastAPI route for `pairConfirm` (LLM-authored body, verified).\"\"\"",
        "import secrets",
        "",
        "import fastapi",
        "from fastapi import APIRouter, Depends, HTTPException, status",
        "from pydantic import BaseModel",
        "from sqlalchemy.orm import Session",
        "",
        "from app.auth import require_auth",
        "from app.db.session import get_db",
        "from app.models.device import Device",
        "from app.validation import is_https_allowlisted",
        "",
        "router = APIRouter()",
        "",
        "",
        "class DevicePairRequest(BaseModel):",
        "    device_name: str | None = None",
        "    device_id: str | None = None",
        "    state: str | None = None",
        "    callback: str",
        "",
        "",
        "@router.post(\"/api/v1/devices/pair-confirm\", status_code=status.HTTP_201_CREATED)",
        "def pair_confirm(",
        "    payload: DevicePairRequest,",
        "    db: Session = Depends(get_db),",
        "    principal: str = Depends(require_auth),",
        ") -> dict:",
        &reindented,
        "",
    ]
    .join("\n")
}

#[test]
#[ignore = "Fork A: needs the claude CLI + Docker; run with --ignored"]
fn llm_authored_pair_confirm_handler_passes_generated_tests() {
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
    let op = spec
        .operations
        .iter()
        .find(|o| o.name == "pairConfirm")
        .unwrap();

    // 1) Ask the subscription CLI (prose mode) for the handler body.
    let body = match llm_author_handler(&spec, op, "claude") {
        LlmOutcome::Authored(b) => b,
        LlmOutcome::Unavailable(e) => panic!("claude CLI unavailable: {e}"),
    };
    eprintln!("================ LLM-authored handler body ================\n{body}\n==========================================================");
    assert!(
        body.contains("db.add") || body.contains("db.commit"),
        "LLM body must persist"
    );
    assert!(body.contains("token"), "LLM body must return a token");

    // 2) Materialize the deterministic tree, then splice the LLM body into the handler.
    let backend = generate_phase3(&spec, &profile, BreakMode::None);
    let out = crate_dir().join("run/generated");
    let _ = std::fs::remove_dir_all(&out);
    backend.materialize_to(&out).unwrap();
    std::fs::write(
        out.join("app/api/pair_confirm.py"),
        handler_with_llm_body(&body),
    )
    .unwrap();

    // 3) Bring it up + run the GENERATED suite in Docker.
    up_and_wait_or_unhealthy();
    let (llm_passed, report) = run_suite();
    eprintln!("======== generated pytest --cov against LLM body (Docker) ========\n{report}\n==================================================================");

    if llm_passed {
        eprintln!(
            "[fork A] LLM-authored handler PASSED the generated coverage-gated suite — accepted."
        );
        return;
    }

    // 4) VERIFY-DON'T-TRUST: the LLM body failed (non-deterministic output is expected
    //    to sometimes hallucinate). The loop's contract is to FALL BACK to the verified
    //    deterministic body — and that MUST pass. Prove the fallback works.
    eprintln!(
        "[fork A] LLM-authored handler FAILED verification → falling back to deterministic body."
    );
    compose_down();
    let det = generate_phase3(&spec, &profile, BreakMode::None);
    let _ = std::fs::remove_dir_all(&out);
    det.materialize_to(&out).unwrap();
    up_and_wait_or_unhealthy();
    let (det_passed, det_report) = run_suite();
    eprintln!("======== generated pytest --cov against DETERMINISTIC body (Docker) ========\n{det_report}\n============================================================================");
    assert!(
        det_passed,
        "deterministic fallback handler MUST pass the generated coverage-gated suite"
    );
}

fn run_suite() -> (bool, String) {
    let container = format!("{PROJECT}-app-1");
    let res = Command::new("docker")
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
        .unwrap();
    let report = format!(
        "{}\n{}",
        String::from_utf8_lossy(&res.stdout),
        String::from_utf8_lossy(&res.stderr)
    );
    (res.status.success(), report)
}

/// Bring the stack up; return once the app is healthy OR (for the LLM run) report the
/// unhealthy state instead of panicking, so the fallback path can run.
fn up_and_wait_or_unhealthy() {
    let up = compose(&["up", "--build", "-d"]);
    assert!(
        up.status.success(),
        "compose up:\n{}",
        String::from_utf8_lossy(&up.stderr)
    );
    let container = format!("{PROJECT}-app-1");
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let out = Command::new("docker")
            .args(["inspect", "-f", "{{.State.Health.Status}}", &container])
            .output()
            .unwrap();
        let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if status == "healthy" || status == "unhealthy" {
            if status == "unhealthy" {
                let logs = Command::new("docker")
                    .args(["logs", &container])
                    .output()
                    .unwrap();
                eprintln!(
                    "[app unhealthy — likely an invalid LLM body; logs]\n{}",
                    String::from_utf8_lossy(&logs.stderr)
                );
            }
            return;
        }
        if Instant::now() > deadline {
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}
