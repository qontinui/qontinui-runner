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
//!    `|| true`, and the script it runs is the vendored copy naming its source,
//!    its body byte-identical to the hash its header records.
//! 4. Every step a rerouted self-hosted leg can run is either gated to hosted
//!    runners or listed in [`SELF_HOSTED_SAFE`] with the reason it writes
//!    nothing outside the workspace, the runner's own work/tool dirs and
//!    `$RUNNER_TEMP`, and touches no system setting. A new step fails this
//!    test until someone decides which it is (route review H1/L2).

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
const SELF_HOSTED_ONLY: &str = "runner.environment == 'self-hosted'";
const ROUTED: [&str; 2] = ["test", "clippy-windows"];
/// coord's `CI_CAPACITY_ANNOTATION_MARKER` (qontinui-coord
/// `pr_merge/author_feedback.rs`), which coord reads as "no verdict". Both
/// sides pin the literal.
const CI_CAPACITY_TITLE: &str = "title=CI CAPACITY (not a verdict on this PR)::";
/// The isolation refusal's title: a host misconfiguration, so coord's generic
/// no-verdict marker (kept red and re-run, never write-skipped). Pinned in
/// qontinui-coord `main_baseline_no_verdict.rs`
/// (`a_host_misconfiguration_refusal_is_kept_red_and_re_run`).
const HOST_NOT_ISOLATED_TITLE: &str = "title=HOST NOT ISOLATED (not a verdict on this PR)::";
const REFUSE_STEP: &str = "Refuse a host that is not isolated (self-hosted)";
const SANDBOX_STEP: &str = "Sandbox the runner config dirs (self-hosted)";

/// True when `cond` confines the step to hosted runners: the gate alone, or
/// conjoined LAST with no `||` anywhere (route review L4). A substring check
/// would accept `always() || runner.environment == 'github-hosted'`.
fn gated_to_hosted(cond: &str) -> bool {
    let c = cond.trim();
    let c = c
        .strip_prefix("${{")
        .and_then(|x| x.strip_suffix("}}"))
        .map_or(c, str::trim);
    c == HOSTED_ONLY || (c.ends_with(&format!("&& {HOSTED_ONLY}")) && !c.contains("||"))
}

/// Steps that MUST be gated to hosted runners: each writes the machine
/// outside the workspace (paging setup, the home's `.qontinui`, the default
/// toolchain, the cargo registry), boots a second runner on the host, or
/// reads machine-wide counters it would misattribute to the job.
const MUST_BE_HOSTED: &[(&str, &str)] = &[
    ("test", "Setup Rust"),
    (
        "test",
        "Configure pagefile (Windows — guard against link-time OOM)",
    ),
    ("test", "Cache Rust dependencies"),
    ("test", "Poison ambient state (files)"),
    ("test", "Windows memory peak after the Rust test steps"),
    ("test", "Unpoison ambient state (files)"),
    ("test", "SDK↔runner contract smoke (direct-exe)"),
    ("clippy-windows", "Setup Rust"),
    (
        "clippy-windows",
        "Configure pagefile (Windows — guard against link-time OOM)",
    ),
    ("clippy-windows", "Cache Rust dependencies"),
];

/// Steps a rerouted self-hosted leg runs ungated, each with why that is safe
/// on a fleet machine. `(job, step name or uses, reason)`.
const SELF_HOSTED_SAFE: &[(&str, &str, &str)] = &[
    ("test", "CI capacity (route said fail_fast)", "prints the reason and exits"),
    ("test", "actions/checkout@v5", "the workspace only"),
    ("test", "Refuse a host that is not isolated (self-hosted)", "reads the home and 127.0.0.1:9876; writes nothing"),
    ("test", "Check out sibling qontinui-schemas", "the workspace's parent: the runner's own _work dir"),
    ("test", "Check out sibling qontinui-web", "the workspace's parent: the runner's own _work dir"),
    ("test", "Check out sibling ui-bridge", "the workspace's parent: the runner's own _work dir"),
    ("test", "Setup pnpm", "installs pinned pnpm into its own dir, PATH for this job only"),
    ("test", "Setup Node.js", "the runner's own tool cache, PATH for this job only"),
    ("test", "Run CI-tooling unit tests (ci-flake-analyze, ci-test-results-ingest, ci-expiry-phase-summary)", "node tests over workspace files"),
    ("test", "Setup Rust (self-hosted, default untouched)", "adds the pinned toolchain; never runs `rustup default`"),
    ("test", "Cargo.lock is in sync with the pinned qontinui-schemas", "resolve-only; adds to the cargo registry cache"),
    ("test", "Purge cached bundled-PostgreSQL units that do not match the pin", "the workspace target dir"),
    ("test", "Install frontend dependencies", "workspace node_modules; adds to the content-addressed pnpm store"),
    ("test", "Lint frontend code", "reads the workspace"),
    ("test", "Verify component-action effect fixture", "reads the workspace"),
    ("test", "Contract-smoke effect-probe verdict (PS 5.1)", "a pure script over a fixture"),
    ("test", "Typecheck frontend code", "reads the workspace"),
    ("test", "Run frontend unit tests", "workspace tests"),
    ("test", "Build frontend", "the workspace dist dir"),
    ("test", "Format Rust code check", "reads the workspace"),
    ("test", "Poison ambient state (env)", "this job's $GITHUB_ENV only"),
    ("test", "Sandbox the runner config dirs (self-hosted)", "$RUNNER_TEMP and this job's env"),
    ("test", "Build Rust tests", "the workspace target dir"),
    ("test", "Run Rust tests", "config dirs sandboxed under $RUNNER_TEMP by the step after the checkout"),
    ("test", "Explain which Rust test phase the bound expired in", "reads this job's logs"),
    ("test", "Report test results to coord (best-effort)", "posts a summary; writes nothing"),
    ("test", "Unpoison ambient state (env)", "this job's $GITHUB_ENV only"),
    ("test", "Build Tauri app (development)", "the workspace target dir, --no-bundle"),
    ("test", "Contract-smoke summary invariants (PS 5.1)", "a pure script over a fixture"),
    ("clippy-windows", "CI capacity (route said fail_fast)", "prints the reason and exits"),
    ("clippy-windows", "actions/checkout@v5", "the workspace only"),
    ("clippy-windows", "Refuse a host that is not isolated (self-hosted)", "reads the home and 127.0.0.1:9876; writes nothing"),
    ("clippy-windows", "Sandbox the runner config dirs (self-hosted)", "$RUNNER_TEMP and this job's env"),
    ("clippy-windows", "Check out sibling qontinui-schemas", "the workspace's parent: the runner's own _work dir"),
    ("clippy-windows", "Check out sibling qontinui-web", "the workspace's parent: the runner's own _work dir"),
    ("clippy-windows", "Check out sibling ui-bridge", "the workspace's parent: the runner's own _work dir"),
    ("clippy-windows", "Setup pnpm", "installs pinned pnpm into its own dir, PATH for this job only"),
    ("clippy-windows", "Setup Node.js", "the runner's own tool cache, PATH for this job only"),
    ("clippy-windows", "Setup Rust (self-hosted, default untouched)", "adds the pinned toolchain; never runs `rustup default`"),
    ("clippy-windows", "Install frontend dependencies", "workspace node_modules; adds to the content-addressed pnpm store"),
    ("clippy-windows", "Build frontend", "the workspace dist dir"),
    ("clippy-windows", "Clippy check", "the workspace target dir"),
];

fn step_key(step: &serde_yaml::Value) -> String {
    let name = str_of(step, "name");
    if name.is_empty() {
        str_of(step, "uses").to_string()
    } else {
        name.to_string()
    }
}

/// The steps of `job` that a windows (routable) leg runs: in `test`, every
/// step not gated to the ubuntu leg.
fn windows_leg_steps(doc: &serde_yaml::Value, job_name: &str) -> Vec<serde_yaml::Value> {
    steps(job(doc, job_name))
        .into_iter()
        .filter(|s| !str_of(s, "if").contains("matrix.platform == 'ubuntu-22.04'"))
        .collect()
}

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
                gated_to_hosted(cond),
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
        cond.contains("matrix.platform == 'windows-latest'") && gated_to_hosted(cond),
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
        assert_eq!(
            str_of(j, "if"),
            "${{ !cancelled() }}",
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
            run.contains(CI_CAPACITY_TITLE) && run.contains("exit 1"),
            "job `{name}`: the annotation title must be coord's marker `{CI_CAPACITY_TITLE}`"
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

    // The body below the header is byte-identical to the coord source whose
    // `tail -n +2` hash the header records (route review L7).
    let recorded = text
        .lines()
        .find_map(|l| l.strip_prefix("# body-sha256: "))
        .expect("the vendored header records `# body-sha256: <hex>`")
        .trim()
        .to_string();
    let marker = "src-tauri/tests/ci_route_gates_hosted_only_steps.rs recomputes it)\n";
    let (_, body) = text
        .split_once(marker)
        .expect("the header's last line names this test");
    use sha2::{Digest, Sha256};
    let actual: String = Sha256::digest(body.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        actual, recorded,
        "the vendored route script's body was edited here: edit qontinui-coord's copy and \
         re-vendor it (header hash = `tail -n +2 route_ci_job.py | sha256sum`)"
    );
}

#[test]
fn every_step_a_self_hosted_leg_runs_is_gated_or_allowlisted() {
    let doc = ci_workflow();
    let mut used: Vec<(&str, String)> = Vec::new();
    for name in ROUTED {
        for step in windows_leg_steps(&doc, name) {
            let key = step_key(&step);
            let cond = str_of(&step, "if");
            if gated_to_hosted(cond) {
                continue;
            }
            assert!(
                SELF_HOSTED_SAFE
                    .iter()
                    .any(|(j, k, _)| *j == name && *k == key),
                "job `{name}` step `{key}` runs on a rerouted self-hosted leg (`if: {cond}`) \
                 but is neither gated on `{HOSTED_ONLY}` nor listed in SELF_HOSTED_SAFE with \
                 why it leaves the machine alone"
            );
            used.push((name, key));
        }
    }
    for (j, k, reason) in SELF_HOSTED_SAFE {
        assert!(!reason.is_empty());
        assert!(
            used.iter().any(|(uj, uk)| uj == j && uk == k),
            "SELF_HOSTED_SAFE lists `{j}` / `{k}`, which is no longer an ungated windows-leg \
             step: drop the stale entry"
        );
    }
    for (j, k) in MUST_BE_HOSTED {
        let step = windows_leg_steps(&doc, j)
            .into_iter()
            .find(|s| step_key(s) == *k)
            .unwrap_or_else(|| panic!("job `{j}` has no step `{k}`"));
        let cond = str_of(&step, "if");
        assert!(
            gated_to_hosted(cond),
            "job `{j}` step `{k}` writes the machine and must be gated on `{HOSTED_ONLY}`, \
             got `if: {cond}`"
        );
    }
}

#[test]
fn the_gate_check_rejects_a_disjunction_and_a_non_final_conjunct() {
    assert!(gated_to_hosted(HOSTED_ONLY));
    assert!(gated_to_hosted(&format!("${{{{ {HOSTED_ONLY} }}}}")));
    assert!(gated_to_hosted(&format!("always() && {HOSTED_ONLY}")));
    assert!(!gated_to_hosted(&format!("always() || {HOSTED_ONLY}")));
    assert!(!gated_to_hosted(&format!("a || b && {HOSTED_ONLY}")));
    assert!(!gated_to_hosted(&format!("{HOSTED_ONLY} || always()")));
    assert!(!gated_to_hosted(&format!("!({HOSTED_ONLY})")));
    assert!(!gated_to_hosted(""));
}

#[test]
fn the_unpoison_steps_always_run_and_remove_only_what_the_poison_created() {
    let doc = ci_workflow();
    let s = steps(job(&doc, "test"));
    let find = |k: &str| {
        s.iter()
            .find(|x| step_key(x) == k)
            .unwrap_or_else(|| panic!("no step `{k}`"))
    };
    let files = find("Poison ambient state (files)");
    let env = find("Poison ambient state (env)");
    let unfiles = find("Unpoison ambient state (files)");
    let unenv = find("Unpoison ambient state (env)");

    // The env half runs on every leg, poison and unpoison alike (M2).
    assert_eq!(str_of(env, "if"), "", "the env poison runs on every leg");
    assert_eq!(
        str_of(unenv, "if"),
        "always()",
        "the env unpoison runs on every leg, always"
    );
    let e = str_of(env, "run");
    assert!(e.contains("COORD_HTTP_URL=http://poison.invalid") && e.contains("GITHUB_ENV"));
    for var in [
        "COORD_HTTP_URL=",
        "QONTINUI_ROOT=",
        "QONTINUI_SERVER_MODE=",
        "QONTINUI_RUNNER_TIER=",
    ] {
        assert!(str_of(unenv, "run").contains(var), "{var}");
    }

    // The file half is hosted only and removes only what it recorded.
    assert!(str_of(unfiles, "if").contains("always()") && gated_to_hosted(str_of(unfiles, "if")));
    let p = str_of(files, "run");
    assert!(p.contains("poison-created-files") && p.contains("refusing to overwrite"));
    assert!(
        p.contains(r#"[ "$HOME" -ef "$profile_dir" ]"#),
        "homes are compared as directories, not spellings (L3)"
    );
    let u = str_of(unfiles, "run");
    assert!(u.contains("poison-created-files"));
    assert!(
        !u.contains("rm -rf"),
        "unpoison removes recorded files by name only"
    );
}

#[test]
fn a_self_hosted_leg_refuses_a_host_that_is_not_isolated_right_after_checkout() {
    let doc = ci_workflow();
    for name in ROUTED {
        let s = steps(job(&doc, name));
        let pos = |k: &str| {
            s.iter()
                .position(|x| step_key(x) == k)
                .unwrap_or_else(|| panic!("job `{name}` has no step `{k}`"))
        };
        let checkout = pos("actions/checkout@v5");
        let refuse = pos(REFUSE_STEP);
        assert_eq!(
            refuse,
            checkout + 1,
            "job `{name}`: the refusal runs directly after the checkout, before any PR code"
        );
        assert_eq!(pos(SANDBOX_STEP), refuse + 1, "job `{name}`");
        let step = &s[refuse];
        assert_eq!(str_of(step, "if"), SELF_HOSTED_ONLY, "job `{name}`");
        let run = str_of(step, "run");
        for f in ["coord-device-jwt", "machine.json", "profiles.json"] {
            assert!(run.contains(f), "job `{name}`: checks {f}");
        }
        assert!(
            run.contains("http://127.0.0.1:9876/health") && run.contains("--max-time 2"),
            "job `{name}`: checks for a live runner"
        );
        assert!(run.contains(HOST_NOT_ISOLATED_TITLE), "job `{name}`");
        assert!(!run.contains(CI_CAPACITY_TITLE), "job `{name}`: not a capacity red");
        assert!(run.contains("exit 1") && run.contains("USERPROFILE"), "job `{name}`");
    }
}

#[test]
fn a_self_hosted_leg_sandboxes_the_config_dirs_before_the_rust_build() {
    let doc = ci_workflow();
    for (name, build) in [("test", "Build Rust tests"), ("clippy-windows", "Clippy check")] {
        let s = steps(job(&doc, name));
        let pos = |k: &str| {
            s.iter()
                .position(|x| step_key(x) == k)
                .unwrap_or_else(|| panic!("job `{name}` has no step `{k}`"))
        };
        assert!(pos(SANDBOX_STEP) < pos(build), "job `{name}`");
    }
    let s = steps(job(&doc, "test"));
    let pos = |k: &str| {
        s.iter()
            .position(|x| step_key(x) == k)
            .unwrap_or_else(|| panic!("no step `{k}`"))
    };
    let sandbox = &s[pos(SANDBOX_STEP)];
    assert!(str_of(sandbox, "if").contains(SELF_HOSTED_ONLY));
    let run = str_of(sandbox, "run");
    for var in [
        "QONTINUI_CONFIG_DIR",
        "QONTINUI_SECURE_STORAGE_DIR",
        "RUNNER_TEMP",
        "GITHUB_ENV",
    ] {
        assert!(run.contains(var), "{var}");
    }
    assert!(pos("Sandbox the runner config dirs (self-hosted)") < pos("Build Rust tests"));
}
