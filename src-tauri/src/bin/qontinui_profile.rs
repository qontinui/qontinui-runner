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
//! qontinui_profile machine init               # mint ~/.qontinui/machine.json + register in coord.machines
//! qontinui_profile machine show               # print machine_id + coord registration status
//! qontinui_profile machine path               # print the machine.json path
//! ```
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

use qontinui_runner_lib::profiles::{
    load_strict, profiles_path, AuthConfig, BlobConfig, Profile, ProfilesFile,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("show");

    match cmd {
        "show" => cmd_show(),
        "list" => cmd_list(),
        "use" => match args.get(2) {
            Some(name) => cmd_use(name),
            None => {
                eprintln!("usage: qontinui_profile use <name>");
                ExitCode::from(2)
            }
        },
        "init" => {
            // Parse `--host <ip>` if present; otherwise default to localhost.
            // Position-independent so `init --host 1.2.3.4` and the
            // accidentally-permuted `init` (no flag) both work.
            let host = parse_host_arg(&args).unwrap_or_else(|| "localhost".to_string());
            cmd_init(&host)
        }
        "path" => cmd_path(),
        "machine" => cmd_machine(&args),
        "help" | "-h" | "--help" => {
            print_help();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown command: {}", other);
            print_help();
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    println!(
        "qontinui_profile — manage ~/.qontinui/profiles.json\n\n\
         Commands:\n\
         \x20 show                       Print the resolved active profile\n\
         \x20 list                       List profile names\n\
         \x20 use <name>                 Set the file's 'active' field\n\
         \x20 init [--host <ip>]         Write a starter profiles.json. Default host is\n\
         \x20                            localhost (PC-local dev). Pass --host <PC-LAN-IP>\n\
         \x20                            from a laptop / third machine to point at the PC.\n\
         \x20 path                       Print the profiles.json path\n\
         \x20 machine <init|show|path>   Manage ~/.qontinui/machine.json (machine identity\n\
         \x20                            for coord.machines registration; required for\n\
         \x20                            /coord/status POSTs and non-NULL claims_audit rows)\n\
         \x20 help                       Show this message\n"
    );
}

fn print_machine_help() {
    println!(
        "qontinui_profile machine — manage ~/.qontinui/machine.json\n\n\
         Commands:\n\
         \x20 init    Mint UUID v4 + hostname to machine.json (atomic), then UPSERT\n\
         \x20         into the active profile's coord.machines. Idempotent: re-runs\n\
         \x20         re-use the existing UUID and bump last_seen_at.\n\
         \x20 show    Print machine_id + coord.machines registration timestamps.\n\
         \x20 path    Print the absolute machine.json path.\n"
    );
}

/// Parse `--host <value>` out of an arg list. Position-independent.
/// Returns None if the flag is absent; returns Some("") if `--host` is the
/// last token (caller should treat that the same as default).
fn parse_host_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if a == "--host" {
            return iter.next().cloned();
        }
    }
    None
}

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
    let dev = Profile {
        // Defaults match qontinui-stack/.env.example. `host` defaults to
        // `localhost` for PC-local dev; LAN clients pass `--host <PC-IP>`.
        database_url: Some(format!(
            "host={host} port=5433 user=qontinui_user password=qontinui_dev_password dbname=qontinui_db"
        )),
        redis_url: Some(format!("redis://:qontinui_dev_redis@{host}:6380/0")),
        blob: Some(BlobConfig {
            kind: "s3-compatible".to_string(),
            endpoint: Some(format!("http://{host}:9100")),
            region: Some("us-east-1".to_string()),
            access_key: Some("minioadmin".to_string()),
            secret_key: Some("minioadmin".to_string()),
            bucket: Some("qontinui-dev".to_string()),
        }),
        // Coord service runs at qontinui-stack's :9870 — see qontinui-coord.
        // ws:// (not wss://) for dev because there's no TLS termination on
        // the LAN. AWS staging flips to wss:// via the ALB.
        coord_url: Some(format!("ws://{host}:9870/ws")),
        auth: Some(AuthConfig {
            kind: "static-dev-token".to_string(),
            token: Some("dev-token-replace-me".to_string()),
            issuer: None,
            client_id: None,
        }),
    };
    let mut profiles = HashMap::new();
    profiles.insert("dev".to_string(), dev);
    let file = ProfilesFile {
        active: Some("dev".to_string()),
        profiles,
    };
    if let Err(e) = atomic_write(&path, &file) {
        eprintln!("write failed: {}", e);
        return ExitCode::from(2);
    }
    println!(
        "wrote starter profiles.json to {}\n\
         active profile: dev (host={host}, ports 5433/6380/9100/9870)",
        path.display()
    );
    if host == "localhost" {
        println!(
            "\nTo wire a laptop / third machine into the PC's canonical stack:\n\
             \x20 qontinui_profile init --host <PC-LAN-IP>"
        );
    }
    ExitCode::SUCCESS
}

/// Write `file` atomically: serialize to a sibling `.tmp`, then rename.
/// Avoids a partial-write window where a reader could observe an
/// invalid-JSON profiles.json.
fn atomic_write(path: &Path, file: &ProfilesFile) -> std::io::Result<()> {
    let pretty = serde_json::to_vec_pretty(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &pretty)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ============================================================================
// machine subcommand
// ============================================================================
//
// Per topology plan §3, every runner has a stable machine identity stored at
// ~/.qontinui/machine.json. The active profile's coord service uses this UUID
// as the foreign key in coord.claims_audit / coord.machine_status / etc.
// `qontinui_profile init` writes profiles.json but NOT machine.json — this
// gap left new machines with NULL machine_id audit rows and rejected
// /coord/status POSTs (qontinui-coord/src/status.rs:116-122).

/// Shape of `~/.qontinui/machine.json`. UUID v4 + hostname only — additional
/// per-machine state (current_branches, last_alembic_head) lives in
/// coord.machines, not this file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MachineFile {
    machine_id: String,
    hostname: String,
}

fn machine_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("machine.json"))
}

fn read_machine_file(path: &Path) -> std::io::Result<MachineFile> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn atomic_write_machine(path: &Path, file: &MachineFile) -> std::io::Result<()> {
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

fn cmd_machine(args: &[String]) -> ExitCode {
    let sub = args.get(2).map(|s| s.as_str()).unwrap_or("show");
    match sub {
        "init" => cmd_machine_init(),
        "show" => cmd_machine_show(),
        "path" => cmd_machine_path(),
        "help" | "-h" | "--help" => {
            print_machine_help();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown machine subcommand: {}", other);
            print_machine_help();
            ExitCode::from(2)
        }
    }
}

fn cmd_machine_path() -> ExitCode {
    match machine_path() {
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

fn cmd_machine_init() -> ExitCode {
    let path = match machine_path() {
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
        match read_machine_file(&path) {
            Ok(mut existing) => {
                existing.hostname = hostname_now.clone();
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
            MachineFile {
                machine_id: id,
                hostname: hostname_now.clone(),
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
    if let Err(e) = atomic_write_machine(&path, &file) {
        eprintln!("write {} failed: {}", path.display(), e);
        return ExitCode::from(2);
    }
    if was_new {
        println!("wrote machine.json: {} (host={})", file.machine_id, file.hostname);
    } else {
        println!("re-using existing machine.json: {} (host={})", file.machine_id, file.hostname);
    }

    // Register with coord by UPSERTing coord.machines on the active profile's
    // PG. File creation succeeds even if coord registration fails — the local
    // identity is the canonical record; coord.machines is a derived view
    // (qontinui-coord re-syncs from machine.json on next /coord/status POST).
    match register_with_coord(&file.machine_id, &file.hostname) {
        Ok(()) => {
            println!("registered with coord (UPSERT into coord.machines)");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!(
                "warning: coord registration failed: {}\n\
                 (machine.json was still written; re-run `qontinui_profile machine init` once coord is reachable)",
                e
            );
            ExitCode::SUCCESS
        }
    }
}

fn cmd_machine_show() -> ExitCode {
    let path = match machine_path() {
        Some(p) => p,
        None => {
            eprintln!("could not resolve home directory");
            return ExitCode::from(2);
        }
    };
    if !path.exists() {
        eprintln!(
            "machine.json not found at {}\n\
             Run 'qontinui_profile machine init' to mint identity and register with coord.",
            path.display()
        );
        return ExitCode::from(1);
    }
    let file = match read_machine_file(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("read failed: {}", e);
            return ExitCode::from(2);
        }
    };

    let coord_status = match query_coord_registration(&file.machine_id) {
        Ok(Some((created_at, last_seen_at))) => {
            json!({ "registered": true, "created_at": created_at, "last_seen_at": last_seen_at })
        }
        Ok(None) => json!({ "registered": false }),
        Err(e) => json!({ "registered": null, "error": e }),
    };

    let out = json!({
        "machine_id": file.machine_id,
        "hostname":   file.hostname,
        "path":       path.display().to_string(),
        "coord":      coord_status,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    ExitCode::SUCCESS
}

/// UPSERT into the active profile's `coord.machines`. Constructs a one-shot
/// tokio runtime since the rest of this CLI is sync — keeps the binary
/// otherwise unchanged. Uses NoTls (LAN/loopback dev posture; staging+ via
/// stunnel or pgbouncer-tls if it ships later).
fn register_with_coord(machine_id: &str, hostname: &str) -> Result<(), String> {
    let id = uuid::Uuid::parse_str(machine_id)
        .map_err(|e| format!("machine_id is not a valid UUID: {}", e))?;
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
        let res = client
            .execute(
                "INSERT INTO coord.machines (machine_id, hostname) \
                 VALUES ($1, $2) \
                 ON CONFLICT (machine_id) DO UPDATE \
                 SET hostname = EXCLUDED.hostname, last_seen_at = now()",
                &[&id, &hostname],
            )
            .await
            .map_err(|e| format!("UPSERT coord.machines failed: {}", e))?;
        drop(client);
        let _ = join.await;
        if res == 0 {
            return Err("UPSERT coord.machines affected 0 rows".to_string());
        }
        Ok(())
    })
}

fn query_coord_registration(machine_id: &str) -> Result<Option<(String, String)>, String> {
    let id = uuid::Uuid::parse_str(machine_id)
        .map_err(|e| format!("machine_id is not a valid UUID: {}", e))?;
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
        let row = client
            .query_opt(
                "SELECT created_at::text, last_seen_at::text \
                 FROM coord.machines WHERE machine_id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("SELECT coord.machines failed: {}", e))?;
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

    #[test]
    fn atomic_write_machine_via_tmp_then_rename() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("machine.json");
        let file = MachineFile {
            machine_id: "00000000-0000-4000-8000-000000000000".to_string(),
            hostname: "test-host".to_string(),
        };
        atomic_write_machine(&path, &file).expect("write");
        let loaded = read_machine_file(&path).expect("read");
        assert_eq!(loaded.machine_id, file.machine_id);
        assert_eq!(loaded.hostname, file.hostname);
        // The .tmp sibling must not linger after a successful rename.
        let tmp = path.with_extension("json.tmp");
        assert!(!tmp.exists(), "tmp file should be renamed away");
    }

    #[test]
    fn detect_hostname_returns_non_empty() {
        let h = detect_hostname();
        assert!(!h.is_empty(), "hostname should be detectable on this host");
    }
}
