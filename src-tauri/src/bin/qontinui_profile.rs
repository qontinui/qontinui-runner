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
        /// with `--browser`.
        #[arg(long, value_name = "OAUTH_TOKEN")]
        auth_token: Option<String>,
        /// Browser mode (default). Mutually exclusive with `--auth-token`.
        #[arg(long)]
        browser: bool,
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
                browser,
            } => cmd_device_pair(auth_token.as_deref(), browser),
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
}

fn load_paired_user_id() -> Option<String> {
    let path = paired_user_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let parsed: PairedUserFile = serde_json::from_slice(&bytes).ok()?;
    Some(parsed.user_id)
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
    let paired_user_id = load_paired_user_id();
    match register_with_coord(
        &file.device_id,
        &file.hostname,
        &display_name,
        paired_user_id.as_deref(),
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
}

fn select_pair_mode(auth_token: Option<&str>, _browser: bool) -> Result<PairMode, String> {
    // clap's `Pair` variant carries both `--browser` and `--auth-token`. The
    // mutually-exclusive constraint is enforced here (we can't use clap's
    // `conflicts_with` because `browser` is a bool flag with a default of
    // false). If `--auth-token` is set, that wins; otherwise browser is the
    // default mode (whether or not `--browser` was explicitly passed).
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

#[derive(Debug, Clone, Deserialize)]
struct PairCompleteResponse {
    device_token: String,
    user_id: String,
    #[serde(default)]
    device_id: Option<String>,
}

fn cmd_device_pair(auth_token: Option<&str>, browser: bool) -> ExitCode {
    // clap can't express "exactly one of browser/auth_token"; reject the
    // combination by hand.
    if auth_token.is_some() && browser {
        eprintln!("error: --browser and --auth-token are mutually exclusive");
        return ExitCode::from(2);
    }
    let mode = match select_pair_mode(auth_token, browser) {
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
    let result: Result<PairCompleteResponse, String> = match mode {
        PairMode::AuthToken(token) => pair_with_auth_token(&base, &token),
        PairMode::Browser => pair_via_browser(&base),
    };
    match result {
        Ok(resp) => {
            if let Err(e) = persist_pairing(&resp) {
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

/// POST `Authorization: Bearer <oauth-token>` to `POST /coord/devices/pair-cli`
/// (headless mode). Coord verifies the OAuth token and returns the
/// device-token JWT + paired user_id.
fn pair_with_auth_token(base: &str, oauth_token: &str) -> Result<PairCompleteResponse, String> {
    let url = format!("{}/coord/devices/pair-cli", base);
    let body = serde_json::json!({
        "hostname": detect_hostname(),
        "name":     detect_hostname(),
        "os":       detect_os(),
        "os_version": detect_os_version(),
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("reqwest client build failed: {}", e))?;
    let resp = client
        .post(&url)
        .bearer_auth(oauth_token)
        .json(&body)
        .send()
        .map_err(|e| format!("POST {} failed: {}", url, e))?;
    let status = resp.status();
    if !status.is_success() {
        let body_text = resp
            .text()
            .unwrap_or_else(|_| "<unable to read response body>".to_string());
        return Err(format!("POST {} -> HTTP {}: {}", url, status, body_text));
    }
    resp.json::<PairCompleteResponse>()
        .map_err(|e| format!("decode pair-cli response failed: {}", e))
}

/// Browser-mediated pair. Spins up a localhost axum server, opens the
/// browser to `/connect-runner?state=…&callback=…`, waits for the user's
/// click + redirect, then exchanges the captured `(state, token)` with
/// coord's `POST /coord/devices/pair-complete` for the device-token JWT.
fn pair_via_browser(coord_base: &str) -> Result<PairCompleteResponse, String> {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    // Generate state nonce. 32 bytes of randomness; hex-encoded so it
    // survives a URL round-trip without escaping.
    let mut state_bytes = [0u8; 32];
    use rand::TryRngCore;
    rand::rng()
        .try_fill_bytes(&mut state_bytes)
        .map_err(|e| format!("rand fill failed: {e}"))?;
    let state_nonce = hex::encode(state_bytes);

    // Web backend URL is read from the active profile's `coord_url` host
    // (assuming web + coord co-locate in dev). If they diverge, the user
    // can override with QONTINUI_WEB_BASE.
    let web_base = std::env::var("QONTINUI_WEB_BASE")
        .ok()
        .unwrap_or_else(|| derive_web_base_from_coord(coord_base));
    let hostname_now = detect_hostname();

    // Bind a port for the callback. Use 0 to let the OS pick — then read it
    // back. We use std::net::TcpListener first to capture the chosen port,
    // then hand it to axum via from_tcp.
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("bind localhost callback failed: {e}"))?;
    let port = std_listener
        .local_addr()
        .map_err(|e| format!("local_addr failed: {e}"))?
        .port();
    std_listener
        .set_nonblocking(true)
        .map_err(|e| format!("set_nonblocking failed: {e}"))?;

    let received: Arc<Mutex<Option<CallbackCapture>>> = Arc::new(Mutex::new(None));
    let received_clone = received.clone();
    let state_expected = state_nonce.clone();

    // Build the redirect URL the browser will land back on. The state
    // round-trips so we can reject mismatched callbacks.
    let callback_url = format!("http://127.0.0.1:{}/auth/runner-token-callback", port);
    let connect_url = format!(
        "{}/connect-runner?state={}&callback={}&device_name={}",
        web_base.trim_end_matches('/'),
        urlencoding::encode(&state_nonce),
        urlencoding::encode(&callback_url),
        urlencoding::encode(&hostname_now),
    );

    // Spawn a current-thread tokio runtime + axum server on a worker thread.
    // We could use the existing src/mcp/auth_callback.rs route, but it
    // depends on Tauri-side state (ApiState/AppState); for the CLI we want
    // an independent server.
    let (server_done_tx, server_done_rx) = std::sync::mpsc::channel::<()>();
    let server_handle = std::thread::spawn(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("runtime build failed: {e}"))?;
        rt.block_on(async move {
            use axum::{extract::Query, response::Html, routing::get, Router};
            let state_for_route = state_expected.clone();
            let received_for_route = received_clone.clone();
            let app = Router::new().route(
                "/auth/runner-token-callback",
                get(move |Query(q): Query<CallbackQuery>| {
                    let state_for_route = state_for_route.clone();
                    let received_for_route = received_for_route.clone();
                    async move {
                        if q.state != state_for_route {
                            return Html(
                                "<h1>State mismatch</h1><p>The callback state did \
                                 not match the pending flow. Restart the pairing.</p>"
                                    .to_string(),
                            );
                        }
                        *received_for_route.lock().expect("mutex") = Some(CallbackCapture {
                            token: q.token.clone(),
                            token_id: q.token_id.clone(),
                        });
                        Html(
                            "<h1>&#10003; Runner paired</h1>\
                             <p>You can close this tab.</p>\
                             <script>setTimeout(()=>window.close(),2000);</script>"
                                .to_string(),
                        )
                    }
                }),
            );
            let tokio_listener = tokio::net::TcpListener::from_std(std_listener)
                .map_err(|e| format!("tokio listener wrap failed: {e}"))?;
            // Run until our flag flips, with a 5-minute hard timeout.
            let serve = axum::serve(tokio_listener, app);
            let timeout = tokio::time::sleep(Duration::from_secs(300));
            tokio::pin!(timeout);
            tokio::select! {
                res = serve => {
                    res.map_err(|e| format!("axum serve failed: {e}"))?;
                }
                _ = &mut timeout => {
                    return Err("pair: timed out after 5 minutes waiting for browser callback".to_string());
                }
            }
            let _ = server_done_tx.send(());
            Ok(())
        })
    });

    println!(
        "opening browser to {} (callback {})",
        connect_url, callback_url
    );
    if let Err(e) = open::that(&connect_url) {
        eprintln!(
            "warning: failed to open browser ({}). Open this URL manually:\n  {}",
            e, connect_url
        );
    }

    // Poll the received slot up to 5 minutes. The axum runtime is on a
    // background thread; we just block here until we see the slot fill or
    // the server-done signal arrives.
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    let capture = loop {
        if let Some(c) = received.lock().expect("mutex").clone() {
            break c;
        }
        if std::time::Instant::now() >= deadline {
            return Err("pair: timed out after 5 minutes waiting for browser callback".to_string());
        }
        // Cheap polling cadence; the axum task wakes on its own.
        std::thread::sleep(Duration::from_millis(200));
        // Drain the server-done channel in case axum exited early.
        if let Ok(()) = server_done_rx.try_recv() {
            // Server stopped without filling the slot — re-check once.
            if let Some(c) = received.lock().expect("mutex").clone() {
                break c;
            }
            return Err("pair: callback server stopped before capturing token".to_string());
        }
    };
    // Best-effort join — we don't fail the operation if the server thread
    // is still running; it'll time out on its own.
    drop(server_handle);

    // Exchange (state, token) for the device-token JWT.
    let url = format!("{}/coord/devices/pair-complete", coord_base);
    let body = serde_json::json!({
        "state":  state_nonce,
        "token":  capture.token,
        "device_id": capture.token_id,
        "hostname":  hostname_now,
        "name":       hostname_now,
        "os":         detect_os(),
        "os_version": detect_os_version(),
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("reqwest client build failed: {}", e))?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .map_err(|e| format!("POST {} failed: {}", url, e))?;
    let status = resp.status();
    if !status.is_success() {
        let body_text = resp
            .text()
            .unwrap_or_else(|_| "<unable to read response body>".to_string());
        return Err(format!("POST {} -> HTTP {}: {}", url, status, body_text));
    }
    resp.json::<PairCompleteResponse>()
        .map_err(|e| format!("decode pair-complete response failed: {}", e))
}

/// Derive a `https://` web base from the coord HTTP base, assuming web +
/// coord live on the same host (the dev-default). Production deployments
/// override via `QONTINUI_WEB_BASE`.
fn derive_web_base_from_coord(coord_base: &str) -> String {
    // Drop the trailing `:9870` (coord port) — leave the rest in place.
    // Imperfect heuristic; the env override is the escape hatch.
    let trimmed = coord_base.trim_end_matches('/');
    if let Some((host, _)) = trimmed.rsplit_once(':') {
        host.to_string()
    } else {
        trimmed.to_string()
    }
}

#[derive(Debug, Clone, Deserialize)]
struct CallbackQuery {
    state: String,
    token: String,
    #[serde(default)]
    token_id: Option<String>,
}

#[derive(Debug, Clone)]
struct CallbackCapture {
    token: String,
    #[allow(dead_code)]
    token_id: Option<String>,
}

/// Persist the device-token JWT (via `qontinui_runner_lib::auth::AuthManager`)
/// and write the paired user_id to `paired_user.json` so the next
/// `device init` attaches it to the register payload.
///
/// Storing the JWT in the `access_token` slot is intentional: the existing
/// `AuthManager` already calls that slot the "primary credential," and the
/// runner's outer code reads `get_access_token()` when authenticating to the
/// web backend. The refresh-token slot is unused for the device-token flow
/// (the device JWT has its own lifecycle managed by coord); we pass an empty
/// string. See the format-change comment in `secure_storage.rs`.
fn persist_pairing(resp: &PairCompleteResponse) -> Result<(), String> {
    use qontinui_runner_lib::auth::AuthManager;
    let mgr = AuthManager::new();
    mgr.store_tokens(&resp.device_token, "")
        .map_err(|e| format!("AuthManager::store_tokens failed: {e}"))?;
    let path = paired_user_path().ok_or_else(|| "could not resolve data_local_dir".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let pf = PairedUserFile {
        user_id: resp.user_id.clone(),
    };
    let pretty =
        serde_json::to_vec_pretty(&pf).map_err(|e| format!("serialize paired_user.json: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &pretty).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    if let Some(did) = &resp.device_id {
        // Best-effort: record device_id alongside the auth tokens so the
        // runner GUI can identify itself without reading machine.json.
        if let Ok(storage) = qontinui_runner_lib::secure_storage::SecureStorage::new() {
            if let Err(e) = storage.store_device_id(did) {
                tracing::debug!("store_device_id non-fatal: {e}");
            }
        }
    }
    Ok(())
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
) -> Result<(), String> {
    // Validate UUID shape up front so a malformed machine.json fails fast
    // with a clear error instead of bouncing off a 400 from coord.
    let _ = uuid::Uuid::parse_str(device_id)
        .map_err(|e| format!("device_id is not a valid UUID: {}", e))?;
    if let Some(uid) = user_id {
        uuid::Uuid::parse_str(uid)
            .map_err(|e| format!("paired user_id is not a valid UUID: {}", e))?;
    }
    let base = coord_http_base()?;
    let url = format!("{}/coord/devices/register", base);
    // `device_id` is the canonical name in coord.devices. We also emit
    // `machine_id` as a duplicate alias key for back-compat with a Phase-2
    // coord that still reads the old name — the field is removed once
    // Phase 2 is merged everywhere.
    let body = serde_json::json!({
        "device_id":    device_id,
        "machine_id":   device_id,
        "hostname":     hostname,
        "name":         name,
        "os":           detect_os(),
        "os_version":   detect_os_version(),
        "capabilities": vec!["runner".to_string()],
        "user_id":      user_id,
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
    Err(format!(
        "POST {} returned HTTP {}: {}",
        url, status, body_text
    ))
}

/// Resolve the coord HTTP base from the active profile's `coord_url`.
/// Profiles store `ws://host:9870/ws` (the WebSocket upgrade URL); we
/// convert that to `http://host:9870` so reqwest can POST to
/// `/coord/devices/register`. Mirrors `qontinui-supervisor/src/fleet.rs`
/// `coord_http_base` so both registration paths follow the same recipe.
fn coord_http_base() -> Result<String, String> {
    let coord_url = load_strict()
        .map_err(|e| format!("active profile load failed: {}", e))?
        .coord_url
        .ok_or_else(|| "active profile has no coord_url".to_string())?;
    let trimmed = coord_url.trim_end_matches("/ws");
    let with_http = trimmed
        .strip_prefix("wss://")
        .map(|rest| format!("https://{rest}"))
        .or_else(|| {
            trimmed
                .strip_prefix("ws://")
                .map(|rest| format!("http://{rest}"))
        })
        .unwrap_or_else(|| trimmed.to_string());
    Ok(with_http)
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
                        browser,
                    },
            }) => {
                assert_eq!(auth_token.as_deref(), Some("oauth"));
                assert!(!browser);
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
                        browser,
                    },
            }) => {
                assert!(auth_token.is_none());
                assert!(browser);
            }
            other => panic!("expected Device::Pair {{browser}}, got {:?}", other),
        }
    }

    #[test]
    fn use_requires_name() {
        let err = Cli::try_parse_from(["qontinui_profile", "use"]).expect_err("requires name");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }
}
