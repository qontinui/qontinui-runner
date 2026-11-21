//! Regression gate: **`.qontinui/ci-targets.toml` names every CI job that
//! gates a merge, and nothing else.**
//!
//! The map's own header carries a MAINTENANCE rule: it "must be updated in the
//! same PR whenever a CI job is added, removed or re-scoped". Nothing enforced
//! that, and coord reads the map as data (plan
//! `2026-11-08-coord-lands-one-candidate-per-repo-where-a-file-overlap-gate-would-admit-parallel-units`,
//! Phase 1b), so a stale map fails silently. It does not fail inside this repo.
//! It fails as a wrong target verdict on coord's side.
//!
//! # What this pins, and what it cannot
//!
//! Two of the three halves of the rule are mechanical, and both are pinned:
//!
//! - **added**: every job that runs on a `pull_request` (or
//!   `pull_request_target`) to any base (stacked bases included, as
//!   `stacked-pr-fastlane` is), or on a `merge-candidate*/**` push, is named
//!   by a target. A target named after a whole workflow covers every gating
//!   job in it, including one added later, so this is pinned per workflow for
//!   those and per job for `<stem>:<job>` targets. When a gating job is missing, the map's semantics
//!   still make the job's files touch every target, so the verdict stays sound.
//!   But the job is invisible to the map, and the next person to narrow a
//!   neighbouring target has no row telling them it exists.
//! - **removed / renamed**: every target names a job that still exists and
//!   still gates. A dangling target is how a rename silently drops a job's
//!   closure.
//!
//! It also checks that every LITERAL path in `global` and `observes` exists,
//! and that every glob matches at least one file (apart from a short
//! allowlist). Suppose a renamed script keeps its old path in the map,
//! and the new path matches only some other target. That is the one unsound
//! direction the map header names.
//!
//! **Re-scoped** has one mechanical half, and it is pinned: a changed
//! trigger-level `paths:` / `paths-ignore:` filter. Every file such a trigger
//! fires on today must be observed by each of its jobs' targets (or by
//! `global`), and a filter entry that matches no file must be spelled in the
//! target as written. The map says each path-filtered target mirrors its
//! workflow's filter as its roots, so a filter widened without the map leaves
//! a file that starts the job listed only under some other target, which is
//! the unsound direction. Filters are matched with `glob_match` after
//! [`github_glob`] rewrites GitHub's `**.ext` shape, which `glob_match` would
//! otherwise read as root-only. GitHub's `+` quantifier is not modelled; no
//! filter uses it.
//!
//! The other half (a job that starts reading a new file through its build or
//! its tests) is not mechanical. It would need the build-graph extractor the
//! map says was never run. This test does not pretend to check it.
//!
//! # Target naming (the convention the map already uses)
//!
//! - `ci.yml`'s jobs are named bare: `test`, `route`, `frontend-tests`, ...
//! - Every other workflow's target is either the workflow stem
//!   (`atlas-schema-check`), which covers EVERY gating job of that workflow, or
//!   `<stem>:<job-id>` (`clippy-tiers:clippy-diff`), which covers that one job.
//!
//! # Which jobs gate
//!
//! A job gates when it runs on a gating event. Any `on:` trigger this test does
//! not classify is a panic, never silently non-gating. The job's `if:` is read
//! with GitHub's precedence (`&&` binds tighter than `||`) and comparisons that
//! ignore case. Only two leaves DENY an event: `github.event_name != '<e>'` and
//! `github.event_name == '<x>'` for another `<x>`. A conjunction denies when any
//! part does, and a disjunction only when every part does. A job whose `needs:` names a job that does not run
//! is skipped too, unless its `if:` carries a status function. With
//! `!cancelled()` (and the like), it is still skipped when its `if:` requires
//! `needs.<that>.result == 'success'`. Any other `if:` is treated as "may
//! run". That is conservative, because an unrecognised condition demands a
//! target rather than excusing one.
//!
//! Sibling precedent: `src-tauri/tests/workflow_paths_self_inclusion.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_yaml::Value;

/// Representative branch names for the merge-candidate push refs coord
/// creates. A `push.branches` filter that matches any of them gates.
const MERGE_CANDIDATE_REFS: [&str; 3] = [
    "merge-candidate/1",
    "merge-candidate-spec/1",
    "merge-candidate-batch/1",
];

/// The workflow whose jobs are named bare in the map.
const BARE_JOB_WORKFLOW: &str = "ci";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri always has a parent")
        .to_path_buf()
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetMap {
    version: u32,
    global: Vec<String>,
    targets: Vec<Target>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    name: String,
    observes: Vec<String>,
}

fn key(s: &str) -> Value {
    Value::String(s.into())
}

/// The `on:` block, normalised to event -> config. A string (`on: push`) or a
/// sequence (`on: [push, pull_request]`) becomes events with a null config.
/// It is looked up under both `on` and YAML 1.1's boolean `true`, as the
/// sibling test explains.
fn on_events(doc: &Value) -> BTreeMap<String, Value> {
    let mapping = doc.as_mapping().expect("workflow is a mapping");
    let on = mapping
        .get(key("on"))
        .or_else(|| mapping.get(Value::Bool(true)))
        .expect("workflow has an `on:` block");
    let mut out = BTreeMap::new();
    match on {
        Value::String(event) => {
            out.insert(event.clone(), Value::Null);
        }
        Value::Sequence(events) => {
            for event in events {
                out.insert(event.as_str().expect("event name").to_string(), Value::Null);
            }
        }
        Value::Mapping(m) => {
            for (k, v) in m {
                out.insert(k.as_str().expect("event name").to_string(), v.clone());
            }
        }
        other => panic!("unsupported `on:` shape: {other:?}"),
    }
    out
}

fn string_list(config: &Value, field: &str) -> Option<Vec<String>> {
    let seq = config.as_mapping()?.get(key(field))?;
    Some(match seq {
        Value::String(s) => vec![s.clone()],
        Value::Sequence(items) => items
            .iter()
            .map(|v| v.as_str().expect("filter entry").to_string())
            .collect(),
        other => panic!("unsupported `{field}:` shape: {other:?}"),
    })
}

/// Refuse negated branch patterns, for the reason the sibling test gives:
/// GitHub applies them positionally, and approximating that would be quietly
/// wrong.
fn refuse_negated_filters(config: &Value) {
    for field in ["branches", "branches-ignore"] {
        for pattern in string_list(config, field).into_iter().flatten() {
            assert!(
                !pattern.starts_with('!'),
                "negated branch pattern `{pattern}`: this check does not model GitHub's \
                 positional negation; extend it rather than dropping the workflow"
            );
        }
    }
}

/// Does a `branches` / `branches-ignore` pair admit `branch`?
fn branch_filter_admits(config: &Value, branch: &str) -> bool {
    refuse_negated_filters(config);
    let include = string_list(config, "branches");
    let ignore = string_list(config, "branches-ignore");
    let included = include.is_none_or(|ps| ps.iter().any(|p| glob_match::glob_match(p, branch)));
    let ignored = ignore.is_some_and(|ps| ps.iter().any(|p| glob_match::glob_match(p, branch)));
    included && !ignored
}

/// Triggers that never run on a pull request or a merge-candidate push.
const NON_GATING_TRIGGERS: [&str; 5] = [
    "schedule",
    "workflow_dispatch",
    "workflow_call",
    "workflow_run",
    "release",
];

/// The gating event names a workflow fires on. These are the values
/// `github.event_name` takes for those runs.
fn gating_events(on: &BTreeMap<String, Value>) -> BTreeSet<&'static str> {
    for trigger in on.keys() {
        assert!(
            ["pull_request", "pull_request_target", "push"].contains(&trigger.as_str())
                || NON_GATING_TRIGGERS.contains(&trigger.as_str()),
            "unclassified trigger `{trigger}`: decide whether it gates a merge (e.g. \
             `merge_group` does) and teach this test, rather than letting it read as \
             non-gating"
        );
    }
    let mut events = BTreeSet::new();
    for (name, event) in [
        ("pull_request", "pull_request"),
        ("pull_request_target", "pull_request_target"),
    ] {
        // Any base counts, not only `main`: coord lands stacked PRs too, and
        // `stacked-pr-fastlane` (`branches-ignore: [main, develop]`) is a
        // target for exactly that reason. The filter is still parsed, so that
        // a negated pattern is refused rather than misread.
        if let Some(config) = on.get(name) {
            refuse_negated_filters(config);
            events.insert(event);
        }
    }
    if let Some(config) = on.get("push") {
        let tags_only = string_list(config, "branches").is_none()
            && string_list(config, "branches-ignore").is_none()
            && (string_list(config, "tags").is_some()
                || string_list(config, "tags-ignore").is_some());
        if !tags_only
            && MERGE_CANDIDATE_REFS
                .iter()
                .any(|r| branch_filter_admits(config, r))
        {
            events.insert("push");
        }
    }
    events
}

/// Split `expr` on top-level `op`, outside quotes and parentheses.
fn split_top(expr: &str, op: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let (mut depth, mut quoted, mut start) = (0i32, false, 0usize);
    let bytes = expr.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => quoted = !quoted,
            b'(' if !quoted => depth += 1,
            b')' if !quoted => depth -= 1,
            _ if !quoted && depth == 0 && bytes[i..].starts_with(op.as_bytes()) => {
                parts.push(strip_parens(
                    expr.get(start..i)
                        .expect("split at an ASCII operator boundary"),
                ));
                i += op.len();
                start = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(strip_parens(
        expr.get(start..)
            .expect("split at an ASCII operator boundary"),
    ));
    parts
}

/// Remove parentheses that wrap the whole expression.
fn strip_parens(expr: &str) -> String {
    let mut s = expr.trim();
    while s.starts_with('(') && s.ends_with(')') {
        let mut depth = 0i32;
        let wraps = s.char_indices().all(|(i, c)| {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            depth > 0 || i == s.len() - 1
        });
        if !wraps {
            break;
        }
        s = s
            .strip_prefix('(')
            .and_then(|inner| inner.strip_suffix(')'))
            .expect("checked by the loop condition")
            .trim();
    }
    s.to_string()
}

/// `github.event_name <op> '<value>'` -> `value`. GitHub treats context names
/// as case-insensitive, so the prefix is matched that way.
fn event_compare(clause: &str, op: &str) -> Option<String> {
    const CONTEXT: &str = "github.event_name";
    let head = clause.get(..CONTEXT.len())?;
    if !head.eq_ignore_ascii_case(CONTEXT) {
        return None;
    }
    let rest = clause.get(CONTEXT.len()..)?.trim_start();
    let rest = rest.strip_prefix(op)?.trim();
    let value = rest.strip_prefix('\'')?.strip_suffix('\'')?;
    (!value.contains('\'')).then(|| value.to_string())
}

/// Fold a condition with GitHub's precedence: a top-level `||` is split
/// first, because `&&` binds tighter. A disjunction holds only when every
/// part does, a conjunction when any part does, and `leaf` decides the rest.
fn expr_forces(expr: &str, leaf: &dyn Fn(&str) -> bool) -> bool {
    let expr = strip_parens(expr);
    let disjuncts = split_top(&expr, "||");
    if disjuncts.len() > 1 {
        return disjuncts.iter().all(|d| expr_forces(d, leaf));
    }
    let conjuncts = split_top(&expr, "&&");
    if conjuncts.len() > 1 {
        return conjuncts.iter().any(|c| expr_forces(c, leaf));
    }
    leaf(&expr)
}

/// Does condition `expr` make the job skip `event`?
fn expr_denies(expr: &str, event: &str) -> bool {
    expr_forces(expr, &|leaf| {
        if leaf.eq_ignore_ascii_case("false") {
            return true;
        }
        if let Some(value) = event_compare(leaf, "!=") {
            return value.eq_ignore_ascii_case(event);
        }
        event_compare(leaf, "==").is_some_and(|value| !value.eq_ignore_ascii_case(event))
    })
}

/// Does condition `expr` require `needs.<need>.result == 'success'`?
fn expr_requires_success(expr: &str, need: &str) -> bool {
    let wanted = format!("needs.{need}.result=='success'");
    expr_forces(expr, &|leaf| {
        leaf.replace(' ', "").eq_ignore_ascii_case(&wanted)
    })
}

fn job_if(job: &Value) -> String {
    let raw = match job.get("if") {
        None | Some(Value::Null) => return String::new(),
        Some(Value::Bool(b)) => return if *b { String::new() } else { "false".into() },
        Some(v) => v.as_str().expect("`if:` is a string").trim().to_string(),
    };
    raw.strip_prefix("${{")
        .and_then(|s| s.strip_suffix("}}"))
        .unwrap_or(&raw)
        .trim()
        .to_string()
}

fn job_needs(job: &Value) -> Vec<String> {
    match job.get("needs") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Sequence(items)) => items
            .iter()
            .map(|v| v.as_str().expect("needs entry").to_string())
            .collect(),
        Some(other) => panic!("unsupported `needs:` shape: {other:?}"),
    }
}

const STATUS_FUNCTIONS: [&str; 4] = ["always()", "failure()", "cancelled()", "success()"];

/// Does job `id` run on `event`? `false` only when one of the recognised
/// shapes skips it (module docs); everything else may run.
fn runs_on(
    jobs: &serde_yaml::Mapping,
    id: &str,
    event: &str,
    memo: &mut BTreeMap<String, bool>,
) -> bool {
    if let Some(&known) = memo.get(id) {
        return known;
    }
    let job = jobs
        .get(key(id))
        .unwrap_or_else(|| panic!("`needs:` names unknown job `{id}`"));
    let cond = job_if(job);
    let mut runs = cond.is_empty() || !expr_denies(&cond, event);
    if runs {
        let lowered = cond.to_ascii_lowercase();
        let has_status_fn = STATUS_FUNCTIONS.iter().any(|f| lowered.contains(f));
        for need in job_needs(job) {
            if runs_on(jobs, &need, event, memo) {
                continue;
            }
            if !has_status_fn || expr_requires_success(&cond, &need) {
                runs = false;
                break;
            }
        }
    }
    memo.insert(id.to_string(), runs);
    runs
}

/// The ids of the jobs in `jobs` that run on `event`.
fn jobs_running_on(jobs: &serde_yaml::Mapping, event: &str) -> BTreeSet<String> {
    let mut memo = BTreeMap::new();
    jobs.keys()
        .map(|id| id.as_str().expect("job id").to_string())
        .filter(|id| runs_on(jobs, id, event, &mut memo))
        .collect()
}

/// Every workflow under `.github/workflows`, as (stem, parsed document),
/// sorted by file name.
fn load_workflows(root: &Path) -> Vec<(String, Value)> {
    let dir = root.join(".github").join("workflows");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.expect("readable dir entry").path())
        .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("yml" | "yaml")))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no workflows under {}", dir.display());
    files
        .into_iter()
        .map(|file| {
            let stem = file
                .file_stem()
                .and_then(|s| s.to_str())
                .expect("utf-8 workflow name")
                .to_string();
            let text = std::fs::read_to_string(&file)
                .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
            let doc: Value = serde_yaml::from_str(&text)
                .unwrap_or_else(|e| panic!("parse {}: {e}", file.display()));
            (stem, doc)
        })
        .collect()
}

fn workflow_jobs<'a>(stem: &str, doc: &'a Value) -> &'a serde_yaml::Mapping {
    doc.get("jobs")
        .and_then(Value::as_mapping)
        .unwrap_or_else(|| panic!("{stem}: no `jobs:` mapping"))
}

/// workflow stem -> the ids of its jobs that run on at least one gating event.
fn gating_jobs(workflows: &[(String, Value)]) -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    for (stem, doc) in workflows {
        let events = gating_events(&on_events(doc));
        if events.is_empty() {
            continue;
        }
        let jobs = workflow_jobs(stem, doc);
        let gating = events
            .iter()
            .flat_map(|event| jobs_running_on(jobs, event))
            .collect();
        out.insert(stem.clone(), gating);
    }
    out
}

/// One gating trigger that carries a trigger-level path filter.
struct PathFilteredTrigger {
    stem: String,
    event: &'static str,
    /// The jobs that run on this event.
    jobs: BTreeSet<String>,
    /// `paths:` entries, negations (`!x`) dropped, in [`github_glob`] form.
    paths: Option<Vec<String>>,
    /// `paths-ignore:` entries in [`github_glob`] form.
    paths_ignore: Option<Vec<String>>,
}

/// Every gating trigger with a `paths:` or `paths-ignore:` filter.
///
/// A `!` entry inside `paths:` can only REMOVE files from the trigger set, so
/// dropping it widens the set this check demands coverage for. That is the
/// conservative direction. A `!` inside `paths-ignore:` would re-include
/// files, and dropping it would narrow the set, so it is refused, as a negated
/// branch pattern is.
fn path_filtered_triggers(workflows: &[(String, Value)]) -> Vec<PathFilteredTrigger> {
    let mut out = Vec::new();
    for (stem, doc) in workflows {
        let on = on_events(doc);
        for event in gating_events(&on) {
            let config = &on[event];
            let paths = string_list(config, "paths").map(|ps| {
                ps.iter()
                    .filter(|p| !p.starts_with('!'))
                    .map(|p| github_glob(p))
                    .collect()
            });
            let paths_ignore = string_list(config, "paths-ignore").map(|ps| {
                ps.iter()
                    .map(|p| {
                        assert!(
                            !p.starts_with('!'),
                            "{stem}.yml: negated `paths-ignore` entry `{p}`: this check does \
                             not model GitHub's positional negation; extend it"
                        );
                        github_glob(p)
                    })
                    .collect()
            });
            if paths.is_none() && paths_ignore.is_none() {
                continue;
            }
            out.push(PathFilteredTrigger {
                stem: stem.clone(),
                event,
                jobs: jobs_running_on(workflow_jobs(stem, doc), event),
                paths,
                paths_ignore,
            });
        }
    }
    out
}

/// A GitHub filter pattern in the form `glob_match` reads the same way.
///
/// GitHub's `**` matches any characters, `/` included, wherever it appears.
/// `glob_match` treats `**` as a globstar only as a whole path segment, so
/// `**.ps1` would match `a.ps1` and never `scripts/a.ps1`. A `**` followed by
/// anything but `/` is rewritten to `**/*` (`**.ps1` -> `**/*.ps1`), which
/// matches the same files when the `**` starts a path segment. A `**` in the
/// middle of a segment (`test**.sh`) has no such equivalent and is refused.
fn github_glob(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 2);
    let mut rest = pattern;
    while let Some(at) = rest.find("**") {
        let starts_segment = (out.is_empty() && at == 0)
            || rest[..at].ends_with('/')
            || (at == 0 && out.ends_with('/'));
        let ends_pattern = rest.len() == at + 2;
        assert!(
            starts_segment || ends_pattern || rest[at + 2..].starts_with('/'),
            "filter pattern `{pattern}` has a `**` inside a path segment, which this check \
             cannot translate for glob_match; extend `github_glob`"
        );
        out.push_str(&rest[..at]);
        out.push_str("**");
        rest = &rest[at + 2..];
        if !rest.is_empty() && !rest.starts_with('/') {
            out.push_str("/*");
        }
    }
    out.push_str(rest);
    out
}

/// The targets that cover job `job` of workflow `stem` (naming convention in
/// the module docs).
fn covering_targets<'a>(map: &'a TargetMap, stem: &str, job: &str) -> Vec<&'a Target> {
    let scoped = format!("{stem}:{job}");
    map.targets
        .iter()
        .filter(|t| {
            t.name == stem || t.name == scoped || (stem == BARE_JOB_WORKFLOW && t.name == job)
        })
        .collect()
}

/// Every file a path-filtered trigger fires on must be observed by each job's
/// target (or by `global`). Otherwise a file that starts the job is listed
/// only under some other target, or under none, and the first case is the
/// one unsound direction the map header names.
fn path_filter_violations(
    map: &TargetMap,
    triggers: &[PathFilteredTrigger],
    files: &[String],
) -> Vec<String> {
    let mut violations = Vec::new();
    for trigger in triggers {
        let stem = &trigger.stem;
        let fires: Vec<&String> = files
            .iter()
            .filter(|f| {
                let included = trigger
                    .paths
                    .as_ref()
                    .is_none_or(|ps| ps.iter().any(|p| glob_match::glob_match(p, f)));
                let ignored = trigger
                    .paths_ignore
                    .as_ref()
                    .is_some_and(|ps| ps.iter().any(|p| glob_match::glob_match(p, f)));
                included && !ignored
            })
            .collect();
        for job in &trigger.jobs {
            let targets = covering_targets(map, stem, job);
            if targets.is_empty() {
                // Reported as an uncovered gating job by the main test.
                continue;
            }
            let observes: Vec<&str> = targets
                .iter()
                .flat_map(|t| t.observes.iter())
                .chain(map.global.iter())
                .map(String::as_str)
                .collect();
            let target_names = targets
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join("`, `");
            let uncovered: Vec<&str> = fires
                .iter()
                .filter(|f| !observes.iter().any(|o| glob_match::glob_match(o, f)))
                .map(|f| f.as_str())
                .collect();
            if !uncovered.is_empty() {
                let shown = uncovered.iter().take(5).copied().collect::<Vec<_>>();
                violations.push(format!(
                    "job `{job}` in .github/workflows/{stem}.yml fires on {} for {} file(s) \
                     target `{target_names}` does not observe (e.g. {}) — add its `paths:` \
                     filter to the target's `observes`",
                    trigger.event,
                    uncovered.len(),
                    shown.join(", ")
                ));
            }
            // A filter entry that matches nothing today cannot be checked file
            // by file, so it must be spelled in the target (in github_glob
            // form, which is how the map spells it).
            for pattern in trigger.paths.iter().flatten() {
                let matches_nothing = !files.iter().any(|f| glob_match::glob_match(pattern, f));
                if matches_nothing
                    && !observes.contains(&pattern.as_str())
                    && !observes.contains(&"**")
                {
                    violations.push(format!(
                        "job `{job}` in .github/workflows/{stem}.yml filters on `{pattern}`, \
                         which matches no file and is not listed in target `{target_names}`"
                    ));
                }
            }
        }
    }
    violations
}

/// Globs the map keeps on purpose although nothing matches them today.
/// `**/*.psm1` mirrors `powershell-parse.yml`'s own filter (the map says so).
const GLOBS_ALLOWED_TO_MATCH_NOTHING: [&str; 1] = ["**/*.psm1"];

/// Directory names never walked: VCS metadata and build or install output,
/// none of which the map describes.
const SKIPPED_DIRS: [&str; 5] = [".git", "node_modules", "target", "target-agent", "dist"];

/// Every file under `root`, repo-relative with forward slashes.
fn repo_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
        for entry in entries {
            let entry = entry.expect("readable dir entry");
            let path = entry.path();
            let file_type = entry.file_type().expect("readable file type");
            if file_type.is_dir() {
                let name = entry.file_name();
                if !SKIPPED_DIRS.iter().any(|s| name == *s) {
                    stack.push(path);
                }
            } else {
                let relative = path
                    .strip_prefix(root)
                    .expect("walked path is under root")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(relative);
            }
        }
    }
    assert!(!out.is_empty(), "walked no files under {}", root.display());
    out
}

fn load_map(root: &Path) -> TargetMap {
    let path = root.join(".qontinui").join("ci-targets.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

#[test]
fn ci_targets_map_names_every_gating_job_and_only_gating_jobs() {
    let root = repo_root();
    let map = load_map(&root);
    assert_eq!(
        map.version, 1,
        "a new map version needs this test (and coord's reader) taught it"
    );
    let workflows = load_workflows(&root);
    let gating = gating_jobs(&workflows);

    // The sweep must find something. An empty one passes vacuously, and that
    // is the failure this test exists to catch.
    let bare_jobs = gating.get(BARE_JOB_WORKFLOW).unwrap_or_else(|| {
        panic!("{BARE_JOB_WORKFLOW}.yml no longer gates — the naming convention moved")
    });
    assert!(
        bare_jobs.contains("test"),
        "ci.yml's `test` job no longer reads as gating"
    );

    let mut violations = Vec::new();

    // A bare ci.yml job id that is also a workflow stem would be ambiguous.
    for job in bare_jobs {
        if job != BARE_JOB_WORKFLOW && gating.contains_key(job) {
            violations.push(format!(
                "ci.yml job `{job}` shares its name with workflow `{job}.yml`; a bare target name is ambiguous"
            ));
        }
    }

    let mut names = BTreeSet::new();
    for target in &map.targets {
        if !names.insert(target.name.as_str()) {
            violations.push(format!("target `{}` is declared twice", target.name));
        }
        if target.observes.is_empty() {
            violations.push(format!("target `{}` observes nothing", target.name));
        }
        let resolves = match target.name.split_once(':') {
            Some((stem, job)) => gating.get(stem).is_some_and(|jobs| jobs.contains(job)),
            None => {
                gating
                    .get(&target.name)
                    .is_some_and(|jobs| !jobs.is_empty())
                    || bare_jobs.contains(&target.name)
            }
        };
        if !resolves {
            violations.push(format!(
                "target `{}` names no job that gates a merge (removed, renamed, or no longer \
                 triggered on a pull_request or a merge-candidate push) — update or drop it",
                target.name
            ));
        }
    }

    for (stem, jobs) in &gating {
        for job in jobs {
            let covered = names.contains(stem.as_str())
                || names.contains(format!("{stem}:{job}").as_str())
                || (stem == BARE_JOB_WORKFLOW && names.contains(job.as_str()));
            if !covered {
                let suggested = if stem == BARE_JOB_WORKFLOW {
                    job.clone()
                } else {
                    format!("{stem}` or `{stem}:{job}")
                };
                violations.push(format!(
                    "gating job `{job}` in .github/workflows/{stem}.yml has no target — add a \
                     [[targets]] entry named `{suggested}` listing the files it observes"
                ));
            }
        }
    }

    // A literal path that no longer exists, or a glob that matches nothing,
    // is a rename the map missed.
    let is_glob = |p: &str| p.contains(['*', '?', '[', '{']);
    let files = repo_files(&root);
    let patterns = map.global.iter().map(|p| ("global", p)).chain(
        map.targets
            .iter()
            .flat_map(|t| t.observes.iter().map(move |p| (t.name.as_str(), p))),
    );
    for (owner, path) in patterns {
        if !is_glob(path) {
            if !root.join(path).exists() {
                violations.push(format!("`{owner}` lists `{path}`, which does not exist"));
            }
        } else if !GLOBS_ALLOWED_TO_MATCH_NOTHING.contains(&path.as_str())
            && !files.iter().any(|f| glob_match::glob_match(path, f))
        {
            violations.push(format!("`{owner}` lists `{path}`, which matches no file"));
        }
    }

    // A changed `paths:` filter is the mechanical half of a re-scope.
    violations.extend(path_filter_violations(
        &map,
        &path_filtered_triggers(&workflows),
        &files,
    ));

    assert!(
        violations.is_empty(),
        ".qontinui/ci-targets.toml is out of step with .github/workflows (its MAINTENANCE \
         rule: update it in the same PR as the CI job):\n  - {}\n",
        violations.join("\n  - ")
    );
}

#[test]
fn if_condition_reader_recognises_the_shapes_it_claims() {
    // The reader decides whether a job is excused from a target, so pin each
    // shape it claims to recognise, and the shapes it must not excuse.
    let denies = expr_denies;
    assert!(denies(
        "github.event_name != 'pull_request'",
        "pull_request"
    ));
    assert!(!denies("github.event_name != 'pull_request'", "push"));
    assert!(denies(
        "github.event_name != 'pull_request' && vars.LANE == 'auto'",
        "pull_request"
    ));
    assert!(denies(
        "(github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')",
        "pull_request"
    ));
    assert!(denies("false", "pull_request"));
    // A disjunction with anything else in it may run: not excused.
    assert!(!denies(
        "github.event_name != 'pull_request' || github.event.pull_request.state != 'closed'",
        "pull_request"
    ));
    assert!(!denies(
        "vars.LANE == 'auto' && (github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository)",
        "pull_request"
    ));
    // `&&` binds tighter than `||`: the deny sits inside the second disjunct.
    assert!(!denies(
        "contains(github.event.pull_request.labels.*.name, 'full') || vars.X == 'on' && github.event_name != 'pull_request'",
        "pull_request"
    ));
    // GitHub compares strings ignoring case.
    assert!(!denies(
        "github.event_name == 'Pull_Request'",
        "pull_request"
    ));
    assert!(denies(
        "GITHUB.EVENT_NAME != 'PULL_REQUEST'",
        "pull_request"
    ));
    assert!(!denies("", "pull_request"));

    // Needs propagation: a dependent of a skipped job is skipped unless it
    // carries a status function, and still skipped when it requires success.
    let jobs: serde_yaml::Mapping = serde_yaml::from_str(
        "plan: {if: \"github.event_name != 'pull_request'\"}\n\
         dependent: {needs: plan}\n\
         always_runs: {needs: plan, if: 'always()'}\n\
         needs_success: {needs: [plan], if: \"${{ !cancelled() && needs.plan.result == 'success' }}\"}\n\
         maybe_success: {needs: [plan], if: \"vars.Y == 'on' || always() && needs.plan.result == 'success'\"}\n",
    )
    .expect("fixture parses");
    let mut memo = BTreeMap::new();
    assert!(!runs_on(&jobs, "plan", "pull_request", &mut memo));
    assert!(!runs_on(&jobs, "dependent", "pull_request", &mut memo));
    assert!(runs_on(&jobs, "always_runs", "pull_request", &mut memo));
    assert!(!runs_on(&jobs, "needs_success", "pull_request", &mut memo));
    assert!(runs_on(&jobs, "maybe_success", "pull_request", &mut memo));
    let mut memo = BTreeMap::new();
    assert!(runs_on(&jobs, "dependent", "push", &mut memo));
}

fn events_of(on_yaml: &str) -> BTreeSet<&'static str> {
    let doc: Value =
        serde_yaml::from_str(&format!("{on_yaml}\njobs: {{}}\n")).expect("fixture parses");
    gating_events(&on_events(&doc))
}

#[test]
fn trigger_reader_classifies_the_shapes_it_claims() {
    let set = |events: &[&'static str]| events.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(events_of("on: push"), set(&["push"]));
    assert_eq!(
        events_of("on: [pull_request, schedule]"),
        set(&["pull_request"])
    );
    assert_eq!(events_of("on:\n  push:\n    tags: ['v*']"), set(&[]));
    assert_eq!(
        events_of("on:\n  push:\n    tags: ['v*']\n    branches: ['merge-candidate/**']"),
        set(&["push"])
    );
    assert_eq!(events_of("on:\n  push:\n    branches: [main]"), set(&[]));
    assert_eq!(
        events_of("on:\n  push:\n    branches-ignore: ['merge-candidate*/**']"),
        set(&[])
    );
    // A stacked-only PR trigger still gates (coord lands stacked PRs).
    assert_eq!(
        events_of("on:\n  pull_request:\n    branches-ignore: [main]"),
        set(&["pull_request"])
    );
    assert_eq!(
        events_of("on:\n  pull_request_target: {}"),
        set(&["pull_request_target"])
    );
    assert_eq!(
        events_of("on:\n  workflow_dispatch: []\n  schedule: []"),
        set(&[])
    );
}

#[test]
#[should_panic(expected = "negated branch pattern")]
fn trigger_reader_refuses_a_negated_branch_pattern() {
    events_of("on:\n  pull_request:\n    branches: ['**', '!main']");
}

#[test]
#[should_panic(expected = "unclassified trigger `merge_group`")]
fn trigger_reader_refuses_an_unclassified_trigger() {
    events_of("on:\n  merge_group: {}");
}

#[test]
fn path_filter_check_flags_a_filter_the_target_does_not_observe() {
    let workflows: Vec<(String, Value)> = [
        (
            "lint",
            "on:\n  pull_request:\n    paths: ['scripts/**', '!scripts/skip.sh', 'gone/**']\n\
             jobs:\n  check: {}\n  scheduled: {if: \"github.event_name == 'schedule'\"}\n",
        ),
        (
            "docs",
            "on:\n  pull_request:\n    paths-ignore: ['src/**']\njobs:\n  build: {}\n",
        ),
        ("plain", "on: [pull_request]\njobs:\n  run: {}\n"),
    ]
    .into_iter()
    .map(|(stem, yaml)| {
        (
            stem.to_string(),
            serde_yaml::from_str(yaml).expect("fixture parses"),
        )
    })
    .collect();
    let files: Vec<String> = [
        "scripts/a.sh",
        "scripts/skip.sh",
        "src/main.rs",
        "README.md",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let triggers = path_filtered_triggers(&workflows);
    // `plain` has no path filter; `scheduled` never runs on a pull request.
    assert_eq!(triggers.len(), 2);
    let lint = triggers.iter().find(|t| t.stem == "lint").expect("lint");
    assert_eq!(lint.jobs, BTreeSet::from(["check".to_string()]));
    assert_eq!(
        lint.paths.as_deref(),
        Some(&["scripts/**".to_string(), "gone/**".to_string()][..]),
        "a `!` entry only narrows, so it is dropped"
    );

    let map = |lint: &[&str], docs: &[&str]| TargetMap {
        version: 1,
        global: vec!["README.md".into()],
        targets: vec![
            Target {
                name: "lint".into(),
                observes: lint.iter().map(|s| s.to_string()).collect(),
            },
            Target {
                name: "docs:build".into(),
                observes: docs.iter().map(|s| s.to_string()).collect(),
            },
        ],
    };

    // Mirrored filters: clean. README.md is covered through `global`.
    let ok = map(&["scripts/**", "gone/**"], &["**"]);
    assert_eq!(
        path_filter_violations(&ok, &triggers, &files),
        Vec::<String>::new()
    );

    // A file the filter fires on, listed nowhere in the target, is flagged.
    // So is the dropped negation's file: `!` is ignored, which is conservative.
    let narrow = map(&["scripts/a.sh", "gone/**"], &["**"]);
    let v = path_filter_violations(&narrow, &triggers, &files);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("scripts/skip.sh"), "{v:?}");

    // A filter entry that matches no file must be spelled in the target.
    let unspelled = map(&["scripts/**"], &["**"]);
    let v = path_filter_violations(&unspelled, &triggers, &files);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("`gone/**`"), "{v:?}");

    // `paths-ignore`: every file NOT ignored fires the job.
    let docs_narrow = map(&["scripts/**", "gone/**"], &["scripts/**"]);
    let v = path_filter_violations(&docs_narrow, &triggers, &files);
    assert_eq!(v.len(), 0, "README.md is global, src/** is ignored: {v:?}");
    let docs_blind = map(&["scripts/**", "gone/**"], &["nothing/**"]);
    let v = path_filter_violations(&docs_blind, &triggers, &files);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("`docs:build`"), "{v:?}");
}

#[test]
fn github_glob_matches_nested_files_for_a_double_star_extension() {
    assert_eq!(github_glob("**.ps1"), "**/*.ps1");
    assert_eq!(github_glob("docs/**.md"), "docs/**/*.md");
    assert_eq!(github_glob("scripts/**"), "scripts/**");
    assert_eq!(github_glob("**/*.ps1"), "**/*.ps1");
    assert_eq!(github_glob("a.yml"), "a.yml");
    assert_eq!(github_glob("src/**/x/**.ts"), "src/**/x/**/*.ts");
    // The shape glob_match alone gets wrong, and the one this fixes.
    assert!(!glob_match::glob_match("**.ps1", "scripts/a.ps1"));
    assert!(glob_match::glob_match(
        &github_glob("**.ps1"),
        "scripts/a.ps1"
    ));
    assert!(glob_match::glob_match(&github_glob("**.ps1"), "a.ps1"));

    // End to end: a `**.ps1` filter fires on a nested script, so a target
    // that observes only the root cannot pass.
    let workflows: Vec<(String, Value)> = vec![(
        "ps".to_string(),
        serde_yaml::from_str("on:\n  pull_request:\n    paths: ['**.ps1']\njobs:\n  parse: {}\n")
            .expect("fixture parses"),
    )];
    let files = vec!["tools/deep/x.ps1".to_string()];
    let triggers = path_filtered_triggers(&workflows);
    let map = |observes: &[&str]| TargetMap {
        version: 1,
        global: vec![],
        targets: vec![Target {
            name: "ps".into(),
            observes: observes.iter().map(|s| s.to_string()).collect(),
        }],
    };
    assert_eq!(
        path_filter_violations(&map(&["**/*.ps1"]), &triggers, &files),
        Vec::<String>::new()
    );
    let v = path_filter_violations(&map(&["*.ps1"]), &triggers, &files);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("tools/deep/x.ps1"), "{v:?}");
}

#[test]
#[should_panic(expected = "negated `paths-ignore` entry")]
fn path_filter_reader_refuses_a_negated_paths_ignore_entry() {
    let workflows: Vec<(String, Value)> = vec![(
        "w".to_string(),
        serde_yaml::from_str(
            "on:\n  pull_request:\n    paths-ignore: ['!docs/**']\njobs:\n  j: {}\n",
        )
        .expect("fixture parses"),
    )];
    path_filtered_triggers(&workflows);
}

#[test]
#[should_panic(expected = "inside a path segment")]
fn github_glob_refuses_a_double_star_inside_a_segment() {
    github_glob("scripts/test**.sh");
}
