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

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
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
    let (machine_id, hostname) = enroll::local_machine_identity();
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

#[cfg(test)]
mod tests {
    use super::*;

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
