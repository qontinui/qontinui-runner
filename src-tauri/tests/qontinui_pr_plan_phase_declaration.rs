//! `qontinui-pr plan-phase-declaration <plan.md>` — the compiled binary's
//! output contract (plan
//! `2026-11-17-a-session-minted-plan-work-unit-carries-no-phase-declaration`,
//! Phase 1). A consumer parses this, so the test pins it through the real
//! dispatch, not just the handler:
//!
//! - exit 0: stdout is exactly one line, a compact JSON object with exactly the
//!   keys `phases`, `phase_labels`, `phases_undeclared`, equal to the runner's
//!   own `metadata::phase_declaration` for the same file; stderr empty.
//! - exit 3: the plan declares no phase — stdout empty, stderr `NO_PHASES`.
//! - exit 2: missing / extra argument, or an unreadable / non-UTF-8 file.

use std::path::Path;
use std::process::{Command, Output};

use qontinui_runner_lib::plan_workunit_adapter::{
    metadata::phase_declaration, parse_work_unit, slug_from_filename, PlanConvention,
};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_qontinui-pr"))
        .arg("plan-phase-declaration")
        .args(args)
        // A subject runner's root would be validated before dispatch.
        .env_remove("QONTINUI_INSTANCE_ROOT")
        .output()
        .expect("spawn qontinui-pr")
}

fn write_plan(dir: &Path, name: &str, body: &[u8]) -> String {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write plan");
    path.to_string_lossy().into_owned()
}

const MIXED_PLAN: &str = "# T\n\n> **Status: IN PROGRESS 2026-11-11.**\n\n### Phase 0 (read-only): re-verify\n\n### Phase A: one\n\n### Phase B: two\n\n### Phase Two — the second\n";

#[test]
fn plan_phase_declaration_prints_the_runners_own_declaration_as_one_json_line() {
    let dir = tempfile::tempdir().unwrap();
    let cases: [(&str, &str); 2] = [
        ("2026-11-17-mixed.md", MIXED_PLAN),
        (
            "2026-09-04-coord-jetstream-durable-consumers-redis-subscriber-migration.md",
            include_str!(
                "../src/plan_workunit_adapter/fixtures/2026-09-04-coord-jetstream-durable-consumers-redis-subscriber-migration.md"
            ),
        ),
    ];
    for (name, body) in cases {
        let path = write_plan(dir.path(), name, body.as_bytes());
        let out = run(&[&path]);
        assert_eq!(out.status.code(), Some(0), "{name}: {out:?}");
        assert!(out.stderr.is_empty(), "{name}: {out:?}");
        let stdout = String::from_utf8(out.stdout).unwrap();
        let line = stdout
            .strip_suffix('\n')
            .expect("one newline-terminated line");
        assert!(
            !line.contains('\n'),
            "{name}: more than one line: {stdout:?}"
        );
        let got: serde_json::Value = serde_json::from_str(line).unwrap();
        let mut keys: Vec<&str> = got
            .as_object()
            .expect("a JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["phase_labels", "phases", "phases_undeclared"]);
        let want = phase_declaration(&parse_work_unit(
            &slug_from_filename(&path),
            &path,
            body,
            &PlanConvention::operator_default(),
        ))
        .expect("the case declares phases");
        // Byte-identical to the compact serialization of what the runner writes.
        assert_eq!(line, want.to_string(), "{name}");
    }

    // The mixed plan's values, spelled out.
    let path = write_plan(dir.path(), "2026-11-17-mixed.md", MIXED_PLAN.as_bytes());
    let got: serde_json::Value =
        serde_json::from_slice(&run(&[&path]).stdout).expect("JSON stdout");
    assert_eq!(
        got["phases"],
        serde_json::json!([{"index": 0, "name": "Phase 0 (read-only): re-verify"}])
    );
    assert_eq!(
        got["phase_labels"],
        serde_json::json!([
            {"label": "A", "name": "Phase A: one"},
            {"label": "B", "name": "Phase B: two"},
        ])
    );
    assert_eq!(got["phases_undeclared"], serde_json::json!(["Two"]));
}

#[test]
fn plan_phase_declaration_no_phase_exits_3_with_no_phases_and_empty_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_plan(
        dir.path(),
        "2026-11-17-none.md",
        b"# T\n\n> **Status: DRAFT 2026-11-17.**\n\nNo phases here.\n",
    );
    let out = run(&[&path]);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "NO_PHASES");
}

#[test]
fn plan_phase_declaration_usage_errors_and_unreadable_files_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    let good = write_plan(dir.path(), "p.md", MIXED_PLAN.as_bytes());
    let bad_utf8 = write_plan(dir.path(), "bin.md", b"# T\n\n## Phase 1 \xff\xfe\n");
    let missing = dir.path().join("absent.md").to_string_lossy().into_owned();
    let cases: [Vec<&str>; 5] = [
        vec![],
        vec![&good, &good],
        vec!["--json"],
        vec![&missing],
        vec![&bad_utf8],
    ];
    for args in cases {
        let out = run(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{args:?}: {out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("plan-phase-declaration <plan.md>"),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn plan_phase_declaration_is_listed_in_help() {
    let out = Command::new(env!("CARGO_BIN_EXE_qontinui-pr"))
        .arg("--help")
        .env_remove("QONTINUI_INSTANCE_ROOT")
        .output()
        .expect("spawn qontinui-pr");
    assert_eq!(out.status.code(), Some(0));
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("qontinui-pr plan-phase-declaration <plan.md>"),
        "{help}"
    );
    assert!(help.contains("NO_PHASES"), "{help}");
}
