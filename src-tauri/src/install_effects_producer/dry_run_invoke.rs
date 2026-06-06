//! Orchestration glue between the install-effects route and the **pure**
//! Phase-2 parsers ([`super::parsers`]): shell out each ecosystem's read-only
//! dry-run + native-audit probes (same `spawn_blocking` / `CREATE_NO_WINDOW`
//! pattern the real install uses), capture their output, and feed the captured
//! text/JSON to the pure parsers.
//!
//! This is the **only** impure part of Phase 2 — everything it produces comes
//! from a pure parser over captured bytes. It is deliberately thin so the
//! parsing stays unit-testable without a toolchain.
//!
//! ## Honest degradation
//!
//! Every probe is best-effort. A missing tool, a non-zero exit, a timeout, or
//! unparseable output ⇒ that signal is simply **absent** (the dry-run stays
//! `None`, `security_audited` stays `false`, CVEs stay empty) — never a
//! fabricated plan. coord then degrades the affected dimension to honest
//! `Unknown`. No probe failure ever blocks the install path.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tracing::debug;

use super::parsers::{audit, dry_run};
use super::pm::{self, DryRunStream};
use super::registry_creds::RegistryEnv;
use super::types::{Cve, DryRunData, PackageManager, PackageSpecInput};

/// Bounded budget for a single read-only probe (dry-run or audit). Tighter than
/// the install budget: a `--dry-run` resolves the graph but writes nothing, and
/// `npm/cargo audit` is a metadata fetch.
const PROBE_TIMEOUT: Duration = Duration::from_secs(180);

/// The Phase-2 ground truth assembled from the probes, ready to splice into the
/// coord [`super::types::DeclareRequest`].
#[derive(Debug, Default, Clone)]
pub struct ProducedGroundTruth {
    /// The predicted plan (`None` ⇒ honest unknown — no parsed dry-run).
    pub dry_run: Option<DryRunData>,
    /// True iff a native audit subprocess actually ran to completion (so an
    /// empty CVE list reads as "clean" rather than "not run"). False on a
    /// missing tool / spawn failure / non-zero-with-no-output.
    pub security_audited: bool,
}

/// Run the dry-run + audit probes for `pm` in `repo_path` and assemble the
/// coord ground truth. Pure-parser-backed; the subprocess plumbing here is the
/// documented impure seam. Never panics, never blocks on failure.
pub fn produce_ground_truth(
    pm: PackageManager,
    repo_path: &Path,
    packages: &[PackageSpecInput],
    dev: bool,
    env: &RegistryEnv,
) -> ProducedGroundTruth {
    let direct_names: Vec<String> = packages.iter().map(|p| p.name.clone()).collect();

    let mut dry = run_dry_run(pm, repo_path, packages, dev, &direct_names, env);

    // Fold predicted CVEs into the dry-run plan (coord reads
    // `dry_run.predicted_cves` for the security dimension) and flag the audit.
    let (cves, security_audited) = run_audit(pm, repo_path, env);
    if !cves.is_empty() {
        let d = dry.get_or_insert_with(DryRunData::default);
        d.predicted_cves = cves;
    }

    ProducedGroundTruth {
        dry_run: dry,
        security_audited,
    }
}

/// Run the dry-run probe and parse it into a [`DryRunData`]. Handles the three
/// stream shapes ([`DryRunStream`]). Returns `None` on any failure or an
/// unrecognized payload.
fn run_dry_run(
    pm: PackageManager,
    repo_path: &Path,
    packages: &[PackageSpecInput],
    dev: bool,
    direct_names: &[String],
    env: &RegistryEnv,
) -> Option<DryRunData> {
    let (cmd, argv, stream) = pm::build_dry_run_command(pm, repo_path, packages, dev, env)?;

    match stream {
        DryRunStream::Stdout => {
            let out = capture(cmd, &argv)?;
            match pm {
                PackageManager::Npm => dry_run::parse_npm_dry_run(&out.stdout, direct_names),
                PackageManager::Pip => dry_run::parse_pip_dry_run(&out.stdout, direct_names),
                _ => None,
            }
        }
        DryRunStream::Stderr => {
            let out = capture(cmd, &argv)?;
            match pm {
                PackageManager::Cargo => dry_run::parse_cargo_dry_run(&out.stderr),
                _ => None,
            }
        }
        DryRunStream::LockfileDiff => {
            // pnpm: snapshot the lockfile, run `--lockfile-only`, re-read it,
            // and diff. The mutation touches only pnpm-lock.yaml; we restore it
            // afterward so the dry-run leaves no trace (honest "dry").
            run_pnpm_lockfile_diff(cmd, &argv, repo_path, direct_names)
        }
    }
}

/// pnpm lockfile-diff dry-run: read `pnpm-lock.yaml` before, run the
/// `--lockfile-only` mutation, read it after, diff, then restore the original
/// (or delete a freshly-created one) so the probe is non-destructive.
fn run_pnpm_lockfile_diff(
    cmd: std::process::Command,
    argv: &[String],
    repo_path: &Path,
    direct_names: &[String],
) -> Option<DryRunData> {
    let lock_path = repo_path.join("pnpm-lock.yaml");
    let before = std::fs::read_to_string(&lock_path).unwrap_or_default();
    let existed = lock_path.exists();

    let _ = capture(cmd, argv)?; // exit code not load-bearing; we read the file

    let after = std::fs::read_to_string(&lock_path).unwrap_or_default();

    // Restore the pre-probe state: rewrite the original, or remove a file the
    // probe created. Best-effort — a restore failure only leaves a refreshed
    // lockfile, never corrupts state.
    if existed {
        let _ = std::fs::write(&lock_path, &before);
    } else if lock_path.exists() {
        let _ = std::fs::remove_file(&lock_path);
    }

    dry_run::parse_pnpm_dry_run(&before, &after, direct_names)
}

/// Run the native-audit probe and parse it. Returns `(cves, ran)` where `ran`
/// is true iff the audit subprocess executed AND produced parseable output we
/// could read as a (possibly-empty) advisory set — the signal coord uses to
/// distinguish "clean" from "not run". A missing tool / spawn failure ⇒
/// `(empty, false)`.
fn run_audit(pm: PackageManager, repo_path: &Path, env: &RegistryEnv) -> (Vec<Cve>, bool) {
    let Some((cmd, argv)) = pm::build_audit_command(pm, repo_path, env) else {
        return (Vec::new(), false);
    };
    let Some(out) = capture(cmd, &argv) else {
        return (Vec::new(), false);
    };
    // Audit tools exit non-zero WHEN they find advisories — exit code is NOT a
    // failure signal. We treat "produced output we can parse" as "ran". An
    // empty stdout (tool printed nothing) ⇒ not a usable audit.
    let body = if out.stdout.trim().is_empty() {
        &out.stderr
    } else {
        &out.stdout
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
    // `ran` is true whenever the body parsed as the expected JSON shape OR was
    // a clean empty advisory set. We can't cheaply distinguish "valid clean
    // JSON" from "garbage" post-parse (both ⇒ empty vec), so require the body to
    // at least deserialize as JSON for npm/cargo/pip before claiming `ran`.
    let parseable = serde_json::from_str::<serde_json::Value>(body).is_ok();
    (cves, parseable)
}

/// Captured probe output.
struct ProbeOut {
    stdout: String,
    stderr: String,
}

/// Spawn `cmd` with a bounded timeout, capturing stdout+stderr. Returns `None`
/// on a spawn failure (tool missing), a join error, or a timeout. The exit code
/// is intentionally NOT a gate — audit/dry-run tools exit non-zero on findings.
fn capture(mut cmd: std::process::Command, argv: &[String]) -> Option<ProbeOut> {
    use std::io::Read;
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

    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    debug!(
                        "install-effects: probe `{}` exceeded {}s — killed",
                        argv.first().map(String::as_str).unwrap_or("?"),
                        PROBE_TIMEOUT.as_secs()
                    );
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
    Some(ProbeOut { stdout, stderr })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The subprocess paths need a real toolchain; the pure parsers are tested in
    // `super::parsers`. Here we only assert the command-shape seam: yarn/poetry
    // have no probe (honest absent), and the audit/dry-run builders are wired.

    /// Empty registry env for the builder-shape tests.
    fn env() -> RegistryEnv {
        RegistryEnv::empty()
    }

    #[test]
    fn yarn_poetry_have_no_dry_run_probe() {
        let d = tempfile::tempdir().unwrap();
        assert!(
            pm::build_dry_run_command(PackageManager::Yarn, d.path(), &[], false, &env()).is_none()
        );
        assert!(
            pm::build_dry_run_command(PackageManager::Poetry, d.path(), &[], false, &env())
                .is_none()
        );
    }

    #[test]
    fn cargo_pip_lockfile_refresh_has_no_add_probe() {
        let d = tempfile::tempdir().unwrap();
        // empty specs ⇒ no `add` dry-run for cargo/pip
        assert!(
            pm::build_dry_run_command(PackageManager::Cargo, d.path(), &[], false, &env())
                .is_none()
        );
        assert!(
            pm::build_dry_run_command(PackageManager::Pip, d.path(), &[], false, &env()).is_none()
        );
    }

    #[test]
    fn npm_dry_run_probe_shape() {
        let d = tempfile::tempdir().unwrap();
        let pkgs = vec![PackageSpecInput {
            name: "left-pad".into(),
            version_req: None,
        }];
        let (_c, argv, stream) =
            pm::build_dry_run_command(PackageManager::Npm, d.path(), &pkgs, false, &env()).unwrap();
        assert_eq!(stream, DryRunStream::Stdout);
        assert!(argv.contains(&"--dry-run".to_string()));
        assert!(argv.contains(&"--json".to_string()));
    }

    #[test]
    fn audit_builders_present_for_supported_pms() {
        let d = tempfile::tempdir().unwrap();
        for pm in [
            PackageManager::Npm,
            PackageManager::Pnpm,
            PackageManager::Cargo,
            PackageManager::Pip,
        ] {
            assert!(
                pm::build_audit_command(pm, d.path(), &env()).is_some(),
                "{pm:?}"
            );
        }
        assert!(pm::build_audit_command(PackageManager::Yarn, d.path(), &env()).is_none());
        assert!(pm::build_audit_command(PackageManager::Poetry, d.path(), &env()).is_none());
    }
}
