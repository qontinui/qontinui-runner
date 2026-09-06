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
//! qontinui_profile tier                       # print the runner tier this settings.json resolves to
//! qontinui_profile tier --set local           # record an explicit tier choice (headless TierStep)
//! qontinui_profile tier --clear-choice        # un-pin: re-open the install to tier inference
//! qontinui_profile env capture                # push this box's secret-free config to the twin
//! qontinui_profile env pull                   # preview what would change here to match canonical
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

use base64::Engine;
use clap::{Parser, Subcommand};
use qontinui_runner_lib::pair::{
    coord_http_base, pair_via_browser, pair_with_auth_token, pair_with_pair_code, persist_pairing,
    tenant_id_from_oauth_claim, PairCompleteResponse,
};
use qontinui_runner_lib::profile_cli::EnvCmd;
use qontinui_runner_lib::profiles::{
    load_strict, profiles_path, AuthConfig, BlobConfig, Profile, ProfilesFile, PROD_API_BASE_URL,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::io::Write;
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
    /// config envelope; `pull` previews what would change here to match the
    /// canonical machine; `apply` reconciles this box toward it (dry-run by
    /// default); `show` prints enrollment state; `scope-root` declares which
    /// directory the toolchain version probes measure.
    Env {
        #[command(subcommand)]
        sub: EnvCmd,
    },
    /// Read or write `settings.json::tier` — the headless equivalent of the
    /// SetupWizard's tier step, and the door `coord doctor` names when a
    /// credentialed box is pinned below Tier 2.
    ///
    /// With no flags it PRINTS: the tier the document resolves to, the raw
    /// fields behind it, and the file it read. `--set` records an explicit
    /// operator choice (tier + `tier_chosen_explicitly`), which closes the
    /// inference for good. `--clear-choice` does the opposite: it clears that
    /// flag so the inference re-opens, without touching the tier itself.
    Tier {
        /// Record an explicit tier choice: `local` | `local_provider` |
        /// `qontinui_account`. Mutually exclusive with `--clear-choice`.
        #[arg(long, value_name = "TIER")]
        set: Option<String>,
        /// Clear `settings.json::tier_chosen_explicitly`, re-opening this
        /// install to the tier inference (pairing / headless launch / legacy
        /// token). Mutually exclusive with `--set`.
        #[arg(long)]
        clear_choice: bool,
    },
}

// `EnvCmd` (the `env enroll/capture/pull/apply/show/scope-root` subcommand tree) lives in
// `qontinui_runner_lib::profile_cli` so this bin AND the main runner binary's
// pre-GUI CLI mode share one implementation.

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
        // The `env` subcommands share one implementation with the main runner
        // binary's pre-GUI CLI mode (`qontinui-runner env …`), in the lib.
        Cmd::Env { sub } => ExitCode::from(qontinui_runner_lib::profile_cli::run_env(sub)),
        Cmd::Tier { set, clear_choice } => cmd_tier(set.as_deref(), clear_choice),
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
///
/// `hostname` is `#[serde(default)]` because it is NOT part of the identity —
/// it is re-detected on every `device init` run. Without the default, a
/// hand-written or partially-migrated `{"device_id":"…"}` file (readable by
/// [`qontinui_runner_lib::machine_identity`] and by every other runtime
/// consumer) made `device init` call the file "unreadable" and point the
/// operator at `rm`, i.e. refusing to repair a file that carries a perfectly
/// good identity — and destroying that identity if the advice was taken.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceFile {
    #[serde(alias = "machine_id")]
    device_id: String,
    #[serde(default)]
    hostname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

fn device_file_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("machine.json"))
}

/// Refusal advice for a file that MIGHT still carry this machine's real
/// `device_id` (unreadable bytes, invalid JSON, a non-object, or a
/// well-formed identity next to some other malformed field).
///
/// `rm` is the wrong advice here and used to be the only advice given: it
/// destroys the identity, and the follow-up `device init` then mints a fresh
/// UUID — a second `coord.devices` row for the same physical machine, which is
/// exactly what this command exists to prevent.
const RECOVERABLE_ADVICE: &str = "this file may still hold this machine's device_id. \
     Inspect and repair it by hand (it only needs to be a JSON object with a non-blank \
     string `device_id`), then re-run. Do NOT `rm` it: that DESTROYS the identity, and \
     the next `device init` mints a new one — a second coord.devices row for this machine.";

/// Refusal advice for a file that provably carries NO identity (no
/// `device_id`/`machine_id` key at all, or a blank one). There is nothing to
/// preserve, so `rm` + `device init` is correct and safe.
const UNRECOVERABLE_ADVICE: &str = "this file carries no device_id, so there is no identity \
     to preserve. Check first that it holds no other state you want (e.g. `active_tenant_id`), \
     then `rm` it and re-run `qontinui_profile device init` to mint one.";

/// Does this raw `machine.json` object carry a usable identity — a non-blank
/// STRING under `device_id` or the legacy `machine_id` spelling?
///
/// Used to pick between [`RECOVERABLE_ADVICE`] and [`UNRECOVERABLE_ADVICE`]
/// when the strict [`DeviceFile`] deserialize fails: the failure may be about
/// some entirely different field (a wrongly-typed `hostname`, say) while the
/// identity itself is perfectly good, and telling the operator to `rm` THAT is
/// how one machine grows two `coord.devices` rows.
fn object_has_usable_identity(obj: &serde_json::Map<String, serde_json::Value>) -> bool {
    ["device_id", "machine_id"].iter().any(|k| {
        obj.get(*k)
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty())
    })
}

fn read_device_file(path: &Path) -> std::io::Result<DeviceFile> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Write the `machine.json` for `device init`, preserving every sibling
/// top-level field verbatim.
///
/// Returns the resulting [`DeviceFile`] plus `was_new` — `true` only on the
/// one legitimate mint (an absent file).
///
/// **Preserving siblings is the point.** This used to round-trip through
/// [`DeviceFile`], which has no `active_tenant_id` field and no
/// `#[serde(flatten)]` — so the documented recovery step (`rm machine.json` is
/// bad advice; `device init` on an existing file is the good one) silently
/// DELETED the tenant pin that `commands/tenant.rs`, `agent_worktree/census`,
/// `fs_backstop` and `maintenance_executor` all read. Patching the raw JSON
/// object keeps `active_tenant_id`, the legacy `machine_id` spelling, and any
/// field a newer runner adds.
///
/// **Never mints when the file exists.** An unreadable/invalid file is an
/// error, not an excuse to overwrite: overwriting would mint a fresh
/// `device_id` and therefore a fresh `coord.devices` row for the same machine.
///
/// The refusals split on whether the identity is RECOVERABLE
/// ([`RECOVERABLE_ADVICE`] — the bytes may still hold a real UUID, so hand
/// repair, never `rm`) or UNRECOVERABLE ([`UNRECOVERABLE_ADVICE`] — there is
/// provably no identity, so `rm` + `device init` is the correct fix). Telling
/// the operator to `rm` a recoverable file is not a cosmetic error: the
/// follow-up `device init` mints, which is the exact outcome this refusal
/// exists to prevent.
/// Plan `2026-08-06-device-identity-is-per-profile-not-per-machine` Phase 2.
fn device_init_write_at(
    path: &Path,
    name_arg: Option<&str>,
    hostname_now: &str,
) -> Result<(DeviceFile, bool), String> {
    let (mut obj, existing, was_new) = if path.exists() {
        let bytes = std::fs::read(path).map_err(|e| {
            format!(
                "machine.json at {} exists but could not be READ ({}). \
                 Refusing to overwrite — {}",
                path.display(),
                e,
                RECOVERABLE_ADVICE
            )
        })?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
            format!(
                "machine.json at {} is not valid JSON ({}). \
                 Refusing to overwrite — {}",
                path.display(),
                e,
                RECOVERABLE_ADVICE
            )
        })?;
        let serde_json::Value::Object(obj) = value else {
            return Err(format!(
                "machine.json at {} is valid JSON but not an object. \
                 Refusing to overwrite — {}",
                path.display(),
                RECOVERABLE_ADVICE
            ));
        };
        // Validate that a usable identity is actually in there before we
        // rewrite anything — a `device_id`-less file must not be blessed.
        let existing: DeviceFile = serde_json::from_value(serde_json::Value::Object(obj.clone()))
            .map_err(|e| {
            format!(
                "machine.json at {} did not deserialize ({}). Refusing to overwrite — {}",
                path.display(),
                e,
                if object_has_usable_identity(&obj) {
                    RECOVERABLE_ADVICE
                } else {
                    UNRECOVERABLE_ADVICE
                }
            )
        })?;
        if existing.device_id.trim().is_empty() {
            return Err(format!(
                "machine.json at {} has a BLANK device_id. \
                 Refusing to overwrite — {}",
                path.display(),
                UNRECOVERABLE_ADVICE
            ));
        }
        (obj, existing, false)
    } else {
        let minted = DeviceFile {
            device_id: uuid::Uuid::new_v4().to_string(),
            hostname: hostname_now.to_string(),
            name: name_arg.map(|s| s.to_string()),
        };
        (serde_json::Map::new(), minted, true)
    };

    // Re-detect hostname every run — a laptop can be renamed between boots
    // and the file should reflect the current name. The identity does not
    // change with it.
    let name = name_arg
        .map(|s| s.to_string())
        .or_else(|| existing.name.clone());
    // TRIM the identity before it is written or presented to coord. The
    // blank-check above already trims, and so does every reader
    // (`machine_identity::read_device_id_at`) — so an on-disk `" abc "` used to
    // be registered by THIS command as `" abc "` while every other path
    // registered `"abc"`. Coord UPSERTs `ON CONFLICT (device_id)`: two spellings
    // are two rows for one machine.
    let file = DeviceFile {
        device_id: existing.device_id.trim().to_string(),
        hostname: hostname_now.to_string(),
        name,
    };

    obj.insert(
        "device_id".to_string(),
        serde_json::Value::String(file.device_id.clone()),
    );
    obj.insert(
        "hostname".to_string(),
        serde_json::Value::String(file.hostname.clone()),
    );
    match &file.name {
        Some(n) => {
            obj.insert("name".to_string(), serde_json::Value::String(n.clone()));
        }
        None => {
            obj.remove("name");
        }
    }

    let pretty = serde_json::to_vec_pretty(&serde_json::Value::Object(obj))
        .map_err(|e| format!("serialize machine.json failed: {e}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {} failed: {e}", parent.display()))?;
    }
    qontinui_runner_lib::fs_atomic::atomic_write(path, &pretty)
        .map_err(|e| format!("write {} failed: {e}", path.display()))?;
    Ok((file, was_new))
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
    // Sibling top-level fields (notably `active_tenant_id`) are preserved.
    let hostname_now = detect_hostname();
    let (file, was_new) = match device_init_write_at(&path, name_arg, &hostname_now) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}", e);
            return ExitCode::from(2);
        }
    };
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
    // Read pairing state through the lib's env-honoring, v2-aware readers
    // (they resolve `QONTINUI_SECURE_STORAGE_DIR` first, then
    // `data_local_dir()/com.qontinui.runner/` — the same chain the write
    // path `persist_pairing` uses). Both are `None` when unpaired.
    let paired_user_id = qontinui_runner_lib::pair::read_paired_user_id_from_disk();
    let paired_tenant_id = qontinui_runner_lib::pair::read_paired_tenant_id_from_disk();
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
//   `SecureStorage` (AES-256-GCM in `auth_tokens.enc`, under
//   `QONTINUI_SECURE_STORAGE_DIR` when that env var is set and non-empty,
//   else `{data_local_dir}/com.qontinui.runner/`). The same file may already
//   hold a pre-Phase-3 `qontinui_runner_<random>` bearer; this overwrites it
//   with the JWT (see the comment in `secure_storage.rs` documenting the
//   format change).
//
// - The paired user_id to `paired_user.json` (same env-var-first directory
//   chain), so the next `device init` carries it on the register payload.

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

/// Which rung of [`resolve_pair_code_base`] produced the URL.
///
/// The three rungs have three DIFFERENT remediations when a redeem fails
/// against the resolved host, and the URL alone does not distinguish them —
/// `https://api.qontinui.io` looks identical whether it came from an operator
/// export, from a coord_url in `profiles.json`, or from the compiled-in
/// default. Reporting the winning arm turns "wrong host" from a guess into a
/// one-line fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairCodeBaseSource {
    /// `$QONTINUI_WEB_BASE` was set to a non-blank value. Fix: correct or
    /// unset that var.
    EnvOverride,
    /// Nothing configured; the compiled-in production default. Fix: set
    /// `$QONTINUI_WEB_BASE` if you are not pairing against production (a
    /// local dev backend is `http://127.0.0.1:8000`).
    ProdDefault,
}

impl PairCodeBaseSource {
    /// Short operator-facing label naming the arm and how to change it.
    fn as_str(self) -> &'static str {
        match self {
            PairCodeBaseSource::EnvOverride => "$QONTINUI_WEB_BASE override",
            PairCodeBaseSource::ProdDefault => {
                "compiled-in production default (no $QONTINUI_WEB_BASE)"
            }
        }
    }
}

/// Resolve the base URL for pair-code redemption, WITH the arm that produced
/// it. Two rungs, in order:
///
/// 1. `web_base_env` (`$QONTINUI_WEB_BASE`) — an explicit operator override.
///    Blank/whitespace counts as unset, matching how every other rung ladder
///    in this workspace reads an exported-but-empty var.
/// 2. [`PROD_API_BASE_URL`] — the fleet's real production default.
///
/// # Why there is no derive-from-coord rung
///
/// There used to be a middle rung that derived this base from the active
/// profile's `coord_url`, on the assumption that web and coord co-locate.
/// It was removed because it is **never** correct, in either environment:
///
/// * In PRODUCTION the two are different services — coord is
///   `coord.qontinui.io`, the web backend is `api.qontinui.io`. The derived
///   base sent a web-backend route to coord, which answers
///   `401 missing operator Bearer token`. Measured on a headless box
///   2026-09-02.
/// * In DEV they share a host but NOT a port, and the derivation strips the
///   port — `http://localhost:9870` derived to `http://localhost`, i.e. port
///   80, while the dev backend listens on 8000.
///
/// So the rung could only ever be right for a deployment serving the web
/// backend on port 80 of coord's own host, which is not a deployment this
/// fleet has. A rung that is never correct is worse than no rung, because it
/// outranks the working default and makes the failure look like a client bug.
///
/// The canonical four-rung resolver (`api_config::resolve_api_base_url`, which
/// additionally weighs `$QONTINUI_WEB_BACKEND_URL`, `$QONTINUI_API_URL` and the
/// persisted `web_integration.backend_url`) is deliberately NOT used here:
/// `api_config` is declared in `main.rs`, so it belongs to the runner binary's
/// module tree and is unreachable from this separate binary. Re-implementing
/// its precedence here would be the second copy of the precedence rule that
/// module's own docs name as the dominant divergence hazard.
///
/// The [`PairCodeBaseSource`] half is returned rather than logged here so the
/// function stays pure and the caller owns the output surface.
fn resolve_pair_code_base(web_base_env: Option<&str>) -> (String, PairCodeBaseSource) {
    if let Some(explicit) = web_base_env.filter(|s| !s.trim().is_empty()) {
        // Trailing slash would build `<base>//api/v1/...`; trim it here so the
        // one operator-supplied rung cannot produce a malformed URL.
        return (
            explicit.trim().trim_end_matches('/').to_string(),
            PairCodeBaseSource::EnvOverride,
        );
    }
    (
        PROD_API_BASE_URL.to_string(),
        PairCodeBaseSource::ProdDefault,
    )
}

/// Decode the `exp` (unix seconds) claim from a JWT's middle segment,
/// without verifying its signature, and render it as an RFC3339 timestamp.
/// Mirrors `auth::decode_jwt_exp` but kept local: that helper is
/// `pub(crate)` to the `qontinui_runner_lib` crate, and this binary is a
/// separate crate that cannot see it. Used only to print a human-readable
/// token expiry in `cmd_device_pair`'s success output — `None` here just
/// means "omit the field", it never fails the pair.
fn decode_jwt_exp_display(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.trim().split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(parts[1]))
        .ok()?;
    let claim: serde_json::Value = serde_json::from_slice(&payload_bytes).ok()?;
    let exp = claim.get("exp")?.as_i64()?;
    chrono::DateTime::from_timestamp(exp, 0).map(|dt| dt.to_rfc3339())
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
    // `base` (the coord HTTP base, derived from the active profile's
    // coord_url) is required unconditionally for Browser/AuthToken modes.
    // PairCode mode only consults it as a secondary fallback — see
    // `resolve_pair_code_base` — so a missing/unreadable profiles.json must
    // NOT hard-error here: that would force every fresh machine to run
    // `qontinui_profile init` (which writes an unrelated local-dev DB/
    // Redis/blob stack profile it will never use) just to redeem a pair
    // code against production. Fleet-join, 2026-08-24.
    let base_result = coord_http_base();
    if !matches!(mode, PairMode::PairCode(_)) {
        if let Err(e) = &base_result {
            eprintln!("error: could not resolve coord_url: {}", e);
            return ExitCode::from(2);
        }
    }
    let base = base_result.unwrap_or_default();

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
            // Pair codes redeem against the web backend. Resolution order
            // lives in `resolve_pair_code_base` — see its doc comment.
            let (web_base, base_source) =
                resolve_pair_code_base(std::env::var("QONTINUI_WEB_BASE").ok().as_deref());
            // Print the URL *and* which rung produced it: the two rungs have
            // different fixes, and the URL alone does not say which one an
            // operator staring at a failed redeem should reach for.
            println!(
                "Redeeming pair code against {} ({})",
                web_base,
                base_source.as_str()
            );
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
            // Running `device pair` IS an explicit interactive credential
            // acquisition, so it ends any interactive logout — same rule as a
            // Cognito sign-in or a pair-code redeem in the UI. Without this the
            // operator would pair successfully (device JWT valid, relay online)
            // and still be held at the runner's LoginScreen by the persisted
            // sign-out marker. Cleared only AFTER `persist_pairing` succeeded,
            // and deliberately NOT inside `persist_pairing` itself — the
            // background device-JWT refresher writes those same credential slots
            // and must never un-logout the operator.
            if let Err(e) =
                qontinui_runner_lib::auth::AuthManager::new().clear_interactive_signed_out()
            {
                eprintln!(
                    "warning: pairing persisted but the interactive sign-out marker could not be \
                     cleared ({e}); the runner UI may still show the sign-in screen"
                );
            }
            // Durable outcome fields an operator needs even if the process is
            // killed a moment later — plan
            // `2026-08-29-qontinui-profile-device-pair-never-exits` Phase 3.
            // `resp.exp` is populated on the pair-cli/auth-token/browser wire
            // but the pair-code redeem response never carries it
            // (`PairCodeRedeemResponse::into_pair_complete` sets `exp: None`
            // deliberately — the web schema doesn't return it), so fall back
            // to decoding the just-minted JWT's own `exp` claim.
            let device_id_display = resp.device_id.as_deref().unwrap_or("<unknown>");
            let exp_display = resp
                .exp
                .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
                .map(|dt| dt.to_rfc3339())
                .or_else(|| decode_jwt_exp_display(&resp.token))
                .unwrap_or_else(|| "<unknown>".to_string());
            let dmk_stored = resp
                .device_machine_key
                .as_deref()
                .map(|k| !k.trim().is_empty())
                .unwrap_or(false);
            let success_line = format!(
                "device paired: user_id={} device_id={} tenant_id={} token_expires={} \
                 dmk_stored={} (device-token JWT saved to auth_tokens.enc)",
                resp.user_id, device_id_display, effective_tenant_id, exp_display, dmk_stored
            );

            // Promote the runner to Tier 2 (`qontinui_account`) — the tier that
            // is allowed to talk to coord. Redeeming a pairing IS a
            // cloud-account bind, and this is the SAME writer the WebView door
            // (`redeem_pair_code`) calls; before it existed, this headless door
            // wrote the credentials and never touched the tier, so a headless
            // box — the one that most needs Tier 2 — was the only box that
            // could not reach it, and `coord doctor` reported `BLOCKED at:
            // tier` on a correctly-paired device.
            //
            // Best-effort, exactly like the sign-out-marker clear above: a
            // failed promotion must never fail a pairing that already
            // persisted. The helper refuses a SECONDARY instance (it would
            // demote the primary's shared settings.json) and refuses an
            // unparseable settings.json (it would clobber recoverable state).
            match qontinui_runner_lib::profiles::promote_tier_to_account() {
                // The path is printed, not implied. This writer resolves
                // `settings.json` from the process env, so "promoted" on its own
                // is unfalsifiable — and a message that named no file is exactly
                // how an empty `QONTINUI_CONFIG_DIR` used to report a write into
                // the operator's CWD as a success.
                Ok((qontinui_runner_lib::profiles::TierWrite::Written, path)) => {
                    println!(
                        "runner tier promoted to qontinui_account in {}",
                        path.display()
                    );
                }
                Ok((qontinui_runner_lib::profiles::TierWrite::Unchanged, path)) => {
                    println!("runner tier already qontinui_account in {}", path.display());
                }
                Ok((qontinui_runner_lib::profiles::TierWrite::SkippedSecondary, _)) => {
                    eprintln!(
                        "warning: QONTINUI_INSTANCE_NAME is set, so this is a SECONDARY runner \
                         instance — refusing to write the shared settings.json (it would demote \
                         the primary). Run `device pair` from the primary, or set the tier there."
                    );
                }
                Err(e) => {
                    eprintln!(
                        "warning: pairing persisted but the runner tier could not be promoted \
                         ({e}); coord access stays blocked until settings.json::tier is \
                         qontinui_account"
                    );
                }
            }

            // Print + flush stdout explicitly, BEFORE any teardown, and also
            // emit the same line to stderr (unbuffered by default). This is
            // the plan's harm #2 fix: `println!` to a non-tty is
            // block-buffered, so a kill after a hang below this point used to
            // discard the only human-readable confirmation that pairing
            // succeeded. The stderr copy survives independently of the
            // stdout flush having reached the terminal driver.
            println!("{success_line}");
            let _ = std::io::stdout().flush();
            eprintln!("{success_line}");
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
///   (`paired_user.json` exists from a prior `device pair` run — under
///   `QONTINUI_SECURE_STORAGE_DIR` when set and non-empty, else
///   `{data_local_dir}/com.qontinui.runner/`). When absent, coord treats
///   this as a "system device" register.
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
    let tenant_uuid = uuid::Uuid::parse_str(tenant_id)
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
    // Present the device-JWT when this box already holds one. Blocking-client
    // sibling of the async `attach_device_auth_for` every other coord writer
    // uses; both share one token source and one coverage counter (see
    // `auth::count_and_resolve_bearer`). Never fatal — a box registering for
    // the FIRST time necessarily holds no credential yet, and coord still
    // accepts anonymous register; the header is what lets the Phase 3(b)
    // enforcement flip see this caller at all.
    //
    // The tenant is the one this very request declares in its body, so the
    // bearer comes from THAT binding's JWT slot. On a slot miss the helper
    // sends unauthenticated rather than presenting another tenant's
    // credential, which is `auth::select_device_bearer`'s documented posture.
    let resp = qontinui_runner_lib::auth::attach_device_auth_blocking(
        client.post(&url).json(&body),
        qontinui_runner_lib::auth::TenantScope::Owned(tenant_uuid),
    )
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

#[expect(
    clippy::disallowed_methods,
    reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
)]
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

// ============================================================================
// `tier` — the headless door to `settings.json::tier`
// ============================================================================

/// Read or write the runner tier from a headless box.
///
/// # Why this subcommand exists
///
/// `coord doctor`'s remediation for a credentialed-but-unauthorized box tells
/// the operator to clear `settings.json::tier_chosen_explicitly`. Until this
/// landed, NOTHING in the tree could: `commands::auth::set_runner_tier` only
/// ever writes `true`, and it is a `#[tauri::command]` behind a WebView that a
/// headless box does not have. The remediation reduced to hand-editing a
/// runner-managed JSON file — the same shape of defect as the one the headless
/// tier plan exists to close, so the fix is the door, not a softer sentence.
///
/// All three modes go through the lib's ONE tier writer
/// (`profiles::apply_tier_edit_at`), so they inherit its guards: a secondary
/// instance is refused, an unparseable `settings.json` is refused rather than
/// clobbered, and a no-op edit writes nothing at all.
fn cmd_tier(set: Option<&str>, clear_choice: bool) -> ExitCode {
    use qontinui_runner_lib::profiles::{
        clear_tier_choice_at, set_tier_choice_at, settings_json_path, TierWrite,
    };

    if set.is_some() && clear_choice {
        eprintln!("--set and --clear-choice are mutually exclusive");
        return ExitCode::from(2);
    }
    let (path, source) = settings_json_path();
    let Some(path) = path else {
        eprintln!("cannot resolve settings.json path (source: {source})");
        return ExitCode::from(2);
    };
    let is_secondary = qontinui_runner_lib::instance_env::is_secondary();

    let outcome = match (set, clear_choice) {
        (Some(tier), _) => set_tier_choice_at(&path, is_secondary, tier),
        (None, true) => clear_tier_choice_at(&path, is_secondary),
        // Read-only: print what the document says and what it resolves to.
        (None, false) => return print_tier(&path),
    };
    match outcome {
        Ok(TierWrite::Written) => {
            match set {
                Some(t) => println!(
                    "runner tier set to {t} (recorded as an explicit choice) in {}",
                    path.display()
                ),
                None => println!(
                    "cleared tier_chosen_explicitly in {} — {}",
                    path.display(),
                    clear_choice_note(persisted_tier_at(&path).as_deref())
                ),
            }
            // The resolved tier can differ from what was just written — a
            // cleared choice re-opens the inference — so print the read-back
            // rather than asserting the write's own intent.
            let _ = print_tier(&path);
            ExitCode::SUCCESS
        }
        Ok(TierWrite::Unchanged) => {
            println!("{} already says that — no write", path.display());
            ExitCode::SUCCESS
        }
        Ok(TierWrite::SkippedSecondary) => {
            eprintln!(
                "QONTINUI_INSTANCE_NAME is set, so this is a SECONDARY runner instance — \
                 refusing to write the shared settings.json (it would demote the primary). \
                 Run this from the primary."
            );
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("tier write failed: {e:#}");
            ExitCode::from(2)
        }
    }
}

/// `settings.json::tier` exactly as WRITTEN, or `None` when the file is
/// absent/unreadable/unparseable or carries no `tier` key.
///
/// Deliberately not [`read_runner_tier_at`]: that returns the RESOLVED tier
/// (inference included), and the question here is which value the inference
/// gate itself keys on.
///
/// [`read_runner_tier_at`]: qontinui_runner_lib::profiles::read_runner_tier_at
fn persisted_tier_at(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    Some(json.get("tier")?.as_str()?.to_string())
}

/// What `--clear-choice` may truthfully claim it accomplished, given the tier
/// the document STILL carries after the write.
///
/// `clear_tier_choice_at` deliberately leaves `tier` alone, and
/// `profiles::tier_is_open_to_inference` re-opens only on `local` or no tier at
/// all — `local_provider` and `qontinui_account` stay closed on the value
/// itself. So on the very document `coord doctor` sends operators here for (its
/// diagnosis names "an explicitly-set `local_provider`") the flag is cleared
/// and the inference does NOT re-open. Announcing that it did was false exactly
/// where it mattered most, so this asks the same predicate the next settings
/// load will ask instead of asserting the happy path.
fn clear_choice_note(persisted_tier: Option<&str>) -> String {
    use qontinui_runner_lib::profiles::{tier_is_open_to_inference, QONTINUI_ACCOUNT_TIER};

    if tier_is_open_to_inference(persisted_tier, /* chosen_explicitly = */ false) {
        return "the tier inference (pairing / QONTINUI_SERVER_MODE / legacy \
                runner_token) is open again on the next settings load"
            .to_string();
    }
    let tier = persisted_tier.map(str::trim).unwrap_or_default();
    if tier == QONTINUI_ACCOUNT_TIER {
        format!(
            "settings.json still says tier={tier}, which already IS the tier that \
             talks to coord — the cleared flag changes nothing this box needs"
        )
    } else {
        format!(
            "but the inference is STILL CLOSED: settings.json says tier={tier}, and \
             only `local` (or no tier at all) re-opens to inference. \
             `--clear-choice` deliberately does not change the tier — run \
             `qontinui_profile tier --set qontinui_account` to set it directly"
        )
    }
}

/// Print the tier this settings document resolves to, the raw fields behind it,
/// and the file that was read.
///
/// The resolved value comes from `profiles::read_runner_tier_at` under
/// `ProcessTierInputs::none()` — the DOCUMENT question, the same one
/// `profiles::read_runner_tier_from_document` and therefore `coord doctor` ask
/// — so this command's answer and the doctor's cannot disagree. It
/// deliberately consults NONE of this shell's process-scoped inputs
/// (`QONTINUI_SERVER_MODE`, `QONTINUI_RUNNER_TOKEN`, `QONTINUI_RUNNER_TIER`):
/// those are properties of a running runner's process, and this shell is the
/// diagnostician's, not the patient's — reporting them here would describe a
/// runner that may not even be running.
///
/// That choice is also the one an operator can most easily misread, so the
/// output SAYS it, the way `coord_doctor`'s `Absent` arm does. On a headless
/// box this command prints `resolves to: local` for a runner that is genuinely
/// running at Tier 2 — and debugging exactly that is what sends an operator
/// here.
fn print_tier(path: &Path) -> ExitCode {
    use qontinui_runner_lib::profiles::{
        read_runner_tier_at, ProcessTierInputs, TierRead, LOCAL_TIER, QONTINUI_ACCOUNT_TIER,
    };

    let paired = qontinui_runner_lib::pair::device_is_paired();
    let resolved = read_runner_tier_at(path, paired, &ProcessTierInputs::none());
    let raw: Option<serde_json::Value> = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let field = |k: &str| -> String {
        raw.as_ref()
            .and_then(|r| r.get(k))
            .map(|v| v.to_string())
            .unwrap_or_else(|| "<absent>".to_string())
    };
    println!("settings.json:            {}", path.display());
    println!("tier (as written):        {}", field("tier"));
    println!("tier_initialized:         {}", field("tier_initialized"));
    println!(
        "tier_chosen_explicitly:   {}",
        field("tier_chosen_explicitly")
    );
    println!("device paired:            {paired}");
    match &resolved {
        TierRead::Known(t) => println!("resolves to:              {t}"),
        TierRead::Absent => println!(
            "resolves to:              <no tier> (the document carries none and \
             nothing on disk infers one)"
        ),
        TierRead::Unknown(e) => {
            println!("resolves to:              UNKNOWN — settings.json unreadable ({e})");
            return ExitCode::from(1);
        }
    }
    // The same caveat `coord_doctor`'s tier check spells out, extended to every
    // process-scoped input this reader ignores. Without it, an operator
    // debugging a headless runner reads `resolves to: local` as the tier that
    // runner is running at — and this is the surface they reach for first.
    println!("note:                     this is what the DOCUMENT resolves to. A RUNNING runner");
    println!("                          also applies process-scoped inputs this command ignores:");
    println!("                          QONTINUI_SERVER_MODE, QONTINUI_RUNNER_TOKEN and");
    println!("                          QONTINUI_RUNNER_TIER are properties of that runner's");
    println!("                          process, not of this file, so a headless runner can be");
    println!(
        "                          running at {QONTINUI_ACCOUNT_TIER} while this line says {LOCAL_TIER}."
    );
    ExitCode::SUCCESS
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
    fn device_init_write_leaves_no_temp_debris() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("machine.json");
        let (file, _) =
            device_init_write_at(&path, Some("test-display-name"), "test-host").expect("write");
        let loaded = read_device_file(&path).expect("read");
        assert_eq!(loaded.device_id, file.device_id);
        assert_eq!(loaded.hostname, "test-host");
        assert_eq!(loaded.name.as_deref(), Some("test-display-name"));
        // No temp sibling of any spelling may linger after a successful write.
        let debris: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(debris.is_empty(), "temp files left behind: {debris:?}");
    }

    /// `--clear-choice` may only claim it re-opened the inference when it
    /// actually did — and on the document `coord doctor` sends operators here
    /// for, it does not.
    ///
    /// `clear_tier_choice_at` writes `tier_chosen_explicitly = false` and
    /// leaves `tier` alone; `tier_is_open_to_inference` re-opens on `local` or
    /// no tier and on nothing else. So on an explicitly-set `local_provider`
    /// the flag clears and the inference stays shut. The old unconditional
    /// message asserted the opposite, in exactly the case the doctor's
    /// `TIER_FIX_UNPIN` remediation is written for.
    #[test]
    fn clear_choice_note_only_claims_re_opening_when_the_inference_is_open() {
        for open in [Some("local"), Some("  local  "), Some(""), None] {
            let note = clear_choice_note(open);
            assert!(
                note.contains("open again"),
                "tier {open:?} IS open to inference: {note}"
            );
        }

        // The doctor's own case: clearing the flag changes nothing here.
        let note = clear_choice_note(Some("local_provider"));
        assert!(note.contains("STILL CLOSED"), "{note}");
        assert!(note.contains("tier=local_provider"), "{note}");
        assert!(
            note.contains("--set qontinui_account"),
            "the message must name the door that does work: {note}"
        );
        assert!(!note.contains("open again"), "{note}");

        // Already Tier 2: closed, but nothing is wrong — say so, rather than
        // sending the operator to a tier they already have.
        let note = clear_choice_note(Some("qontinui_account"));
        assert!(note.contains("already IS the tier"), "{note}");
        assert!(!note.contains("open again"), "{note}");
    }

    /// `persisted_tier_at` reads the RAW `tier` field, and answers `None` for
    /// every document that carries none — which is the value
    /// `tier_is_open_to_inference` treats as open.
    #[test]
    fn persisted_tier_at_reads_the_raw_field_and_none_otherwise() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("settings.json");

        assert_eq!(persisted_tier_at(&path), None, "absent file");
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(persisted_tier_at(&path), None, "unparseable file");
        std::fs::write(&path, br#"{"tier_initialized":true}"#).unwrap();
        assert_eq!(persisted_tier_at(&path), None, "no tier key");
        std::fs::write(&path, br#"{"tier":123}"#).unwrap();
        assert_eq!(persisted_tier_at(&path), None, "non-string tier");
        std::fs::write(&path, br#"{"tier":"local_provider"}"#).unwrap();
        assert_eq!(
            persisted_tier_at(&path).as_deref(),
            Some("local_provider"),
            "the value as written"
        );
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

    // ------------------------------------------------------------------
    // `device init` mint-once / re-use-always — plan
    // `2026-08-06-device-identity-is-per-profile-not-per-machine` Phase 2(a).
    //
    // Coord UPSERTs `ON CONFLICT (device_id)`, so re-presenting the stored id
    // is the ONLY thing keeping one physical machine to one `coord.devices`
    // row. Nothing used to fail if a refactor re-introduced a mint here.
    // Every test uses a tempdir — never the real ~/.qontinui/machine.json.
    // ------------------------------------------------------------------

    const PINNED_ID: &str = "c79a07d5-0000-4000-8000-000000000001";

    /// `device init` against an EXISTING machine.json re-uses the stored
    /// `device_id` — it does not mint, on any number of runs.
    #[test]
    fn device_init_reuses_the_stored_device_id() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("machine.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "device_id": PINNED_ID,
                "hostname": "old-host",
            }))
            .unwrap(),
        )
        .expect("seed");

        for _ in 0..3 {
            let (file, was_new) =
                device_init_write_at(&path, None, "new-host").expect("init must succeed");
            assert!(!was_new, "an existing file must never report a fresh mint");
            assert_eq!(file.device_id, PINNED_ID, "device_id must be RE-USED");
            assert_eq!(file.hostname, "new-host", "hostname is re-detected");
        }
        assert_eq!(read_device_file(&path).unwrap().device_id, PINNED_ID);
    }

    /// `device init` PRESERVES `active_tenant_id` and every other sibling
    /// field. The old `DeviceFile` round-trip dropped them, so the documented
    /// recovery step silently unpinned the machine's tenant.
    #[test]
    fn device_init_preserves_active_tenant_id_and_siblings() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("machine.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "device_id": PINNED_ID,
                "hostname": "spaceship",
                "active_tenant_id": "c231d9da-0000-4000-8000-000000000002",
                "some_future_field": {"nested": true},
            }))
            .unwrap(),
        )
        .expect("seed");

        device_init_write_at(&path, Some("display"), "spaceship").expect("init");

        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).expect("json");
        assert_eq!(
            v.get("active_tenant_id").and_then(|x| x.as_str()),
            Some("c231d9da-0000-4000-8000-000000000002"),
            "device init must NOT delete the tenant pin"
        );
        assert_eq!(
            v.get("some_future_field"),
            Some(&serde_json::json!({"nested": true})),
            "unknown top-level fields must survive the round-trip"
        );
        assert_eq!(v.get("device_id").and_then(|x| x.as_str()), Some(PINNED_ID));
        assert_eq!(v.get("name").and_then(|x| x.as_str()), Some("display"));
    }

    /// `device init` against an UNREADABLE/corrupt machine.json refuses to
    /// overwrite — overwriting would mint a fresh identity and a new
    /// `coord.devices` row for the same machine.
    #[test]
    fn device_init_refuses_to_overwrite_an_unreadable_file() {
        let dir = tempfile::tempdir().expect("tmpdir");

        for (label, contents) in [
            ("corrupt json", &b"{ not json at all"[..]),
            ("not an object", &b"[1,2,3]"[..]),
            ("missing device_id", &br#"{"hostname":"spaceship"}"#[..]),
            (
                "blank device_id",
                &br#"{"device_id":"  ","hostname":"h"}"#[..],
            ),
        ] {
            let path = dir.path().join(format!("{}.json", label.replace(' ', "_")));
            std::fs::write(&path, contents).expect("seed");
            let err = match device_init_write_at(&path, None, "spaceship") {
                Ok(_) => panic!("{label} must be refused, not written"),
                Err(e) => e,
            };
            assert!(
                err.contains("Refusing to overwrite"),
                "{label}: expected a refusal, got: {err}"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                contents,
                "{label}: the file must be left byte-identical for inspection"
            );
        }
    }

    /// The one legitimate mint: an ABSENT file. The very next run re-uses it.
    #[test]
    fn device_init_mints_once_on_an_absent_file_then_reuses() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("machine.json");

        let (first, was_new) = device_init_write_at(&path, None, "spaceship").expect("mint");
        assert!(was_new, "an absent file is the mint case");
        assert!(uuid::Uuid::parse_str(&first.device_id).is_ok());

        let (second, was_new_again) =
            device_init_write_at(&path, None, "spaceship").expect("reuse");
        assert!(!was_new_again);
        assert_eq!(
            second.device_id, first.device_id,
            "the second run must RE-USE, not re-mint"
        );
    }

    /// A PADDED on-disk `device_id` must be trimmed before it is written back
    /// or handed to coord.
    ///
    /// `cmd_device_init` registers `file.device_id` (the value this function
    /// returns) while `machine_identity::read_device_id_at` — every other
    /// path's reader — trims. So a stored `"  abc  "` used to be registered as
    /// `"  abc  "` here and as `"abc"` everywhere else, and coord UPSERTs
    /// `ON CONFLICT (device_id)`: two rows, one machine. The written file and
    /// the coord-facing value must agree, and both must be the trimmed form.
    #[test]
    fn device_init_trims_a_padded_device_id_on_disk_and_on_the_wire() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("machine.json");
        std::fs::write(&path, format!(r#"{{"device_id":"  {PINNED_ID}  "}}"#)).expect("seed");

        // The value `cmd_device_init` hands to `register_with_coord`.
        let (file, was_new) = device_init_write_at(&path, None, "spaceship").expect("init");
        assert!(!was_new);
        assert_eq!(
            file.device_id, PINNED_ID,
            "the coord-facing id must be trimmed"
        );

        // The value on disk, and the value every other reader resolves.
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).expect("json");
        assert_eq!(
            v.get("device_id").and_then(|x| x.as_str()),
            Some(PINNED_ID),
            "the on-disk id must be trimmed"
        );
        assert_eq!(
            qontinui_runner_lib::machine_identity::read_device_id_at(&path).unwrap(),
            file.device_id,
            "disk and wire must agree — a padding-only difference is two coord.devices rows"
        );
    }

    /// A `{"device_id": "…"}` file with NO `hostname` is readable by every
    /// runtime consumer, so `device init` must REPAIR it (re-detecting the
    /// hostname), not call it unreadable and point at `rm`. Before
    /// `#[serde(default)]` on `DeviceFile::hostname` this refused, and the
    /// refusal's own advice destroyed a perfectly good identity.
    #[test]
    fn device_init_repairs_a_file_missing_only_the_hostname() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("machine.json");
        std::fs::write(&path, format!(r#"{{"device_id":"{PINNED_ID}"}}"#)).expect("seed");

        let (file, was_new) = device_init_write_at(&path, None, "spaceship")
            .expect("a hostname-less file carries a good identity and must be repaired");
        assert!(!was_new, "repair is not a mint");
        assert_eq!(file.device_id, PINNED_ID);
        assert_eq!(file.hostname, "spaceship");
        assert_eq!(read_device_file(&path).unwrap().hostname, "spaceship");
    }

    /// The refusals must distinguish RECOVERABLE (the file may still hold the
    /// real UUID → inspect by hand, never `rm`) from UNRECOVERABLE (no identity
    /// present → `rm` + `device init` is correct).
    ///
    /// Four messages used to say `rm` for all four classes, and for the
    /// recoverable ones taking that advice mints a fresh identity — a second
    /// `coord.devices` row for the same machine, i.e. precisely the failure
    /// this command's refusal exists to prevent.
    #[test]
    fn device_init_refusals_distinguish_recoverable_from_unrecoverable() {
        let dir = tempfile::tempdir().expect("tmpdir");
        for (label, contents, recoverable) in [
            ("corrupt json", &b"{ not json at all"[..], true),
            ("not an object", &b"[1,2,3]"[..], true),
            // A good identity beside a malformed sibling: still recoverable.
            (
                "bad hostname type",
                &br#"{"device_id":"c79a07d5-0000-4000-8000-000000000001","hostname":42}"#[..],
                true,
            ),
            (
                "missing device_id",
                &br#"{"hostname":"spaceship"}"#[..],
                false,
            ),
            (
                "blank device_id",
                &br#"{"device_id":"  ","hostname":"h"}"#[..],
                false,
            ),
        ] {
            let path = dir.path().join(format!("{}.json", label.replace(' ', "_")));
            std::fs::write(&path, contents).expect("seed");
            let err = match device_init_write_at(&path, None, "spaceship") {
                Ok(_) => panic!("{label} must be refused, not written"),
                Err(e) => e,
            };
            assert!(
                err.contains("Refusing to overwrite"),
                "{label}: expected a refusal, got: {err}"
            );
            if recoverable {
                assert!(
                    err.contains("Do NOT `rm` it"),
                    "{label}: a file that may hold the identity must forbid `rm`, got: {err}"
                );
                assert!(
                    err.contains("Inspect and repair it by hand"),
                    "{label}: must offer hand repair, got: {err}"
                );
            } else {
                assert!(
                    err.contains("no identity to preserve"),
                    "{label}: must state there is nothing to lose, got: {err}"
                );
                assert!(
                    err.contains("`rm` it and re-run"),
                    "{label}: `rm` + init is the CORRECT advice here, got: {err}"
                );
            }
            assert_eq!(
                std::fs::read(&path).unwrap(),
                contents,
                "{label}: the file must be left byte-identical for inspection"
            );
        }
    }

    /// Defect 4: the machine.json writers must not use the single shared
    /// `machine.json.tmp` path (raced by every runner instance at startup).
    /// Squatting that exact path with a directory makes any writer that still
    /// uses it fail.
    #[test]
    fn device_init_does_not_use_the_shared_fixed_temp_path() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("machine.json");
        let squatted = path.with_extension("json.tmp");
        std::fs::create_dir(&squatted).expect("squat the legacy tmp path");

        let (file, _) = device_init_write_at(&path, None, "spaceship")
            .expect("write must succeed despite the squatted tmp path");
        assert_eq!(read_device_file(&path).unwrap().device_id, file.device_id);
        assert!(squatted.is_dir(), "the squatted path must be untouched");
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

    // ------------------------------------------------------------------
    // resolve_pair_code_base — fleet-join, 2026-08-24. A fresh machine
    // with no profiles.json and no override must still be able to redeem
    // a pair code against production, not hard-error.
    // ------------------------------------------------------------------

    #[test]
    fn resolve_pair_code_base_prefers_env_override_over_everything() {
        assert_eq!(
            resolve_pair_code_base(Some("https://custom.example")),
            (
                "https://custom.example".to_string(),
                PairCodeBaseSource::EnvOverride
            )
        );
    }

    #[test]
    fn resolve_pair_code_base_ignores_an_empty_env_override() {
        // An empty string is not a real override — e.g. `QONTINUI_WEB_BASE=`
        // in an env file. Falls through exactly as if unset, and must report
        // the arm that actually won rather than the one that was skipped.
        assert_eq!(
            resolve_pair_code_base(Some("")),
            (
                PROD_API_BASE_URL.to_string(),
                PairCodeBaseSource::ProdDefault
            )
        );
    }

    #[test]
    fn resolve_pair_code_base_never_returns_the_coord_host() {
        // REGRESSION. A middle rung used to derive this base from the active
        // profile's coord_url. In production that sent a WEB-backend route to
        // coord, which answers 401 (measured headless 2026-09-02); in dev it
        // stripped the port and pointed at :80 instead of :8000. Nothing may
        // reintroduce a coord-derived answer here: with no override the ONLY
        // permitted result is the production web base.
        let (base, arm) = resolve_pair_code_base(None);
        assert_eq!(base, PROD_API_BASE_URL);
        assert_eq!(arm, PairCodeBaseSource::ProdDefault);
        assert!(
            !base.contains("coord"),
            "pair-code base must never resolve to a coord host, got {base}"
        );
    }

    #[test]
    fn resolve_pair_code_base_treats_whitespace_env_as_unset() {
        // An exported-but-blank var is how a shell says "absent"; it must not
        // win the ladder and produce an empty base.
        assert_eq!(
            resolve_pair_code_base(Some("   ")),
            (
                PROD_API_BASE_URL.to_string(),
                PairCodeBaseSource::ProdDefault
            )
        );
    }

    #[test]
    fn resolve_pair_code_base_trims_a_trailing_slash_from_the_override() {
        // `QONTINUI_WEB_BASE=https://x/` would otherwise build `https://x//api/v1/...`.
        assert_eq!(
            resolve_pair_code_base(Some("https://custom.example/")),
            (
                "https://custom.example".to_string(),
                PairCodeBaseSource::EnvOverride
            )
        );
    }

    #[test]
    fn resolve_pair_code_base_falls_back_to_prod_default_with_nothing_configured() {
        // The fresh-machine case this whole fix is for: no profiles.json
        // (coord_base is None), no QONTINUI_WEB_BASE. Must resolve to the
        // fleet's real production API host, not error and not silently
        // point at localhost.
        assert_eq!(
            resolve_pair_code_base(None),
            (
                PROD_API_BASE_URL.to_string(),
                PairCodeBaseSource::ProdDefault
            )
        );
    }

    #[test]
    fn pair_code_base_sources_have_distinct_operator_labels() {
        // The whole point of the second return value: an operator reading the
        // printed line must be able to tell the three rungs apart, because
        // each has a different fix. Identical labels would be worse than none.
        let labels = [
            PairCodeBaseSource::EnvOverride.as_str(),
            PairCodeBaseSource::ProdDefault.as_str(),
        ];
        let unique: std::collections::HashSet<&str> = labels.iter().copied().collect();
        assert_eq!(
            unique.len(),
            labels.len(),
            "labels must be distinct: {labels:?}"
        );
        assert!(labels.iter().all(|l| !l.is_empty()));
        // Each label must name the knob the operator would turn.
        assert!(PairCodeBaseSource::EnvOverride
            .as_str()
            .contains("QONTINUI_WEB_BASE"));
        assert!(PairCodeBaseSource::ProdDefault.as_str().contains("default"));
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

    /// The `tier` door exists at the argv layer, and its two write modes are
    /// distinct flags — `coord doctor`'s unpin remediation names
    /// `--clear-choice` by that spelling, so a rename here breaks the fix line
    /// it prints.
    #[test]
    fn tier_subcommand_parses_all_three_modes() {
        let cli = Cli::try_parse_from(["qontinui_profile", "tier"]).expect("parses");
        match cli.cmd {
            Some(Cmd::Tier { set, clear_choice }) => {
                assert!(set.is_none());
                assert!(!clear_choice);
            }
            other => panic!("expected Tier, got {:?}", other),
        }

        let cli =
            Cli::try_parse_from(["qontinui_profile", "tier", "--set", "local"]).expect("parses");
        match cli.cmd {
            Some(Cmd::Tier { set, clear_choice }) => {
                assert_eq!(set.as_deref(), Some("local"));
                assert!(!clear_choice);
            }
            other => panic!("expected Tier {{set}}, got {:?}", other),
        }

        let cli =
            Cli::try_parse_from(["qontinui_profile", "tier", "--clear-choice"]).expect("parses");
        match cli.cmd {
            Some(Cmd::Tier { set, clear_choice }) => {
                assert!(set.is_none());
                assert!(clear_choice, "the door TIER_FIX_UNPIN names");
            }
            other => panic!("expected Tier {{clear_choice}}, got {:?}", other),
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

    #[test]
    fn env_pull_parses_bare_and_with_json() {
        let cli = Cli::try_parse_from(["qontinui_profile", "env", "pull"]).expect("parses");
        match cli.cmd {
            Some(Cmd::Env {
                sub: EnvCmd::Pull { json },
            }) => assert!(!json, "--json defaults off"),
            other => panic!("expected Env::Pull, got {:?}", other),
        }

        let cli =
            Cli::try_parse_from(["qontinui_profile", "env", "pull", "--json"]).expect("parses");
        match cli.cmd {
            Some(Cmd::Env {
                sub: EnvCmd::Pull { json },
            }) => assert!(json),
            other => panic!("expected Env::Pull {{json}}, got {:?}", other),
        }
    }
}
