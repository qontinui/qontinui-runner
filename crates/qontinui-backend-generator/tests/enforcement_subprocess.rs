//! Fork D — the enforcement loop's **REAL** security gate, `#[ignore]`d for CI.
//!
//! These tests run an ACTUAL security scanner subprocess (`bandit`, the static Python
//! analyzer the claude-config `security-scan` skill invokes: `poetry run bandit -r .`)
//! over a *materialized* generated backend. They are `#[ignore]`d because CI hosts have
//! no scanner installed; run them on a dev host that has one:
//!
//! ```sh
//! python -m pip install bandit            # if not present
//! cargo test -p qontinui-backend-generator --test enforcement_subprocess -- --ignored --nocapture
//! ```
//!
//! Two cases, proven against the REAL scan (output printed with `--nocapture`):
//!
//! 1. A backend whose generated handler does raw SQL string-concat of user input AND
//!    embeds a hardcoded secret → the real scan FLAGS it (B608 + B105), the gate BLOCKS,
//!    and an unresolved blocker surfaces as a re-dispatch `CoverageGap`.
//! 2. The clean, deterministically-generated Phase-3 backend → the real scan finds no
//!    blocking `security` finding and the gate PASSES.

use std::path::PathBuf;
use std::process::Command;

use qontinui_backend_generator::{
    generate_phase3, unresolved_blocker_gap, BreakMode, EnforcementGate, GeneratedBackend,
    SubprocessSecurityGate,
};
use qontinui_types::completeness_verdict::SpecSection;
use qontinui_types::functional_spec::FunctionalSpec;
use qontinui_types::priorities_profile::{EnforcementProfile, Profile};

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

/// The `block_on: [security]` enforcement config (matches the connect-runner profile).
fn security_blocks() -> EnforcementProfile {
    EnforcementProfile {
        run: vec!["security-scan".to_string()],
        block_on: vec!["security".to_string()],
    }
}

/// Skip (with a printed note) if bandit is not installed, so the test is a no-op rather
/// than a false failure on a host without the scanner.
fn bandit_present() -> bool {
    let direct = Command::new("bandit").arg("--version").output();
    if direct.map(|o| o.status.success()).unwrap_or(false) {
        return true;
    }
    Command::new("python")
        .args(["-m", "bandit", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The real subprocess security gate, scanning with the `bandit` console script
/// (installed alongside the module by `pip install bandit`).
fn security_gate() -> SubprocessSecurityGate {
    SubprocessSecurityGate::default()
}

#[test]
#[ignore = "runs the real bandit subprocess; CI has no scanner"]
fn vulnerable_generated_backend_is_blocked_by_real_scan() {
    if !bandit_present() {
        eprintln!("SKIP: bandit not installed (pip install bandit); not a failure.");
        return;
    }
    let spec = load_spec();
    let profile = load_profile();

    // Start from the real generated Phase-3 backend, then inject a deliberately-vulnerable
    // handler: raw SQL string-concat of user input (SQL injection) + a hardcoded secret.
    // This is exactly the kind of body an LLM codegen pass could emit, so blocking it is
    // the loop doing its job.
    let mut backend: GeneratedBackend = generate_phase3(&spec, &profile, BreakMode::None);
    let vulnerable = r#""""DELIBERATELY VULNERABLE handler (test fixture for the security gate)."""
import sqlite3

from fastapi import APIRouter

router = APIRouter()

API_SECRET = "sk_live_super_secret_hardcoded_key_123456"  # hardcoded secret


@router.get("/api/v1/devices/lookup")
def lookup_device(device_id: str) -> dict:
    conn = sqlite3.connect("app.db")
    cur = conn.cursor()
    # SQL injection: user input concatenated straight into the query string.
    query = "SELECT * FROM devices WHERE device_id = '" + device_id + "'"
    cur.execute(query)
    return {"rows": cur.fetchall(), "secret": API_SECRET}
"#;
    backend.files.insert(
        "app/api/lookup_device.py".to_string(),
        vulnerable.to_string(),
    );

    let gate = security_gate();
    let findings = gate.scan(&backend);

    println!("\n========== REAL SCAN OUTPUT (VULNERABLE backend) ==========");
    println!("scanner: {} (gate `{}`)", gate.scanner_bin, gate.name());
    println!("findings: {}", findings.len());
    for f in &findings {
        println!("  - {} :: {}", f.file, f.detail);
    }
    println!("===========================================================\n");

    // The real scan must flag the injected vulnerabilities.
    assert!(
        !findings.is_empty(),
        "real scan must produce findings on the vulnerable backend"
    );
    assert!(
        findings.iter().any(|f| f.detail.contains("B608")),
        "real scan must flag SQL injection (B608); got: {findings:#?}"
    );
    assert!(
        findings
            .iter()
            .any(|f| f.detail.contains("B105") || f.detail.contains("B106")),
        "real scan must flag the hardcoded secret (B105/B106); got: {findings:#?}"
    );
    assert!(
        findings
            .iter()
            .any(|f| f.file == "app/api/lookup_device.py"),
        "finding must point at the injected handler; got: {findings:#?}"
    );

    // Run the full enforcement: a `security` finding under `block_on:[security]` blocks,
    // and the unresolved blocker surfaces as a re-dispatch CoverageGap (never a silent pass).
    let report = qontinui_backend_generator::run_enforcement_with(
        &security_blocks(),
        &backend,
        &[Box::new(security_gate()) as Box<dyn EnforcementGate>],
    );
    assert!(!report.passed(), "gate must BLOCK the vulnerable backend");
    assert!(
        !report.blocking.is_empty(),
        "blocking findings must be recorded"
    );

    let gap = unresolved_blocker_gap(&report, "operations.pairConfirm")
        .expect("an unresolved blocker must surface a re-dispatch CoverageGap");
    assert_eq!(gap.section, SpecSection::Operations);
    println!(
        "re-dispatch CoverageGap emitted: ref={} reason={:?}",
        gap.r#ref, gap.reason
    );
    println!(
        "blocking feedback fed to next codegen prompt:\n{}",
        report.blocking_feedback()
    );
}

#[test]
#[ignore = "runs the real bandit subprocess; CI has no scanner"]
fn clean_generated_backend_passes_real_scan() {
    if !bandit_present() {
        eprintln!("SKIP: bandit not installed (pip install bandit); not a failure.");
        return;
    }
    let spec = load_spec();
    let profile = load_profile();

    // The clean, deterministically-generated Phase-3 backend (no injected vuln).
    let backend = generate_phase3(&spec, &profile, BreakMode::None);

    let gate = security_gate();
    let findings = gate.scan(&backend);

    println!("\n========== REAL SCAN OUTPUT (CLEAN backend) ==========");
    println!("scanner: {} (gate `{}`)", gate.scanner_bin, gate.name());
    println!("findings: {}", findings.len());
    for f in &findings {
        println!("  - {} :: {}", f.file, f.detail);
    }
    println!("======================================================\n");

    let report = qontinui_backend_generator::run_enforcement_with(
        &security_blocks(),
        &backend,
        &[Box::new(security_gate()) as Box<dyn EnforcementGate>],
    );
    assert!(
        report.passed(),
        "clean generated backend must PASS the real scan; blocking findings: {:#?}",
        report.blocking
    );
    assert!(
        unresolved_blocker_gap(&report, "operations.pairConfirm").is_none(),
        "a passing report yields no re-dispatch gap"
    );
}
