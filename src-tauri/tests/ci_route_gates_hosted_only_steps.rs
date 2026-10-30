//! The heavy Windows jobs are routed by coord (plan
//! 2026-10-27-coord-routes-ci-jobs-by-runner-capability, Phase 7): the `route`
//! job in `.github/workflows/ci.yml` asks `GET /coord/ci/route` where
//! `test (windows-latest)` and `Clippy (windows)` should run, and either may
//! land on a self-hosted Windows host. These tests pin the three properties
//! that make that safe:
//!
//! 1. A rerouted self-hosted leg runs NO `Configure pagefile` step (and no
//!    direct-exe contract smoke): both are gated on `runner.environment ==
//!    'github-hosted'`, never on the route `lane`, which is empty when the
//!    route job did not run.
//! 2. A failed or skipped route job cannot SKIP a required check: both
//!    dependants run on `!cancelled()` and fall back to the static
//!    `windows-latest` for an empty output.
//! 3. The route job cannot fail: every route step is `continue-on-error` and
//!    `|| true`, and the script it runs is the vendored copy naming its source.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri always has a parent")
        .to_path_buf()
}

fn ci_workflow() -> serde_yaml::Value {
    let path = repo_root().join(".github").join("workflows").join("ci.yml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_yaml::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn job<'a>(doc: &'a serde_yaml::Value, name: &str) -> &'a serde_yaml::Value {
    doc.get("jobs")
        .and_then(|j| j.get(name))
        .unwrap_or_else(|| panic!("ci.yml has no job `{name}`"))
}

fn steps(job: &serde_yaml::Value) -> Vec<serde_yaml::Value> {
    job.get("steps")
        .and_then(|s| s.as_sequence())
        .cloned()
        .unwrap_or_default()
}

fn str_of<'a>(v: &'a serde_yaml::Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("")
}

const HOSTED_ONLY: &str = "runner.environment == 'github-hosted'";
const ROUTED: [&str; 2] = ["test", "clippy-windows"];

#[test]
fn every_pagefile_step_runs_only_on_a_hosted_runner() {
    let doc = ci_workflow();
    let jobs = doc
        .get("jobs")
        .and_then(|j| j.as_mapping())
        .expect("ci.yml has a jobs mapping");
    let mut seen = 0;
    for (name, job) in jobs {
        for step in steps(job) {
            let is_pagefile = str_of(&step, "uses")
                .starts_with("al-cheb/configure-pagefile-action")
                || str_of(&step, "name").starts_with("Configure pagefile");
            if !is_pagefile {
                continue;
            }
            seen += 1;
            let cond = str_of(&step, "if");
            assert!(
                cond.contains(HOSTED_ONLY),
                "job `{}`: the pagefile step must be gated on `{HOSTED_ONLY}` (a routed \
                 self-hosted host must not have its paging rewritten), got `if: {cond}`",
                name.as_str().unwrap_or("?")
            );
            assert!(
                !cond.contains("needs.route"),
                "job `{}`: gate the pagefile on the runner, never on the route output, which \
                 is empty when the route job did not run",
                name.as_str().unwrap_or("?")
            );
        }
    }
    assert!(
        seen >= 2,
        "expected the test and clippy-windows pagefile steps, saw {seen}"
    );
}

#[test]
fn the_direct_exe_contract_smoke_runs_only_on_a_hosted_runner() {
    let doc = ci_workflow();
    let step = steps(job(&doc, "test"))
        .into_iter()
        .find(|s| str_of(s, "name") == "SDK↔runner contract smoke (direct-exe)")
        .expect("the direct-exe contract smoke step exists");
    let cond = str_of(&step, "if");
    assert!(
        cond.contains("matrix.platform == 'windows-latest'") && cond.contains(HOSTED_ONLY),
        "the direct-exe smoke boots a second runner on the host; got `if: {cond}`"
    );
}

#[test]
fn a_routed_job_cannot_be_skipped_by_its_route_job() {
    let doc = ci_workflow();
    for name in ROUTED {
        let j = job(&doc, name);
        let needs = j.get("needs").expect("a routed job needs the route job");
        let needs_route = needs.as_str() == Some("route")
            || needs
                .as_sequence()
                .is_some_and(|s| s.iter().any(|n| n.as_str() == Some("route")));
        assert!(needs_route, "job `{name}` must `needs: route`");
        assert!(
            str_of(j, "if").contains("!cancelled()"),
            "job `{name}` must run on `!cancelled()`: a failed `needs:` job otherwise SKIPS \
             it, and a skipped required check reads as a pass"
        );
        let runs_on = str_of(j, "runs-on");
        assert!(
            runs_on.contains("fromJSON(needs.route.outputs.")
                && runs_on.contains("|| '\"windows-latest\"'"),
            "job `{name}`'s runs-on must read the route output with the static \
             windows-latest default for an empty one, got `{runs_on}`"
        );
    }
}

#[test]
fn a_fail_fast_answer_fails_the_job_first() {
    let doc = ci_workflow();
    for name in ROUTED {
        let first = steps(job(&doc, name))
            .into_iter()
            .next()
            .expect("the job has steps");
        assert_eq!(
            str_of(&first, "name"),
            "CI capacity (route said fail_fast)",
            "job `{name}`"
        );
        assert!(
            str_of(&first, "if").contains("_lane == 'fail_fast'"),
            "job `{name}`"
        );
        let run = str_of(&first, "run");
        assert!(
            run.contains("ci-capacity") && run.contains("exit 1"),
            "job `{name}`"
        );
        assert!(
            !run.contains("${{"),
            "job `{name}`: the reason is untrusted text and travels through env, never \
             interpolated into the script"
        );
    }
}

#[test]
fn the_route_job_cannot_fail() {
    let doc = ci_workflow();
    let route = job(&doc, "route");
    let route_steps: Vec<_> = steps(route)
        .into_iter()
        .filter(|s| str_of(s, "run").contains("route_ci_job.py"))
        .collect();
    assert_eq!(route_steps.len(), 2, "one route step per routed job");
    for s in &route_steps {
        assert_eq!(
            s.get("continue-on-error").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert!(str_of(s, "run").trim_end().ends_with("|| true"));
        assert_eq!(str_of(s, "shell"), "bash");
    }
    let script = repo_root()
        .join(".github")
        .join("scripts")
        .join("route_ci_job.py");
    let text = std::fs::read_to_string(&script)
        .unwrap_or_else(|e| panic!("read {}: {e}", script.display()));
    assert!(
        text.contains(
            "VENDORED COPY -- the source is qontinui-coord/.github/scripts/route_ci_job.py"
        ),
        "the vendored route script must name its source"
    );
    assert!(text.contains("THIS JOB MUST NOT FAIL"));
}
