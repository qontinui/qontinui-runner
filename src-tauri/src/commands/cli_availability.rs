//! `cli_profile_availability` — is a CLI profile's program installed where the
//! runner can run it? (plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 6).
//!
//! The Terminal page lists every served CLI profile as a launch choice. A
//! provider whose binary is absent is listed DISABLED, with the reason and the
//! profile's per-OS install command — never hidden. A probe that could not
//! decide is reported UNKNOWN, not absent: a CLI that timed out on `--version`
//! may be perfectly usable.
//!
//! The probe has the shape `commands/ai_settings.rs` uses for its CLI
//! connection tests (`<program> --version`; `cmd /c` on Windows so npm's
//! `.cmd` shims run), bounded by [`crate::process_helpers::run_probe_quiet`] so
//! a wedged CLI cannot hang the command.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::process_helpers::{DegradeReason, ProbeOutcome};

/// Budget for one `<program> --version` probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// One profile's availability. `available` is three-valued: `Some(true)` —
/// the program ran and answered `--version`; `Some(false)` — the program is
/// not on the runner's `PATH`; `None` — UNKNOWN (found, but the probe failed or
/// timed out). `error` says why whenever `available` is not `Some(true)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliAvailability {
    /// The profile id probed.
    pub id: String,
    /// See the type docs.
    pub available: Option<bool>,
    /// First non-empty line of the `--version` output, when it ran.
    pub version: Option<String>,
    /// Why the CLI is not (known to be) available.
    pub error: Option<String>,
}

/// Probe the program of the CLI profile `id`. An unknown id is an error, not
/// an "absent" answer: there is no program to look for.
#[tauri::command]
pub async fn cli_profile_availability(id: String) -> Result<CliAvailability, String> {
    let profile = qontinui_runner_lib::cli_profile::profile_for(&id)
        .ok_or_else(|| format!("no CLI profile {id:?}"))?;
    let program = profile
        .programs
        .first()
        .cloned()
        .ok_or_else(|| format!("the {id:?} profile names no program"))?;
    tokio::task::spawn_blocking(move || probe(&id, &program))
        .await
        .map_err(|e| format!("availability probe for profile {:?} did not complete: {e}", profile.id))
}

/// Locate `program` on the runner's `PATH`, then run its `--version`.
fn probe(id: &str, program: &str) -> CliAvailability {
    let path = std::env::var_os("PATH");
    let Some(found) = find_on_path(program, path.as_deref()) else {
        return classify(id, program, None, None);
    };
    let outcome = crate::process_helpers::run_probe_quiet(
        version_command(&found),
        PROBE_TIMEOUT,
        "cli_availability: --version probe",
    );
    classify(id, program, Some(&found), Some(outcome))
}

/// `<found> --version`, through `cmd /c` on Windows so a `.cmd` shim runs.
fn version_command(found: &Path) -> std::process::Command {
    #[cfg(target_os = "windows")]
    {
        let mut cmd = crate::process_helpers::cmd_no_window();
        cmd.arg("/c").arg(found).arg("--version");
        cmd
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut cmd = crate::process_helpers::no_window(found);
        cmd.arg("--version");
        cmd
    }
}

/// The candidate file names for `program` in one directory: the bare name,
/// plus the launcher suffixes Windows resolves.
fn candidate_names(program: &str) -> Vec<String> {
    let mut names = vec![program.to_string()];
    if cfg!(target_os = "windows") {
        names.extend([".exe", ".cmd", ".bat", ".com"].map(|ext| format!("{program}{ext}")));
    }
    names
}

/// The first file named `program` (see [`candidate_names`]) in a `PATH`
/// directory, skipping the runner's own identity/install shim dirs — a shim
/// there wraps the real CLI and proves nothing about it being installed.
pub fn find_on_path(program: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    use crate::install_effects_producer::intercept::shim_materializer::{
        IDENTITY_DIR_PREFIX, SHIM_DIR_PREFIX,
    };
    let is_shim_dir = |dir: &Path| {
        dir.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(IDENTITY_DIR_PREFIX) || n.starts_with(SHIM_DIR_PREFIX))
    };
    let names = candidate_names(program);
    std::env::split_paths(path?)
        .filter(|dir| !dir.as_os_str().is_empty() && !is_shim_dir(dir))
        .find_map(|dir| {
            names
                .iter()
                .map(|name| dir.join(name))
                .find(|candidate| candidate.is_file())
        })
}

/// The verdict for one probe. Pure, so every arm is testable without a CLI.
fn classify(
    id: &str,
    program: &str,
    found: Option<&Path>,
    outcome: Option<ProbeOutcome>,
) -> CliAvailability {
    let verdict = |available, version, error| CliAvailability {
        id: id.to_string(),
        available,
        version,
        error,
    };
    let (Some(found), Some(outcome)) = (found, outcome) else {
        return verdict(
            Some(false),
            None,
            Some(format!("`{program}` is not on the runner's PATH")),
        );
    };
    match outcome {
        ProbeOutcome::Captured(stdout) => {
            let text = String::from_utf8_lossy(&stdout);
            let version = text
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_string);
            verdict(Some(true), version, None)
        }
        ProbeOutcome::Degraded(reason) => {
            let why = match reason {
                DegradeReason::Status => "exited non-zero".to_string(),
                DegradeReason::SpawnError => "could not be started".to_string(),
                DegradeReason::TimedOut { .. } => {
                    format!("did not answer within {}s", PROBE_TIMEOUT.as_secs())
                }
                DegradeReason::Truncated(_) => "produced output that could not be read".to_string(),
            };
            verdict(
                None,
                None,
                Some(format!("`{} --version` {why}", found.display())),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_program_is_unavailable_with_a_reason() {
        let v = classify("codex", "codex", None, None);
        assert_eq!(v.available, Some(false));
        assert!(v.error.unwrap().contains("not on the runner's PATH"));
        assert_eq!(v.version, None);
    }

    #[test]
    fn a_version_answer_is_available_with_its_first_line() {
        let v = classify(
            "codex",
            "codex",
            Some(Path::new("/usr/bin/codex")),
            Some(ProbeOutcome::Captured(b"\n codex-cli 0.159.1 \nmore\n".to_vec())),
        );
        assert_eq!(v.available, Some(true));
        assert_eq!(v.version.as_deref(), Some("codex-cli 0.159.1"));
        assert_eq!(v.error, None);
    }

    /// A found binary whose probe failed is UNKNOWN — never "absent".
    #[test]
    fn a_failed_probe_is_unknown_not_absent() {
        for reason in [
            DegradeReason::Status,
            DegradeReason::SpawnError,
            DegradeReason::TimedOut {
                pid: 1,
                reaped: true,
            },
        ] {
            let v = classify(
                "codex",
                "codex",
                Some(Path::new("/usr/bin/codex")),
                Some(ProbeOutcome::Degraded(reason)),
            );
            assert_eq!(v.available, None);
            assert!(v.error.unwrap().contains("--version"));
        }
    }

    #[test]
    fn path_search_finds_the_program_and_skips_runner_shim_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let shim = tmp.path().join("qontinui-identity-abc");
        let real = tmp.path().join("bin");
        std::fs::create_dir_all(&shim).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(shim.join("codex"), "shim").unwrap();
        let path = std::env::join_paths([shim.clone(), real.clone()]).unwrap();

        assert_eq!(find_on_path("codex", Some(&path)), None, "only a shim");
        std::fs::write(real.join("codex"), "real").unwrap();
        assert_eq!(find_on_path("codex", Some(&path)), Some(real.join("codex")));
        assert_eq!(find_on_path("codex", None), None);
    }

    /// The probe end to end: a missing program is absent, and a real
    /// executable's `--version` answer is read through the bounded probe.
    #[cfg(target_os = "linux")]
    #[test]
    fn probe_runs_a_real_program_and_reports_a_missing_one_absent() {
        assert_eq!(
            probe("x", "qontinui-no-such-cli-zz").available,
            Some(false)
        );
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("fakecli");
        std::fs::write(&script, "#!/bin/sh\necho 'fakecli 1.2.3'\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let outcome = crate::process_helpers::run_probe_quiet(
            version_command(&script),
            PROBE_TIMEOUT,
            "test",
        );
        let v = classify("x", "fakecli", Some(&script), Some(outcome));
        assert_eq!(v.available, Some(true));
        assert_eq!(v.version.as_deref(), Some("fakecli 1.2.3"));
    }
}
