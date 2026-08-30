//! Regression gate: **a workflow with a `paths:` filter must match its own
//! file.**
//!
//! One line, two independent failure modes, both already paid for in this
//! repo. `qontinui-types-drift.yml` carries the invariant in a header comment
//! and depends on it structurally; nothing enforced it, and one workflow
//! silently did not hold it.
//!
//! # Why it bites on `push`: the frozen coord baseline
//!
//! coord scores `main` from the LAST run of each workflow, and only a `push`
//! event establishes that baseline —
//! `crates/coord/src/ci_baseline.rs` pins it:
//!
//! ```text
//! assert!(establishes_main_baseline(Some("push")));
//! assert!(!establishes_main_baseline(Some("workflow_dispatch")));
//! assert!(!establishes_main_baseline(Some("schedule")));
//! ```
//!
//! A paths-filtered workflow only gets a `push` run when a commit landing on
//! `main` touches one of its paths. So if it goes RED for a reason OUTSIDE its
//! own filter — the standing case is a sibling repo drifting — no in-repo
//! commit can re-run it, the red baseline freezes, and coord blocks every NEW
//! runner PR at enqueue with `block_reason_code: "main-red"`. `gh workflow run
//! --ref main` cannot clear it: dispatch is exactly the event class coord
//! ignores, so the gate reads green in the GitHub UI while the merge train
//! stays blocked.
//!
//! Listing the workflow's own path in its `push` filter is the escape hatch:
//! editing the file is then itself a valid thaw, because landing that edit
//! fires a fresh `push` run. `qontinui-types-drift.yml` was frozen ten days
//! (2026-08-12 .. 2026-08-22) and PR #1107 landed on that lever — it worked
//! only because the entry happened to already be there.
//!
//! Note what this does NOT promise. The lever is a thaw, not immunity: a
//! workflow whose red comes from outside its filter still freezes until
//! someone notices and pushes. The invariant guarantees a remedy EXISTS in
//! this repo; it does not fire it.
//!
//! # Why it bites on `pull_request`: the vacuous green
//!
//! The mirror image, and the one this test actually caught. A PR that edits a
//! paths-filtered gate does not trigger that gate unless the filter names the
//! gate's own file — so the edit lands never having been exercised. That is
//! how a drift check rots into a check that passes because it never ran.
//! `atlas-exclude-fresh.yml` was in exactly that state when this test was
//! written (`paths: ["atlas/**"]`, which cannot match
//! `.github/workflows/atlas-exclude-fresh.yml`), guarding a data-loss-class
//! footgun it would not have re-run on its own edit.
//!
//! # Scope
//!
//! Only workflows that HAVE a `paths:` filter are constrained. An unfiltered
//! trigger already runs on every change, self-inclusion included, and adding
//! a filter to a currently-unfiltered workflow is a deliberate act that this
//! test then starts checking. `schedule` and `workflow_dispatch` take no
//! `paths:` and are untouched.
//!
//! This runs inside the existing `cargo test` job, so it needs no workflow
//! edit — which matters here beyond convenience: `ci-integrity.yml` reds any
//! PR that touches a gating workflow, so an enforcement mechanism that lived
//! in `.github/workflows/` could not land without an operator override.

use std::path::{Path, PathBuf};

/// Triggers that accept a `paths:` filter and that we hold to the invariant.
///
/// `push` and `pull_request` are the two the rationale above is written for.
/// `pull_request_target` is included because it is a `pull_request` that runs
/// the BASE branch's copy of the file — the self-verification argument applies
/// unchanged, and `ci-integrity.yml` (the repo's gate-integrity guard) is
/// exactly such a workflow.
const FILTERED_TRIGGERS: [&str; 3] = ["push", "pull_request", "pull_request_target"];

/// Repo root: `CARGO_MANIFEST_DIR` is `src-tauri/`, so its parent is the
/// checkout root that holds `.github/`.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri always has a parent")
        .to_path_buf()
}

/// The `on:` mapping of a parsed workflow.
///
/// Looked up under BOTH the string key `on` and the boolean key `true`: `on`
/// is a YAML 1.1 boolean literal, so a parser resolving that way turns the
/// key itself into `true` before we ever see it. Which one we get is a
/// property of the YAML library, not of the file, and a lookup that silently
/// missed would make every workflow vacuously pass — the exact failure shape
/// this test exists to catch.
fn on_block(doc: &serde_yaml::Value) -> Option<&serde_yaml::Mapping> {
    let mapping = doc.as_mapping()?;
    mapping
        .get(serde_yaml::Value::String("on".into()))
        .or_else(|| mapping.get(serde_yaml::Value::Bool(true)))?
        .as_mapping()
}

/// The `paths:` entries of one trigger, or `None` when the trigger is absent,
/// is not a mapping (`workflow_dispatch: {}`, or a bare `pull_request:` whose
/// value is null), or carries no `paths:` key.
fn paths_filter(on: &serde_yaml::Mapping, trigger: &str) -> Option<Vec<String>> {
    let entries = on
        .get(serde_yaml::Value::String(trigger.into()))?
        .as_mapping()?
        .get(serde_yaml::Value::String("paths".into()))?
        .as_sequence()?;
    Some(
        entries
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
    )
}

/// Does this filter select `path`?
///
/// `glob_match` gives us GitHub's globstar semantics — `*` stops at `/`, `**`
/// crosses it — which is what the filters are written against.
///
/// Negations are REJECTED rather than interpreted. GitHub applies `!pattern`
/// positionally (a later `!` un-selects, a later positive re-selects), and
/// approximating that ordering would make this test quietly wrong in the one
/// case where being right matters. No filter in this repo uses one; if that
/// changes, teach this function the real semantics rather than loosening it.
fn selects(filter: &[String], path: &str) -> Result<bool, String> {
    if let Some(negation) = filter.iter().find(|p| p.starts_with('!')) {
        return Err(format!(
            "negated pattern `{negation}` — this check does not model GitHub's \
             positional negation; extend it rather than dropping the workflow"
        ));
    }
    Ok(filter
        .iter()
        .any(|pattern| glob_match::glob_match(pattern, path)))
}

/// Repo-relative, forward-slashed path of a workflow file — the form GitHub
/// matches `paths:` against, and NOT what `Path::display` yields on Windows.
fn repo_relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or_else(|e| panic!("{} is not under {}: {e}", file.display(), root.display()))
        .to_string_lossy()
        .replace('\\', "/")
}

fn workflow_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.expect("readable dir entry").path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("yml") | Some("yaml")
            )
        })
        .collect();
    files.sort();
    files
}

#[test]
fn paths_filtered_workflows_match_their_own_file() {
    let root = repo_root();
    let dir = root.join(".github").join("workflows");
    let files = workflow_files(&dir);

    // An empty sweep would pass silently and prove nothing — the same vacuous
    // green this test is about. A moved or renamed workflow directory must
    // fail loudly here.
    assert!(
        !files.is_empty(),
        "no workflow files under {} — has the directory moved?",
        dir.display()
    );

    let mut violations: Vec<String> = Vec::new();

    for file in &files {
        let relative = repo_relative(&root, file);
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        let doc: serde_yaml::Value =
            serde_yaml::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", file.display()));

        let Some(on) = on_block(&doc) else {
            violations.push(format!("{relative}: no `on:` mapping"));
            continue;
        };

        for trigger in FILTERED_TRIGGERS {
            let Some(filter) = paths_filter(on, trigger) else {
                continue;
            };
            match selects(&filter, &relative) {
                Ok(true) => {}
                Ok(false) => violations.push(format!(
                    "{relative}: `{trigger}.paths` does not match this file\n      \
                     filter: {filter:?}\n      \
                     fix: add `- \"{relative}\"` to that filter"
                )),
                Err(why) => violations.push(format!("{relative}: `{trigger}.paths` {why}")),
            }
        }
    }

    assert!(
        violations.is_empty(),
        "workflow `paths:` filters must match the workflow's own file, so that \
         editing the workflow both exercises it on the PR and re-baselines it \
         on main (see this file's header for what each half prevents):\n  - {}\n",
        violations.join("\n  - ")
    );
}
