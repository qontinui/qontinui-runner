//! Phase 3 post-install **observation assembly**: read the actual post-install
//! lockfile (→ `observed_pinned`), run a native audit (→ `observed_cves` +
//! `observed_audit_ran`), run the per-affected-file typecheck loopback against
//! the runner's OWN `/code-semantics/typecheck` endpoint (→ `observed_typecheck`
//! + `typecheck_ran`), and capture the manifest/lockfile pre/post blob shas for
//! the Ξ_FS push.
//!
//! Honesty rules (mirror Phase 2):
//! - `observed_pinned` empty ⇒ coord reads the dep-graph dimension as a gap, not
//!   a fabricated empty set.
//! - `observed_audit_ran` true ONLY when the audit subprocess ran AND produced
//!   parseable JSON.
//! - `typecheck_ran` true ONLY when the loop actually ran ≥1 affected file.
//!
//! Everything here that shells out is best-effort: a missing tool / unparseable
//! output ⇒ the signal is simply absent. The verify CALL itself (in [`super`])
//! is load-bearing; this module only assembles its inputs.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use tracing::debug;

use super::parsers::{audit, lockfile};
use super::pm;
use super::registry_creds::RegistryEnv;
use super::types::{Cve, FsObservation, PackageManager, TypecheckResult};
use crate::agent_worktree::fs_observer::blob_sha_for_file;

/// Cap on the number of affected files the typecheck loopback runs. A full
/// `cargo check` / `mypy` per file is expensive; the importer scan already
/// bounds the candidate set, but a wide install (many imported packages) can
/// still produce a long list. 25 covers the realistic blast radius while
/// bounding the route's wall-clock.
pub const MAX_TYPECHECK_FILES: usize = 25;

/// Per-typecheck-loopback HTTP timeout. A `cargo check` over a cold crate can be
/// slow; this bounds one call so a hung checker can't pin the route.
const TYPECHECK_TIMEOUT: Duration = Duration::from_secs(180);

/// The manifest + lockfile filenames an install of `pm` touches, in
/// (manifest, lockfile) order. Used both for the FS-observation paths and as the
/// post-install lockfile to parse. pip has no lockfile (its observed pins come
/// from `pip list`); poetry uses `poetry.lock`.
pub fn touched_paths(pm: PackageManager) -> Vec<&'static str> {
    match pm {
        PackageManager::Npm => vec!["package.json", "package-lock.json"],
        PackageManager::Yarn => vec!["package.json", "yarn.lock"],
        PackageManager::Pnpm => vec!["package.json", "pnpm-lock.yaml"],
        PackageManager::Cargo => vec!["Cargo.toml", "Cargo.lock"],
        PackageManager::Pip => vec!["requirements.txt"],
        PackageManager::Poetry => vec!["pyproject.toml", "poetry.lock"],
    }
}

/// Capture pre-install blob shas for every touched path that currently exists.
/// A path absent now (the install will create it) is simply omitted ⇒ `pre_sha`
/// None in the eventual observation. Pure read; reuses `fs_observer`'s
/// `blob_sha_for_file` (the same `git hash-object` semantics coord's Ξ_FS ingest
/// expects). Returns `path → pre_sha`.
pub fn capture_pre_shas(pm: PackageManager, repo_path: &Path) -> BTreeMap<String, Option<String>> {
    let mut out = BTreeMap::new();
    for rel in touched_paths(pm) {
        let abs = repo_path.join(rel);
        // Ok(None) ⇒ file doesn't exist yet (install will create it).
        match blob_sha_for_file(&abs) {
            Ok(sha) => {
                out.insert(rel.to_string(), sha);
            }
            Err(e) => {
                debug!(path = %rel, error = %e, "install-effects: pre-sha read failed — skip");
            }
        }
    }
    out
}

/// Build the post-install [`FsObservation`] list by re-reading each touched path
/// and pairing its post-sha with the captured pre-sha. A path that exists in
/// NEITHER pre nor post is dropped (nothing happened); a deletion (existed pre,
/// gone post) is dropped (coord requires a `post_sha`). `hunks` is an empty
/// array (we don't compute manifest hunks here — the post-image sha is the
/// load-bearing Ξ_FS signal coord composes on).
pub fn build_fs_observations(
    pm: PackageManager,
    repo_path: &Path,
    pre_shas: &BTreeMap<String, Option<String>>,
) -> Vec<FsObservation> {
    let mut out = Vec::new();
    for rel in touched_paths(pm) {
        let abs = repo_path.join(rel);
        let post_sha = match blob_sha_for_file(&abs) {
            Ok(Some(sha)) => sha,
            Ok(None) => continue, // absent post-install (never created / deleted)
            Err(e) => {
                debug!(path = %rel, error = %e, "install-effects: post-sha read failed — skip");
                continue;
            }
        };
        // pre_sha: flatten the Option<Option<String>> — outer None ⇒ path wasn't
        // checked pre (treat as newly-created), inner None ⇒ didn't exist pre.
        let pre_sha = pre_shas.get(rel).cloned().flatten();
        out.push(FsObservation {
            path: rel.to_string(),
            pre_sha,
            post_sha,
            hunks: serde_json::Value::Array(Vec::new()),
        });
    }
    out
}

/// Read the post-install pinned set from the lockfile (or `pip list`). Empty map
/// ⇒ honest "could not read" (coord treats dep-graph as a gap). Each parser is
/// total over garbage (Phase 2).
pub fn observe_pinned(
    pm: PackageManager,
    repo_path: &Path,
    env: &RegistryEnv,
) -> BTreeMap<String, String> {
    match pm {
        PackageManager::Npm | PackageManager::Yarn => read_text(repo_path, "package-lock.json")
            .map(|t| lockfile::parse_package_lock(&t))
            .unwrap_or_default(),
        PackageManager::Pnpm => read_text(repo_path, "pnpm-lock.yaml")
            .map(|t| lockfile::parse_pnpm_lock(&t))
            .unwrap_or_default(),
        PackageManager::Cargo => read_text(repo_path, "Cargo.lock")
            .map(|t| lockfile::parse_cargo_lock(&t))
            .unwrap_or_default(),
        PackageManager::Pip | PackageManager::Poetry => {
            // pip/poetry: no universal lockfile we parse here — run
            // `pip list --format=json` for the honest installed inventory.
            run_pip_list(repo_path, env)
                .map(|t| lockfile::parse_pip_list(&t))
                .unwrap_or_default()
        }
    }
}

fn read_text(repo_path: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(repo_path.join(name)).ok()
}

/// Run `pip list --format=json` in `repo_path`, capturing stdout. `None` on a
/// spawn failure (pip absent) or timeout.
fn run_pip_list(repo_path: &Path, env: &RegistryEnv) -> Option<String> {
    use crate::pm_detect::pm_command;
    let mut cmd = pm_command("pip");
    cmd.current_dir(repo_path).args(["list", "--format=json"]);
    // `pip list` doesn't need registry auth, but inject for symmetry so a
    // private index URL is honored if pip resolves anything.
    env.apply(&mut cmd);
    let argv = vec![
        "pip".to_string(),
        "list".to_string(),
        "--format=json".to_string(),
    ];
    capture_stdout(cmd, &argv)
}

/// Run the post-install native audit and parse it into `(cves, ran)`. `ran` is
/// true iff the audit subprocess produced parseable JSON (same honesty rule as
/// Phase 2's `run_audit`). A missing tool ⇒ `(empty, false)`.
pub fn observe_audit(pm: PackageManager, repo_path: &Path, env: &RegistryEnv) -> (Vec<Cve>, bool) {
    let Some((cmd, argv)) = pm::build_audit_command(pm, repo_path, env) else {
        return (Vec::new(), false);
    };
    let Some((stdout, stderr)) = capture_both(cmd, &argv) else {
        return (Vec::new(), false);
    };
    let body = if stdout.trim().is_empty() {
        &stderr
    } else {
        &stdout
    };
    if body.trim().is_empty() {
        return (Vec::new(), false);
    }
    let cves = match pm {
        PackageManager::Npm | PackageManager::Pnpm => audit::parse_npm_audit(body),
        PackageManager::Cargo => audit::parse_cargo_audit(body),
        PackageManager::Pip => audit::parse_pip_audit(body),
        PackageManager::Yarn | PackageManager::Poetry => return (Vec::new(), false),
    };
    let parseable = serde_json::from_str::<serde_json::Value>(body).is_ok();
    (cves, parseable)
}

// ---------------------------------------------------------------------------
// typecheck loopback
// ---------------------------------------------------------------------------

/// Run the per-affected-file typecheck loopback against the runner's OWN
/// `/code-semantics/typecheck` endpoint at `loopback_base` (e.g.
/// `http://localhost:9876`). Returns `(results, ran)` where `ran` is true iff
/// the loop executed ≥1 file. Capped at [`MAX_TYPECHECK_FILES`].
///
/// `escalation_overridden` ⇒ each result's `provenance` is suffixed with
/// `+overridden` so coord's audit trail records that the verify rode an
/// operator-overridden escalation.
///
/// Files whose extension isn't `.rs`/`.py` are still POSTed (the endpoint
/// dispatches; a TS file goes through the language-service path) but a file the
/// endpoint can't check (engine unavailable / cold) yields no result — it is
/// simply not added (honest: we couldn't check it). The loop still counts as
/// `ran` because it attempted.
pub async fn run_typecheck_loopback(
    loopback_base: &str,
    repo_path: &Path,
    affected_files: &[String],
    escalation_overridden: bool,
) -> (Vec<TypecheckResult>, bool) {
    if affected_files.is_empty() {
        return (Vec::new(), false);
    }
    let client = match reqwest::Client::builder()
        .timeout(TYPECHECK_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            debug!("install-effects: typecheck loopback client build failed: {e}");
            return (Vec::new(), false);
        }
    };
    let url = format!(
        "{}/code-semantics/typecheck",
        loopback_base.trim_end_matches('/')
    );

    let mut results = Vec::new();
    let mut ran = false;
    for rel in affected_files.iter().take(MAX_TYPECHECK_FILES) {
        ran = true; // we attempted this file — the loop ran
                    // The endpoint resolves the scope from the (absolute) file path.
        let abs = repo_path.join(rel);
        let file_arg = abs.to_string_lossy().to_string();
        let body = serde_json::json!({ "file": file_arg });
        let resp = match client.post(&url).json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                debug!(file = %rel, error = %e, "install-effects: typecheck loopback send failed");
                continue;
            }
        };
        if !resp.status().is_success() {
            debug!(file = %rel, status = resp.status().as_u16(), "install-effects: typecheck loopback non-2xx");
            continue;
        }
        let env: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                debug!(file = %rel, error = %e, "install-effects: typecheck loopback decode failed");
                continue;
            }
        };
        if let Some(tc) = typecheck_from_envelope(rel, &env, escalation_overridden) {
            results.push(tc);
        }
    }
    (results, ran)
}

/// Map a `/code-semantics/typecheck` envelope into a coord [`TypecheckResult`].
/// `pass` ⇐ `result.ok` (false when absent — a checker that couldn't decide is
/// not a pass); `coverage` ⇐ `result.coverage` else envelope `coverage`;
/// `provenance` ⇐ envelope `provenance` (+`+overridden` suffix when the
/// escalation was overridden); `diagnostics` ⇐ stringified `result.errors`.
/// Returns `None` for an `indexing`/`engine_unavailable` envelope (coverage 0,
/// not a real check) so a cold scope reads as "not checked", not a false pass.
fn typecheck_from_envelope(
    rel: &str,
    env: &serde_json::Value,
    escalation_overridden: bool,
) -> Option<TypecheckResult> {
    let provenance_raw = env.get("provenance").and_then(|p| p.as_str()).unwrap_or("");
    // An indexing / unavailable envelope is NOT a real typecheck — skip it so it
    // doesn't read as a (false) pass at coverage 0.
    if matches!(provenance_raw, "indexing" | "engine_unavailable") {
        return None;
    }
    let result = env.get("result");
    let pass = result
        .and_then(|r| r.get("ok"))
        .and_then(|o| o.as_bool())
        .unwrap_or(false);
    let coverage = result
        .and_then(|r| r.get("coverage"))
        .and_then(|c| c.as_f64())
        .or_else(|| env.get("coverage").and_then(|c| c.as_f64()))
        .unwrap_or(0.0);
    let diagnostics = result
        .and_then(|r| r.get("errors"))
        .and_then(|e| e.as_array())
        .map(|arr| {
            arr.iter()
                .map(|d| match d.as_str() {
                    Some(s) => s.to_string(),
                    None => d.to_string(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let provenance = if provenance_raw.is_empty() {
        None
    } else if escalation_overridden {
        Some(format!("{provenance_raw}+overridden"))
    } else {
        Some(provenance_raw.to_string())
    };
    Some(TypecheckResult {
        file: rel.to_string(),
        pass,
        provenance,
        coverage,
        diagnostics,
    })
}

// ---------------------------------------------------------------------------
// subprocess capture (mirrors dry_run_invoke::capture; kept local + thin)
// ---------------------------------------------------------------------------

fn capture_stdout(cmd: std::process::Command, argv: &[String]) -> Option<String> {
    capture_both(cmd, argv).map(|(out, _)| out)
}

/// Spawn `cmd` with a bounded timeout, capturing stdout+stderr. `None` on spawn
/// failure / timeout. Exit code is intentionally NOT a gate (audit/list tools
/// exit non-zero on findings).
fn capture_both(mut cmd: std::process::Command, argv: &[String]) -> Option<(String, String)> {
    use std::io::Read;
    use std::process::Stdio;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            debug!(
                "install-effects: probe `{}` did not spawn (tool absent?): {e}",
                argv.first().map(String::as_str).unwrap_or("?")
            );
            return None;
        }
    };
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();

    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
    let mut stdout = String::new();
    if let Some(mut p) = stdout_pipe.take() {
        let _ = p.read_to_string(&mut stdout);
    }
    let mut stderr = String::new();
    if let Some(mut p) = stderr_pipe.take() {
        let _ = p.read_to_string(&mut stderr);
    }
    Some((stdout, stderr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn touched_paths_per_pm() {
        assert_eq!(
            touched_paths(PackageManager::Npm),
            vec!["package.json", "package-lock.json"]
        );
        assert_eq!(
            touched_paths(PackageManager::Cargo),
            vec!["Cargo.toml", "Cargo.lock"]
        );
        assert_eq!(touched_paths(PackageManager::Pip), vec!["requirements.txt"]);
    }

    #[test]
    fn observe_pinned_reads_lockfile() {
        let d = tempdir().unwrap();
        std::fs::write(
            d.path().join("Cargo.lock"),
            "[[package]]\nname = \"serde\"\nversion = \"1.0.1\"\n",
        )
        .unwrap();
        let pins = observe_pinned(PackageManager::Cargo, d.path(), &RegistryEnv::empty());
        assert_eq!(pins.get("serde"), Some(&"1.0.1".to_string()));
    }

    #[test]
    fn observe_pinned_missing_lockfile_is_empty() {
        let d = tempdir().unwrap();
        assert!(observe_pinned(PackageManager::Npm, d.path(), &RegistryEnv::empty()).is_empty());
    }

    #[test]
    fn pre_post_shas_capture_modification() {
        let d = tempdir().unwrap();
        // pre: a package.json exists; no lockfile yet
        std::fs::write(d.path().join("package.json"), "{\"name\":\"x\"}").unwrap();
        let pre = capture_pre_shas(PackageManager::Npm, d.path());
        // package.json has a pre sha; lockfile absent (Some(None))
        assert!(matches!(pre.get("package.json"), Some(Some(_))));
        assert!(matches!(pre.get("package-lock.json"), Some(None)));

        // install: modify manifest + create the lockfile
        std::fs::write(d.path().join("package.json"), "{\"name\":\"x\",\"v\":2}").unwrap();
        std::fs::write(d.path().join("package-lock.json"), "{}").unwrap();

        let obs = build_fs_observations(PackageManager::Npm, d.path(), &pre);
        let by: BTreeMap<&str, &FsObservation> = obs.iter().map(|o| (o.path.as_str(), o)).collect();
        // manifest: pre_sha present, post differs
        let man = by["package.json"];
        assert!(man.pre_sha.is_some());
        assert_ne!(man.pre_sha.as_deref(), Some(man.post_sha.as_str()));
        // lockfile: newly created ⇒ no pre_sha
        let lock = by["package-lock.json"];
        assert!(lock.pre_sha.is_none());
        assert!(!lock.post_sha.is_empty());
    }

    #[test]
    fn fs_observations_skip_absent_paths() {
        let d = tempdir().unwrap();
        // nothing on disk ⇒ no observations
        let pre = capture_pre_shas(PackageManager::Cargo, d.path());
        let obs = build_fs_observations(PackageManager::Cargo, d.path(), &pre);
        assert!(obs.is_empty());
    }

    #[test]
    fn typecheck_envelope_maps_pass_and_overridden_suffix() {
        let env = serde_json::json!({
            "provenance": "cargo-check",
            "coverage": 1.0,
            "result": { "ok": true, "errors": [], "coverage": 1.0 }
        });
        let tc = typecheck_from_envelope("src/a.rs", &env, false).unwrap();
        assert!(tc.pass);
        assert_eq!(tc.provenance.as_deref(), Some("cargo-check"));
        assert_eq!(tc.coverage, 1.0);

        let tc2 = typecheck_from_envelope("src/a.rs", &env, true).unwrap();
        assert_eq!(tc2.provenance.as_deref(), Some("cargo-check+overridden"));
    }

    #[test]
    fn typecheck_envelope_failing_carries_diagnostics() {
        let env = serde_json::json!({
            "provenance": "mypy",
            "coverage": 1.0,
            "result": {
                "ok": false,
                "errors": ["a.py:3: error: bad type", { "msg": "structured" }],
                "coverage": 1.0
            }
        });
        let tc = typecheck_from_envelope("a.py", &env, false).unwrap();
        assert!(!tc.pass);
        assert_eq!(tc.diagnostics.len(), 2);
        assert!(tc.diagnostics[0].contains("bad type"));
    }

    #[test]
    fn typecheck_envelope_indexing_is_skipped() {
        let env = serde_json::json!({
            "provenance": "indexing",
            "coverage": 0.0,
            "result": {}
        });
        assert!(typecheck_from_envelope("a.rs", &env, false).is_none());
        let env2 = serde_json::json!({ "provenance": "engine_unavailable", "result": {} });
        assert!(typecheck_from_envelope("a.rs", &env2, false).is_none());
    }
}
