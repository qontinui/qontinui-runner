//! Fleet topology publisher (Row 2 Phase 1, runner side).
//!
//! See `plans/2026-05-14-fleet-topology-and-build-pool-design.md` §3.2.
//! On every runner boot, this module:
//!
//! 1. Reads the local machine identity from `~/.qontinui/machine.json`
//!    (already minted by `qontinui_profile machine init`).
//! 2. Detects local resources (cpu_cores, memory_gb, disk_total_gb)
//!    via `sysinfo`.
//! 3. Derives the agent-side budget per §3.2:
//!    `max_concurrent_agents = floor((memory_gb - 4) / 4)`.
//! 4. UPSERTs role + budget columns onto `coord.machines` via direct
//!    PG. Direct PG UPSERT matches the existing identity-registration
//!    path in `qontinui_profile machine init::register_with_coord` —
//!    keeps the runner bootable when qontinui-coord HTTP is down.
//!
//! Phase 1 is visibility-only — the row appears in `GET /coord/fleet`
//! but the coord doesn't enforce caps yet (Phase 5). Failures here log
//! a warning and are swallowed; the runner still boots.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::database::pg::PgDb;

/// §3.2 declared role. Mirrors `qontinui-coord::fleet::MachineRole`.
/// The runner publishes itself as `Agent`; the supervisor publishes
/// as `Build`. Dev workstations collapse both onto one machine_id
/// (last-writer wins for Phase 1).
#[derive(Debug, Clone, Copy)]
pub enum MachineRole {
    Agent,
    Build,
}

impl MachineRole {
    fn as_str(&self) -> &'static str {
        match self {
            MachineRole::Agent => "agent",
            MachineRole::Build => "build",
        }
    }
}

/// Local-host resource fingerprint. All numbers in CPU-core / GiB units.
#[derive(Debug, Clone, Copy)]
pub struct Resources {
    pub cpu_cores: u32,
    pub memory_gb: u32,
    pub disk_total_gb: u64,
}

/// §3.2 default policy: a dev workstation reserves 4 GiB for the OS +
/// 4 GiB per agent steady-state. Underflow saturates at 0 for tiny
/// machines.
pub fn derive_max_agents(memory_gb: u32) -> u32 {
    memory_gb.saturating_sub(4) / 4
}

/// Detect cpu_cores / memory_gb / disk_total_gb on the current host.
///
/// `cpu_cores` uses [`std::thread::available_parallelism`] which
/// respects cgroup CPU limits on Linux — closer to "what we actually
/// get to use" than physical core count. Falls back to sysinfo's
/// reported physical core count on platforms where `available_parallelism`
/// returns 0 (none observed but defensive).
pub fn detect_resources() -> Resources {
    use sysinfo::{Disks, System};

    let cpu_cores: u32 = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or_else(|_| {
            let sys = System::new_all();
            sys.cpus().len() as u32
        })
        .max(1);

    let mut sys = System::new();
    sys.refresh_memory();
    // sysinfo reports bytes. GiB = bytes / 2^30. Saturating math keeps
    // exotic platforms from panicking on overflow.
    let memory_gb: u32 = (sys.total_memory() / (1024 * 1024 * 1024)).min(u32::MAX as u64) as u32;

    // Disk: sum of all unique mountpoints. Per §3.4 the budget tracks
    // *available local* disk; on Windows that's typically C:\. We
    // dedupe by mount_point so RAID arrays exposed as one volume don't
    // double-count.
    let mut seen_mounts = std::collections::HashSet::<PathBuf>::new();
    let disks = Disks::new_with_refreshed_list();
    let mut disk_total_bytes: u64 = 0;
    for d in disks.list() {
        let mount: PathBuf = d.mount_point().to_path_buf();
        if seen_mounts.insert(mount) {
            disk_total_bytes = disk_total_bytes.saturating_add(d.total_space());
        }
    }
    let disk_total_gb: u64 = disk_total_bytes / (1024 * 1024 * 1024);

    Resources {
        cpu_cores,
        memory_gb,
        disk_total_gb,
    }
}

/// `~/.qontinui/machine.json` shape — mirrors
/// `bin/qontinui_profile.rs::MachineFile` so we don't need to expose
/// it from the binary crate.
#[derive(Debug, Clone, Deserialize)]
struct MachineFile {
    machine_id: String,
    hostname: String,
}

fn machine_file_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("machine.json"))
}

fn load_machine_file() -> Option<MachineFile> {
    let path = machine_file_path()?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Publish role + budget to `coord.machines`. Best-effort: failures
/// log a warning and return Ok(()) so they don't break startup.
///
/// `disk_reserved_gb` defaults to 0 in Phase 1 — Phase 5 of the
/// fleet plan will add per-machine overrides for system + non-fleet
/// reservation. Callers can pass a non-zero value if their config
/// already knows the reservation.
pub async fn publish_budget(
    pg: &Arc<PgDb>,
    role: MachineRole,
    resources: Resources,
    disk_reserved_gb: u64,
) -> Result<(), String> {
    let machine = match load_machine_file() {
        Some(m) => m,
        None => {
            warn!(
                "fleet::publish_budget: ~/.qontinui/machine.json missing — \
                 run `qontinui_profile machine init` to register identity. Skipping budget publish."
            );
            return Ok(());
        }
    };

    let machine_id = match uuid::Uuid::parse_str(&machine.machine_id) {
        Ok(id) => id,
        Err(e) => {
            warn!(
                "fleet::publish_budget: machine.json machine_id is not a valid UUID ({e}). Skipping."
            );
            return Ok(());
        }
    };

    let max_concurrent_agents: i32 = match role {
        MachineRole::Agent => derive_max_agents(resources.memory_gb) as i32,
        MachineRole::Build => 0,
    };
    // Runner is Agent role — leaves build slots to the supervisor on
    // dev workstations. The supervisor publisher overwrites the
    // build-side fields when it starts.
    let max_concurrent_builds: i32 = 0;
    let cpu_cores_i: i32 = resources.cpu_cores.min(i32::MAX as u32) as i32;
    let memory_gb_i: i32 = resources.memory_gb.min(i32::MAX as u32) as i32;
    let disk_total_i: i64 = resources.disk_total_gb.min(i64::MAX as u64) as i64;
    let disk_reserved_i: i64 = disk_reserved_gb.min(i64::MAX as u64) as i64;
    let role_str = role.as_str();

    let conn = pg.pool().get().await.map_err(|e| format!("PG pool: {e}"))?;

    // UPSERT: INSERT new row if this is the first time we've seen the
    // machine_id; else UPDATE the budget columns. Matches the pattern
    // in `bin/qontinui_profile.rs::register_with_coord` so the
    // identity-only registration path stays compatible.
    //
    // search_path is `project, public` (set by the post_create hook),
    // so `coord.machines` MUST be schema-qualified — we never want PG
    // to silently resolve to a `project.machines` table that doesn't
    // exist.
    let affected = conn
        .execute(
            "INSERT INTO coord.machines \
                 (machine_id, hostname, role, cpu_cores, memory_gb, \
                  disk_total_gb, disk_reserved_gb, \
                  max_concurrent_agents, max_concurrent_builds, \
                  budget_updated_at, last_seen_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now(), now()) \
             ON CONFLICT (machine_id) DO UPDATE SET \
                 hostname = EXCLUDED.hostname, \
                 role = EXCLUDED.role, \
                 cpu_cores = EXCLUDED.cpu_cores, \
                 memory_gb = EXCLUDED.memory_gb, \
                 disk_total_gb = EXCLUDED.disk_total_gb, \
                 disk_reserved_gb = EXCLUDED.disk_reserved_gb, \
                 max_concurrent_agents = EXCLUDED.max_concurrent_agents, \
                 max_concurrent_builds = EXCLUDED.max_concurrent_builds, \
                 budget_updated_at = now(), \
                 last_seen_at = now()",
            &[
                &machine_id,
                &machine.hostname,
                &role_str,
                &cpu_cores_i,
                &memory_gb_i,
                &disk_total_i,
                &disk_reserved_i,
                &max_concurrent_agents,
                &max_concurrent_builds,
            ],
        )
        .await
        .map_err(|e| format!("UPSERT coord.machines: {e}"))?;

    if affected == 0 {
        warn!("fleet::publish_budget: UPSERT affected 0 rows (unexpected)");
    } else {
        info!(
            "fleet::publish_budget: machine_id={machine_id} hostname={} role={role_str} \
             cpu_cores={cpu_cores_i} memory_gb={memory_gb_i} disk_total_gb={disk_total_i} \
             max_concurrent_agents={max_concurrent_agents}",
            machine.hostname
        );
    }
    Ok(())
}

/// Convenience: detect + publish in one call with default reservations.
/// Used from `main.rs::run_app()` after PG initialization.
pub async fn publish_on_startup(pg: &Arc<PgDb>, role: MachineRole) {
    let resources = detect_resources();
    if let Err(e) = publish_budget(pg, role, resources, 0).await {
        warn!("fleet::publish_on_startup failed (non-fatal — runner still boots): {e}");
    }
}

// =============================================================================
// HTTP heartbeat to coord (plan 2026-05-18-push-aware-fleet-liveness.md §5).
//
// The direct-PG `publish_budget` path above is a one-shot at boot. To keep
// `coord.machines.last_seen_at` fresh under coord's new push-aware liveness
// model, the runner periodically POSTs `{machine_id, hostname}` to coord's
// `/coord/machine/register` endpoint. The handler's UPSERT refreshes
// `last_seen_at` and `COALESCE`s a previously-advertised `health_url`, so
// heartbeating from the runner side is a clean, side-effect-free refresh.
//
// The HTTP heartbeat path is intentionally additive: it never touches the
// direct-PG publisher and tolerates missing identity, missing profile, or
// network failure with `info!`/`warn!` and a retry on the next tick.
// =============================================================================

/// `~/.qontinui/profiles.json` — minimum subset we need (the active
/// profile's `coord_url`). Mirrors the supervisor's shape at
/// `qontinui-supervisor/src/fleet.rs:96-107` verbatim so the same
/// active-profile + trim-`/ws` logic applies on both sides.
#[derive(Debug, Clone, Deserialize)]
struct Profiles {
    #[serde(default)]
    active: Option<String>,
    #[serde(default)]
    profiles: std::collections::HashMap<String, Profile>,
}

#[derive(Debug, Clone, Deserialize)]
struct Profile {
    #[serde(default)]
    coord_url: Option<String>,
}

fn profiles_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("profiles.json"))
}

/// Resolve the coord HTTP base from the active profile's `coord_url`.
/// Profile stores `ws://host:9870/ws` or `wss://host:9870/ws` (the
/// WebSocket upgrade URL); convert that to `http://host:9870` /
/// `https://host:9870` so reqwest can POST to `/coord/machine/register`.
/// Returns `None` if profiles.json is missing or the active profile has
/// no coord_url. Mirrors `qontinui-supervisor/src/fleet.rs:122-138`.
///
/// Re-read each tick — `profiles.json` is tiny and the active profile
/// may change between ticks; caching would defeat that.
fn coord_http_base() -> Option<String> {
    let bytes = std::fs::read(profiles_path()?).ok()?;
    let pf: Profiles = serde_json::from_slice(&bytes).ok()?;
    let active = pf.active.as_deref().unwrap_or("dev");
    let coord_url = pf.profiles.get(active)?.coord_url.as_deref()?;

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
    Some(with_http)
}

#[derive(Debug, serde::Serialize)]
struct HeartbeatPayload {
    machine_id: uuid::Uuid,
    hostname: String,
}

/// POST `{machine_id, hostname}` to `<base>/coord/machine/register`.
///
/// `health_url` is deliberately omitted — coord's `register_machine`
/// handler `COALESCE`s `EXCLUDED.health_url` with the existing value,
/// so omitting from the heartbeat preserves any URL the machine
/// previously advertised. Failures are reported as `Err(String)` so
/// the caller can log them; the loop never panics.
pub async fn heartbeat_to_coord() -> Result<(), String> {
    let machine = match load_machine_file() {
        Some(m) => m,
        None => {
            info!(
                "fleet::heartbeat: ~/.qontinui/machine.json missing — \
                 run `qontinui_profile machine init` to enable fleet visibility. Skipping."
            );
            return Ok(());
        }
    };

    let machine_id = match uuid::Uuid::parse_str(&machine.machine_id) {
        Ok(id) => id,
        Err(e) => {
            warn!("fleet::heartbeat: machine.json machine_id is not a valid UUID ({e}). Skipping.");
            return Ok(());
        }
    };

    let base = match coord_http_base() {
        Some(b) => b,
        None => {
            info!(
                "fleet::heartbeat: ~/.qontinui/profiles.json missing or active profile \
                 has no coord_url — no coord to heartbeat to. Skipping."
            );
            return Ok(());
        }
    };

    let payload = HeartbeatPayload {
        machine_id,
        hostname: machine.hostname.clone(),
    };
    let url = format!("{base}/coord/machine/register");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("reqwest builder: {e}"))?;

    let resp = client
        .post(&url)
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;

    let status = resp.status();
    if status.is_success() {
        // 30s cadence — `info!` would be noisy, debug! keeps the
        // happy path quiet while still discoverable.
        debug!(
            "fleet::heartbeat: ok machine_id={machine_id} hostname={} status={}",
            machine.hostname, status
        );
        Ok(())
    } else {
        let body = resp.text().await.unwrap_or_default();
        let excerpt: String = body.chars().take(200).collect();
        Err(format!("coord returned {status} for POST {url}: {excerpt}"))
    }
}

/// Spawn the periodic heartbeat task on the ambient tokio runtime.
///
/// Interval is read from `COORD_HEARTBEAT_INTERVAL_SECS` (default 30s,
/// floored at 1s). `MissedTickBehavior::Skip` mirrors the watcher in
/// `qontinui-coord/src/health_watcher.rs:99-100` — if a tick is missed
/// (e.g. system suspend), skip catch-up and resume on the next aligned
/// tick. Heartbeat failures `warn!` and retry on the next interval;
/// the loop never panics.
pub fn spawn_heartbeat() {
    let secs: u64 = std::env::var("COORD_HEARTBEAT_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30)
        .max(1);

    info!(
        "fleet::heartbeat: starting periodic heartbeat task, interval={}s",
        secs
    );

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(e) = heartbeat_to_coord().await {
                warn!("fleet::heartbeat: {e}");
            }
        }
    });
}

// =============================================================================
// Primary-tree state publisher
// (plan 2026-05-19-coordinator-production-readiness.md Phase 1)
//
// Periodically walks each qontinui-* repo under `D:/qontinui-root/` (resolved
// from `QONTINUI_ROOT` else the platform default), runs `git status --porcelain`
// + `git rev-parse HEAD` + `git symbolic-ref --short HEAD` + mtime-scans the
// working tree, then POSTs one row per repo to `<base>/coord/trees/upsert`.
//
// Mirrors the `heartbeat_to_coord` + `spawn_heartbeat` pair above:
// - Reuses `load_machine_file` for identity.
// - Reuses `coord_http_base` for the coord HTTP endpoint resolver.
// - Best-effort: errors `warn!` and the next tick retries; the loop never
//   panics.
// - Same `MissedTickBehavior::Skip` posture so a system suspend doesn't
//   blast catch-up ticks.
//
// NOT supervisor-side: the supervisor is dev-only and doesn't run on
// production fleet machines (see PR #179's commit message). The runner
// is the universal vantage point — every fleet host has one.
// =============================================================================

/// `POST /coord/trees/upsert` body (mirror of
/// `qontinui-coord/src/primary_trees.rs::UpsertRequest`).
#[derive(Debug, serde::Serialize)]
struct TreeStatePayload {
    device_id: uuid::Uuid,
    repo: String,
    branch: String,
    head_sha: String,
    dirty: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    dirty_files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_edit_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Maximum number of dirty paths included per row (the column is unbounded
/// but operator triage doesn't benefit from a 10k-row dump). Anything past
/// this is silently truncated; `dirty=true` still flags the tree.
const MAX_DIRTY_FILES_REPORTED: usize = 50;

/// Resolve the root directory the publisher walks for qontinui-* repos.
/// `QONTINUI_ROOT` env override → `D:/qontinui-root` on Windows →
/// `$HOME/qontinui-root` on unix. Returns `None` if neither resolves to
/// a real directory.
fn qontinui_root() -> Option<PathBuf> {
    if let Ok(s) = std::env::var("QONTINUI_ROOT") {
        let p = PathBuf::from(s);
        if p.is_dir() {
            return Some(p);
        }
    }
    #[cfg(target_os = "windows")]
    {
        let p = PathBuf::from("D:/qontinui-root");
        if p.is_dir() {
            return Some(p);
        }
    }
    if let Some(home) = dirs::home_dir() {
        let p = home.join("qontinui-root");
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

/// Capture the state of a single primary git tree at `repo_path`. Returns
/// `None` when the directory isn't a git repo (no `.git/` dir). All
/// `git` calls use `Command::new("git")` so they go through the operator's
/// PATH-resolved git — same as the rest of the runner.
fn capture_tree(repo_path: &std::path::Path) -> Option<TreeStatePayload> {
    use std::process::Command;

    let dot_git = repo_path.join(".git");
    if !dot_git.exists() {
        return None;
    }

    let repo_name = repo_path.file_name()?.to_string_lossy().to_string();

    // HEAD SHA
    let head_sha = Command::new("git")
        .args(["-C", repo_path.to_str()?, "rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })?;

    // Current branch (best-effort — detached HEAD returns empty)
    let branch = Command::new("git")
        .args([
            "-C",
            repo_path.to_str()?,
            "symbolic-ref",
            "--short",
            "HEAD",
        ])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "(detached)".to_string());

    // Dirty status — `git status --porcelain=v1` is one line per change.
    let status_out = Command::new("git")
        .args(["-C", repo_path.to_str()?, "status", "--porcelain=v1"])
        .output()
        .ok()?;
    if !status_out.status.success() {
        return None;
    }
    let status_str = String::from_utf8_lossy(&status_out.stdout);
    let dirty_files: Vec<String> = status_str
        .lines()
        .filter_map(|line| {
            // porcelain v1: XY<space>path  (XY can have spaces in
            // rename forms; we just trim and take whatever comes after
            // the first 3 chars).
            if line.len() < 4 {
                return None;
            }
            Some(line[3..].to_string())
        })
        .take(MAX_DIRTY_FILES_REPORTED)
        .collect();
    let dirty = !dirty_files.is_empty() || !status_str.lines().next().unwrap_or("").is_empty();

    // last_edit_at — newest mtime among the dirty files OR (when clean)
    // the HEAD commit time. The watcher's "stale-WIP" rule is about
    // *uncommitted* idleness, so the dirty-files mtime is the more
    // meaningful signal; a clean tree's commit-time is informational
    // only.
    let last_edit_at: Option<chrono::DateTime<chrono::Utc>> = if dirty {
        dirty_files
            .iter()
            .filter_map(|p| {
                let full = repo_path.join(p);
                let meta = std::fs::metadata(&full).ok()?;
                let modified = meta.modified().ok()?;
                let secs = modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_secs() as i64;
                chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
            })
            .max()
    } else {
        // git -C <path> log -1 --format=%cI  — committer-date ISO-8601.
        Command::new("git")
            .args([
                "-C",
                repo_path.to_str()?,
                "log",
                "-1",
                "--format=%cI",
            ])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .ok()
                        .map(|d| d.with_timezone(&chrono::Utc))
                } else {
                    None
                }
            })
    };

    // device_id is filled in by the caller (it's identity-side, not
    // per-repo). Punch in a placeholder; the publisher overwrites it.
    Some(TreeStatePayload {
        device_id: uuid::Uuid::nil(),
        repo: repo_name,
        branch,
        head_sha,
        dirty,
        dirty_files: if dirty_files.is_empty() {
            None
        } else {
            Some(dirty_files)
        },
        last_edit_at,
    })
}

/// One publish pass: discover qontinui-* dirs under `QONTINUI_ROOT`,
/// capture each one's tree state, POST one row per repo to
/// `<base>/coord/trees/upsert`.
///
/// Best-effort: per-repo errors `warn!` and continue; the function only
/// returns `Err` for terminal conditions (no machine identity, no coord
/// URL, no root dir). Caller (`spawn_tree_publisher`) treats `Err` the
/// same as it treats heartbeat errors — log + retry next tick.
pub async fn publish_tree_state() -> Result<(), String> {
    let machine = match load_machine_file() {
        Some(m) => m,
        None => {
            info!(
                "fleet::tree_publisher: ~/.qontinui/machine.json missing — \
                 run `qontinui_profile machine init` to enable tree-state \
                 publishing. Skipping."
            );
            return Ok(());
        }
    };

    let device_id = match uuid::Uuid::parse_str(&machine.machine_id) {
        Ok(id) => id,
        Err(e) => {
            warn!(
                "fleet::tree_publisher: machine.json machine_id is not a valid UUID ({e}). Skipping."
            );
            return Ok(());
        }
    };

    let base = match coord_http_base() {
        Some(b) => b,
        None => {
            info!(
                "fleet::tree_publisher: ~/.qontinui/profiles.json missing or active \
                 profile has no coord_url — no coord to publish to. Skipping."
            );
            return Ok(());
        }
    };

    let root = match qontinui_root() {
        Some(p) => p,
        None => {
            info!(
                "fleet::tree_publisher: no qontinui-root directory found (set \
                 QONTINUI_ROOT to override). Skipping."
            );
            return Ok(());
        }
    };

    // Walk top-level entries; only `qontinui-*` dirs with a `.git/`
    // count. Skip `.wt/` sibling worktree dirs explicitly — those are
    // agent worktrees, not the primary tree we're publishing.
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(e) => {
            return Err(format!(
                "read_dir {root}: {e}",
                root = root.display()
            ))
        }
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("reqwest builder: {e}"))?;
    let url = format!("{base}/coord/trees/upsert");

    let mut total = 0usize;
    let mut posted = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if !name.starts_with("qontinui-") {
            continue;
        }
        if !path.is_dir() {
            continue;
        }

        let mut payload = match capture_tree(&path) {
            Some(p) => p,
            None => {
                debug!(
                    "fleet::tree_publisher: {} skipped (not a git repo or capture failed)",
                    name
                );
                continue;
            }
        };
        total += 1;
        payload.device_id = device_id;

        match client.post(&url).json(&payload).send().await {
            Ok(resp) if resp.status().is_success() => {
                posted += 1;
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                let excerpt: String = body.chars().take(200).collect();
                warn!(
                    "fleet::tree_publisher: coord returned {status} for {repo}: {excerpt}",
                    repo = payload.repo
                );
            }
            Err(e) => {
                warn!(
                    "fleet::tree_publisher: POST {url} for {repo} failed: {e}",
                    repo = payload.repo
                );
            }
        }
    }

    if total > 0 {
        debug!(
            "fleet::tree_publisher: published {posted}/{total} repos device_id={device_id}"
        );
    }
    Ok(())
}

/// Spawn the periodic tree-state publisher on the ambient tokio runtime.
///
/// Interval read from `COORD_TREE_PUBLISH_INTERVAL_SECS` (default 60s,
/// floored at 5s). `MissedTickBehavior::Skip` mirrors `spawn_heartbeat`
/// above. Failures `warn!` and retry on the next tick; the loop never
/// panics.
pub fn spawn_tree_publisher() {
    let secs: u64 = std::env::var("COORD_TREE_PUBLISH_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60)
        .max(5);

    info!(
        "fleet::tree_publisher: starting periodic tree-state publisher, interval={}s",
        secs
    );

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(e) = publish_tree_state().await {
                warn!("fleet::tree_publisher: {e}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_max_agents_examples() {
        assert_eq!(derive_max_agents(32), 7);
        assert_eq!(derive_max_agents(16), 3);
        assert_eq!(derive_max_agents(8), 1);
        assert_eq!(derive_max_agents(4), 0);
        assert_eq!(derive_max_agents(0), 0);
    }

    #[test]
    fn detect_resources_returns_non_zero_on_dev_host() {
        // Smoke test: we shouldn't be able to run any CI without at
        // least 1 core, 1 GiB RAM, and 1 GiB disk. If this fails the
        // sysinfo / available_parallelism path is broken.
        let r = detect_resources();
        assert!(
            r.cpu_cores >= 1,
            "expected ≥1 cpu_core, got {}",
            r.cpu_cores
        );
        assert!(r.memory_gb >= 1, "expected ≥1 GiB RAM, got {}", r.memory_gb);
        assert!(
            r.disk_total_gb >= 1,
            "expected ≥1 GiB disk, got {}",
            r.disk_total_gb
        );
    }
}
