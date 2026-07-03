//! `qontinui_profile` — manage `~/.qontinui/profiles.json` from the CLI.
//!
//! Per topology plan §3 (`tmp_canonical_db_topology_plan.md`), every runner
//! reads its DB / Redis / blob / coord-service connection from a profile in
//! `~/.qontinui/profiles.json`. This binary lets a developer inspect, switch,
//! and bootstrap that file without hand-editing JSON.
//!
//! ## Usage
//!
//! ```text
//! qontinui_profile show                       # print the resolved active profile
//! qontinui_profile list                       # list available profiles
//! qontinui_profile use <name>                 # set the file's "active" field
//! qontinui_profile init                       # write starter profiles.json (host=localhost)
//! qontinui_profile init --host 192.168.1.x    # ... pointing at a remote canonical-stack host
//! qontinui_profile path                       # print the profiles.json path
//! qontinui_profile device init                # mint ~/.qontinui/machine.json + register in coord.devices
//! qontinui_profile device show                # print device_id + coord registration status
//! qontinui_profile device path                # print the machine.json path
//! qontinui_profile device pair                # pair the device with a web user (browser or --auth-token)
//! ```
//!
//! ## Unified Devices Registry — naming
//!
//! The on-disk identity file is still `~/.qontinui/machine.json` (the legacy
//! filename is preserved to avoid churn); inside, the field is now `device_id`
//! (renamed from `machine_id`), but the file is read with backward-compatible
//! deserialization so any pre-rename machine.json still loads. The coord table
//! `coord.machines` has been renamed to `coord.devices`; the runner POSTs to
//! `POST /coord/devices/register` (was `/coord/machine/register`).
//!
//! The legacy `machine` subcommand is kept as an alias for `device` so scripts
//! and operator muscle memory keep working.
//!
//! `init --host` is the LAN-client setup path: an MSI laptop / third
//! machine runs `qontinui_profile init --host <PC-LAN-IP>` once and is
//! immediately wired into the PC's canonical Postgres + Redis + MinIO
//! + coord service.
//!
//! Environment overrides (`QONTINUI_ENV`) still take precedence at runtime;
//! `qontinui_profile use foo` only updates the file's stored default.
//!
//! Exit codes follow the convention used by `runner_coordination/runner_lock.py`:
//! `0` success, `1` recoverable failure (e.g. profile not found), `2` error.
//!
//! ## Argv parsing
//!
//! The CLI uses `clap` derive macros, not hand-rolled argv inspection.
//! This matters for `--help`: an earlier hand-rolled dispatcher checked
//! `args.get(2)` for the subcommand and executed it before inspecting
//! later tokens, so `qontinui_profile machine init --help` *executed*
//! `init` (minting a machine_id, writing `machine.json`, UPSERTing
//! `coord.machines`) instead of printing help. An MSI fleet-join agent
//! hit this on 2026-05-18 and left a stranded row in canonical PG.
//! Clap's derive macros short-circuit `--help` at every level for free,
//! so the destructive paths are unreachable when help is requested.

use clap::{Parser, Subcommand};
use qontinui_runner_lib::pair::{
    coord_http_base, derive_web_base_from_coord, pair_via_browser, pair_with_auth_token,
    pair_with_pair_code, persist_pairing, tenant_id_from_oauth_claim, PairCompleteResponse,
};
use qontinui_runner_lib::profiles::{
    load_strict, profiles_path, AuthConfig, BlobConfig, Profile, ProfilesFile,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

// ============================================================================
// CLI surface
// ============================================================================

#[derive(Parser, Debug)]
#[command(
    name = "qontinui_profile",
    about = "Manage ~/.qontinui/profiles.json and ~/.qontinui/machine.json",
    long_about = "Manage ~/.qontinui/profiles.json (DB/Redis/blob/coord connection \
                  config) and ~/.qontinui/machine.json (per-device UUID used as a \
                  foreign key in coord.devices / coord.claims_audit).\n\n\
                  When invoked with no subcommand, prints the resolved active profile \
                  (equivalent to `qontinui_profile show`)."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Print the resolved active profile (default when no subcommand is given).
    Show,
    /// List profile names; the active one is marked with `*`.
    List,
    /// Set the file's `active` field to the named profile.
    Use {
        /// Profile name (must exist in profiles.json).
        name: String,
    },
    /// Write a starter profiles.json. Default host is localhost (PC-local dev).
    /// Pass `--host <PC-LAN-IP>` from a laptop / third machine to point at the PC.
    Init {
        /// Host / IP for DB, Redis, blob, and coord URLs. Defaults to localhost.
        #[arg(long, default_value = "localhost")]
        host: String,
    },
    /// Print the absolute profiles.json path.
    Path,
    /// Manage ~/.qontinui/machine.json (per-device identity for coord.devices
    /// registration; required for /coord/status POSTs and non-NULL claims_audit
    /// rows). Phase 3 (Unified Devices Registry) canonical name.
    Device {
        #[command(subcommand)]
        sub: DeviceCmd,
    },
    /// Legacy alias for `device` — kept so scripts and operator muscle memory
    /// keep working. Dispatches to the same handlers as `device`. The on-disk
    /// filename `~/.qontinui/machine.json` is also preserved for compat.
    Machine {
        #[command(subcommand)]
        sub: DeviceCmd,
    },
    /// Manage the machine-side dev-environment capture agent
    /// (feat/devenv-environments). `enroll` binds this machine to a web
    /// environment via a per-machine API key; `capture` pushes a secret-free
    /// config envelope; `show` prints enrollment state.
    Env {
        #[command(subcommand)]
        sub: EnvCmd,
    },
}

#[derive(Subcommand, Debug)]
enum EnvCmd {
    /// Enroll this machine into a web environment via an enrollment code.
    /// POSTs `{enrollment_code, machine_id, hostname}` to
    /// `{backend}/api/v1/devenv/agent/enroll`; on success stores the returned
    /// `mk_<token>` machine key in secure storage and writes
    /// `~/.qontinui/env-agent.json` with the RESPONSE environment_id.
    Enroll {
        /// The enrollment code minted by the web dashboard.
        #[arg(long)]
        code: String,
        /// Override the backend base URL. Falls back to `QONTINUI_WEB_BASE`,
        /// then a web base derived from the active profile's coord_url.
        #[arg(long)]
        backend: Option<String>,
        /// Reserved override for the target environment id. Normally the
        /// environment is assigned by the enroll response; this is only used
        /// for diagnostics / re-enroll against a specific environment.
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

#[derive(Subcommand, Debug)]
enum DeviceCmd {
    /// Mint UUID v4 + hostname to machine.json (atomic), then UPSERT into the
    /// active profile's coord.devices via POST /coord/devices/register.
    /// Idempotent: re-runs re-use the existing UUID and refresh hostname.
    /// `--name` picks a user-friendly display name (defaults to hostname).
    Init {
        /// Optional display name; defaults to hostname.
        #[arg(long)]
        name: Option<String>,
    },
    /// Print device_id + coord.devices registration timestamps as JSON.
    Show,
    /// Print the absolute machine.json path.
    Path,
    /// Bind this device to a web-backend user. Default mode opens
    /// /connect-runner in the system browser and waits for the user's
    /// confirmation; `--auth-token <token>` takes a pre-issued OAuth token
    /// and headlessly POSTs to coord. On success persists the device-token
    /// JWT + paired user_id locally.
    Pair {
        /// Headless mode: use a pre-issued OAuth token. Mutually exclusive
        /// with `--browser` and `--pair-code`.
        #[arg(long, value_name = "OAUTH_TOKEN")]
        auth_token: Option<String>,
        /// Headless mode: redeem a 6-char single-use pair code minted from
        /// the dashboard's Auth Tokens tab. Mutually exclusive with
        /// `--browser` and `--auth-token`. The pair code itself is the
        /// credential — no OAuth token required.
        #[arg(long, value_name = "CODE")]
        pair_code: Option<String>,
        /// Browser mode (default). Mutually exclusive with `--auth-token`
        /// and `--pair-code`.
        #[arg(long)]
        browser: bool,
        /// Explicit tenant scope for the new device. Overrides any
        /// `tenant_id` claim auto-extracted from the OAuth token.
        /// Required for `--browser` mode (no OAuth token to read a claim
        /// from). Ignored for `--pair-code` — the tenant is burned into
        /// the code at mint time and propagates through the redeem
        /// response. Phase 2 of the default-tenant-propagation plan
        /// (Q3 resolution: OAuth claim with `--tenant-id` override).
        #[arg(long, value_name = "UUID")]
        tenant_id: Option<String>,
    },
}

// ============================================================================
// Entry point
// ============================================================================

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.cmd.unwrap_or(Cmd::Show) {
        Cmd::Show => cmd_show(),
        Cmd::List => cmd_list(),
        Cmd::Use { name } => cmd_use(&name),
        Cmd::Init { host } => cmd_init(&host),
        Cmd::Path => cmd_path(),
        // `machine` is a legacy alias — both variants dispatch to the same
        // device handlers. Phase 3 unified the canonical name on `device`.
        Cmd::Device { sub } | Cmd::Machine { sub } => match sub {
            DeviceCmd::Init { name } => cmd_device_init(name.as_deref()),
            DeviceCmd::Show => cmd_device_show(),
            DeviceCmd::Path => cmd_device_path(),
            DeviceCmd::Pair {
                auth_token,
                pair_code,
                browser,
                tenant_id,
            } => cmd_device_pair(
                auth_token.as_deref(),
                pair_code.as_deref(),
                browser,
                tenant_id.as_deref(),
            ),
        },
        Cmd::Env { sub } => match sub {
            EnvCmd::Enroll {
                code,
                backend,
                environment,
            } => cmd_env_enroll(&code, backend.as_deref(), environment.as_deref()),
            EnvCmd::Capture { dry_run } => cmd_env_capture(dry_run),
            EnvCmd::Show => cmd_env_show(),
        },
    }
}

// ============================================================================
// profile-level helpers
// ============================================================================

fn cmd_path() -> ExitCode {
    match profiles_path() {
        Some(p) => {
            println!("{}", p.display());
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("could not resolve home directory");
            ExitCode::from(2)
        }
    }
}

fn cmd_show() -> ExitCode {
    match load_strict() {
        Ok(p) => {
            // Redact secrets in display — same idea as `printenv` not
            // dumping passwords. The DSN is shown as-is because that's the
            // primary debugging signal; access keys and tokens get masked.
            let mut blob_view = serde_json::Value::Null;
            if let Some(b) = &p.blob {
                blob_view = json!({
                    "kind":     b.kind,
                    "endpoint": b.endpoint,
                    "region":   b.region,
                    "bucket":   b.bucket,
                    "access_key": b.access_key.as_ref().map(|_| "<set>"),
                    "secret_key": b.secret_key.as_ref().map(|_| "<set>"),
                });
            }
            let mut auth_view = serde_json::Value::Null;
            if let Some(a) = &p.auth {
                auth_view = json!({
                    "kind":      a.kind,
                    "issuer":    a.issuer,
                    "client_id": a.client_id,
                    "token":     a.token.as_ref().map(|_| "<set>"),
                });
            }
            let out = json!({
                "active":       p.source,
                "database_url": p.database_url,
                "redis_url":    p.redis_url,
                "blob":         blob_view,
                "coord_url":    p.coord_url,
                "auth":         auth_view,
            });
            println!("{}", serde_json::to_string_pretty(&out).unwrap());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(1)
        }
    }
}

fn cmd_list() -> ExitCode {
    let path = match profiles_path() {
        Some(p) => p,
        None => {
            eprintln!("could not resolve home directory");
            return ExitCode::from(2);
        }
    };
    if !path.exists() {
        eprintln!(
            "profiles.json not found at {}\n\
             Run 'qontinui_profile init' (PC-local) or 'qontinui_profile init --host <PC-LAN-IP>' (laptop / 3rd machine).",
            path.display()
        );
        return ExitCode::from(1);
    }
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("read failed: {}", e);
            return ExitCode::from(2);
        }
    };
    let file: ProfilesFile = match serde_json::from_slice(&bytes) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("parse failed: {}", e);
            return ExitCode::from(2);
        }
    };
    let active = file.active.as_deref().unwrap_or("(unset)");
    let mut names: Vec<&str> = file.profiles.keys().map(|s| s.as_str()).collect();
    names.sort();
    for n in names {
        let marker = if n == active { "*" } else { " " };
        println!("{} {}", marker, n);
    }
    ExitCode::SUCCESS
}

fn cmd_use(name: &str) -> ExitCode {
    let path = match profiles_path() {
        Some(p) => p,
        None => {
            eprintln!("could not resolve home directory");
            return ExitCode::from(2);
        }
    };
    if !path.exists() {
        eprintln!(
            "profiles.json not found at {}\n\
             Run 'qontinui_profile init' (PC-local) or 'qontinui_profile init --host <PC-LAN-IP>' (laptop / 3rd machine).",
            path.display()
        );
        return ExitCode::from(1);
    }
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("read failed: {}", e);
            return ExitCode::from(2);
        }
    };
    let mut file: ProfilesFile = match serde_json::from_slice(&bytes) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("parse failed: {}", e);
            return ExitCode::from(2);
        }
    };
    if !file.profiles.contains_key(name) {
        let mut available: Vec<&str> = file.profiles.keys().map(|s| s.as_str()).collect();
        available.sort();
        eprintln!(
            "profile '{}' not found. Available: {}",
            name,
            available.join(", ")
        );
        return ExitCode::from(1);
    }
    file.active = Some(name.to_string());
    if let Err(e) = atomic_write(&path, &file) {
        eprintln!("write failed: {}", e);
        return ExitCode::from(2);
    }
    println!("active profile set to '{}'", name);
    ExitCode::SUCCESS
}

fn cmd_init(host: &str) -> ExitCode {
    let path = match profiles_path() {
        Some(p) => p,
        None => {
            eprintln!("could not resolve home directory");
            return ExitCode::from(2);
        }
    };
    if path.exists() {
        eprintln!(
            "profiles.json already exists at {} — refusing to overwrite. \
             Edit by hand, or `rm {}` and re-run.",
            path.display(),
            path.display()
        );
        return ExitCode::from(1);
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("mkdir {} failed: {}", parent.display(), e);
            return ExitCode::from(2);
        }
    }

    let mut profiles = HashMap::new();
    let canonical = Profile {
        database_url: Some(format!(
            "postgresql://qontinui:qontinui@{host}:6543/qontinui_canonical"
        )),
        redis_url: Some(format!("redis://{host}:6379/0")),
        blob: Some(BlobConfig {
            kind: "minio".to_string(),
            endpoint: Some(format!("http://{host}:9000")),
            region: Some("us-east-1".to_string()),
            bucket: Some("qontinui-blobs".to_string()),
            access_key: Some("qontinui".to_string()),
            secret_key: Some("qontinui-dev-secret".to_string()),
        }),
        coord_url: Some(format!("ws://{host}:9870/ws")),
        auth: Some(AuthConfig {
            kind: "issuer".to_string(),
            issuer: Some(format!("http://{host}:8000")),
            client_id: Some("qontinui-runner".to_string()),
            token: None,
        }),
    };
    profiles.insert("canonical".to_string(), canonical);
    let file = ProfilesFile {
        active: Some("canonical".to_string()),
        profiles,
    };
    if let Err(e) = atomic_write(&path, &file) {
        eprintln!("write {} failed: {}", path.display(), e);
        return ExitCode::from(2);
    }
    println!("wrote {} (active=canonical, host={})", path.display(), host);
    ExitCode::SUCCESS
}

fn atomic_write(path: &Path, file: &ProfilesFile) -> std::io::Result<()> {
    let pretty = serde_json::to_vec_pretty(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &pretty)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ============================================================================
// device subcommand
// ============================================================================
//
// Per topology plan §3, every runner has a stable device identity stored at
// ~/.qontinui/machine.json (legacy filename). The active profile's coord
// service uses this UUID as the foreign key in coord.claims_audit /
// coord.device_status / etc.  `qontinui_profile init` writes profiles.json
// but NOT machine.json — this gap left new devices with NULL device_id audit
// rows and rejected /coord/status POSTs (qontinui-coord/src/status.rs:116-122).
//
// Phase 3 (Unified Devices Registry): `coord.machines` → `coord.devices`.
// The HTTP endpoint moved to `POST /coord/devices/register`; the on-disk
// field renamed from `machine_id` to `device_id` (with a `machine_id` serde
// alias for back-compat with pre-rename machine.json files).

/// Shape of `~/.qontinui/machine.json`. UUID v4 + hostname only — additional
/// per-device state (current_branches, last_alembic_head) lives in
/// coord.devices, not this file. `name` is optional and defaults to hostname
/// at register-time when absent.
///
/// `device_id` is serde-aliased to `machine_id` so a pre-Phase-3 machine.json
/// (which writes `"machine_id": "..."`) deserializes without manual migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceFile {
    #[serde(alias = "machine_id")]
    device_id: String,
    hostname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

fn device_file_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("machine.json"))
}

fn read_device_file(path: &Path) -> std::io::Result<DeviceFile> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn atomic_write_device(path: &Path, file: &DeviceFile) -> std::io::Result<()> {
    let pretty = serde_json::to_vec_pretty(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &pretty)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn detect_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Per the platform docs: `std::env::consts::OS` is one of {"linux",
/// "macos", "windows", "ios", "android", "freebsd", "dragonfly", "netbsd",
/// "openbsd", "solaris"}. Always available; no dependency required.
fn detect_os() -> String {
    std::env::consts::OS.to_string()
}

/// OS version string via the already-present `sysinfo = "0.32"` crate
/// (`src-tauri/Cargo.toml:77`). `sysinfo::System::long_os_version()` returns
/// e.g. "Windows 11 Pro 23H2 (build 22631)" or "macOS 14.4 Sonoma". `None`
/// on platforms where sysinfo can't probe; the runner is fine sending None.
fn detect_os_version() -> Option<String> {
    use sysinfo::System;
    System::long_os_version().or_else(System::os_version)
}

/// Path to the paired-user JSON written by `device pair` and read by `device
/// init` to attach a `user_id` to the register payload. Living under the
/// Tauri app's local data directory (alongside `auth_tokens.enc`) keeps the
/// CLI bin and the GUI runner reading the same file.
fn paired_user_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("com.qontinui.runner").join("paired_user.json"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PairedUserFile {
    user_id: String,
    /// Mirror of `qontinui_runner_lib::pair::PairedUserFile::tenant_id`.
    /// `#[serde(default)]` keeps legacy files (user_id-only) parsing.
    /// The CLI bootstrap (`register_with_coord`) forwards this to coord
    /// on `POST /coord/devices/register`; absent → JWT-claim fallback.
    #[serde(default)]
    tenant_id: Option<String>,
}

fn load_paired_user_id() -> Option<String> {
    let path = paired_user_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let parsed: PairedUserFile = serde_json::from_slice(&bytes).ok()?;
    Some(parsed.user_id)
}

/// Load `(user_id, tenant_id_opt)` from `paired_user.json`. Returns
/// `None` if the file doesn't exist or isn't parseable. Phase 2 of the
/// default-tenant-propagation plan — the CLI bootstrap needs both
/// fields to satisfy coord's `tenant_id_required` gate.
fn load_paired_user() -> Option<(String, Option<String>)> {
    let path = paired_user_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let parsed: PairedUserFile = serde_json::from_slice(&bytes).ok()?;
    Some((parsed.user_id, parsed.tenant_id))
}

fn cmd_device_path() -> ExitCode {
    match device_file_path() {
        Some(p) => {
            println!("{}", p.display());
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("could not resolve home directory");
            ExitCode::from(2)
        }
    }
}

fn cmd_device_init(name_arg: Option<&str>) -> ExitCode {
    let path = match device_file_path() {
        Some(p) => p,
        None => {
            eprintln!("could not resolve home directory");
            return ExitCode::from(2);
        }
    };

    // Read existing machine.json if present (re-use UUID for idempotence);
    // otherwise mint a fresh UUID v4. Hostname is always re-detected — a
    // laptop can rename between boots and the file should reflect current.
    let hostname_now = detect_hostname();
    let (file, was_new) = if path.exists() {
        match read_device_file(&path) {
            Ok(mut existing) => {
                existing.hostname = hostname_now.clone();
                if let Some(n) = name_arg {
                    existing.name = Some(n.to_string());
                }
                (existing, false)
            }
            Err(e) => {
                eprintln!(
                    "machine.json at {} is unreadable ({}). \
                     Refusing to overwrite — inspect or `rm` and re-run.",
                    path.display(),
                    e
                );
                return ExitCode::from(2);
            }
        }
    } else {
        let id = uuid::Uuid::new_v4().to_string();
        (
            DeviceFile {
                device_id: id,
                hostname: hostname_now.clone(),
                name: name_arg.map(|s| s.to_string()),
            },
            true,
        )
    };

    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("mkdir {} failed: {}", parent.display(), e);
            return ExitCode::from(2);
        }
    }
    if let Err(e) = atomic_write_device(&path, &file) {
        eprintln!("write {} failed: {}", path.display(), e);
        return ExitCode::from(2);
    }
    // Phase 0 multi-user readiness — ensure the canonical `device_id` key
    // lands even if a legacy `machine_id`-only file slipped past the
    // DeviceFile serialization (e.g. a sibling reader rewrote it). No-op
    // when already canonical.
    qontinui_runner_lib::pair::ensure_device_id_persisted_at(&path);
    if was_new {
        println!(
            "wrote machine.json: {} (host={})",
            file.device_id, file.hostname
        );
    } else {
        println!(
            "re-using existing machine.json: {} (host={})",
            file.device_id, file.hostname
        );
    }

    // Register with coord via HTTP `POST /coord/devices/register`. File
    // creation succeeds even if coord registration fails — the local
    // machine.json is the canonical record; coord.devices is a derived view
    // (qontinui-coord re-syncs from machine.json on next /coord/status POST).
    let display_name = file.name.clone().unwrap_or_else(|| file.hostname.clone());
    let paired = load_paired_user();
    let (paired_user_id, paired_tenant_id) = match paired {
        Some((uid, tid)) => (Some(uid), tid),
        None => (None, None),
    };
    // Resolve tenant_id for the register payload (coord rejects with
    // 400 `tenant_id_required` otherwise). Order: paired_user.json
    // → cached device-token JWT claim → hard error. Matches the
    // heartbeat resolution chain in `fleet::resolve_tenant_id`.
    let tenant_id = match paired_tenant_id {
        Some(s) => s,
        None => {
            let token = qontinui_runner_lib::auth::AuthManager::new()
                .get_access_token()
                .unwrap_or_default();
            match tenant_id_from_oauth_claim(&token) {
                Some(s) => s,
                None => {
                    eprintln!(
                        "warning: cannot register with coord — tenant_id unresolvable. \
                         Run `qontinui_profile device pair --tenant-id <uuid> [--auth-token <oauth>]` \
                         first so coord knows which tenant owns this device.\n\
                         (machine.json was still written.)"
                    );
                    return ExitCode::SUCCESS;
                }
            }
        }
    };
    match register_with_coord(
        &file.device_id,
        &file.hostname,
        &display_name,
        paired_user_id.as_deref(),
        &tenant_id,
    ) {
        Ok(()) => {
            println!("registered with coord via HTTP (POST /coord/devices/register)");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!(
                "warning: coord registration failed: {}\n\
                 (machine.json was still written; re-run `qontinui_profile device init` once coord is reachable)",
                e
            );
            ExitCode::SUCCESS
        }
    }
}

fn cmd_device_show() -> ExitCode {
    let path = match device_file_path() {
        Some(p) => p,
        None => {
            eprintln!("could not resolve home directory");
            return ExitCode::from(2);
        }
    };
    if !path.exists() {
        eprintln!(
            "machine.json not found at {}\n\
             Run 'qontinui_profile device init' to mint identity and register with coord.",
            path.display()
        );
        return ExitCode::from(1);
    }
    let file = match read_device_file(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("read failed: {}", e);
            return ExitCode::from(2);
        }
    };

    let coord_status = match query_coord_registration(&file.device_id) {
        Ok(Some((created_at, last_seen_at))) => {
            json!({ "registered": true, "created_at": created_at, "last_seen_at": last_seen_at })
        }
        Ok(None) => json!({ "registered": false }),
        Err(e) => json!({ "registered": null, "error": e }),
    };

    let out = json!({
        "device_id": file.device_id,
        "hostname":  file.hostname,
        "name":      file.name,
        "path":      path.display().to_string(),
        "coord":     coord_status,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    ExitCode::SUCCESS
}

// ============================================================================
// device pair — bind this device to a web-backend user
// ============================================================================
//
// Two modes:
//
// 1. `--browser` (default): opens `{web_backend}/connect-runner?state=<nonce>
//    &callback=http://127.0.0.1:<port>/auth/runner-token-callback
//    &device_name=<hostname>` in the user's default browser. We spin up a
//    one-shot localhost axum server, wait for the redirect, capture
//    `(state, token, token_id=device_id)`, POST `(state, token)` to coord's
//    `POST /coord/devices/pair-complete`, and receive a device-token JWT.
//
// 2. `--auth-token <oauth>`: headless. POSTs `Authorization: Bearer <oauth>`
//    to coord's `POST /coord/devices/pair-cli`; receives the device-token JWT
//    directly.
//
// On success we persist:
//
// - The device-token JWT via the runner's existing `AuthManager` /
//   `SecureStorage` (AES-256-GCM at `{data_local_dir}/com.qontinui.runner/
//   auth_tokens.enc`). The same file may already hold a pre-Phase-3
//   `qontinui_runner_<random>` bearer; this overwrites it with the JWT (see
//   the comment in `secure_storage.rs` documenting the format change).
//
// - The paired user_id to `paired_user.json` (under the same data_local_dir),
//   so the next `device init` carries it on the register payload.

enum PairMode {
    Browser,
    AuthToken(String),
    PairCode(String),
}

fn select_pair_mode(
    auth_token: Option<&str>,
    pair_code: Option<&str>,
    _browser: bool,
) -> Result<PairMode, String> {
    // clap's `Pair` variant carries `--browser`, `--auth-token`, and
    // `--pair-code`. The mutually-exclusive constraint is enforced here
    // (we can't use clap's `conflicts_with` because `browser` is a bool
    // flag with a default of false). Priority: pair_code > auth_token >
    // browser-default.
    if let Some(code) = pair_code {
        if code.is_empty() {
            return Err("--pair-code requires a value: --pair-code <CODE>".to_string());
        }
        return Ok(PairMode::PairCode(code.to_string()));
    }
    match auth_token {
        Some(token) => {
            if token.is_empty() {
                return Err("--auth-token requires a value: --auth-token <oauth-token>".to_string());
            }
            Ok(PairMode::AuthToken(token.to_string()))
        }
        None => Ok(PairMode::Browser),
    }
}

fn cmd_device_pair(
    auth_token: Option<&str>,
    pair_code: Option<&str>,
    browser: bool,
    tenant_id_flag: Option<&str>,
) -> ExitCode {
    // clap can't express "exactly one of browser/auth_token/pair_code";
    // reject the combinations by hand.
    let exclusive_set = [auth_token.is_some(), pair_code.is_some(), browser];
    if exclusive_set.iter().filter(|b| **b).count() > 1 {
        eprintln!("error: --browser, --auth-token, and --pair-code are mutually exclusive");
        return ExitCode::from(2);
    }
    let mode = match select_pair_mode(auth_token, pair_code, browser) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {}", e);
            return ExitCode::from(2);
        }
    };
    let base = match coord_http_base() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: could not resolve coord_url: {}", e);
            return ExitCode::from(2);
        }
    };

    // Resolve tenant_id when needed. For PairCode mode the tenant is
    // carried back in the redeem response (we resolve it from there
    // post-pair); for AuthToken + Browser we need an explicit value
    // up-front. Phase 2 of the default-tenant-propagation plan.
    let preflight_tenant_id: Option<uuid::Uuid> = match &mode {
        PairMode::PairCode(_) => None,
        _ => {
            let resolved: Result<uuid::Uuid, String> = match tenant_id_flag {
                Some(s) => uuid::Uuid::parse_str(s.trim())
                    .map_err(|e| format!("--tenant-id is not a valid UUID: {e}")),
                None => match &mode {
                    PairMode::AuthToken(token) => match tenant_id_from_oauth_claim(token) {
                        Some(s) => uuid::Uuid::parse_str(s.trim()).map_err(|e| {
                            format!(
                                "OAuth token's tenant_id claim is not a valid UUID ({e}); \
                                 pass --tenant-id <uuid> to override"
                            )
                        }),
                        None => Err("no `tenant_id` claim found in OAuth token; \
                             pass --tenant-id <uuid> explicitly"
                            .to_string()),
                    },
                    PairMode::Browser => {
                        Err("--tenant-id <uuid> is required for the browser pair flow \
                             (no OAuth token to read a claim from)"
                            .to_string())
                    }
                    PairMode::PairCode(_) => unreachable!("handled above"),
                },
            };
            match resolved {
                Ok(t) => Some(t),
                Err(e) => {
                    eprintln!("error: {}", e);
                    return ExitCode::from(2);
                }
            }
        }
    };

    let result: Result<PairCompleteResponse, String> = match &mode {
        PairMode::PairCode(code) => {
            let device_id = match qontinui_runner_lib::pair::read_device_id_from_disk() {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("error: {}", e);
                    return ExitCode::from(2);
                }
            };
            // Pair codes redeem against the web backend. Derive the web
            // base from the active profile's coord_url just like
            // pair_via_browser does — operator can override via
            // QONTINUI_WEB_BASE.
            let web_base = std::env::var("QONTINUI_WEB_BASE")
                .ok()
                .unwrap_or_else(|| derive_web_base_from_coord(&base));
            pair_with_pair_code(&web_base, code, &device_id)
        }
        PairMode::AuthToken(token) => {
            pair_with_auth_token(&base, token, preflight_tenant_id.expect("set above"))
        }
        PairMode::Browser => pair_via_browser(&base, preflight_tenant_id.expect("set above")),
    };

    match result {
        Ok(resp) => {
            // For PairCode mode the tenant came back in the response;
            // for the other modes we resolved it pre-flight.
            let effective_tenant_id: uuid::Uuid = match preflight_tenant_id {
                Some(t) => t,
                None => {
                    // PairCode path: the redeem response sets
                    // PairCompleteResponse.tenant_id (Option<String>).
                    // Parse it; bail if missing / malformed (would indicate
                    // a backend protocol break, not a runner-side bug).
                    match resp.tenant_id.as_deref() {
                        Some(s) => match uuid::Uuid::parse_str(s.trim()) {
                            Ok(t) => t,
                            Err(e) => {
                                eprintln!(
                                    "error: pairing succeeded but server returned malformed tenant_id ({e}); not persisting"
                                );
                                return ExitCode::from(2);
                            }
                        },
                        None => {
                            eprintln!(
                                "error: pairing succeeded but server omitted tenant_id; not persisting"
                            );
                            return ExitCode::from(2);
                        }
                    }
                }
            };
            if let Err(e) = persist_pairing(&resp, effective_tenant_id) {
                eprintln!(
                    "error: pairing succeeded but persisting locally failed: {}",
                    e
                );
                return ExitCode::from(2);
            }
            println!(
                "device paired: user_id={} (device-token JWT saved to auth_tokens.enc)",
                resp.user_id
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: pairing failed: {}", e);
            ExitCode::from(1)
        }
    }
}

/// Register this device with coord via `POST /coord/devices/register`.
/// The endpoint UPSERTs `coord.devices` and returns the resulting row.
/// Replaces the prior direct-PG INSERT path so the runner no longer needs
/// PG credentials to coord's database.
///
/// The CLI bootstrap doesn't know its health URL — that's the supervisor's
/// job (Phase 3 of the fleet-health-url advertisement plan). We pass
/// `health_url: null`; coord's endpoint treats missing/null as "leave the
/// stored health_url as-is" (UPSERT semantics).
///
/// Phase 3 (Unified Devices Registry) body additions:
/// - `os`: from `std::env::consts::OS` (always available, no dep).
/// - `os_version`: from `sysinfo::System::long_os_version()` (best effort).
/// - `capabilities`: feature-flag enumeration; starts with `["runner"]` as
///   a sentinel for the canonical runner role. Future capabilities (e.g.
///   `vision`, `accessibility-bridge`) get pushed by Phase 6 work.
/// - `name`: user-supplied display name; defaults to hostname.
/// - `user_id`: optional UUID, present only if the device has been paired
///   (the file `{data_local_dir}/com.qontinui.runner/paired_user.json`
///   exists from a prior `device pair` run). When absent, coord treats this
///   as a "system device" register.
fn register_with_coord(
    device_id: &str,
    hostname: &str,
    name: &str,
    user_id: Option<&str>,
    tenant_id: &str,
) -> Result<(), String> {
    // Validate UUID shape up front so a malformed machine.json fails fast
    // with a clear error instead of bouncing off a 400 from coord.
    let _ = uuid::Uuid::parse_str(device_id)
        .map_err(|e| format!("device_id is not a valid UUID: {}", e))?;
    if let Some(uid) = user_id {
        uuid::Uuid::parse_str(uid)
            .map_err(|e| format!("paired user_id is not a valid UUID: {}", e))?;
    }
    let _ = uuid::Uuid::parse_str(tenant_id)
        .map_err(|e| format!("tenant_id is not a valid UUID: {}", e))?;
    let base = coord_http_base()?;
    let url = format!("{}/coord/devices/register", base);
    // `device_id` is the canonical name in coord.devices. We also emit
    // `machine_id` as a duplicate alias key for back-compat with a Phase-2
    // coord that still reads the old name — the field is removed once
    // Phase 2 is merged everywhere.
    //
    // `tenant_id` is REQUIRED by coord's `post_device_register` handler
    // (`routes_phase3.rs:257-269` returns `400 tenant_id_required`
    // otherwise). Phase 2 of the default-tenant-propagation plan.
    let body = serde_json::json!({
        "device_id":    device_id,
        "machine_id":   device_id,
        "hostname":     hostname,
        "name":         name,
        "os":           detect_os(),
        "os_version":   detect_os_version(),
        "capabilities": vec!["runner".to_string()],
        "user_id":      user_id,
        "tenant_id":    tenant_id,
        "health_url":   serde_json::Value::Null,
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("reqwest client build failed: {}", e))?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .map_err(|e| format!("POST {} failed (coord unreachable?): {}", url, e))?;
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let body_text = resp
        .text()
        .unwrap_or_else(|_| "<unable to read response body>".to_string());
    // Phase 2 of the unknown-tenant plan: coord now returns HTTP 400 with
    // `{"error":"unknown_tenant","tenant_id":"<uuid>", ...}` when the supplied
    // tenant_id is not present in coord.tenants. Surface an actionable recovery
    // hint instead of the raw body. Any other 400 / non-2xx keeps the generic
    // error below.
    if status.as_u16() == 400 {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body_text) {
            if json.get("error").and_then(|v| v.as_str()) == Some("unknown_tenant") {
                let body_tenant = json
                    .get("tenant_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or(tenant_id);
                return Err(format!(
                    "coord rejected device registration — tenant_id {body_tenant} is not registered. \
                     Common cause: paired_user.json carries a stale tenant_id from a re-paired session. \
                     Re-pair the device (`qontinui_profile device pair --tenant-id {body_tenant}`) or \
                     update the runner's paired_user.json to the current tenant_id (look up via web UI \
                     Settings → Account)."
                ));
            }
        }
    }
    Err(format!(
        "POST {} returned HTTP {}: {}",
        url, status, body_text
    ))
}

fn query_coord_registration(device_id: &str) -> Result<Option<(String, String)>, String> {
    let id = uuid::Uuid::parse_str(device_id)
        .map_err(|e| format!("device_id is not a valid UUID: {}", e))?;
    let dsn = active_profile_dsn()?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime build failed: {}", e))?;
    rt.block_on(async move {
        let (client, conn) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
            .await
            .map_err(|e| format!("connect to coord PG failed: {}", e))?;
        let join = tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!("pg connection ended: {}", e);
            }
        });
        // Try coord.devices first (Phase 3 target). If the table doesn't
        // exist yet (Phase 2 not landed), fall back to coord.machines so the
        // CLI remains usable during the migration window.
        let row = match client
            .query_opt(
                "SELECT created_at::text, last_seen_at::text \
                 FROM coord.devices WHERE device_id = $1",
                &[&id],
            )
            .await
        {
            Ok(r) => r,
            Err(_) => client
                .query_opt(
                    "SELECT created_at::text, last_seen_at::text \
                     FROM coord.machines WHERE machine_id = $1",
                    &[&id],
                )
                .await
                .map_err(|e| format!("SELECT coord.devices/.machines failed: {}", e))?,
        };
        drop(client);
        let _ = join.await;
        Ok(row.map(|r| (r.get::<_, String>(0), r.get::<_, String>(1))))
    })
}

fn active_profile_dsn() -> Result<String, String> {
    load_strict()
        .map(|p| p.database_url)
        .map_err(|e| format!("active profile has no database_url: {}", e))
}

// ============================================================================
// env subcommand — machine-side dev-environment capture agent
// ============================================================================
//
// `env enroll` binds this machine to a web environment via a per-machine API
// key (`mk_<token>`), stored in the encrypted secure storage. `env capture`
// pushes a SECRET-FREE config envelope. `env show` prints enrollment state.
//
// The capture/push logic + envelope assembly live in
// `qontinui_runner_lib::env_agent` so the runner GUI's background task and this
// CLI share one code path.

fn cmd_env_enroll(
    code: &str,
    backend_arg: Option<&str>,
    environment_arg: Option<&str>,
) -> ExitCode {
    use qontinui_runner_lib::env_agent::enroll::{self, EnrollParams};

    if code.trim().is_empty() {
        eprintln!("error: --code requires a non-empty enrollment code");
        return ExitCode::from(2);
    }

    let backend = match enroll::resolve_backend_base(backend_arg) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };

    // machine_id + hostname come from ~/.qontinui/machine.json (run
    // `qontinui_profile device init` first for a stable identity). Absence is
    // tolerated — the shared core sends nulls and the backend may assign a
    // machine_id.
    let (machine_id, hostname) = enroll::local_machine_identity();
    if machine_id.is_none() {
        eprintln!(
            "note: no readable ~/.qontinui/machine.json — enrolling with null machine_id/hostname \
             (run `qontinui_profile device init` first for a stable identity)"
        );
    }

    // The POST + key storage + env-agent.json write live in the shared
    // `env_agent::enroll` core, so this CLI, the in-app Tauri command, and the
    // (Phase 3) dispatched directive consumer all enroll through one code path.
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
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn cmd_env_capture(dry_run: bool) -> ExitCode {
    // Publish a lazy PG pool from the active profile so the high-value
    // `db_schema` collector (alembic_head + schema/table census) can run. The
    // full runner does this at boot (main.rs fleet-publishers block); the
    // standalone CLI has no pre-built pool, so without this the section is
    // always omitted and schema drift is invisible from the CLI. Best-effort:
    // a build failure here (or a connect failure later inside the collector)
    // just omits the `db_schema` section — capture still succeeds.
    let profile = qontinui_runner_lib::profiles::load();
    if let Err(e) = qontinui_runner_lib::env_agent::publish_pg_pool_from_url(&profile.database_url)
    {
        eprintln!("note: db_schema collector unavailable — {e}");
    }

    if dry_run {
        match qontinui_runner_lib::env_agent::build_envelope_blocking() {
            Ok(envelope) => match serde_json::to_string_pretty(&envelope) {
                Ok(s) => {
                    println!("{s}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: serialize envelope failed: {e}");
                    ExitCode::from(2)
                }
            },
            Err(e) => {
                eprintln!("error: building envelope failed: {e}");
                ExitCode::from(2)
            }
        }
    } else {
        match qontinui_runner_lib::env_agent::capture_and_push_blocking() {
            Ok(()) => {
                println!("capture pushed (or skipped — machine not enrolled)");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: capture/push failed: {e}");
                ExitCode::from(1)
            }
        }
    }
}

fn cmd_env_show() -> ExitCode {
    let cfg = qontinui_runner_lib::env_agent::config::EnvAgentConfig::load();
    let key_stored = qontinui_runner_lib::secure_storage::SecureStorage::new()
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
            "config_path": qontinui_runner_lib::env_agent::config::EnvAgentConfig::path()
                .map(|p| p.display().to_string()),
        }),
        None => json!({
            "enrolled": false,
            "machine_key_stored": key_stored,
            "config_path": qontinui_runner_lib::env_agent::config::EnvAgentConfig::path()
                .map(|p| p.display().to_string()),
            "note": "no env-agent.json — run `qontinui_profile env enroll --code <code>`",
        }),
    };
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use clap::CommandFactory;

    #[test]
    fn atomic_write_device_via_tmp_then_rename() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("machine.json");
        let file = DeviceFile {
            device_id: "00000000-0000-4000-8000-000000000000".to_string(),
            hostname: "test-host".to_string(),
            name: Some("test-display-name".to_string()),
        };
        atomic_write_device(&path, &file).expect("write");
        let loaded = read_device_file(&path).expect("read");
        assert_eq!(loaded.device_id, file.device_id);
        assert_eq!(loaded.hostname, file.hostname);
        assert_eq!(loaded.name, file.name);
        // The .tmp sibling must not linger after a successful rename.
        let tmp = path.with_extension("json.tmp");
        assert!(!tmp.exists(), "tmp file should be renamed away");
    }

    /// Back-compat: a pre-Phase-3 machine.json (using the old `machine_id`
    /// field name) deserializes via serde alias.
    #[test]
    fn read_device_file_accepts_legacy_machine_id_alias() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("machine.json");
        let legacy = serde_json::json!({
            "machine_id": "00000000-0000-4000-8000-000000000000",
            "hostname": "legacy-host",
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).expect("write");
        let loaded = read_device_file(&path).expect("read legacy");
        assert_eq!(loaded.device_id, "00000000-0000-4000-8000-000000000000");
        assert_eq!(loaded.hostname, "legacy-host");
        assert!(loaded.name.is_none());
    }

    #[test]
    fn detect_hostname_returns_non_empty() {
        let h = detect_hostname();
        assert!(!h.is_empty(), "hostname should be detectable on this host");
    }

    #[test]
    fn detect_os_returns_known_value() {
        let os = detect_os();
        // std::env::consts::OS is one of a fixed set; we don't pin which,
        // but it should be non-empty.
        assert!(!os.is_empty(), "detect_os should return a non-empty string");
    }

    /// Clap's CLI surface should compile-time validate: this catches a
    /// missing required arg / typoed subcommand at `cargo test` time
    /// rather than at runtime.
    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    // ------------------------------------------------------------------
    // --help / -h short-circuit regression tests.
    //
    // The original hand-rolled argv parser executed subcommands before
    // inspecting later tokens, so `qontinui_profile machine init --help`
    // ran `init` (minting a fresh machine.json + UPSERTing coord.machines)
    // instead of printing help. An MSI fleet-join agent hit this 2026-05-18
    // and stranded a row in canonical PG.
    //
    // Clap's derive macros return `Err(ErrorKind::DisplayHelp)` from
    // `try_parse_from` when ANY level of the command tree sees `--help` or
    // `-h`. We assert that here for every destructive path so a future
    // refactor that drops back to hand-rolled parsing can't silently
    // regress the bug.
    // ------------------------------------------------------------------

    fn assert_help(argv: &[&str]) {
        let err = Cli::try_parse_from(argv).expect_err("clap should short-circuit on --help");
        assert_eq!(
            err.kind(),
            ErrorKind::DisplayHelp,
            "argv {:?} should print help, not execute. got {:?}",
            argv,
            err.kind()
        );
    }

    #[test]
    fn top_level_help_short_circuits() {
        assert_help(&["qontinui_profile", "--help"]);
        assert_help(&["qontinui_profile", "-h"]);
    }

    #[test]
    fn machine_help_short_circuits() {
        assert_help(&["qontinui_profile", "machine", "--help"]);
        assert_help(&["qontinui_profile", "machine", "-h"]);
    }

    /// The original incident: `machine init --help` executed `init`.
    /// Clap MUST treat this as a help request, not a command execution.
    #[test]
    fn machine_init_help_short_circuits() {
        assert_help(&["qontinui_profile", "machine", "init", "--help"]);
        assert_help(&["qontinui_profile", "machine", "init", "-h"]);
    }

    #[test]
    fn machine_show_help_short_circuits() {
        assert_help(&["qontinui_profile", "machine", "show", "--help"]);
    }

    #[test]
    fn machine_path_help_short_circuits() {
        assert_help(&["qontinui_profile", "machine", "path", "--help"]);
    }

    // ------------------------------------------------------------------
    // Phase 3 `device` subcommand mirror of the `--help` short-circuit tests.
    // The `device` name is the canonical Phase 3 alias; `machine` is the
    // legacy. Both must short-circuit on --help.
    // ------------------------------------------------------------------

    #[test]
    fn device_help_short_circuits() {
        assert_help(&["qontinui_profile", "device", "--help"]);
        assert_help(&["qontinui_profile", "device", "-h"]);
    }

    #[test]
    fn device_init_help_short_circuits() {
        assert_help(&["qontinui_profile", "device", "init", "--help"]);
        assert_help(&["qontinui_profile", "device", "init", "-h"]);
    }

    #[test]
    fn device_show_help_short_circuits() {
        assert_help(&["qontinui_profile", "device", "show", "--help"]);
    }

    #[test]
    fn device_path_help_short_circuits() {
        assert_help(&["qontinui_profile", "device", "path", "--help"]);
    }

    #[test]
    fn device_pair_help_short_circuits() {
        assert_help(&["qontinui_profile", "device", "pair", "--help"]);
    }

    #[test]
    fn init_help_short_circuits() {
        // `init` is destructive at the top level too (writes profiles.json).
        assert_help(&["qontinui_profile", "init", "--help"]);
    }

    #[test]
    fn use_help_short_circuits() {
        // `use` has a required positional; `--help` must short-circuit
        // before the missing-arg error fires.
        assert_help(&["qontinui_profile", "use", "--help"]);
    }

    // ------------------------------------------------------------------
    // Argv parsing happy-path: assert subcommands route to the expected
    // variants. These don't execute any handler — they just exercise the
    // clap parser.
    // ------------------------------------------------------------------

    #[test]
    fn no_subcommand_means_show() {
        let cli = Cli::try_parse_from(["qontinui_profile"]).expect("parses");
        assert!(cli.cmd.is_none());
    }

    #[test]
    fn init_defaults_to_localhost() {
        let cli = Cli::try_parse_from(["qontinui_profile", "init"]).expect("parses");
        match cli.cmd {
            Some(Cmd::Init { host }) => assert_eq!(host, "localhost"),
            other => panic!("expected Init, got {:?}", other),
        }
    }

    #[test]
    fn init_accepts_host_flag() {
        let cli = Cli::try_parse_from(["qontinui_profile", "init", "--host", "192.168.1.42"])
            .expect("parses");
        match cli.cmd {
            Some(Cmd::Init { host }) => assert_eq!(host, "192.168.1.42"),
            other => panic!("expected Init, got {:?}", other),
        }
    }

    #[test]
    fn machine_init_parses_as_machine_init() {
        let cli = Cli::try_parse_from(["qontinui_profile", "machine", "init"]).expect("parses");
        match cli.cmd {
            Some(Cmd::Machine {
                sub: DeviceCmd::Init { name },
            }) => assert!(name.is_none()),
            other => panic!("expected Machine::Init, got {:?}", other),
        }
    }

    #[test]
    fn device_init_parses_as_device_init() {
        let cli = Cli::try_parse_from(["qontinui_profile", "device", "init"]).expect("parses");
        match cli.cmd {
            Some(Cmd::Device {
                sub: DeviceCmd::Init { name },
            }) => assert!(name.is_none()),
            other => panic!("expected Device::Init, got {:?}", other),
        }
    }

    #[test]
    fn device_init_accepts_name_flag() {
        let cli =
            Cli::try_parse_from(["qontinui_profile", "device", "init", "--name", "my-laptop"])
                .expect("parses");
        match cli.cmd {
            Some(Cmd::Device {
                sub: DeviceCmd::Init { name },
            }) => assert_eq!(name.as_deref(), Some("my-laptop")),
            other => panic!("expected Device::Init {{name}}, got {:?}", other),
        }
    }

    #[test]
    fn device_pair_parses_with_auth_token() {
        let cli = Cli::try_parse_from([
            "qontinui_profile",
            "device",
            "pair",
            "--auth-token",
            "oauth",
        ])
        .expect("parses");
        match cli.cmd {
            Some(Cmd::Device {
                sub:
                    DeviceCmd::Pair {
                        auth_token,
                        pair_code,
                        browser,
                        tenant_id,
                    },
            }) => {
                assert_eq!(auth_token.as_deref(), Some("oauth"));
                assert!(pair_code.is_none());
                assert!(!browser);
                assert!(tenant_id.is_none());
            }
            other => panic!("expected Device::Pair, got {:?}", other),
        }
    }

    #[test]
    fn device_pair_parses_with_browser_flag() {
        let cli = Cli::try_parse_from(["qontinui_profile", "device", "pair", "--browser"])
            .expect("parses");
        match cli.cmd {
            Some(Cmd::Device {
                sub:
                    DeviceCmd::Pair {
                        auth_token,
                        pair_code,
                        browser,
                        tenant_id,
                    },
            }) => {
                assert!(auth_token.is_none());
                assert!(pair_code.is_none());
                assert!(browser);
                assert!(tenant_id.is_none());
            }
            other => panic!("expected Device::Pair {{browser}}, got {:?}", other),
        }
    }

    #[test]
    fn device_pair_parses_with_pair_code_flag() {
        let cli = Cli::try_parse_from([
            "qontinui_profile",
            "device",
            "pair",
            "--pair-code",
            "A7K2P3",
        ])
        .expect("parses");
        match cli.cmd {
            Some(Cmd::Device {
                sub:
                    DeviceCmd::Pair {
                        auth_token,
                        pair_code,
                        browser,
                        tenant_id,
                    },
            }) => {
                assert!(auth_token.is_none());
                assert_eq!(pair_code.as_deref(), Some("A7K2P3"));
                assert!(!browser);
                assert!(tenant_id.is_none());
            }
            other => panic!("expected Device::Pair {{pair_code}}, got {:?}", other),
        }
    }

    #[test]
    fn use_requires_name() {
        let err = Cli::try_parse_from(["qontinui_profile", "use"]).expect_err("requires name");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }

    // ------------------------------------------------------------------
    // env subcommand — capture agent CLI surface.
    // ------------------------------------------------------------------

    #[test]
    fn env_enroll_help_short_circuits() {
        assert_help(&["qontinui_profile", "env", "enroll", "--help"]);
    }

    #[test]
    fn env_capture_help_short_circuits() {
        assert_help(&["qontinui_profile", "env", "capture", "--help"]);
    }

    #[test]
    fn env_enroll_requires_code() {
        let err = Cli::try_parse_from(["qontinui_profile", "env", "enroll"])
            .expect_err("requires --code");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn env_enroll_parses_with_code_and_backend() {
        let cli = Cli::try_parse_from([
            "qontinui_profile",
            "env",
            "enroll",
            "--code",
            "ABC123",
            "--backend",
            "http://localhost:8000",
        ])
        .expect("parses");
        match cli.cmd {
            Some(Cmd::Env {
                sub:
                    EnvCmd::Enroll {
                        code,
                        backend,
                        environment,
                    },
            }) => {
                assert_eq!(code, "ABC123");
                assert_eq!(backend.as_deref(), Some("http://localhost:8000"));
                assert!(environment.is_none());
            }
            other => panic!("expected Env::Enroll, got {:?}", other),
        }
    }

    #[test]
    fn env_capture_parses_with_dry_run() {
        let cli = Cli::try_parse_from(["qontinui_profile", "env", "capture", "--dry-run"])
            .expect("parses");
        match cli.cmd {
            Some(Cmd::Env {
                sub: EnvCmd::Capture { dry_run },
            }) => assert!(dry_run),
            other => panic!("expected Env::Capture {{dry_run}}, got {:?}", other),
        }
    }

    #[test]
    fn env_show_parses() {
        let cli = Cli::try_parse_from(["qontinui_profile", "env", "show"]).expect("parses");
        match cli.cmd {
            Some(Cmd::Env { sub: EnvCmd::Show }) => {}
            other => panic!("expected Env::Show, got {:?}", other),
        }
    }
}
