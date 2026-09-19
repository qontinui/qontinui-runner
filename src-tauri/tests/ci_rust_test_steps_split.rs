//! Regression gate: **the Rust test job builds and runs under SEPARATE,
//! separately-bounded steps, and the RUN step owns the ingest's log.**
//!
//! Plan `2026-09-17-the-windows-test-gate-is-a-90-minute-build-wearing-a-test-shaped-bound`,
//! Phases 1 and 3.
//!
//! # What went wrong, and why a shape test is the right gate
//!
//! `ci.yml`'s job `test` used to run one `cargo test --verbose` under one
//! `timeout-minutes: 90`. `cargo test` compiles *and* executes, so that single
//! bound covered (dependency compile + workspace codegen + test-binary link +
//! test execution) — while the step's own documenting comment reasoned almost
//! entirely about **test** durations and test **hangs**. Measured on the
//! windows leg 2026-09-17: the suite reports `finished in 117.72s` and the step
//! median is ~50 min, so roughly 92% of what the bound bounded was `rustc`.
//!
//! On run `35043646051` attempt 1 that produced the failure this test exists to
//! stop recurring: the step expired at exactly 90 minutes **two minutes after**
//! the suite had reported `test result: ok. 1354 passed; 0 failed`, so a
//! branch-protection-required check went red on a PR whose tests had passed. A
//! bound sized against the wrong quantity cannot be tuned correctly and cannot
//! attribute its own expiry.
//!
//! The remedy is structural rather than numeric, which is exactly the kind of
//! property a workflow-shape test can hold and a comment cannot: two steps, two
//! bounds, the run bound strictly smaller. Merge them back and the attribution
//! is gone again with nothing to notice.
//!
//! # The log-ownership assertion is not decoration
//!
//! `scripts/ci-test-results-ingest.mjs` parses `cargo-test-output.log`, and its
//! recognition gate (`hasCargoTestOutput` in `scripts/ci-flake-analyze.mjs`)
//! keys ONLY on run-phase lines — `test result:`, `running N tests`,
//! `test … ... ok|FAILED|ignored`. A `--no-run` log carries none of them. So if
//! the BUILD step were ever the one writing that filename, every ingest would
//! parse as `unparsed` and coord's flakiness history for this repo would
//! silently stop being written. The run step owning that name is a contract.
//!
//! # Scope
//!
//! This asserts the SHAPE of job `test`, never the numbers themselves beyond
//! their ordering. Re-sizing either bound against a re-measured window is the
//! plan's Phase 5 and is deliberately left free — what is pinned is that both
//! bounds EXIST and that the run bound stays the smaller of the two.
//!
//! Sibling precedent, and the file to copy when adding another of these:
//! `src-tauri/tests/workflow_paths_self_inclusion.rs`.

use std::path::PathBuf;

/// Repo root: `CARGO_MANIFEST_DIR` is `src-tauri/`, so its parent is the
/// checkout root that holds `.github/`.
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

/// The `steps:` sequence of one job.
fn job_steps(doc: &serde_yaml::Value, job: &str) -> Vec<serde_yaml::Value> {
    doc.get("jobs")
        .and_then(|j| j.get(job))
        .and_then(|j| j.get("steps"))
        .and_then(|s| s.as_sequence())
        .unwrap_or_else(|| panic!("job `{job}` has no `steps:` sequence in ci.yml"))
        .clone()
}

fn step_name(step: &serde_yaml::Value) -> Option<&str> {
    step.get("name").and_then(|n| n.as_str())
}

fn find_step<'a>(steps: &'a [serde_yaml::Value], name: &str) -> &'a serde_yaml::Value {
    steps
        .iter()
        .find(|s| step_name(s) == Some(name))
        .unwrap_or_else(|| {
            let present: Vec<&str> = steps.iter().filter_map(step_name).collect();
            panic!("job `test` has no step named `{name}`. Present: {present:?}")
        })
}

fn position(steps: &[serde_yaml::Value], name: &str) -> usize {
    steps
        .iter()
        .position(|s| step_name(s) == Some(name))
        .unwrap_or_else(|| panic!("no step named `{name}`"))
}

/// `timeout-minutes` as an integer. A step that carries none fails loudly
/// rather than defaulting — an unbounded step is the condition this whole test
/// exists over.
fn timeout_minutes(step: &serde_yaml::Value, name: &str) -> u64 {
    step.get("timeout-minutes")
        .unwrap_or_else(|| {
            panic!(
                "step `{name}` carries no `timeout-minutes`. Both halves of the \
                 build/run split must be bounded — an unbounded half re-creates \
                 the composite bound this split removed, one step over."
            )
        })
        .as_u64()
        .unwrap_or_else(|| {
            panic!(
                "step `{name}`'s `timeout-minutes` is not a plain integer. GitHub \
                 accepts an expression here, but this job's bounds are read by \
                 humans sizing them against a measured distribution, so keep them \
                 literal."
            )
        })
}

fn run_body(step: &serde_yaml::Value, name: &str) -> String {
    step.get("run")
        .and_then(|r| r.as_str())
        .unwrap_or_else(|| panic!("step `{name}` has no `run:` body"))
        .to_string()
}

/// A `run:` body with its shell COMMENT lines removed.
///
/// This distinction is load-bearing and was found by this very test failing on
/// its first run: the build step's body carries a comment explaining that it
/// deliberately does **not** write `cargo-test-output.log`, and a naive
/// `body.contains("cargo-test-output.log")` matched that explanation and
/// declared the contract broken. A shape test that cannot tell a command from a
/// comment about the command produces exactly the confident-wrong answer it
/// exists to prevent — so every assertion about what a step *does* reads this,
/// and only prose assertions may read the raw body.
///
/// Whole-line comments only: a trailing `#` inside a command can be quoted, and
/// this file is not in the business of re-implementing shell lexing.
fn command_lines(step: &serde_yaml::Value, name: &str) -> String {
    run_body(step, name)
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

const BUILD: &str = "Build Rust tests";
const RUN: &str = "Run Rust tests";

#[test]
fn rust_tests_are_built_and_run_in_separate_bounded_steps() {
    let doc = ci_workflow();
    let steps = job_steps(&doc, "test");

    let build = find_step(&steps, BUILD);
    let run = find_step(&steps, RUN);

    // Order matters: a run step that precedes its build step would execute
    // against whatever the previous job left behind.
    assert!(
        position(&steps, BUILD) < position(&steps, RUN),
        "`{BUILD}` must come before `{RUN}`"
    );

    // The `id:`s are pinned HERE and not merely referenced elsewhere. Three
    // later steps read `${{ steps.<id>.outcome }}`; a renamed or deleted `id`
    // makes every one of those expressions expand to the EMPTY STRING in CI —
    // silently, since a GitHub expression referencing an unknown step is not an
    // error. The substring assertion on the ingest's `run:` body below would
    // still pass, because it never looks at these steps. This was the one
    // assertion in this file that would have passed against a broken workflow.
    assert_eq!(
        build.get("id").and_then(|v| v.as_str()),
        Some("build_rust_tests"),
        "`{BUILD}` must keep `id: build_rust_tests` — the expiry summariser and \
         the memory sampler both read `steps.build_rust_tests.outcome`, and an \
         unknown step id expands to the empty string rather than erroring"
    );
    assert_eq!(
        run.get("id").and_then(|v| v.as_str()),
        Some("run_rust_tests"),
        "`{RUN}` must keep `id: run_rust_tests` — the expiry summariser, the \
         memory sampler and the coord ingest's `--gating-outcome` all read \
         `steps.run_rust_tests.outcome`"
    );

    let build_bound = timeout_minutes(build, BUILD);
    let run_bound = timeout_minutes(run, RUN);

    // The ordering — not the values — is the invariant. It is what encodes
    // "this bound is a TEST bound and that one is a BUILD bound"; equal bounds
    // would mean the split had been made and then un-made in effect.
    assert!(
        run_bound < build_bound,
        "the run bound ({run_bound}) must be strictly smaller than the build \
         bound ({build_bound}). This assertion pins the ORDERING only — the \
         numbers are Phase 5's to re-size. What the ordering encodes is that one \
         of these is a compile clock and the other is a test clock: measured on \
         run 35043646051 attempt 2, the build phase is ~37 min and the whole run \
         phase is 5m26s."
    );

    let build_run = command_lines(build, BUILD);
    let run_run = command_lines(run, RUN);

    assert!(
        build_run.contains("--no-run"),
        "`{BUILD}` must invoke `cargo test --no-run`; without it both steps run \
         the suite and the split measures nothing"
    );
    assert!(
        !run_run.contains("--no-run"),
        "`{RUN}` must actually run the suite, so it must not carry `--no-run`"
    );

    // `set -o pipefail` before every `| tee`: without it `tee`'s exit code
    // (always 0) is the step's, silently turning every red green. The step's
    // own comment has recorded this since before the split; it is load-bearing
    // on BOTH halves now.
    // `shell: bash` already runs as `bash --noprofile --norc -eo pipefail {0}`,
    // so this is defence in depth rather than the mechanism — the claim that
    // "without it every red goes green" is NOT true of these steps as spelled.
    // What it guards is the shell being respelled later (`shell: sh`, an
    // explicit `bash {0}`, a `bash -c` wrapper), after which `cargo test | tee`
    // really would report tee's always-zero exit code.
    for (name, body) in [(BUILD, &build_run), (RUN, &run_run)] {
        assert!(
            body.contains("set -o pipefail"),
            "step `{name}` pipes cargo into `tee` and must `set -o pipefail` \
             first. GitHub's own `shell: bash` invocation already sets it, so \
             this is defence in depth against the shell being respelled — but \
             it is cheap and the failure it prevents is every red reading green"
        );
    }
}

#[test]
fn the_run_step_is_the_one_that_writes_the_coord_ingest_log() {
    let doc = ci_workflow();
    let steps = job_steps(&doc, "test");

    let build_run = command_lines(find_step(&steps, BUILD), BUILD);
    let run_run = command_lines(find_step(&steps, RUN), RUN);

    assert!(
        run_run.contains("cargo-test-output.log"),
        "`{RUN}` must write `cargo-test-output.log` — that is the file \
         `scripts/ci-test-results-ingest.mjs` parses, and its recognition gate \
         (`hasCargoTestOutput`) keys only on run-phase lines"
    );
    assert!(
        !build_run.contains("cargo-test-output.log"),
        "`{BUILD}` must NOT write `cargo-test-output.log`. A `--no-run` log \
         carries no `test result:` / `running N tests` / `test … ... ok` line, \
         so the ingest would parse every run as `unparsed` and coord's \
         flakiness history for this repo would silently stop being written."
    );

    // The ingest itself must be told the gating step's outcome (Phase 4a), or a
    // timed-out job's rows land in coord.test_results unqualified — the #1545
    // shape, 7112 passing rows recorded against a `failure` job.
    let ingest = find_step(&steps, "Report test results to coord (best-effort)");
    let ingest_run = command_lines(ingest, "Report test results to coord (best-effort)");
    assert!(
        ingest_run.contains("--gating-outcome"),
        "the coord ingest must be passed `--gating-outcome`, or an ingest \
         written after a non-success gating step is indistinguishable from a \
         clean one"
    );
    assert!(
        ingest_run.contains("steps.run_rust_tests.outcome"),
        "`--gating-outcome` must be fed from the RUN step's own `outcome`, not \
         from a literal or from the job status"
    );
}

#[test]
fn an_expiry_is_explained_and_the_memory_peak_is_sampled() {
    let doc = ci_workflow();
    let steps = job_steps(&doc, "test");

    // Phase 2: the expiry summariser. It must be conditioned on `failure()`,
    // because a default `success()` would never run on the one state it exists
    // to explain.
    let explain = find_step(&steps, "Explain which Rust test phase the bound expired in");
    let explain_if = explain
        .get("if")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("the expiry summariser must carry an `if:` condition"));
    assert!(
        explain_if.contains("failure()"),
        "the expiry summariser's `if:` must include `failure()`, got `{explain_if}` \
         — a step that only runs on success cannot explain a failure"
    );

    // Phase 3 step 1: the post-step memory sampler. `if: always()` is the whole
    // point — the runs whose peak matters are the ones that EXPIRED, and a
    // sampler gated on success measures exactly the population that did not
    // need measuring.
    let sampler = find_step(&steps, "Windows memory peak after the Rust test steps");
    let sampler_if = sampler
        .get("if")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("the memory sampler must carry an `if:` condition"));
    assert!(
        sampler_if.contains("always()"),
        "the memory sampler's `if:` must include `always()`, got `{sampler_if}` \
         — without it an expired step reports no peak, and the expired runs are \
         precisely the ones the CARGO_BUILD_JOBS retune is gated on"
    );

    // Neither diagnostic may become a second way to fail the job.
    for (name, step) in [
        (
            "Explain which Rust test phase the bound expired in",
            explain,
        ),
        ("Windows memory peak after the Rust test steps", sampler),
    ] {
        assert_eq!(
            step.get("continue-on-error").and_then(|v| v.as_bool()),
            Some(true),
            "step `{name}` is a diagnostic and must carry `continue-on-error: true`"
        );
        assert!(
            step.get("timeout-minutes").is_none(),
            "step `{name}` must NOT carry a step-level `timeout-minutes`: paired \
             with `continue-on-error` a trip cancels the whole JOB, not the step \
             (the mechanism that broke the reverted nextest shadow)"
        );
    }
}

#[test]
fn both_halves_keep_the_two_env_entries_the_split_argues_for() {
    // Phase 3 step 2 will move the `CARGO_BUILD_JOBS` value once a soak window
    // of peaks exists. What must not happen in the meantime is the expression
    // being flattened to a single literal (which silently retunes the ubuntu
    // leg too) or either entry being dropped from a half.
    //
    // BOTH halves, deliberately. An earlier version of this test read the build
    // step only, while `ci.yml` spent a paragraph on why each entry belongs on
    // the RUN step as well — `CARGO_BUILD_JOBS` because rustdoc compiles the
    // doctests there, `QONTINUI_DISABLE_KEYCHAIN` because that is where tests
    // actually execute and the keychain hang is a runtime hang. Both were
    // deletable from the run step with every test green.
    let doc = ci_workflow();
    let steps = job_steps(&doc, "test");

    for name in [BUILD, RUN] {
        let step = find_step(&steps, name);
        let env = step
            .get("env")
            .unwrap_or_else(|| panic!("step `{name}` must carry an `env:` mapping"));

        let jobs = env
            .get("CARGO_BUILD_JOBS")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("step `{name}` must carry a `CARGO_BUILD_JOBS` env entry"));
        assert!(
            jobs.contains("matrix.platform") && jobs.contains("windows-latest"),
            "step `{name}`'s `CARGO_BUILD_JOBS` must stay a per-platform \
             expression, got `{jobs}` — the windows and ubuntu legs are \
             throttled for different reasons (pagefile commit headroom vs the \
             OOM killer) and a single literal retunes both at once"
        );

        assert_eq!(
            env.get("QONTINUI_DISABLE_KEYCHAIN")
                .and_then(|v| v.as_str()),
            Some("1"),
            "step `{name}` must keep `QONTINUI_DISABLE_KEYCHAIN: \"1\"`. It \
             short-circuits `keychain_enabled()` in src-tauri/src/auth.rs; \
             without it the macOS keychain blocks indefinitely on a runner with \
             no desktop session and eats the whole bound. macOS is out of the \
             matrix today, which is exactly why dropping this would go unnoticed \
             until it is re-added."
        );
    }
}

#[test]
fn the_memory_sampler_is_gated_to_the_windows_leg() {
    // Without the platform gate the step runs on ubuntu, `powershell` is not
    // found, `bash -e` aborts the group, and `continue-on-error: true` swallows
    // it — a diagnostic that reports nothing and says nothing about reporting
    // nothing. Asserted separately from the `always()` check so a failure names
    // which half broke.
    let doc = ci_workflow();
    let steps = job_steps(&doc, "test");
    let sampler = find_step(&steps, "Windows memory peak after the Rust test steps");
    let cond = sampler
        .get("if")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("the memory sampler must carry an `if:` condition"));

    assert!(
        cond.contains("windows-latest"),
        "the memory sampler's `if:` must gate on the windows leg, got `{cond}`"
    );
}
