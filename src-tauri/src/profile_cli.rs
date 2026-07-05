//! Pre-GUI CLI mode for the runner binary + the standalone `qontinui_profile`.
//!
//! The `env enroll` / `env capture` / `env show` subcommands are shared between:
//! - the standalone `bin/qontinui_profile.rs` (its `Cmd::Env` arm delegates to
//!   [`run_env`]), and
//! - the **main runner binary** — `main.rs` calls [`try_run_cli`] at the very top
//!   of `main()`, so `qontinui-runner env enroll …` runs the enroll flow and
//!   exits BEFORE any Tauri/GUI init. The installed runner binary thus doubles as
//!   the on-box enroll tool, with no separately-bundled sidecar (see the plan's
//!   OQ#3: `externalBin` validates at compile time and breaks every `cargo
//!   build`, so a CLI mode on the always-present main binary is the clean fit).
//!
//! Exit codes: 0 success, 1 runtime failure, 2 usage/serialize error.

use std::path::PathBuf;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;

use crate::env_agent::config::EnvAgentConfig;
use crate::env_agent::enroll::{self, EnrollParams};

/// The `env` subcommand tree — the machine-side dev-environment capture agent.
/// `enroll` binds this machine to a web environment via a per-machine API key;
/// `capture` pushes a secret-free config envelope; `show` prints enrollment state.
#[derive(Subcommand, Debug)]
pub enum EnvCmd {
    /// Enroll this machine into a web environment via an enrollment code.
    /// POSTs `{enrollment_code, machine_id, hostname}` to
    /// `{backend}/api/v1/devenv/agent/enroll`; on success stores the returned
    /// `mk_<token>` machine key and writes `~/.qontinui/env-agent.json` with the
    /// RESPONSE environment_id.
    Enroll {
        /// The enrollment code minted by the web dashboard.
        #[arg(long)]
        code: String,
        /// Override the backend base URL. Falls back to `QONTINUI_WEB_BASE`,
        /// then a web base derived from the active profile's coord_url.
        #[arg(long)]
        backend: Option<String>,
        /// Reserved override for the target environment id. Normally assigned by
        /// the enroll response; diagnostics / re-enroll only.
        #[arg(long)]
        environment: Option<String>,
    },
    /// Capture the current dev-environment config and push it to the backend.
    /// `--dry-run` assembles + pretty-prints the envelope WITHOUT pushing.
    Capture {
        /// Assemble + print the envelope without POSTing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Print enrollment state (from env-agent.json) + whether a machine key is
    /// stored.
    Show,
}

/// Run an `env` subcommand. Returns a process exit code (0/1/2).
pub fn run_env(cmd: EnvCmd) -> u8 {
    match cmd {
        EnvCmd::Enroll {
            code,
            backend,
            environment,
        } => cmd_env_enroll(&code, backend.as_deref(), environment.as_deref()),
        EnvCmd::Capture { dry_run } => cmd_env_capture(dry_run),
        EnvCmd::Show => cmd_env_show(),
    }
}

fn cmd_env_enroll(code: &str, backend_arg: Option<&str>, environment_arg: Option<&str>) -> u8 {
    if code.trim().is_empty() {
        eprintln!("error: --code requires a non-empty enrollment code");
        return 2;
    }
    let backend = match enroll::resolve_backend_base(backend_arg) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let (machine_id, hostname, coord_device_id) = enroll::local_machine_identity();
    if machine_id.is_none() {
        eprintln!(
            "note: no readable ~/.qontinui/machine.json — enrolling with null \
             machine_id/hostname (run `qontinui_profile device init` first for a \
             stable identity)"
        );
    }
    match enroll::run_enroll(EnrollParams {
        code: code.to_string(),
        backend,
        machine_id,
        hostname,
        coord_device_id,
        environment_override: environment_arg.map(|s| s.to_string()),
    }) {
        Ok(outcome) => {
            println!(
                "enrolled: machine_id={} environment_id={} backend={} (machine key stored)",
                outcome.machine_id, outcome.environment_id, outcome.backend_url
            );
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn cmd_env_capture(dry_run: bool) -> u8 {
    // Publish a lazy PG pool from the active profile so the `db_schema` collector
    // can run. Best-effort: a failure just omits that section.
    let profile = crate::profiles::load();
    if let Err(e) = crate::env_agent::publish_pg_pool_from_url(&profile.database_url) {
        eprintln!("note: db_schema collector unavailable — {e}");
    }

    if dry_run {
        match crate::env_agent::build_envelope_blocking() {
            Ok(envelope) => match serde_json::to_string_pretty(&envelope) {
                Ok(s) => {
                    println!("{s}");
                    0
                }
                Err(e) => {
                    eprintln!("error: serialize envelope failed: {e}");
                    2
                }
            },
            Err(e) => {
                eprintln!("error: building envelope failed: {e}");
                2
            }
        }
    } else {
        match crate::env_agent::capture_and_push_blocking() {
            Ok(()) => {
                println!("capture pushed (or skipped — machine not enrolled)");
                0
            }
            Err(e) => {
                eprintln!("error: capture/push failed: {e}");
                1
            }
        }
    }
}

fn cmd_env_show() -> u8 {
    let cfg = EnvAgentConfig::load();
    let key_stored = crate::secure_storage::SecureStorage::new()
        .ok()
        .and_then(|s| s.get_agent_machine_key().ok().flatten())
        .map(|k| !k.is_empty())
        .unwrap_or(false);

    let out = match cfg {
        Some(c) => json!({
            "enrolled": c.is_enrolled() && key_stored,
            "backend_url": c.backend_url,
            "machine_id": c.machine_id,
            "environment_id": c.environment_id,
            "enrolled_at": c.enrolled_at,
            "machine_key_stored": key_stored,
            "config_path": EnvAgentConfig::path().map(|p| p.display().to_string()),
        }),
        None => json!({
            "enrolled": false,
            "machine_key_stored": key_stored,
            "config_path": EnvAgentConfig::path().map(|p| p.display().to_string()),
            "note": "no env-agent.json — run `qontinui-runner env enroll --code <code>`",
        }),
    };
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    0
}

/// True iff argv names a recognized runner-binary CLI subcommand. Deliberately
/// narrow — ONLY `env …` diverts, so a normal GUI launch (no args) or a
/// `qontinui://…` deep-link launch never triggers CLI mode.
fn is_cli_subcommand(args: &[String]) -> bool {
    matches!(args.get(1).map(String::as_str), Some("env"))
}

/// The runner binary's pre-GUI CLI entry — call at the TOP of `main()`. Returns
/// `Some(exit_code)` when argv is a recognized CLI subcommand (caller should
/// `std::process::exit`), or `None` for a normal GUI / deep-link launch (fall
/// through to Tauri init).
pub fn try_run_cli() -> Option<u8> {
    let args: Vec<String> = std::env::args().collect();
    if !is_cli_subcommand(&args) {
        return None;
    }
    Some(run_env_from_args(&args))
}

/// Parse `env <sub> …` from full argv and run it. `args[1]` (`"env"`) is treated
/// as the clap program name; the rest parse against [`EnvCmd`].
fn run_env_from_args(args: &[String]) -> u8 {
    #[derive(Parser)]
    struct EnvWrapper {
        #[command(subcommand)]
        cmd: EnvCmd,
    }
    match EnvWrapper::try_parse_from(args.iter().skip(1).cloned()) {
        Ok(w) => run_env(w.cmd),
        Err(e) => {
            let _ = e.print();
            match e.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => 0,
                _ => 2,
            }
        }
    }
}

// ===========================================================================
// Terminal-command PATH provisioning (Phase 1a follow-up)
// ===========================================================================
//
// After install the runner binary IS `qontinui-runner(.exe)` (Cargo name =
// Tauri's default `mainBinaryName`), but its install dir is not on PATH, so the
// dashboard's `qontinui-runner env enroll …` copy-paste command doesn't resolve
// in a fresh terminal. This adds the install dir to the USER PATH (opt-in, from
// the Settings panel) so the bare command works.
//
// Windows uses `[Environment]::SetEnvironmentVariable(...,'User')` via PowerShell
// — the correct API (no `setx` 1024-char truncation; it persists AND broadcasts
// `WM_SETTINGCHANGE` so new shells pick it up). No registry crate, no admin.

/// Outcome of a PATH-provisioning attempt (returned to the Settings UI).
#[derive(Debug, Clone, Serialize)]
pub struct CliPathOutcome {
    /// True when this call modified the user PATH.
    pub added: bool,
    /// True when the install dir was already on the user PATH (no-op).
    pub already_present: bool,
    /// The runner install dir that is / was added.
    pub dir: String,
    /// Human-readable note for the UI.
    pub message: String,
}

/// The runner's install directory — the folder containing the running
/// `qontinui-runner` executable.
fn runner_install_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("resolve current exe: {e}"))?;
    exe.parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "executable has no parent directory".to_string())
}

/// Compute the PATH value with `dir` appended, or `None` when `dir` is already
/// present. Entries are compared after trimming whitespace + a trailing
/// separator, case-insensitively (Windows PATH is case-insensitive). Pure —
/// unit-tested.
fn compute_path_addition(current: &str, dir: &str, sep: char) -> Option<String> {
    let norm = |s: &str| s.trim().trim_end_matches(['/', '\\']).to_ascii_lowercase();
    let target = norm(dir);
    if target.is_empty() {
        return None;
    }
    let present = current
        .split(sep)
        .any(|e| !e.trim().is_empty() && norm(e) == target);
    if present {
        return None;
    }
    if current.trim().is_empty() {
        Some(dir.to_string())
    } else {
        // Preserve the existing value verbatim; append with one separator.
        let trimmed = current.trim_end_matches(sep);
        Some(format!("{trimmed}{sep}{dir}"))
    }
}

/// Read the current USER-scope `Path` (Windows). Returns "" when unset.
#[cfg(windows)]
fn read_user_path() -> Result<String, String> {
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Environment]::GetEnvironmentVariable('Path','User')",
        ])
        .output()
        .map_err(|e| format!("read user PATH (powershell): {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "read user PATH failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_string())
}

/// Persist a new USER-scope `Path` (Windows). The value is passed via an env var
/// so a long PATH with special characters survives argument parsing intact.
#[cfg(windows)]
fn write_user_path(new_value: &str) -> Result<(), String> {
    let out = std::process::Command::new("powershell")
        .env("QONTINUI_NEW_USER_PATH", new_value)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Environment]::SetEnvironmentVariable('Path', $env:QONTINUI_NEW_USER_PATH, 'User')",
        ])
        .output()
        .map_err(|e| format!("write user PATH (powershell): {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "write user PATH failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Whether the runner's install dir is already on the user PATH.
pub fn cli_dir_on_user_path() -> Result<bool, String> {
    let dir = runner_install_dir()?;
    let dir_str = dir.to_string_lossy();
    #[cfg(windows)]
    {
        let current = read_user_path()?;
        Ok(compute_path_addition(&current, &dir_str, ';').is_none())
    }
    #[cfg(not(windows))]
    {
        // Best-effort: consult the process PATH. We don't edit shell profiles on
        // Unix (the target boxes are Windows).
        let current = std::env::var("PATH").unwrap_or_default();
        Ok(compute_path_addition(&current, &dir_str, ':').is_none())
    }
}

/// Add the runner's install dir to the user PATH so `qontinui-runner …` resolves
/// in a terminal. Idempotent. Windows persists to the USER `Path` via PowerShell;
/// other platforms are unsupported and return the dir to add manually.
pub fn install_cli_on_user_path() -> Result<CliPathOutcome, String> {
    let dir = runner_install_dir()?;
    let dir_str = dir.to_string_lossy().to_string();

    #[cfg(windows)]
    {
        let current = read_user_path()?;
        match compute_path_addition(&current, &dir_str, ';') {
            None => Ok(CliPathOutcome {
                added: false,
                already_present: true,
                dir: dir_str,
                message: "Already on your PATH — `qontinui-runner env enroll …` works in a new \
                          terminal."
                    .to_string(),
            }),
            Some(new_value) => {
                write_user_path(&new_value)?;
                Ok(CliPathOutcome {
                    added: true,
                    already_present: false,
                    dir: dir_str,
                    message: "Added to your PATH. Open a NEW terminal, then `qontinui-runner env \
                              enroll …` will work."
                        .to_string(),
                })
            }
        }
    }
    #[cfg(not(windows))]
    {
        Err(format!(
            "Automatic PATH setup is Windows-only for now. Add this directory to your PATH \
             manually: {dir_str}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_addition_appends_when_absent() {
        assert_eq!(
            compute_path_addition("C:\\a;C:\\b", "C:\\qontinui", ';').as_deref(),
            Some("C:\\a;C:\\b;C:\\qontinui")
        );
        // Empty current → just the dir (no leading separator).
        assert_eq!(
            compute_path_addition("", "C:\\qontinui", ';').as_deref(),
            Some("C:\\qontinui")
        );
        // A trailing separator is collapsed (no double `;;`).
        assert_eq!(
            compute_path_addition("C:\\a;", "C:\\qontinui", ';').as_deref(),
            Some("C:\\a;C:\\qontinui")
        );
    }

    #[test]
    fn path_addition_is_idempotent_and_case_trailing_insensitive() {
        // Exact match → None.
        assert!(compute_path_addition("C:\\a;C:\\qontinui", "C:\\qontinui", ';').is_none());
        // Case-insensitive + trailing-slash-insensitive → still present → None.
        assert!(
            compute_path_addition("C:\\A;C:\\QONTINUI\\", "c:\\qontinui", ';').is_none(),
            "windows PATH is case-insensitive; trailing slash ignored"
        );
        // A substring that isn't a full entry does NOT count as present.
        assert_eq!(
            compute_path_addition("C:\\qontinui-runner-old", "C:\\qontinui", ';').as_deref(),
            Some("C:\\qontinui-runner-old;C:\\qontinui"),
        );
        // Empty target dir → never adds.
        assert!(compute_path_addition("C:\\a", "", ';').is_none());
    }

    fn v(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn env_subcommand_diverts_to_cli() {
        assert!(is_cli_subcommand(&v(&["qontinui-runner", "env", "show"])));
        assert!(is_cli_subcommand(&v(&[
            "qontinui-runner",
            "env",
            "enroll",
            "--code",
            "X"
        ])));
    }

    #[test]
    fn gui_and_deeplink_launches_never_divert() {
        // Bare GUI launch.
        assert!(!is_cli_subcommand(&v(&["qontinui-runner"])));
        // Single-instance / deep-link launch forwards a qontinui:// URL as argv.
        assert!(!is_cli_subcommand(&v(&[
            "qontinui-runner",
            "qontinui://open/foo"
        ])));
        // Any non-allow-listed subcommand stays GUI (e.g. an OS-passed flag).
        assert!(!is_cli_subcommand(&v(&[
            "qontinui-runner",
            "--some-os-flag"
        ])));
        assert!(!is_cli_subcommand(&v(&[
            "qontinui-runner",
            "device",
            "init"
        ])));
    }
}
