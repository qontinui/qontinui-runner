//! Fleet topology publisher (Row 2 Phase 1, runner side).
//!
//! See `plans/2026-05-14-fleet-topology-and-build-pool-design.md` §3.2.
//! On every runner boot, this module:
//!
//! 1. Reads the local device identity from `~/.qontinui/machine.json`
//!    (already minted by `qontinui_profile device init`).
//! 2. Detects local resources (cpu_cores, memory_gb, disk_total_gb)
//!    via `sysinfo`.
//! 3. Derives the agent-side budget per §3.2:
//!    `max_concurrent_agents = floor((memory_gb - 4) / 4)`.
//! 4. POSTs role + budget columns to `POST /coord/devices/{device_id}/budget`
//!    via coord HTTP — Phase 3 (Unified Devices Registry) replaces the
//!    direct-PG UPSERT path so the runner no longer needs PG credentials
//!    to coord's database.
//!
//! ## Runner-bootable-when-coord-down property
//!
//! The original direct-PG path was chosen specifically to keep the
//! runner bootable when qontinui-coord HTTP is down. We preserve that
//! property in the HTTP variant via:
//!
//! - **Exponential backoff retry** (2s → 60s, capped at 60s, ~6 attempts
//!   then we give up for this boot cycle and return Ok(()).
//! - **Last-budget cache** at `~/.qontinui/last_budget.json` so an operator
//!   can inspect the most-recent payload even when coord is unreachable.
//! - **Best-effort semantics**: terminal failures log a warning and
//!   return Ok(()), so `publish_on_startup` never blocks the runner.
//!
//! Phase 1 is visibility-only — the row appears in `GET /coord/fleet`
//! but the coord doesn't enforce caps yet (Phase 5). Failures here log
//! a warning and are swallowed; the runner still boots.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
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
/// `bin/qontinui_profile.rs::DeviceFile` so we don't need to expose
/// it from the binary crate.
///
/// `device_id` is serde-aliased to `machine_id` so a pre-Phase-3
/// machine.json (which used the old field name) still deserializes
/// without manual migration.
#[derive(Debug, Clone, Deserialize)]
struct DeviceFile {
    #[serde(alias = "machine_id")]
    device_id: String,
    hostname: String,
}

fn device_file_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("machine.json"))
}

fn load_device_file() -> Option<DeviceFile> {
    let path = device_file_path()?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Cache file for the last successful budget payload. Inspectable when
/// coord is unreachable; lets operators verify "what was advertised" from
/// the runner side without coord access. Path: `~/.qontinui/last_budget.json`.
fn last_budget_cache_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("last_budget.json"))
}

/// Wire shape of `POST /coord/devices/{device_id}/budget`.
///
/// Mirrors `qontinui_profile::register_with_coord`; both go through coord
/// HTTP, replacing the prior split between direct-PG fleet writes and
/// HTTP identity registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceBudgetRequest {
    hostname: String,
    role: String,
    cpu_cores: i32,
    memory_gb: i32,
    disk_total_gb: i64,
    disk_reserved_gb: i64,
    max_concurrent_agents: i32,
    max_concurrent_builds: i32,
}

/// Resolve the coord HTTP base from `~/.qontinui/profiles.json` active
/// profile's `coord_url`. Mirrors the CLI helper of the same name.
fn coord_http_base() -> Option<String> {
    let coord_url = qontinui_runner_lib::profiles::load_strict()
        .ok()?
        .coord_url?;
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

/// POST the budget payload with exponential backoff (2s, 4s, 8s, 16s, 32s, 60s).
/// Returns Ok on first success; returns Err with the last error if every
/// attempt fails. The caller decides whether to surface or swallow.
///
/// Per the runner-bootable-when-coord-down property, this is the ONLY
/// place we busy-wait on coord HTTP; the caller wraps this in
/// `publish_budget` which logs+swallows on failure.
async fn post_budget_with_retry(
    coord_base: &str,
    device_id: &str,
    body: &DeviceBudgetRequest,
) -> Result<(), String> {
    let url = format!("{}/coord/devices/{}/budget", coord_base, device_id);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("reqwest client build: {e}"))?;
    let mut last_err = String::new();
    let backoff_ms: [u64; 6] = [2_000, 4_000, 8_000, 16_000, 32_000, 60_000];
    for (attempt, delay_ms) in backoff_ms.iter().enumerate() {
        match client.post(&url).json(body).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(());
                }
                let body_text = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "<unable to read response body>".to_string());
                last_err = format!("POST {url} -> HTTP {status}: {body_text}");
            }
            Err(e) => {
                last_err = format!("POST {url} failed: {e}");
            }
        }
        // Don't sleep after the final attempt.
        if attempt + 1 < backoff_ms.len() {
            warn!(
                "fleet::publish_budget: attempt {} failed ({}); retrying in {}s",
                attempt + 1,
                last_err,
                delay_ms / 1000
            );
            tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
        }
    }
    Err(last_err)
}

fn write_last_budget_cache(body: &DeviceBudgetRequest, device_id: &str) {
    let Some(path) = last_budget_cache_path() else {
        return;
    };
    let payload = serde_json::json!({
        "device_id": device_id,
        "captured_at": chrono::Utc::now().to_rfc3339(),
        "payload": body,
    });
    let pretty = match serde_json::to_vec_pretty(&payload) {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!("fleet::publish_budget: cache serialize failed: {e}");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::debug!("fleet::publish_budget: cache mkdir failed: {e}");
            return;
        }
    }
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, &pretty) {
        tracing::debug!("fleet::publish_budget: cache write failed: {e}");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        tracing::debug!("fleet::publish_budget: cache rename failed: {e}");
    }
}

/// Publish role + budget to `coord.devices` via `POST
/// /coord/devices/{device_id}/budget`. Best-effort: failures log a
/// warning and return Ok(()) so they don't break startup.
///
/// `disk_reserved_gb` defaults to 0 in Phase 1 — Phase 5 of the
/// fleet plan will add per-device overrides for system + non-fleet
/// reservation. Callers can pass a non-zero value if their config
/// already knows the reservation.
///
/// Phase 3 (Unified Devices Registry): this replaces the prior direct-PG
/// UPSERT on `coord.machines` with a coord HTTP call. The exponential-backoff
/// retry + last-budget cache (see module docs) preserve the
/// runner-bootable-when-coord-down property of the old direct-PG path.
///
/// The `_pg` parameter is retained for now — call sites pass the connection
/// pool from `main.rs::run_app()`. Once coord HTTP is universally available
/// the parameter can be removed in a follow-up cleanup PR.
pub async fn publish_budget(
    _pg: &Arc<PgDb>,
    role: MachineRole,
    resources: Resources,
    disk_reserved_gb: u64,
) -> Result<(), String> {
    let device = match load_device_file() {
        Some(d) => d,
        None => {
            warn!(
                "fleet::publish_budget: ~/.qontinui/machine.json missing — \
                 run `qontinui_profile device init` to register identity. Skipping budget publish."
            );
            return Ok(());
        }
    };

    let device_id_uuid = match uuid::Uuid::parse_str(&device.device_id) {
        Ok(id) => id,
        Err(e) => {
            warn!(
                "fleet::publish_budget: machine.json device_id is not a valid UUID ({e}). Skipping."
            );
            return Ok(());
        }
    };
    let device_id_str = device_id_uuid.to_string();

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
    let role_str = role.as_str().to_string();

    let body = DeviceBudgetRequest {
        hostname: device.hostname.clone(),
        role: role_str.clone(),
        cpu_cores: cpu_cores_i,
        memory_gb: memory_gb_i,
        disk_total_gb: disk_total_i,
        disk_reserved_gb: disk_reserved_i,
        max_concurrent_agents,
        max_concurrent_builds,
    };

    // Cache the payload regardless of whether the POST succeeds — this is
    // the operator's lifeline when coord is unreachable.
    write_last_budget_cache(&body, &device_id_str);

    let coord_base = match coord_http_base() {
        Some(b) => b,
        None => {
            warn!(
                "fleet::publish_budget: active profile has no coord_url; skipping budget publish (cache written)"
            );
            return Ok(());
        }
    };

    match post_budget_with_retry(&coord_base, &device_id_str, &body).await {
        Ok(()) => {
            info!(
                "fleet::publish_budget: device_id={device_id_str} hostname={} role={role_str} \
                 cpu_cores={cpu_cores_i} memory_gb={memory_gb_i} disk_total_gb={disk_total_i} \
                 max_concurrent_agents={max_concurrent_agents}",
                device.hostname
            );
            Ok(())
        }
        Err(e) => {
            warn!(
                "fleet::publish_budget: terminal failure after retries ({e}); \
                 runner continues, payload cached at ~/.qontinui/last_budget.json"
            );
            // Return Ok so callers don't treat HTTP unreachability as fatal.
            // The boot-when-coord-down property is the load-bearing contract.
            Ok(())
        }
    }
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
// The HTTP `publish_budget` path above is a one-shot at boot. To keep
// `coord.devices.last_seen_at` fresh under coord's new push-aware liveness
// model, the runner periodically POSTs `{device_id, hostname}` to coord's
// `/coord/devices/register` endpoint. The handler's UPSERT refreshes
// `last_seen_at` and `COALESCE`s a previously-advertised `health_url`, so
// heartbeating from the runner side is a clean, side-effect-free refresh.
//
// The HTTP heartbeat path is intentionally additive: it tolerates missing
// identity, missing profile, or network failure with `info!`/`warn!` and a
// retry on the next tick.
//
// Phase 3 (Unified Devices Registry) renames: the heartbeat now uses the
// `DeviceFile` struct + `load_device_file` helper from §3.2 above, and the
// URL flipped from `/coord/machine/register` to `/coord/devices/register`
// to match `register_with_coord` in `bin/qontinui_profile.rs`.
// =============================================================================

#[derive(Debug, serde::Serialize)]
struct HeartbeatPayload {
    device_id: uuid::Uuid,
    hostname: String,
}

/// POST `{device_id, hostname}` to `<base>/coord/devices/register`.
///
/// `health_url` is deliberately omitted — coord's `register_device`
/// handler `COALESCE`s `EXCLUDED.health_url` with the existing value,
/// so omitting from the heartbeat preserves any URL the device
/// previously advertised. Failures are reported as `Err(String)` so
/// the caller can log them; the loop never panics.
pub async fn heartbeat_to_coord() -> Result<(), String> {
    let device = match load_device_file() {
        Some(d) => d,
        None => {
            info!(
                "fleet::heartbeat: ~/.qontinui/machine.json missing — \
                 run `qontinui_profile device init` to enable fleet visibility. Skipping."
            );
            return Ok(());
        }
    };

    let device_id = match uuid::Uuid::parse_str(&device.device_id) {
        Ok(id) => id,
        Err(e) => {
            warn!("fleet::heartbeat: machine.json device_id is not a valid UUID ({e}). Skipping.");
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
        device_id,
        hostname: device.hostname.clone(),
    };
    let url = format!("{base}/coord/devices/register");

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
            "fleet::heartbeat: ok device_id={device_id} hostname={} status={}",
            device.hostname, status
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
// - Reuses `load_device_file` for identity.
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
    /// Commits HEAD is behind `origin/<branch>` (or `origin/main` if
    /// HEAD is detached). Reflects last-fetched remote state — no
    /// network fetch is performed. `None` if neither remote ref
    /// resolves locally.
    #[serde(skip_serializing_if = "Option::is_none")]
    behind_count: Option<i32>,
    /// True when HEAD is detached (not pointing at a named branch).
    /// `None` if status couldn't be determined.
    #[serde(skip_serializing_if = "Option::is_none")]
    head_detached: Option<bool>,
    /// `git ls-files --others --exclude-standard` count — files not
    /// tracked and not gitignored. The orphan-untracked-file signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    untracked_count: Option<i32>,
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
    let symbolic_ref = Command::new("git")
        .args(["-C", repo_path.to_str()?, "symbolic-ref", "--short", "HEAD"])
        .output()
        .ok();
    let head_detached = symbolic_ref
        .as_ref()
        .map(|o| !o.status.success())
        .or(Some(false));
    let branch = symbolic_ref
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
            .args(["-C", repo_path.to_str()?, "log", "-1", "--format=%cI"])
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

    // behind_count: commits HEAD is behind `origin/<branch>` (or
    // `origin/main` when detached/no branch). Uses last-fetched remote
    // state — no `git fetch` is performed here (publisher runs every
    // 60s; an implicit fetch every cycle would be too aggressive).
    // The operator's normal `git fetch` / `pull-all` cadence keeps the
    // remote refs current.
    let remote_ref = if head_detached.unwrap_or(false) || branch == "(detached)" {
        "origin/main".to_string()
    } else {
        format!("origin/{branch}")
    };
    let behind_count: Option<i32> = Command::new("git")
        .args([
            "-C",
            repo_path.to_str()?,
            "rev-list",
            "--count",
            &format!("HEAD..{remote_ref}"),
        ])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse::<i32>()
                    .ok()
            } else {
                None
            }
        });

    // untracked_count: `git ls-files --others --exclude-standard` — one
    // line per untracked file. The orphan-untracked-file signal that
    // catches sub-agent worktree builds spilling scratch into the
    // primary tree.
    let untracked_count: Option<i32> = Command::new("git")
        .args([
            "-C",
            repo_path.to_str()?,
            "ls-files",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                let s = String::from_utf8_lossy(&o.stdout);
                let n: i32 = s.lines().filter(|l| !l.is_empty()).count() as i32;
                Some(n)
            } else {
                None
            }
        });

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
        behind_count,
        head_detached,
        untracked_count,
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
    // Phase 3 unified-devices rename: the on-disk file is still
    // `~/.qontinui/machine.json` (legacy filename, kept for tooling
    // back-compat) but the in-memory `DeviceFile` struct now exposes
    // `device_id` (serde-aliased from `machine_id` per :131-133). Fix-
    // forwards a Phase 1 fix-forward needed on origin/main per the
    // `feedback_check_main_red_before_blaming_pr` discovery (PR #188 +
    // PR #49 landed slightly out of phase; this 1-line rename unblocks
    // runner CI).
    let device = match load_device_file() {
        Some(d) => d,
        None => {
            info!(
                "fleet::tree_publisher: ~/.qontinui/machine.json missing — \
                 run `qontinui_profile device init` to enable tree-state \
                 publishing. Skipping."
            );
            return Ok(());
        }
    };

    let device_id = match uuid::Uuid::parse_str(&device.device_id) {
        Ok(id) => id,
        Err(e) => {
            warn!(
                "fleet::tree_publisher: machine.json device_id is not a valid UUID ({e}). Skipping."
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
        Err(e) => return Err(format!("read_dir {root}: {e}", root = root.display())),
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
        debug!("fleet::tree_publisher: published {posted}/{total} repos device_id={device_id}");
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
