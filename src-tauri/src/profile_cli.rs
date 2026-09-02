//! Pre-GUI CLI mode for the runner binary + the standalone `qontinui_profile`.
//!
//! The `env enroll` / `env capture` / `env pull` / `env apply` / `env show` /
//! `env scope-root` subcommands are shared between:
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

use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;

use crate::env_agent::config::EnvAgentConfig;
use crate::env_agent::enroll::{self, EnrollParams};

/// The `env` subcommand tree — the machine-side dev-environment capture agent.
/// `enroll` binds this machine to a web environment via a per-machine API key;
/// `capture` pushes a secret-free config envelope; `pull` previews what would
/// change here to match the canonical machine; `apply` reconciles this box
/// toward it (dry-run by default); `show` prints enrollment state; `scope-root`
/// declares which directory the toolchain probes measure.
#[derive(Subcommand, Debug)]
pub enum EnvCmd {
    /// Enroll this machine into a web environment via an enrollment code.
    /// POSTs `{enrollment_code, hostname, coord_device_id}` to
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
    /// Pull the environment's canonical config and show what WOULD change on
    /// this machine to match it. Read-only — `pull` never modifies this box;
    /// the decision to apply is taken locally, later.
    Pull {
        /// Emit the plan as JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Reconcile THIS machine toward the environment's canonical config.
    ///
    /// The decision is local: nothing is ever pushed onto a box from the
    /// server. Dry-run by default — it prints exactly what it would write and
    /// changes nothing until `--confirm` is given.
    ///
    /// Only sections the server marks `applyable` are eligible, and within them
    /// only a fixed key set per section, so a backend that grows a section can
    /// never widen what this runner writes.
    Apply {
        /// Actually write the changes. Without it this is a preview.
        #[arg(long)]
        confirm: bool,
        /// Restrict the apply to these sections (repeatable). Default: every
        /// section this runner supports.
        #[arg(long = "section")]
        sections: Vec<String>,
        /// Target profile in `~/.qontinui/profiles.json` for the `services`
        /// apply. Defaults to the active profile (`QONTINUI_ENV` → the file's
        /// `active` → `dev`) — the same one the capture read, which is what
        /// makes the drift actually clear.
        #[arg(long)]
        profile: Option<String>,
        /// Emit the report as JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Print enrollment state (from env-agent.json) + whether a machine key is
    /// stored.
    Show,
    /// Declare the **capture scope** — the directory the toolchain `--version`
    /// probes (`node`/`python`/`rustc`) run in.
    ///
    /// Unset (the default) measures the box's DEFAULT toolchain from the home
    /// directory: what shim-based managers (rustup's default toolchain, `nvm
    /// alias default`, `pyenv global`) answer outside any project tree. Point it
    /// at a project root to measure that tree instead.
    ///
    /// Without this command the field was settable only by hand-editing
    /// `~/.qontinui/env-agent.json`.
    ScopeRoot {
        /// Directory to probe in. A relative path is resolved against the
        /// current working directory HERE, at set time — where it means what
        /// you typed — and stored absolute, because a relative path in the
        /// config file would resolve against the runner's launch directory
        /// instead and reintroduce the launch-dependence this setting removes.
        #[arg(long, conflicts_with = "clear")]
        path: Option<String>,
        /// Clear the declared scope, reverting to the home-directory default.
        #[arg(long)]
        clear: bool,
    },
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
        EnvCmd::Pull { json } => cmd_env_pull(json),
        EnvCmd::Apply {
            confirm,
            sections,
            profile,
            json,
        } => cmd_env_apply(confirm, sections, profile, json),
        EnvCmd::Show => cmd_env_show(),
        EnvCmd::ScopeRoot { path, clear } => cmd_env_scope_root(path.as_deref(), clear),
    }
}

/// Resolve an operator-supplied scope root to a stored value: trimmed, made
/// absolute against the CURRENT working directory, and checked to be a real
/// directory.
///
/// Relative input is accepted and absolutized rather than rejected — at the
/// operator's shell a relative path is unambiguous, and resolving it once here
/// is what keeps the *stored* value absolute. (The probe-side resolver rejects a
/// relative value outright, since by then the only cwd available is the
/// runner's launch directory, which means nothing to the operator.)
fn resolve_scope_root_arg(path: &str) -> Result<String, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("--path requires a non-empty directory".to_string());
    }
    let candidate = PathBuf::from(trimmed);
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("could not resolve the current directory for {trimmed:?}: {e}"))?;
        cwd.join(candidate)
    };
    if !absolute.is_dir() {
        return Err(format!(
            "{} is not an existing directory",
            absolute.display()
        ));
    }
    Ok(absolute.display().to_string())
}

/// `env scope-root` — declare (or clear) the capture scope.
///
/// Validates the directory HERE rather than letting the capture discover it is
/// unusable 15 minutes later: the probe path is fail-open by design, so a bad
/// value there degrades to a WARN and a silently different measurement. This is
/// the one place the operator can be told "no" synchronously.
fn cmd_env_scope_root(path: Option<&str>, clear: bool) -> u8 {
    let Some(mut cfg) = EnvAgentConfig::load() else {
        eprintln!(
            "error: no ~/.qontinui/env-agent.json — run `qontinui-runner env enroll --code <code>` first"
        );
        return 1;
    };

    // Neither flag: report the current setting rather than silently no-op.
    if path.is_none() && !clear {
        match cfg.scope_root.as_deref() {
            Some(v) => println!("scope_root = {v}"),
            None => println!("scope_root is unset (probing the home directory by default)"),
        }
        return 0;
    }

    let new_value = match path {
        Some(p) => match resolve_scope_root_arg(p) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("error: {e}");
                return 2;
            }
        },
        None => None,
    };

    cfg.scope_root = new_value.clone();
    if let Err(e) = cfg.save() {
        eprintln!("error: writing env-agent.json failed: {e}");
        return 1;
    }
    match new_value {
        Some(v) => println!("scope_root = {v}"),
        None => println!("scope_root cleared (probing the home directory by default)"),
    }
    0
}

/// The operator-facing note printed when this machine has no usable local
/// identity file. Keyed off the EXPLICIT presence signal
/// (`LocalMachineIdentity::machine_json_present`) — never off an enroll wire
/// field, which is `None` on every enroll by design.
fn missing_machine_json_note(identity: &enroll::LocalMachineIdentity) -> Option<&'static str> {
    if identity.machine_json_present {
        return None;
    }
    Some(
        "note: no readable ~/.qontinui/machine.json — enrolling with null \
         hostname/coord_device_id (run `qontinui_profile device init` first for a \
         stable identity)",
    )
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
    let identity = enroll::local_machine_identity();
    if let Some(note) = missing_machine_json_note(&identity) {
        eprintln!("{note}");
    }
    match enroll::run_enroll(EnrollParams {
        code: code.to_string(),
        backend,
        // ALWAYS None for a locally-initiated enroll: the devenv machine UUID is
        // what enroll RETURNS, and the enrollment code already identifies the
        // machine. Coord's device_id is a different identity space and travels in
        // `coord_device_id` — sending it here caused `409 machine_id_mismatch`.
        machine_id: None,
        hostname: identity.hostname,
        coord_device_id: identity.coord_device_id,
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

/// `env pull` — fetch canonical, diff this box against it, print the plan.
/// Read-only by construction: the pull path has no mutation branch at all, so
/// there is no `--dry-run` to forget (unlike `capture`, where dry-run is opt-in).
fn cmd_env_pull(as_json: bool) -> u8 {
    // Same PG-pool publish as capture — without it the `db_schema` collector
    // yields nothing and the plan would wrongly look clean on that section.
    let profile = crate::profiles::load();
    if let Err(e) = crate::env_agent::publish_pg_pool_from_url(&profile.database_url) {
        eprintln!("note: db_schema collector unavailable — {e}");
    }

    match crate::env_agent::pull::pull_and_plan_blocking() {
        Ok(plan) => {
            if as_json {
                let v = crate::env_agent::pull::plan_to_json(&plan);
                match serde_json::to_string_pretty(&v) {
                    Ok(s) => {
                        println!("{s}");
                        0
                    }
                    Err(e) => {
                        eprintln!("error: serialize plan failed: {e}");
                        2
                    }
                }
            } else {
                print!("{}", crate::env_agent::pull::render_plan(&plan));
                0
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

/// `env apply` — pull canonical, then reconcile THIS box toward it.
///
/// Dry-run by default. `--confirm` is the local confirm the plan's P2b bullet
/// requires: the box that owns the box decides, and it decides explicitly.
///
/// Exit code 0 covers both a dry run and a successful apply; a section this
/// runner could not apply is reported in the body (with its reason) rather than
/// failing the command, since one blocked section must not hide another
/// section's successful reconcile.
fn cmd_env_apply(
    confirm: bool,
    sections: Vec<String>,
    profile: Option<String>,
    as_json: bool,
) -> u8 {
    // Same PG-pool publish as capture/pull — without it the `db_schema`
    // collector yields nothing and that section would wrongly look clean.
    let active = crate::profiles::load();
    if let Err(e) = crate::env_agent::publish_pg_pool_from_url(&active.database_url) {
        eprintln!("note: db_schema collector unavailable — {e}");
    }

    let opts = crate::env_agent::apply::ApplyOptions {
        confirm,
        sections,
        profile,
    };
    match crate::env_agent::apply::apply_blocking(&opts) {
        Ok(report) => {
            if as_json {
                let v = crate::env_agent::apply::report_to_json(&report);
                match serde_json::to_string_pretty(&v) {
                    Ok(s) => {
                        println!("{s}");
                        0
                    }
                    Err(e) => {
                        eprintln!("error: serialize report failed: {e}");
                        2
                    }
                }
            } else {
                print!("{}", crate::env_agent::apply::render_report(&report));
                0
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

/// Human-readable state of the declared capture scope, for `env show`.
///
/// Reported because the probe-side fall-through is deliberately SILENT in the
/// envelope: a dropped `scope_root` still produces a well-formed capture, just
/// measured somewhere the operator did not choose. Without a readout there is
/// no way to tell a honoured declaration from an ignored one.
fn scope_root_status(configured: Option<&str>) -> String {
    let scope = crate::env_agent::collectors::resolve_probe_scope(configured);
    match (scope.rejected, configured) {
        (Some(rejection), _) => format!("ignored ({}) — using the default", rejection.reason()),
        (None, Some(_)) => "configured".to_string(),
        (None, None) if scope.root.is_some() => "default (home directory)".to_string(),
        (None, None) => {
            "no home directory — probes inherit the runner's working directory".to_string()
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
            // The declared capture scope, as configured AND as actually
            // resolved — these differ whenever a stale/relative value was
            // dropped, which is exactly what an operator needs to see.
            "scope_root": c.scope_root,
            "scope_root_status": scope_root_status(c.scope_root.as_deref()),
            "probe_scope_root": crate::env_agent::collectors::resolve_probe_scope(
                c.scope_root.as_deref(),
            )
            .root
            .map(|p| p.display().to_string()),
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
    /// For the identity-shim install ONLY: whether the shim will ACTUALLY shadow
    /// `claude` in a fresh shell. `Some(true)` — it wins resolution; `Some(false)`
    /// — a real `claude` earlier on PATH (typically on the un-editable SYSTEM
    /// PATH) still wins, so the opt-in is installed but INERT (the `message` names
    /// the blocker). `None` — not evaluated (every non-identity path, e.g. the
    /// CLI-enroll toggle, and non-Windows). Honest reporting: prepending to the
    /// USER PATH cannot beat a SYSTEM-PATH `claude`, so a bare "added: true" would
    /// otherwise claim success on a silent no-op.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective: Option<bool>,
}

/// The runner's install directory — the folder containing the running
/// `qontinui-runner` executable.
fn runner_install_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("resolve current exe: {e}"))?;
    exe.parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "executable has no parent directory".to_string())
}

/// The **persistent identity-shim directory** —
/// `~/.qontinui/runner/identity-shim` (plan
/// `2026-07-17-universal-coord-device-identity-for-any-session` §6). `None` when
/// the home dir is unresolvable.
///
/// # Why this constant lives in the LIB crate
///
/// Same reason as [`crate::runner_breadcrumb`]: the WRITER is the runner bin
/// (`install_effects_producer::intercept::shim_materializer`, which materializes
/// `claude.exe` here) and a READER is the standalone `qontinui-shim` `.exe`
/// (`bin/qontinui_shim.rs::own_shim_dirs`, which MUST exclude this dir from its
/// real-tool PATH scan or the stub resolves ITSELF). A second bin cannot import
/// from the runner bin's module tree, so one lib module ⇒ one path ⇒ the two can
/// never drift. A drift here is not cosmetic: it is an infinite self-spawn loop.
///
/// # Why not the runner install dir
///
/// [`install_cli_on_user_path`] already puts the install dir on PATH. Dropping a
/// `claude.exe` shadow in there would make that unrelated opt-in silently also
/// enable identity shadowing. The two gates must stay independent, so the
/// identity shim gets its own dir under the established runner app-data root
/// (`~/.qontinui/runner/`, alongside `session-restore/` and the port
/// breadcrumb) — a stable location, NOT `temp_dir()` (the per-PTY dirs live
/// there and are swept).
pub fn identity_shim_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("runner").join("identity-shim"))
}

/// File name of the per-machine operator opt-in marker for session-provisioned
/// coord identity, under `~/.qontinui/` (plan
/// `2026-07-17-universal-coord-device-identity-for-any-session` §1). Its mere
/// existence is the signal; contents are never read.
///
/// # Why this constant lives in the LIB crate
///
/// It is the SINGLE source of truth for the opt-in marker, shared by TWO
/// processes that must agree byte-for-byte:
/// - the runner-side AUTHORITATIVE gate (`coord_mcp::session_identity_gate`,
///   in the runner BIN crate), and
/// - the standalone `qontinui-shim` `.exe`'s dark-posture short-circuit
///   (`bin/qontinui_shim.rs::session_identity_marker_exists`).
///
/// A second bin cannot import from the runner bin's module tree, so — exactly
/// like [`identity_shim_dir`] and [`crate::runner_breadcrumb`] — the constant
/// AND its path resolver live here so the gate and the shim can never drift. A
/// rename that desynced them would silently either keep the shim POSTing on an
/// un-opted machine, or stop it provisioning on an opted-in one.
pub const SESSION_IDENTITY_MARKER_FILE: &str = "allow-session-coord-identity";

/// Absolute path of the session-coord-identity opt-in marker
/// (`~/.qontinui/allow-session-coord-identity`). `None` when the home dir is
/// unresolvable — every caller treats that as NOT opted in (fail-closed on the
/// gate side, fail-open on the shim side), never as consent. Both the runner
/// gate and the shim resolve the marker through THIS one function, so the whole
/// path (directory + filename) is guaranteed identical, not just the filename.
pub fn session_identity_marker_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join(SESSION_IDENTITY_MARKER_FILE))
}

/// File name of the runner's per-start **loopback handshake key**, under
/// `~/.qontinui/` (plan
/// `2026-08-24-headless-box-has-no-working-coord-credential-door`, Phase 1).
///
/// # Why this file exists at all
///
/// `127.0.0.1:9876` is a plain TCP socket, and on every supported platform ANY
/// local user can connect to loopback — there is no peer-credential check on a
/// TCP socket. So "the request came from loopback" is **not** a same-user proof,
/// and the opt-in marker ([`SESSION_IDENTITY_MARKER_FILE`]) is only a *presence*
/// check, which is not one either. Without a further control, a DIFFERENT local
/// user could drive `POST /coord-mcp/provision-session` and mint a coord-mcp
/// nonce they provably cannot obtain today, because `auth_tokens.enc` is
/// owner-only (`0600` / a protected DACL) and closed to them.
///
/// This file reproduces the *file's* boundary on the *socket*: it is written
/// owner-only ([`crate::fs_perms::write_owner_only`]) at every runner start, so
/// only the owning user can read the secret, and the mint route requires that
/// secret in the `X-Qontinui-Loopback-Key` header
/// ([`RUNNER_LOOPBACK_KEY_HEADER`]).
///
/// # Rotated per runner start, deliberately
///
/// A leaked secret dies at the next runner start rather than persisting
/// indefinitely, and persistence would buy nothing: the nonce this handshake
/// unlocks is paired to the runner's *bound port*, so a secret that outlives the
/// process outlives its own usefulness anyway.
pub const RUNNER_LOOPBACK_KEY_FILE: &str = "runner-loopback-key";

/// Absolute path of the loopback handshake key
/// (`~/.qontinui/runner-loopback-key`). `None` when the home dir is
/// unresolvable — the runner then writes no key and the mint route denies every
/// request (fail-closed: an unresolvable home must never read as consent).
///
/// Lives in the LIB crate for the same reason
/// [`session_identity_marker_path`] does: TWO processes must agree on the whole
/// path byte-for-byte — the runner BIN's authoritative gate
/// (`coord_mcp::session_identity_gate`) which WRITES it, and the standalone
/// `qontinui-shim` `.exe` which READS it to authenticate its own mint POST.
pub fn runner_loopback_key_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join(RUNNER_LOOPBACK_KEY_FILE))
}

/// The request header the mint route requires the loopback handshake secret in.
///
/// Shared with the shim for the same drift reason as the path above. Two typed
/// 403s distinguish "not presented" (`COORD_MCP_PROVISION_NO_HANDSHAKE`) from
/// "presented but wrong" (`COORD_MCP_PROVISION_HANDSHAKE_MISMATCH`), because the
/// fixes differ — the runner's no-silent-empty rule.
pub const RUNNER_LOOPBACK_KEY_HEADER: &str = "X-Qontinui-Loopback-Key";

/// Where [`install_dir_on_user_path`] places a newly-added dir on the USER PATH.
/// PATH resolution is first-match-wins, so placement is load-bearing for the
/// identity shim: a `claude` shadow that must WIN over the real `claude` already
/// on an earlier PATH entry has to go at the FRONT, or the whole §6 feature is a
/// silent no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Add at the FRONT — the dir wins resolution over any later entry. The
    /// identity-shim install needs this (its `claude` shadow must be found before
    /// the real `claude`).
    Prepend,
    /// Add at the END — mere presence is enough, and appending avoids shadowing a
    /// same-named tool the operator already has earlier. The CLI-enroll toggle
    /// wants this (it only needs `qontinui-runner` to resolve at all).
    Append,
}

/// Compute the PATH value with `dir` added at `placement`, or `None` when no
/// change is needed. Entries are compared after trimming whitespace + a trailing
/// separator, case-insensitively (Windows PATH is case-insensitive). Pure —
/// unit-tested.
///
/// The idempotence predicate is **placement-specific**, because PATH resolution
/// is first-match-wins:
///
/// - [`Placement::Append`] — presence ANYWHERE suffices ("no change" ⇒ `None`).
///   A toggle that only needs the tool to resolve at all does not care WHERE.
/// - [`Placement::Prepend`] — the predicate is "already at the FRONT", NOT
///   "present anywhere". A shim dir present but NOT first means the real `claude`
///   on an earlier entry still wins resolution, so a position-blind "present ⇒
///   None" would make the identity-shim opt-in a silent no-op (the exact
///   regression this predicate fixes). So a prepend that finds the dir present
///   but not first REMOVES every existing occurrence and puts it first (dedup +
///   move-to-front); only an already-front dir returns `None`.
fn compute_path_addition(
    current: &str,
    dir: &str,
    sep: char,
    placement: Placement,
) -> Option<String> {
    let norm = |s: &str| s.trim().trim_end_matches(['/', '\\']).to_ascii_lowercase();
    let target = norm(dir);
    if target.is_empty() {
        return None;
    }
    // Empty current → just the dir (no leading/trailing separator), either way.
    if current.trim().is_empty() {
        return Some(dir.to_string());
    }
    match placement {
        Placement::Append => {
            // Present ANYWHERE (ignoring empty segments) ⇒ no change.
            let present = current
                .split(sep)
                .any(|e| !e.trim().is_empty() && norm(e) == target);
            if present {
                return None;
            }
            // Preserve the existing value verbatim; join with exactly one separator.
            let trimmed = current.trim_end_matches(sep);
            Some(format!("{trimmed}{sep}{dir}"))
        }
        Placement::Prepend => {
            // Already the FIRST non-empty entry ⇒ it already wins resolution ⇒
            // no change. (Empty leading segments are skipped, matching how
            // Windows ignores them during command resolution.)
            let already_first = current
                .split(sep)
                .find(|e| !e.trim().is_empty())
                .map(norm)
                .as_deref()
                == Some(target.as_str());
            if already_first {
                return None;
            }
            // Present-but-not-first, or absent: drop EVERY existing occurrence of
            // the target (keeping empty segments and all other entries verbatim,
            // exactly as [`compute_path_removal`] does) and put it first. This
            // both dedups and moves-to-front in a single pass, so the shim's
            // `claude` shadow wins over a real `claude` that was earlier on PATH.
            let kept: Vec<&str> = current
                .split(sep)
                .filter(|e| e.trim().is_empty() || norm(e) != target)
                .collect();
            let rest = kept.join(&sep.to_string());
            let rest = rest.trim_start_matches(sep);
            Some(format!("{dir}{sep}{rest}"))
        }
    }
}

/// Compute the PATH value with every entry matching `dir` REMOVED, or `None`
/// when `dir` is not present (nothing to do). Matching is identical to
/// [`compute_path_addition`]'s (trimmed, trailing-separator- and
/// case-insensitive), so a dir this function removes is exactly one that
/// function would consider present — install and uninstall can never disagree
/// about what "on PATH" means. Every OTHER entry is preserved verbatim,
/// including duplicates and empty segments the operator may rely on. Pure —
/// unit-tested.
fn compute_path_removal(current: &str, dir: &str, sep: char) -> Option<String> {
    let norm = |s: &str| s.trim().trim_end_matches(['/', '\\']).to_ascii_lowercase();
    let target = norm(dir);
    if target.is_empty() {
        return None;
    }
    let kept: Vec<&str> = current
        .split(sep)
        .filter(|e| e.trim().is_empty() || norm(e) != target)
        .collect();
    if kept.len() == current.split(sep).count() {
        return None; // not present — nothing to remove
    }
    Some(kept.join(&sep.to_string()))
}

/// Read a `Path` variable at the given scope (`"User"` or `"Machine"`) on
/// Windows. Returns "" when unset. Reading the `Machine` (SYSTEM) scope needs no
/// elevation — only WRITING it does. `scope` is passed via an env var so it can
/// never be misread as a PowerShell operator.
#[cfg(windows)]
fn read_path_scope(scope: &str) -> Result<String, String> {
    let out = crate::process_helpers::no_window("powershell")
        .env("QONTINUI_PATH_SCOPE", scope)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Environment]::GetEnvironmentVariable('Path', $env:QONTINUI_PATH_SCOPE)",
        ])
        .output()
        .map_err(|e| format!("read {scope} PATH (powershell): {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "read {scope} PATH failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_string())
}

/// Read the current USER-scope `Path` (Windows). Returns "" when unset.
#[cfg(windows)]
fn read_user_path() -> Result<String, String> {
    read_path_scope("User")
}

/// Persist a new USER-scope `Path` (Windows). The value is passed via an env var
/// so a long PATH with special characters survives argument parsing intact.
#[cfg(windows)]
fn write_user_path(new_value: &str) -> Result<(), String> {
    let out = crate::process_helpers::no_window("powershell")
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

/// Whether `dir` is already on the user PATH. The generic core behind
/// [`cli_dir_on_user_path`] and
/// [`crate::profile_cli::identity_shim_on_user_path`].
pub fn dir_on_user_path(dir: &Path) -> Result<bool, String> {
    let dir_str = dir.to_string_lossy();
    // Presence detection is placement-agnostic (a `None` result means "already
    // present" for either placement), so `Append` here carries no meaning.
    #[cfg(windows)]
    {
        let current = read_user_path()?;
        Ok(compute_path_addition(&current, &dir_str, ';', Placement::Append).is_none())
    }
    #[cfg(not(windows))]
    {
        // Best-effort: consult the process PATH. We don't edit shell profiles on
        // Unix (the target boxes are Windows).
        let current = std::env::var("PATH").unwrap_or_default();
        Ok(compute_path_addition(&current, &dir_str, ':', Placement::Append).is_none())
    }
}

/// Whether the runner's install dir is already on the user PATH.
pub fn cli_dir_on_user_path() -> Result<bool, String> {
    dir_on_user_path(&runner_install_dir()?)
}

/// Add `dir` to the USER PATH at `placement`. Idempotent. The generic core
/// behind [`install_cli_on_user_path`] (which appends — mere presence) and
/// [`install_identity_shim_on_user_path`] (which PREPENDS — its `claude` shadow
/// must win resolution over the real `claude`): `present_message` /
/// `added_message` let each caller speak to its own operator-facing surface
/// while the read → compute → write seam stays single.
///
/// Windows persists to the USER `Path` via PowerShell; other platforms are
/// unsupported and return the dir to add manually.
pub fn install_dir_on_user_path(
    dir: &Path,
    placement: Placement,
    present_message: &str,
    added_message: &str,
) -> Result<CliPathOutcome, String> {
    let dir_str = dir.to_string_lossy().to_string();
    #[cfg(windows)]
    {
        let current = read_user_path()?;
        match compute_path_addition(&current, &dir_str, ';', placement) {
            None => Ok(CliPathOutcome {
                added: false,
                already_present: true,
                dir: dir_str,
                message: present_message.to_string(),
                effective: None,
            }),
            Some(new_value) => {
                write_user_path(&new_value)?;
                Ok(CliPathOutcome {
                    added: true,
                    already_present: false,
                    dir: dir_str,
                    message: added_message.to_string(),
                    effective: None,
                })
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (placement, present_message, added_message);
        Err(format!(
            "Automatic PATH setup is Windows-only for now. Add this directory to your PATH \
             manually: {dir_str}"
        ))
    }
}

/// Remove `dir` from the USER PATH. Idempotent — a dir that is not present
/// returns `added: false, already_present: false` and writes nothing.
///
/// This is the REVERSE of [`install_dir_on_user_path`] and exists because
/// reversibility is the whole reason a Settings toggle beats a hand-dropped
/// dotfile: an operator must be able to revoke, in the same place they enabled,
/// something they may have forgotten they turned on.
pub fn remove_dir_from_user_path(
    dir: &Path,
    removed_message: &str,
    absent_message: &str,
) -> Result<CliPathOutcome, String> {
    let dir_str = dir.to_string_lossy().to_string();
    #[cfg(windows)]
    {
        let current = read_user_path()?;
        match compute_path_removal(&current, &dir_str, ';') {
            None => Ok(CliPathOutcome {
                added: false,
                already_present: false,
                dir: dir_str,
                message: absent_message.to_string(),
                effective: None,
            }),
            Some(new_value) => {
                write_user_path(&new_value)?;
                Ok(CliPathOutcome {
                    added: false,
                    already_present: true,
                    dir: dir_str,
                    message: removed_message.to_string(),
                    effective: None,
                })
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (removed_message, absent_message);
        Err(format!(
            "Automatic PATH setup is Windows-only for now. Remove this directory from your PATH \
             manually: {dir_str}"
        ))
    }
}

/// Add the runner's install dir to the user PATH so `qontinui-runner …` resolves
/// in a terminal. Idempotent. Windows persists to the USER `Path` via PowerShell;
/// other platforms are unsupported and return the dir to add manually.
pub fn install_cli_on_user_path() -> Result<CliPathOutcome, String> {
    install_dir_on_user_path(
        &runner_install_dir()?,
        // Presence is all the CLI-enroll toggle needs; append so it never
        // shadows a same-named tool the operator already has earlier on PATH.
        Placement::Append,
        "Already on your PATH — `qontinui-runner env enroll …` works in a new terminal.",
        "Added to your PATH. Open a NEW terminal, then `qontinui-runner env enroll …` will work.",
    )
}

/// Whether the persistent identity-shim dir ([`identity_shim_dir`]) is on the
/// user PATH — i.e. whether this machine has opted in to session coord identity
/// for bare (non-runner-spawned) terminals.
pub fn identity_shim_on_user_path() -> Result<bool, String> {
    let dir = identity_shim_dir().ok_or_else(|| "home directory unresolvable".to_string())?;
    dir_on_user_path(&dir)
}

/// Add the persistent identity-shim dir to the USER PATH — **the delivery gate**
/// for session coord identity (plan §6, trust-boundary option C).
///
/// This does NOT materialize the dir; the runner bin's shim materializer does
/// that first (it owns `copy_exe_stub`). Splitting them keeps this module free
/// of the Windows-only stub-copy machinery while the PATH seam stays single.
///
/// Reverse: [`uninstall_identity_shim_from_user_path`].
pub fn install_identity_shim_on_user_path() -> Result<CliPathOutcome, String> {
    let dir = identity_shim_dir().ok_or_else(|| "home directory unresolvable".to_string())?;
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut outcome = install_dir_on_user_path(
        &dir,
        // PREPEND — the shim's `claude` shadow must be resolved BEFORE the real
        // `claude` that is already on an earlier PATH entry, or this whole opt-in
        // is a silent no-op (the real tool keeps winning). Mirrors the per-PTY
        // path's `shim_materializer::prepend_path`.
        Placement::Prepend,
        "Already enabled — new terminals already get coord identity.",
        "Enabled. Open a NEW terminal; `claude` there will now request coord device identity \
         from the running runner.",
    )?;
    // HONESTY check (Windows): a fresh shell composes `Path` as the MACHINE
    // (SYSTEM) value FOLLOWED by the USER value, and a USER-PATH entry can never
    // win resolution against a `claude` already on the SYSTEM PATH. So prepending
    // to the USER PATH can install the opt-in yet leave it INERT. Resolve where
    // `claude` will ACTUALLY come from in a fresh shell and report the truth
    // rather than an unqualified "added: true". We do NOT try to edit the SYSTEM
    // PATH (needs elevation; out of scope).
    #[cfg(windows)]
    {
        match identity_shim_shadow_blocker(&dir) {
            Some(blocker) => {
                outcome.effective = Some(false);
                outcome.message = format!(
                    "Installed on your USER PATH, but it will NOT take effect: `{}` is \
                     resolved before the shim (typically because it is on the SYSTEM PATH, \
                     which a USER-PATH entry cannot override). Remove/relocate that `claude`, \
                     or place the shim dir ahead of it on the SYSTEM PATH (needs an elevated \
                     shell), for coord identity to apply to bare terminals.",
                    blocker.display()
                );
            }
            None => outcome.effective = Some(true),
        }
    }
    Ok(outcome)
}

/// The candidate `claude` filenames to probe within a PATH dir, in Windows
/// PATHEXT resolution-preference order (`.EXE` before `.CMD` before `.BAT`),
/// then the bare name. Only the DIR order decides shadowing; the extension order
/// is honored for completeness / to report the file a shell would actually run.
#[cfg_attr(not(windows), allow(dead_code))]
fn claude_candidate_names() -> [&'static str; 4] {
    ["claude.exe", "claude.cmd", "claude.bat", "claude"]
}

/// A real `claude` executable in `dir`, if one exists (PATHEXT order). `None`
/// otherwise. The `is_file` probe is injected in tests via
/// [`claude_shadowing_shim`]'s closure, so this concrete filesystem version is
/// only the production probe.
#[cfg_attr(not(windows), allow(dead_code))]
fn probe_claude_in(dir: &Path) -> Option<PathBuf> {
    claude_candidate_names()
        .iter()
        .map(|n| dir.join(n))
        .find(|c| c.is_file())
}

/// Given the ORDERED dirs a fresh shell searches, the shim dir, and a probe that
/// reports a real `claude` in a dir, return the real `claude` that would SHADOW
/// the shim (resolve before it), or `None` if the shim wins. Pure over its
/// inputs so the resolution rule is unit-testable with a stubbed PATH — no real
/// `claude` on disk required. Walking in order: the FIRST of {a dir with a real
/// `claude`, the shim dir} decides — a real `claude` first ⇒ it shadows; the
/// shim dir first ⇒ the shim wins (and its own copy is never probed).
#[cfg_attr(not(windows), allow(dead_code))]
fn claude_shadowing_shim(
    dirs: &[PathBuf],
    shim_dir: &Path,
    probe: impl Fn(&Path) -> Option<PathBuf>,
) -> Option<PathBuf> {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .trim_end_matches(['/', '\\'])
            .to_ascii_lowercase()
    };
    let shim_norm = norm(shim_dir);
    for dir in dirs {
        if norm(dir) == shim_norm {
            return None; // reached the shim dir first → it wins from here on
        }
        if let Some(found) = probe(dir) {
            return Some(found); // a real claude resolves before the shim
        }
    }
    None // shim not on the list, or no real claude anywhere → nothing shadows it
}

/// The ordered dirs a fresh Windows shell searches: the MACHINE (`system`) `Path`
/// entries FOLLOWED by the USER (`user`) `Path` entries — exactly how the OS
/// composes a new process's `Path`. Empty segments are dropped (they never hold a
/// resolvable tool).
#[cfg_attr(not(windows), allow(dead_code))]
fn fresh_shell_path_dirs(system: &str, user: &str) -> Vec<PathBuf> {
    system
        .split(';')
        .chain(user.split(';'))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Windows: after the USER-PATH prepend, resolve whether a real `claude` will
/// shadow the shim in a fresh shell. Reads the SYSTEM then USER `Path` (reading
/// SYSTEM needs no elevation), composes the fresh-shell search order, and probes
/// for a real `claude` ahead of the shim dir. A read failure fails OPEN to "not
/// shadowed" (`None`) — the install already succeeded; we never turn a failed
/// diagnostic into a failed install.
#[cfg(windows)]
fn identity_shim_shadow_blocker(shim_dir: &Path) -> Option<PathBuf> {
    let system = read_path_scope("Machine").unwrap_or_default();
    let user = read_user_path().unwrap_or_default();
    let dirs = fresh_shell_path_dirs(&system, &user);
    claude_shadowing_shim(&dirs, shim_dir, probe_claude_in)
}

/// Remove the persistent identity-shim dir from the USER PATH — the operator's
/// revocation of the §6 delivery gate. Idempotent.
pub fn uninstall_identity_shim_from_user_path() -> Result<CliPathOutcome, String> {
    let dir = identity_shim_dir().ok_or_else(|| "home directory unresolvable".to_string())?;
    remove_dir_from_user_path(
        &dir,
        "Disabled. New terminals resolve the real `claude` directly, exactly as before.",
        "Already disabled — not on your PATH.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- env scope-root ----

    /// An absolute existing directory is stored verbatim.
    #[test]
    fn scope_root_arg_accepts_an_absolute_directory() {
        let dir = tempfile::tempdir().unwrap();
        let stored = resolve_scope_root_arg(&dir.path().to_string_lossy()).unwrap();
        assert!(PathBuf::from(&stored).is_dir(), "got {stored}");
        assert!(PathBuf::from(&stored).is_absolute(), "got {stored}");
    }

    /// A RELATIVE argument is absolutized against the caller's cwd rather than
    /// rejected — the operator's shell is the one place a relative path is
    /// unambiguous — and what gets STORED is absolute. Storing the relative form
    /// would push resolution to the runner's launch directory, which is the
    /// launch-dependence the scope root exists to remove.
    #[test]
    fn scope_root_arg_absolutizes_a_relative_directory() {
        let stored =
            resolve_scope_root_arg("src").expect("`src` exists relative to the crate root");
        let stored = PathBuf::from(stored);
        assert!(stored.is_absolute(), "stored value must be absolute");
        assert!(stored.ends_with("src"), "got {}", stored.display());
    }

    /// A nonexistent path is refused SYNCHRONOUSLY. The probe path is fail-open
    /// and would merely warn and measure elsewhere, so this is the only moment
    /// the operator can be told their path is wrong.
    #[test]
    fn scope_root_arg_rejects_a_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("definitely_not_created");
        let err = resolve_scope_root_arg(&missing.to_string_lossy()).unwrap_err();
        assert!(err.contains("not an existing directory"), "got: {err}");
    }

    /// A file is not a directory — a file cwd fails every probe spawn.
    #[test]
    fn scope_root_arg_rejects_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a_file");
        std::fs::write(&file, b"x").unwrap();
        let err = resolve_scope_root_arg(&file.to_string_lossy()).unwrap_err();
        assert!(err.contains("not an existing directory"), "got: {err}");
    }

    #[test]
    fn scope_root_arg_rejects_blank() {
        let err = resolve_scope_root_arg("   ").unwrap_err();
        assert!(err.contains("non-empty"), "got: {err}");
    }

    /// `env show` must distinguish a HONOURED declaration from an IGNORED one.
    /// Both leave the same `scope_root` string in the file, so a status that
    /// only echoed the config would report a dropped scope as if it were live.
    #[test]
    fn scope_root_status_distinguishes_honoured_from_ignored() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            scope_root_status(Some(&dir.path().to_string_lossy())),
            "configured"
        );

        let ignored = scope_root_status(Some("relative/path"));
        assert!(ignored.starts_with("ignored ("), "got: {ignored}");
        assert!(ignored.contains("relative"), "got: {ignored}");

        assert!(scope_root_status(None).starts_with("default"));
    }

    /// The note must fire ONLY on a genuinely missing/unreadable `machine.json`.
    /// It used to key off the enroll wire field `machine_id`, which is now `None`
    /// on every enroll by design — keying off that would spam the note on every
    /// successful enroll.
    #[test]
    fn missing_machine_json_note_keys_off_presence_not_wire_field() {
        // Present + readable → silent, even though the enroll wire `machine_id`
        // is None.
        let present = enroll::LocalMachineIdentity {
            machine_json_present: true,
            hostname: Some("box-1".to_string()),
            coord_device_id: Some(uuid::Uuid::nil()),
        };
        assert!(missing_machine_json_note(&present).is_none());

        // Present but with an unparseable (non-UUID) device_id → still silent:
        // the file IS readable.
        let present_no_uuid = enroll::LocalMachineIdentity {
            machine_json_present: true,
            hostname: None,
            coord_device_id: None,
        };
        assert!(missing_machine_json_note(&present_no_uuid).is_none());

        // Genuinely absent → the note still fires.
        let absent = enroll::LocalMachineIdentity::default();
        let note = missing_machine_json_note(&absent).expect("note must fire when file is absent");
        assert!(
            note.contains("no readable ~/.qontinui/machine.json"),
            "{note}"
        );
    }

    #[test]
    fn path_addition_appends_when_absent() {
        assert_eq!(
            compute_path_addition("C:\\a;C:\\b", "C:\\qontinui", ';', Placement::Append).as_deref(),
            Some("C:\\a;C:\\b;C:\\qontinui")
        );
        // Empty current → just the dir (no leading separator).
        assert_eq!(
            compute_path_addition("", "C:\\qontinui", ';', Placement::Append).as_deref(),
            Some("C:\\qontinui")
        );
        // A trailing separator is collapsed (no double `;;`).
        assert_eq!(
            compute_path_addition("C:\\a;", "C:\\qontinui", ';', Placement::Append).as_deref(),
            Some("C:\\a;C:\\qontinui")
        );
    }

    /// The identity-shim install PREPENDS: its `claude` shadow dir must land at
    /// the FRONT of a PATH that ALREADY contains a real `claude` dir, or PATH's
    /// first-match-wins resolution would keep finding the real `claude` and the
    /// whole §6 feature would be a silent no-op.
    #[test]
    fn path_prepend_places_dir_before_an_existing_claude_dir() {
        // A PATH whose FIRST entry is the real `claude` dir.
        let path = "C:\\tools\\claude;C:\\Windows\\System32";
        let shim = "C:\\Users\\me\\.qontinui\\runner\\identity-shim";
        let result = compute_path_addition(path, shim, ';', Placement::Prepend).unwrap();
        assert!(
            result.starts_with(shim),
            "the shim dir must be FIRST so it wins over the real claude, got {result}"
        );
        assert_eq!(
            result,
            format!("{shim};{path}"),
            "prepend joins with exactly one separator and preserves the rest verbatim"
        );
        // The real-claude dir is still present, just no longer first.
        assert!(result.contains("C:\\tools\\claude"));

        // REGRESSION (FIX A): the shim dir already present but AFTER a real
        // `claude` dir must be MOVED to the front, and its old occurrence
        // REMOVED (no duplicate). A position-blind "present ⇒ None" left the real
        // `claude` (earlier) winning, making the opt-in a silent no-op.
        let shim_later = format!("C:\\tools\\claude;{shim};C:\\Windows\\System32");
        let moved = compute_path_addition(&shim_later, shim, ';', Placement::Prepend).unwrap();
        assert!(
            moved.starts_with(shim),
            "the shim dir must be moved to the FRONT, got {moved}"
        );
        assert_eq!(
            moved.matches(shim).count(),
            1,
            "the old occurrence must be gone — no duplicate shim entry, got {moved}"
        );
        assert_eq!(
            moved,
            format!("{shim};C:\\tools\\claude;C:\\Windows\\System32"),
            "move-to-front preserves the other entries in order"
        );

        // Already FIRST → None (no churn), even with more entries after it.
        assert!(
            compute_path_addition(
                &format!("{shim};C:\\tools\\claude"),
                shim,
                ';',
                Placement::Prepend
            )
            .is_none(),
            "prepend when the dir is already at the front is a no-op"
        );
        // Move-to-front is case-insensitive: a differently-cased existing entry
        // is still deduped (never left as a stale duplicate).
        let mixed = format!("C:\\tools\\claude;{}", shim.to_uppercase());
        let moved_ci = compute_path_addition(&mixed, shim, ';', Placement::Prepend).unwrap();
        assert!(moved_ci.starts_with(shim));
        assert_eq!(
            moved_ci,
            format!("{shim};C:\\tools\\claude"),
            "a case-variant existing occurrence is removed, not duplicated"
        );

        // Empty current → just the dir (no dangling separator), same as append.
        assert_eq!(
            compute_path_addition("", shim, ';', Placement::Prepend).as_deref(),
            Some(shim)
        );
        // A leading separator is collapsed (no double `;;`).
        assert_eq!(
            compute_path_addition(";C:\\a", "C:\\q", ';', Placement::Prepend).as_deref(),
            Some("C:\\q;C:\\a")
        );
    }

    #[test]
    fn path_addition_is_idempotent_and_case_trailing_insensitive() {
        // APPEND: present ANYWHERE → None (placement-agnostic presence suffices).
        assert!(compute_path_addition(
            "C:\\a;C:\\qontinui",
            "C:\\qontinui",
            ';',
            Placement::Append
        )
        .is_none());
        // PREPEND idempotence is POSITION-aware: a no-op ONLY when the dir is
        // already the first entry. Present-but-not-first is NOT idempotent — it
        // moves to the front (asserted in the dedicated prepend test below).
        assert!(
            compute_path_addition(
                "C:\\qontinui;C:\\a",
                "C:\\qontinui",
                ';',
                Placement::Prepend
            )
            .is_none(),
            "prepend is a no-op only when the dir already wins resolution (is first)"
        );
        // Case-insensitive + trailing-slash-insensitive → still present → None.
        assert!(
            compute_path_addition(
                "C:\\A;C:\\QONTINUI\\",
                "c:\\qontinui",
                ';',
                Placement::Append
            )
            .is_none(),
            "windows PATH is case-insensitive; trailing slash ignored"
        );
        // A substring that isn't a full entry does NOT count as present.
        assert_eq!(
            compute_path_addition(
                "C:\\qontinui-runner-old",
                "C:\\qontinui",
                ';',
                Placement::Append
            )
            .as_deref(),
            Some("C:\\qontinui-runner-old;C:\\qontinui"),
        );
        // Empty target dir → never adds (either placement).
        assert!(compute_path_addition("C:\\a", "", ';', Placement::Append).is_none());
        assert!(compute_path_addition("C:\\a", "", ';', Placement::Prepend).is_none());
    }

    /// Uninstall is the exact inverse of install: it removes precisely the
    /// entries `compute_path_addition` would have considered "present", and
    /// leaves every other entry verbatim.
    #[test]
    fn path_removal_is_the_inverse_of_addition() {
        // Present → removed, neighbours preserved verbatim.
        assert_eq!(
            compute_path_removal("C:\\a;C:\\qontinui;C:\\b", "C:\\qontinui", ';').as_deref(),
            Some("C:\\a;C:\\b")
        );
        // Case- and trailing-slash-insensitive, matching the addition rule —
        // otherwise install would say "present" while uninstall said "absent".
        assert_eq!(
            compute_path_removal("C:\\A;C:\\QONTINUI\\;C:\\b", "c:\\qontinui", ';').as_deref(),
            Some("C:\\A;C:\\b")
        );
        // Absent → None (no write at all).
        assert!(compute_path_removal("C:\\a;C:\\b", "C:\\qontinui", ';').is_none());
        // A substring that isn't a full entry is NOT removed.
        assert!(compute_path_removal("C:\\qontinui-runner-old", "C:\\qontinui", ';').is_none());
        // Empty target → never touches the PATH.
        assert!(compute_path_removal("C:\\a", "", ';').is_none());
        // Duplicates are all removed; empty segments survive.
        assert_eq!(
            compute_path_removal("C:\\q;;C:\\q;C:\\b", "C:\\q", ';').as_deref(),
            Some(";C:\\b")
        );
        // Round-trip: add then remove returns the original value.
        let original = "C:\\a;C:\\b";
        let added =
            compute_path_addition(original, "C:\\qontinui", ';', Placement::Append).unwrap();
        assert_eq!(
            compute_path_removal(&added, "C:\\qontinui", ';').as_deref(),
            Some(original)
        );
    }

    /// The identity-shim dir is a STABLE app-data path — never `temp_dir()`
    /// (swept), and never the runner install dir (whose own PATH opt-in must not
    /// silently also enable identity shadowing). It is the single source of
    /// truth the shim bin's `own_shim_dirs` reads to avoid self-spawning.
    #[test]
    fn identity_shim_dir_is_stable_app_data_not_temp_or_install_dir() {
        let Some(dir) = identity_shim_dir() else {
            return; // no home dir in this environment — nothing to assert
        };
        assert!(dir.ends_with("identity-shim"));
        assert!(
            dir.parent().unwrap().ends_with(".qontinui/runner")
                || dir.parent().unwrap().ends_with(".qontinui\\runner"),
            "must live under the established runner app-data root, got {}",
            dir.display()
        );
        assert!(
            !dir.starts_with(std::env::temp_dir()),
            "must NOT live in temp_dir — the per-PTY dirs live there and are swept"
        );
        if let Ok(install) = runner_install_dir() {
            assert_ne!(
                dir, install,
                "must NOT be the runner install dir — the two PATH opt-ins are independent"
            );
        }
    }

    /// FIX D — the identity-shim install reports HONESTLY whether it will shadow
    /// `claude`. Driven through the pure resolver with a stubbed `is_file` probe
    /// so no real `claude` on disk is needed (and it runs on every CI leg).
    #[test]
    fn identity_shim_shadow_detection_is_position_aware() {
        let shim = PathBuf::from("C:\\Users\\me\\.qontinui\\runner\\identity-shim");
        let node = PathBuf::from("C:\\tools\\node");
        // A real `claude` lives in the node dir only.
        let probe = |d: &Path| (d == node.as_path()).then(|| d.join("claude.cmd"));

        // A real `claude` on a SYSTEM dir BEFORE the shim → shadowed; the blocker
        // is named.
        let dirs = vec![
            PathBuf::from("C:\\Windows\\System32"),
            node.clone(),
            shim.clone(),
        ];
        assert_eq!(
            claude_shadowing_shim(&dirs, &shim, probe),
            Some(node.join("claude.cmd")),
            "a real claude ahead of the shim dir shadows it"
        );

        // Shim dir FIRST → it wins; a later `claude` never shadows it.
        let dirs_shim_first = vec![shim.clone(), node.clone()];
        assert_eq!(claude_shadowing_shim(&dirs_shim_first, &shim, probe), None);

        // No real claude anywhere → the shim wins.
        assert_eq!(claude_shadowing_shim(&dirs, &shim, |_| None), None);

        // Case-insensitive shim-dir match: an upper-cased shim entry still counts
        // as "reached the shim" (so a claude AFTER it does not shadow).
        let dirs_ci = vec![
            PathBuf::from(shim.to_string_lossy().to_uppercase()),
            node.clone(),
        ];
        assert_eq!(claude_shadowing_shim(&dirs_ci, &shim, probe), None);

        // Fresh-shell composition: SYSTEM entries precede USER entries, empties
        // dropped.
        let dirs = fresh_shell_path_dirs("C:\\Windows;;C:\\sys", "C:\\Users\\me\\bin; ");
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("C:\\Windows"),
                PathBuf::from("C:\\sys"),
                PathBuf::from("C:\\Users\\me\\bin"),
            ]
        );
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
