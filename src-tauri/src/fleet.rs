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

/// Resolve the device's `tenant_id` for coord-side requests
/// (`POST /coord/devices/register`). Returns the first hit:
///
/// 1. `paired_user.json::tenant_id` — present on newly-paired devices
///    (written by `pair::persist_pairing` since 2026-05-22).
/// 2. JWT-claim fallback — decode `tenant_id` from the cached
///    device-token JWT via
///    [`qontinui_runner_lib::pair::tenant_id_from_oauth_claim`].
///    On success, opportunistically rewrites `paired_user.json` with
///    the resolved value so subsequent heartbeats hit branch 1.
///    Best-effort: rewrite IO errors are swallowed.
/// 3. `None` — neither source has a usable tenant_id. Callers must
///    skip the request (coord rejects with `400 tenant_id_required`).
fn resolve_tenant_id() -> Option<uuid::Uuid> {
    // Branch 1 — paired_user.json
    if let Some(s) = qontinui_runner_lib::pair::read_paired_tenant_id_from_disk() {
        if let Ok(t) = uuid::Uuid::parse_str(s.trim()) {
            return Some(t);
        }
    }

    // Branch 2 — cached device-token JWT
    let token = crate::auth::AuthManager::new()
        .get_access_token()
        .ok()
        .unwrap_or_default();
    if token.is_empty() {
        return None;
    }
    let claim = qontinui_runner_lib::pair::tenant_id_from_oauth_claim(&token)?;
    let parsed = uuid::Uuid::parse_str(claim.trim()).ok()?;

    // Opportunistic backfill — best-effort, ignore IO errors.
    if let Err(e) = qontinui_runner_lib::pair::backfill_paired_tenant_id(&parsed) {
        tracing::debug!("fleet::resolve_tenant_id: backfill non-fatal: {e}");
    }

    Some(parsed)
}

/// Parse the authoritative `tenant_id` out of a successful
/// `POST /coord/devices/register` response body (the serialized
/// `DeviceStateRow`). Returns `None` when the body isn't JSON, has no
/// `tenant_id`, or it isn't a valid UUID — all of which the caller
/// treats as "nothing to heal this tick". Pure (no IO) so the heal's
/// extraction is unit-testable without an HTTP mock.
fn response_tenant_id(body: &str) -> Option<uuid::Uuid> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .as_ref()
        .and_then(|j| j.get("tenant_id"))
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok())
}

/// Dedupe the "tenant_id unresolvable" startup warning — the heartbeat
/// ticks every 30s so logging on every miss would flood the journal.
/// Fires exactly once per process lifetime; subsequent skips are silent
/// (the operator already saw the recovery hint).
static TENANT_ID_UNRESOLVABLE_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn warn_tenant_id_unresolvable_once() {
    if !TENANT_ID_UNRESOLVABLE_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        warn!(
            "fleet::heartbeat: tenant_id unresolvable (no paired_user.json::tenant_id, \
             no claim in cached device-token JWT); skipping all heartbeats until next runner \
             restart. Re-run `qontinui_profile device pair --tenant-id <uuid>` to recover."
        );
    }
}

/// Dedupe the "coord rejected our tenant_id as unknown" heartbeat warning.
/// coord returns HTTP 400 `{"error":"unknown_tenant", ...}` when our
/// tenant_id is not present in `coord.tenants`; the heartbeat ticks every 30s
/// so a stale `paired_user.json` would otherwise flood the journal forever.
/// Fires exactly once per process lifetime.
static UNKNOWN_TENANT_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Atomic gate behind [`warn_unknown_tenant_once`]: returns `true` exactly
/// once per process (the first caller), `false` thereafter. Factored out so
/// the once-per-process semantics are unit-testable without asserting on the
/// `warn!` side effect.
fn should_warn_unknown_tenant() -> bool {
    !UNKNOWN_TENANT_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
}

fn warn_unknown_tenant_once() {
    if should_warn_unknown_tenant() {
        warn!(
            "fleet::heartbeat: coord rejected this device's tenant_id as unknown \
             (not present in coord.tenants); heartbeats will keep being rejected \
             until paired_user.json carries a valid tenant_id. Re-run \
             `qontinui_profile device pair --tenant-id <uuid>` to recover. \
             Logging once per process."
        );
    }
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

/// Render an error with its full `source()` chain. `Display` on
/// `reqwest::Error` alone collapses connect/DNS/TLS detail into the generic
/// "error sending request for url (…)" — which made the 2026-06-03
/// fleet-heartbeat outage undiagnosable from the WARN logs (every failed
/// tick printed the same opaque line while the root cause stayed hidden in
/// the source chain). Walk the chain so failure WARNs carry the root cause
/// (e.g. "dns error", "os error 10060", schannel detail).
fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(cause) = src {
        s.push_str(": ");
        s.push_str(&cause.to_string());
        src = cause.source();
    }
    s
}

/// Resolve the coord HTTP base. Source-of-truth chain: env `COORD_HTTP_URL`
/// → `~/.qontinui/profiles.json` active profile's `coord_url` (ws→http).
///
/// Honors `COORD_HTTP_URL` to match `mcp::agent_worktrees::coord_http_base`,
/// `commands::claims`, and `commands::productivity`, so per-machine
/// staging-pointing of the heartbeat no longer requires a profiles.json edit.
/// Unlike those resolvers this deliberately returns `None` (rather than
/// defaulting to `http://localhost:9870`) when nothing is configured, so the
/// heartbeat cleanly skips instead of spamming connection errors every tick.
fn coord_http_base() -> Option<String> {
    if let Ok(v) = std::env::var("COORD_HTTP_URL") {
        if !v.is_empty() {
            return Some(v);
        }
    }
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
                last_err = format!("POST {url} failed: {}", error_chain(&e));
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
    /// PR Merge Orchestrator Phase 8 D8.0 — whether `claude --version`
    /// resolves on this device's PATH. Coord's `tenant_has_audit_capable_device`
    /// helper joins on `coord.devices.claude_code_available = true` AND
    /// `last_seen_at > now() - 5m`; the auditor + merge-specialist spawn
    /// path refuses to route work to a device whose latest heartbeat
    /// reported `false`. Cached for 60s per `claude_code_probe()` to keep
    /// the heartbeat's hot path cheap.
    #[serde(skip_serializing_if = "is_false")]
    claude_code_available: bool,
    /// REQUIRED by coord's `post_device_register` handler — absence
    /// produces `400 tenant_id_required` (see qontinui-coord
    /// `routes_phase3.rs:257-269`). Resolved via [`resolve_tenant_id`]
    /// before the payload is constructed; if `None` there, the
    /// heartbeat is skipped rather than 400-spamming coord.
    /// Phase 2 of the default-tenant-propagation plan.
    tenant_id: uuid::Uuid,
}

/// Suppress serializing the field when it's the default. Keeps the
/// heartbeat wire small for the historical "no claude installed" case
/// while still flipping `coord.devices.claude_code_available` to `true`
/// the moment the probe starts passing.
fn is_false(b: &bool) -> bool {
    !*b
}

/// POST `{device_id, hostname, claude_code_available}` to
/// `<base>/coord/devices/register`.
///
/// `health_url` is deliberately omitted — coord's `register_device`
/// handler `COALESCE`s `EXCLUDED.health_url` with the existing value,
/// so omitting from the heartbeat preserves any URL the device
/// previously advertised.
///
/// `claude_code_available` (PR Merge Orchestrator Phase 8 D8.0) is the
/// straight-write field that gates the auditor spawn. Detected via
/// [`claude_code_probe`] (cached 60s). Failures are reported as
/// `Err(String)` so the caller can log them; the loop never panics.
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

    let tenant_id = match resolve_tenant_id() {
        Some(t) => t,
        None => {
            warn_tenant_id_unresolvable_once();
            return Ok(());
        }
    };

    let claude_code_available = claude_code_probe();

    let payload = HeartbeatPayload {
        device_id,
        hostname: device.hostname.clone(),
        claude_code_available,
        tenant_id,
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
        .map_err(|e| format!("POST {url}: {}", error_chain(&e)))?;

    let status = resp.status();
    if status.is_success() {
        // 30s cadence — `info!` would be noisy, debug! keeps the
        // happy path quiet while still discoverable.
        debug!(
            "fleet::heartbeat: ok device_id={device_id} hostname={} status={}",
            device.hostname, status
        );
        // Soft-heal write-back. Coord's response carries the authoritative
        // (possibly soft-healed) `tenant_id` for this device — if our cached
        // value was a stale orphan, coord rescued the call using its stored
        // tenant and returned THAT here. Persist it to paired_user.json so
        // the next tick's `resolve_tenant_id` branch 1 sends the corrected
        // value and the loop converges (coord stops soft-healing). The
        // helper is idempotent (`pair::backfill_paired_tenant_id` no-ops
        // when unchanged), so steady state is one write then silence.
        // Best-effort: a parse/IO miss just retries the heal next tick.
        let body = resp.text().await.unwrap_or_default();
        if let Some(resp_tenant) = response_tenant_id(&body) {
            if let Err(e) = qontinui_runner_lib::pair::backfill_paired_tenant_id(&resp_tenant) {
                tracing::debug!("fleet::heartbeat: tenant_id write-back non-fatal: {e}");
            }
        }
        Ok(())
    } else {
        let body = resp.text().await.unwrap_or_default();
        // Phase 2 of the unknown-tenant plan: coord returns HTTP 400
        // `{"error":"unknown_tenant","tenant_id":"<uuid>", ...}` when our
        // tenant_id is not present in coord.tenants. A stale paired_user.json
        // would 400 here forever (30s cadence) — warn once, then keep
        // returning Err so the caller logs+swallows (same posture as the
        // tenant_id-unresolvable skip; we never exit the runner).
        if status.as_u16() == 400 {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                if json.get("error").and_then(|v| v.as_str()) == Some("unknown_tenant") {
                    warn_unknown_tenant_once();
                    let body_tenant = json
                        .get("tenant_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("<unknown>");
                    return Err(format!(
                        "coord rejected heartbeat: unknown_tenant (tenant_id={body_tenant}); \
                         see warn-once log"
                    ));
                }
            }
        }
        let excerpt: String = body.chars().take(200).collect();
        Err(format!("coord returned {status} for POST {url}: {excerpt}"))
    }
}

// =============================================================================
// PR Merge Orchestrator Phase 8 D8.0 — Claude Code availability probe.
//
// The auditor + merge-specialist spawn paths require an audit-capable device
// (paired + `claude --version` resolves). The runner self-probes and reports
// the result on every heartbeat tick (`coord.devices.claude_code_available`).
//
// Probe is cached in-process for 60s — `claude --version` is cheap (<100ms
// typical) but spawning a subprocess every 30s heartbeat is wasteful and
// adds spurious load on the host. Cache invalidates after 60s, so a fresh
// install / uninstall / PATH change is reflected within at most 1.5
// heartbeat intervals.
// =============================================================================

use std::sync::Mutex;

/// Cached result of the most recent `claude --version` probe + when it
/// was taken. `Mutex` serializes the (cheap, infrequent) re-probe call.
static CLAUDE_PROBE_CACHE: Mutex<Option<(bool, std::time::Instant)>> = Mutex::new(None);

/// Cache TTL — 60s. Picked to be slightly larger than the default
/// heartbeat interval (30s), so two consecutive heartbeats reuse a
/// single probe result. Override via `COORD_CLAUDE_PROBE_TTL_SECS` for
/// tests.
fn claude_probe_ttl() -> Duration {
    let secs: u64 = std::env::var("COORD_CLAUDE_PROBE_TTL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    Duration::from_secs(secs)
}

/// Detect whether `claude --version` resolves on PATH. Returns `true`
/// iff `claude` is invokable AND exits 0 within a 3s budget.
///
/// Cached per [`CLAUDE_PROBE_CACHE`] for [`claude_probe_ttl`]. The
/// probe is `Command::new("claude").arg("--version")` with stdout +
/// stderr captured (so a Claude Code that prints to stderr still counts
/// as available). A child that times out, panics, or never spawns
/// counts as unavailable.
pub fn claude_code_probe() -> bool {
    {
        // Fast-path: cache hit.
        let guard = CLAUDE_PROBE_CACHE.lock();
        if let Ok(g) = guard {
            if let Some((cached, taken)) = *g {
                if taken.elapsed() < claude_probe_ttl() {
                    return cached;
                }
            }
        }
    }
    let detected = detect_claude_code_now();
    if let Ok(mut g) = CLAUDE_PROBE_CACHE.lock() {
        *g = Some((detected, std::time::Instant::now()));
    }
    detected
}

/// Uncached one-shot probe. Pure side-effect (spawns + waits on a
/// subprocess); should never be called from a hot path directly.
fn detect_claude_code_now() -> bool {
    use std::process::Command;
    use std::time::Instant;

    let started = Instant::now();
    let child = Command::new("claude")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            debug!("claude_code_probe: spawn failed ({e}) — treating as unavailable");
            return false;
        }
    };
    // Bounded wait: poll at 50ms intervals up to 3s. Avoids dragging the
    // heartbeat tick when a `claude` binary is wedged.
    let deadline = started + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let ok = status.success();
                debug!(
                    "claude_code_probe: claude --version exited status={:?} ok={ok} in {}ms",
                    status.code(),
                    started.elapsed().as_millis()
                );
                return ok;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    warn!(
                        "claude_code_probe: claude --version exceeded 3s budget — treating as unavailable"
                    );
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                warn!("claude_code_probe: try_wait failed ({e}) — treating as unavailable");
                return false;
            }
        }
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
        // Failure-streak counter: each WARN carries the running count (so
        // log timestamps reveal whether ticks are actually firing every
        // `secs`), and recovery logs once at info — the success path is
        // otherwise debug-quiet, which hid the 2026-06-03 outage's
        // intermittency from the default log level.
        let mut consecutive_failures: u32 = 0;
        loop {
            tick.tick().await;
            match heartbeat_to_coord().await {
                Err(e) => {
                    consecutive_failures += 1;
                    warn!("fleet::heartbeat: {e} (consecutive_failures={consecutive_failures})");
                }
                Ok(()) if consecutive_failures > 0 => {
                    info!(
                        "fleet::heartbeat: recovered after {consecutive_failures} failed tick(s)"
                    );
                    consecutive_failures = 0;
                }
                Ok(()) => {}
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
    /// Commits the LOCAL default branch (`main`) is ahead of
    /// `origin/<default>` — unpushed local commits on default
    /// (`git rev-list --count origin/<default>..<default>`). Computed
    /// regardless of which branch is currently checked out (it's a
    /// property of the local default ref, not HEAD). The coord-side
    /// `repo_pull` verdict keys on this: a clean default-branch tree
    /// with `local_ahead > 0` is DIVERGED and must escalate rather than
    /// auto-ff. `None` when `origin/<default>` or the local `<default>`
    /// ref doesn't resolve (coord persists the column's `DEFAULT 0`).
    #[serde(skip_serializing_if = "Option::is_none")]
    local_ahead: Option<i32>,
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

    // local_ahead: commits the LOCAL default branch is ahead of
    // `origin/<default>` — unpushed local commits on default. Computed
    // against the default ref regardless of which branch is checked out
    // (the coord `repo_pull` verdict needs the default's divergence even
    // when the operator is sitting on a feature branch). Resolve the
    // default branch from `origin/HEAD` (e.g. `origin/main` -> `main`),
    // falling back to `main`. No network fetch — uses last-fetched refs,
    // same posture as `behind_count` above.
    let default_branch = Command::new("git")
        .args([
            "-C",
            repo_path.to_str()?,
            "symbolic-ref",
            "--short",
            "refs/remotes/origin/HEAD",
        ])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                // `origin/main` -> `main`
                s.strip_prefix("origin/").map(|b| b.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "main".to_string());
    let local_ahead: Option<i32> = Command::new("git")
        .args([
            "-C",
            repo_path.to_str()?,
            "rev-list",
            "--count",
            &format!("origin/{default_branch}..{default_branch}"),
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
                // Either ref unresolved (fresh clone, no local default
                // ref, detached genesis) — report nothing; coord keeps
                // the column's DEFAULT 0.
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
        local_ahead,
    })
}

// =============================================================================
// repo_pull executor (Coordination-Layer Pull Decision plan §5)
//
// After publishing a repo's tree state, for repos that are behind origin this
// requests coord's `repo_pull` verdict (POST /coord/trees/pull-decision,
// device-scoped) and applies the SAFE action with a re-check at apply time:
//   - Pull (timing=Now): `git pull --ff-only origin <default>` — only after
//     re-verifying the tree is still on the default branch AND clean (multi-
//     agent paranoia, /pull-scoped rule 5). ff failure → record, never force.
//   - DefaultRefSync: `git fetch origin <default>:<default>` — a pure local-ref
//     fast-forward (git refuses a non-ff ref update, so an unpushed local
//     default is never clobbered); the working tree is untouched.
//   - Pull (timing=Defer) / Hold / UpToDate: no-op this tick.
//   - Escalate (Diverged / malformed): no-op; coord put it in the operator inbox.
//
// The executor NEVER auto-stashes, `reset --hard`, `--force`, rebases, or
// touches a feature branch's working tree — the /pull-scoped hard rules are
// invariants enforced here even if a (buggy/spoofed) Decision asked otherwise.
//
// GATED OFF by default: set `COORD_PULL_EXECUTOR_ENABLED=1` to opt in. The
// decision REQUEST is harmless, but the apply mutates the working tree, so an
// existing runner stays inert until the operator flips the flag (the standing
// autonomous-pull authorization is per-operator — no-surprise default).
// =============================================================================

/// Is the auto-pull executor opted in? Off unless `COORD_PULL_EXECUTOR_ENABLED`
/// is a truthy value (`1`/`true`/`yes`, case-insensitive).
fn pull_executor_enabled() -> bool {
    std::env::var("COORD_PULL_EXECUTOR_ENABLED")
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

/// Resolve a repo's default branch from `origin/HEAD` (`origin/main` -> `main`),
/// falling back to `main`. No network — uses the last-fetched symbolic ref.
fn resolve_default_branch(repo_path: &std::path::Path) -> String {
    use std::process::Command;
    Command::new("git")
        .args([
            "-C",
            repo_path.to_str().unwrap_or("."),
            "symbolic-ref",
            "--short",
            "refs/remotes/origin/HEAD",
        ])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                s.strip_prefix("origin/").map(|b| b.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "main".to_string())
}

/// The outcome the executor reports back to coord's flywheel + logs.
struct PullOutcome {
    chosen_option: String,
    reasoning: String,
    /// `Some` git op to also append to the fleet feed (`git_ops.record`).
    git_op: Option<(String, Option<String>)>, // (op_kind, message)
}

/// Apply one safe verdict to one repo's working tree. Blocking git via the
/// caller's `spawn_blocking`. Returns the outcome to record. NEVER performs an
/// unsafe op regardless of the verdict (defense in depth, plan §5).
fn apply_pull_verdict_blocking(
    repo_path: &std::path::Path,
    verdict_kind: &str,
    timing_now: bool,
    hold_reason: Option<&str>,
) -> PullOutcome {
    use std::process::Command;
    let default_branch = resolve_default_branch(repo_path);
    let repo_str = repo_path.to_str().unwrap_or(".");

    match verdict_kind {
        "up_to_date" => PullOutcome {
            chosen_option: "up_to_date".to_string(),
            reasoning: "coord verdict UpToDate — nothing to pull".to_string(),
            git_op: None,
        },
        "hold" => PullOutcome {
            chosen_option: format!("held_{}", hold_reason.unwrap_or("unknown")),
            reasoning: format!(
                "coord verdict Hold ({}) — not safe to act, surfaced not applied",
                hold_reason.unwrap_or("unknown")
            ),
            git_op: None,
        },
        "default_ref_sync" => {
            // Pure local-default ref fast-forward; never touches the checked-out
            // tree. git refuses a non-ff ref update, so an unpushed local
            // default is safe.
            let out = Command::new("git")
                .args([
                    "-C",
                    repo_str,
                    "fetch",
                    "origin",
                    &format!("{default_branch}:{default_branch}"),
                ])
                .output();
            match out {
                Ok(o) if o.status.success() => PullOutcome {
                    chosen_option: "default_ref_sync".to_string(),
                    reasoning: format!("fast-forwarded local {default_branch} ref to origin"),
                    git_op: Some((
                        "fetch".to_string(),
                        Some(format!("ref-sync {default_branch}")),
                    )),
                },
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr);
                    let excerpt: String = err.trim().chars().take(200).collect();
                    PullOutcome {
                        chosen_option: "ref_sync_failed".to_string(),
                        reasoning: format!(
                            "git fetch origin {default_branch}:{default_branch} failed (likely \
                             local default diverged — not forced): {excerpt}"
                        ),
                        git_op: None,
                    }
                }
                Err(e) => PullOutcome {
                    chosen_option: "ref_sync_failed".to_string(),
                    reasoning: format!("git fetch invocation failed: {e}"),
                    git_op: None,
                },
            }
        }
        "pull" => {
            if !timing_now {
                return PullOutcome {
                    chosen_option: "deferred".to_string(),
                    reasoning: "coord verdict Pull but timing=Defer — re-evaluate next tick"
                        .to_string(),
                    git_op: None,
                };
            }
            // §5 apply-time safety re-check: branch + clean-tree can change
            // between the decision and now. Re-verify we are STILL on the
            // default branch and the tree is STILL clean before pulling.
            let cur_branch = Command::new("git")
                .args(["-C", repo_str, "symbolic-ref", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
            let on_default = cur_branch.as_deref() == Some(default_branch.as_str());
            let clean = Command::new("git")
                .args(["-C", repo_str, "status", "--porcelain=v1"])
                .output()
                .ok()
                .map(|o| o.status.success() && o.stdout.is_empty())
                .unwrap_or(false);
            if !on_default || !clean {
                return PullOutcome {
                    chosen_option: "skipped_recheck".to_string(),
                    reasoning: format!(
                        "apply-time re-check failed (on_default={on_default}, clean={clean}) — \
                         tree changed since the verdict; not pulling"
                    ),
                    git_op: None,
                };
            }
            // ff-only pull — the ONLY tree-mutating op the executor performs.
            let out = Command::new("git")
                .args([
                    "-C",
                    repo_str,
                    "pull",
                    "--ff-only",
                    "origin",
                    &default_branch,
                ])
                .output();
            match out {
                Ok(o) if o.status.success() => {
                    let new_sha = Command::new("git")
                        .args(["-C", repo_str, "rev-parse", "HEAD"])
                        .output()
                        .ok()
                        .filter(|x| x.status.success())
                        .map(|x| String::from_utf8_lossy(&x.stdout).trim().to_string());
                    let sha_suffix = new_sha
                        .as_ref()
                        .map(|s| format!(" (HEAD={})", &s[..s.len().min(12)]))
                        .unwrap_or_default();
                    PullOutcome {
                        chosen_option: "pulled".to_string(),
                        reasoning: format!(
                            "ff-only pull of origin/{default_branch} succeeded{sha_suffix}"
                        ),
                        git_op: Some((
                            "pull".to_string(),
                            Some(format!("ff-only origin/{default_branch}")),
                        )),
                    }
                }
                Ok(o) => {
                    // ff failed — someone pushed a local commit since the verdict
                    // (now diverged). Record Diverged; NEVER force.
                    let err = String::from_utf8_lossy(&o.stderr);
                    let excerpt: String = err.trim().chars().take(200).collect();
                    PullOutcome {
                        chosen_option: "ff_failed".to_string(),
                        reasoning: format!(
                            "git pull --ff-only failed (default diverged since verdict — not \
                             forced): {excerpt}"
                        ),
                        git_op: None,
                    }
                }
                Err(e) => PullOutcome {
                    chosen_option: "ff_failed".to_string(),
                    reasoning: format!("git pull invocation failed: {e}"),
                    git_op: None,
                },
            }
        }
        other => PullOutcome {
            chosen_option: "unknown_verdict".to_string(),
            reasoning: format!("unrecognized verdict kind `{other}` — no action"),
            git_op: None,
        },
    }
}

/// Request coord's `repo_pull` verdict for one behind repo and apply the safe
/// action. Best-effort: any error logs `warn!`/`debug!` and returns — the next
/// publish tick retries. `payload` is the just-published tree state.
async fn request_and_apply_pull(
    client: &reqwest::Client,
    base: &str,
    device_id: uuid::Uuid,
    repo_path: std::path::PathBuf,
    payload: &TreeStatePayload,
) {
    // 1. Request the decision (device-scoped — coord resolves tenant from
    //    device_id; the executor's fresh git state rides in `context` so the
    //    verdict can fall back to it if coord's row lags).
    let context = serde_json::json!({
        "repo": payload.repo,
        "branch": payload.branch,
        "behind": payload.behind_count,
        "dirty": payload.dirty,
        "untracked": payload.untracked_count,
        "detached": payload.head_detached,
        "local_ahead": payload.local_ahead,
    });
    let body = serde_json::json!({
        "device_id": device_id,
        "repo": payload.repo,
        "surface": "infra",
        "context": context,
    });
    let url = format!("{base}/coord/trees/pull-decision");
    let resp = match client.post(&url).json(&body).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let status = r.status();
            let txt = r.text().await.unwrap_or_default();
            let excerpt: String = txt.chars().take(200).collect();
            warn!(
                "fleet::pull_executor: decision HTTP {status} for {}: {excerpt}",
                payload.repo
            );
            return;
        }
        Err(e) => {
            warn!(
                "fleet::pull_executor: decision request for {} failed: {e}",
                payload.repo
            );
            return;
        }
    };
    let json: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "fleet::pull_executor: decision body parse for {} failed: {e}",
                payload.repo
            );
            return;
        }
    };

    let resolution_id = json
        .get("resolution_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let kind = json.get("kind").and_then(|v| v.as_str()).unwrap_or("");

    if kind == "escalate" {
        let reason = json
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("(no reason)");
        info!(
            "fleet::pull_executor: {} escalated to operator (not auto-pulled): {reason}",
            payload.repo
        );
        return;
    }
    if kind != "decision" {
        debug!(
            "fleet::pull_executor: {} resolution kind `{kind}` — no action",
            payload.repo
        );
        return;
    }

    // 2. Parse the verdict payload the Decision carries in action.log_message.
    let log_message = json
        .get("action")
        .and_then(|a| a.get("log_message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let verdict_payload: serde_json::Value = match serde_json::from_str(log_message) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "fleet::pull_executor: {} verdict payload parse failed: {e}",
                payload.repo
            );
            return;
        }
    };
    let autonomy = verdict_payload
        .get("autonomy")
        .and_then(|v| v.as_str())
        .unwrap_or("auto_decide");
    let verdict = verdict_payload
        .get("verdict")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let verdict_kind = verdict
        .get("verdict")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let timing_now = verdict
        .get("timing")
        .and_then(|t| t.get("when"))
        .and_then(|v| v.as_str())
        .map(|w| w == "now")
        .unwrap_or(true); // non-Pull verdicts carry no timing → treat as "now"
    let hold_reason = verdict
        .get("reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // 3. Honor the autonomy dial: guidance_only surfaces the recommendation
    //    without mutating the tree.
    if autonomy != "auto_decide" {
        info!(
            "fleet::pull_executor: {} recommendation `{verdict_kind}` (autonomy={autonomy}) — \
             surfacing, not applying",
            payload.repo
        );
        record_pull_outcome(
            client,
            base,
            device_id,
            resolution_id,
            "surfaced",
            &format!("guidance_only: recommended {verdict_kind}, not applied"),
        )
        .await;
        return;
    }

    // 4. Apply the safe verdict (blocking git on a worker thread).
    let rp = repo_path.clone();
    let vk = verdict_kind.clone();
    let hr = hold_reason.clone();
    let outcome = match tokio::task::spawn_blocking(move || {
        apply_pull_verdict_blocking(&rp, &vk, timing_now, hr.as_deref())
    })
    .await
    {
        Ok(o) => o,
        Err(e) => {
            warn!(
                "fleet::pull_executor: {} apply task panicked: {e}",
                payload.repo
            );
            return;
        }
    };

    if outcome.chosen_option == "pulled" || outcome.chosen_option == "default_ref_sync" {
        info!(
            "fleet::pull_executor: {} -> {} ({})",
            payload.repo, outcome.chosen_option, outcome.reasoning
        );
    } else {
        debug!(
            "fleet::pull_executor: {} -> {} ({})",
            payload.repo, outcome.chosen_option, outcome.reasoning
        );
    }

    // 5. Close the flywheel + (best-effort) append the actual op to the fleet feed.
    record_pull_outcome(
        client,
        base,
        device_id,
        resolution_id,
        &outcome.chosen_option,
        &outcome.reasoning,
    )
    .await;
    if let Some((op_kind, message)) = outcome.git_op {
        record_git_op_fleet_feed(&payload.repo, &payload.branch, &op_kind, message).await;
    }
}

/// POST /coord/trees/pull-decision/record — close the Mode-C flywheel.
async fn record_pull_outcome(
    client: &reqwest::Client,
    base: &str,
    device_id: uuid::Uuid,
    resolution_id: Option<String>,
    chosen_option: &str,
    reasoning: &str,
) {
    let Some(resolution_id) = resolution_id else {
        debug!("fleet::pull_executor: no resolution_id to record against");
        return;
    };
    let body = serde_json::json!({
        "device_id": device_id,
        "resolution_id": resolution_id,
        "chosen_option": chosen_option,
        "reasoning": reasoning,
    });
    let url = format!("{base}/coord/trees/pull-decision/record");
    if let Err(e) = client.post(&url).json(&body).send().await {
        debug!("fleet::pull_executor: record outcome failed: {e}");
    }
}

/// Append the actual git op to the fleet feed (`git_ops.record`) — observability
/// distinct from the decision audit. Best-effort; resolves tenant locally.
async fn record_git_op_fleet_feed(
    repo: &str,
    branch: &str,
    op_kind: &str,
    message: Option<String>,
) {
    let Some(tenant_id) = resolve_tenant_id() else {
        debug!("fleet::pull_executor: no tenant_id — skipping git_ops fleet-feed record");
        return;
    };
    let Some(base) = qontinui_runner_lib::observable_bridge::git_ops_client::coord_http_base()
    else {
        return;
    };
    let client = match qontinui_runner_lib::observable_bridge::git_ops_client::build_client() {
        Ok(c) => c,
        Err(_) => return,
    };
    let req = qontinui_types::git_ops::RecordGitOpRequest {
        repo: repo.to_string(),
        branch: Some(branch.to_string()),
        op_kind: op_kind.to_string(),
        sha: None,
        message,
        metadata: Some(serde_json::json!({"source": "repo_pull_executor"})),
    };
    if let Err(e) = qontinui_runner_lib::observable_bridge::git_ops_client::record(
        &client, &base, "", tenant_id, &req,
    )
    .await
    {
        debug!("fleet::pull_executor: git_ops fleet-feed record failed: {e}");
    }
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

        // capture_tree shells out to ~7 synchronous git subprocesses per
        // repo — multi-second on big or index-locked worktrees, and this
        // machine has dozens of them. Run it on the blocking pool so the
        // fleet-publishers runtime's async worker isn't pinned for the
        // whole walk (PR #391 isolated the heartbeat from this starvation;
        // this removes the starvation at its source). A panic inside the
        // closure surfaces as a JoinError → treated as a skip, same as
        // capture_tree returning None.
        let capture_path = path.clone();
        let mut payload = match tokio::task::spawn_blocking(move || capture_tree(&capture_path))
            .await
            .ok()
            .flatten()
        {
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

        let upsert_ok = match client.post(&url).json(&payload).send().await {
            Ok(resp) if resp.status().is_success() => {
                posted += 1;
                true
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                let excerpt: String = body.chars().take(200).collect();
                warn!(
                    "fleet::tree_publisher: coord returned {status} for {repo}: {excerpt}",
                    repo = payload.repo
                );
                false
            }
            Err(e) => {
                warn!(
                    "fleet::tree_publisher: POST {url} for {repo} failed: {}",
                    error_chain(&e),
                    repo = payload.repo
                );
                false
            }
        };

        // repo_pull executor (plan §5): once the fresh tree state has landed in
        // coord, for a repo that is behind origin, request the pull verdict and
        // apply the safe action. Gated OFF by default (opt-in via
        // COORD_PULL_EXECUTOR_ENABLED) — the apply mutates the working tree.
        if upsert_ok && pull_executor_enabled() && payload.behind_count.unwrap_or(0) > 0 {
            request_and_apply_pull(&client, &base, device_id, path.clone(), &payload).await;
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

    /// Phase 2 unknown-tenant warn-once dedupe: across N successive
    /// invocations the gate must return `true` exactly once (the first
    /// caller) and `false` thereafter. We assert on the atomic gate rather
    /// than the `warn!` side effect for determinism. Reset the flag first so
    /// the test is independent of any earlier in-process call.
    #[test]
    fn unknown_tenant_warn_fires_exactly_once() {
        UNKNOWN_TENANT_WARNED.store(false, std::sync::atomic::Ordering::Relaxed);
        let mut fired = 0usize;
        for _ in 0..5 {
            if should_warn_unknown_tenant() {
                fired += 1;
            }
        }
        assert_eq!(
            fired, 1,
            "expected the warn gate to open exactly once across 5 calls"
        );
        // Reset so we don't leak state into other tests sharing the process.
        UNKNOWN_TENANT_WARNED.store(false, std::sync::atomic::Ordering::Relaxed);
    }

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

    // ---- PR Merge Orchestrator Phase 8 D8.0 — claude probe -----------

    /// `error_chain` must surface every nested `source()` — the whole
    /// point is recovering the connect/DNS/TLS detail that `Display` on
    /// the outermost error hides. (NB: `io::Error::new(kind, payload)`
    /// does NOT expose the payload via `source()` — it forwards the
    /// payload's own source — so the fixture needs a real wrapper type.)
    #[test]
    fn error_chain_walks_sources() {
        #[derive(Debug)]
        struct Outer(std::io::Error);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "error sending request")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let outer = Outer(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "os error 10061",
        ));
        let rendered = error_chain(&outer);
        assert_eq!(
            rendered, "error sending request: os error 10061",
            "full source chain must be rendered"
        );
    }

    /// Smoke: the probe never panics; whether `claude` is on PATH varies
    /// per host so we only assert the function returns a `bool`. This
    /// also exercises the cache-fill path (first call) + cache-hit path
    /// (second call) without spawning twice.
    #[test]
    fn claude_code_probe_smoke() {
        // Clear any inherited cache state from earlier tests in the
        // same process.
        if let Ok(mut g) = CLAUDE_PROBE_CACHE.lock() {
            *g = None;
        }
        let first = claude_code_probe();
        let second = claude_code_probe();
        // Cache invariant — two back-to-back calls must agree.
        assert_eq!(
            first, second,
            "claude_code_probe must be cache-stable across consecutive calls"
        );
    }

    /// The heartbeat payload's `claude_code_available` field is
    /// `#[serde(skip_serializing_if = "is_false")]` so devices without
    /// claude installed continue to serialize the same wire shape as
    /// pre-Phase-8 (no regression for the long-tail of fleet hosts).
    /// Devices WITH claude flip the field to `true` and it appears on
    /// the wire.
    #[test]
    fn heartbeat_payload_omits_false_claude_field() {
        let p = HeartbeatPayload {
            device_id: uuid::Uuid::nil(),
            hostname: "test".into(),
            claude_code_available: false,
            tenant_id: uuid::Uuid::nil(),
        };
        let body = serde_json::to_value(&p).unwrap();
        assert!(
            body.get("claude_code_available").is_none(),
            "false value should be omitted from heartbeat wire"
        );
    }

    #[test]
    fn heartbeat_payload_includes_true_claude_field() {
        let p = HeartbeatPayload {
            device_id: uuid::Uuid::nil(),
            hostname: "test".into(),
            claude_code_available: true,
            tenant_id: uuid::Uuid::nil(),
        };
        let body = serde_json::to_value(&p).unwrap();
        assert_eq!(
            body.get("claude_code_available").and_then(|v| v.as_bool()),
            Some(true),
            "true value must appear on the heartbeat wire"
        );
    }

    /// `tenant_id` is REQUIRED by coord — `post_device_register` rejects
    /// with `400 tenant_id_required` if the field is absent. Pin that the
    /// serialized wire shape includes the field as a UUID string.
    /// Phase 2 of the default-tenant-propagation plan.
    #[test]
    fn heartbeat_payload_serializes_with_tenant_id() {
        let tenant = uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let p = HeartbeatPayload {
            device_id: uuid::Uuid::nil(),
            hostname: "test".into(),
            claude_code_available: true,
            tenant_id: tenant,
        };
        let body = serde_json::to_value(&p).unwrap();
        assert_eq!(
            body.get("tenant_id").and_then(|v| v.as_str()),
            Some("11111111-2222-3333-4444-555555555555"),
            "tenant_id must serialize as a UUID string on the heartbeat wire"
        );
    }

    /// Regression guard against future refactors that drop
    /// `claude_code_available` when extending `HeartbeatPayload`. PR #216
    /// shipped this field; the auditor spawn path depends on it.
    #[test]
    fn heartbeat_payload_still_includes_claude_code_available() {
        let p = HeartbeatPayload {
            device_id: uuid::Uuid::nil(),
            hostname: "test".into(),
            claude_code_available: true,
            tenant_id: uuid::Uuid::nil(),
        };
        let body = serde_json::to_value(&p).unwrap();
        assert_eq!(
            body.get("claude_code_available").and_then(|v| v.as_bool()),
            Some(true),
            "claude_code_available must remain on the heartbeat wire (PR #216 shape)"
        );
    }

    /// The soft-heal write-back depends on extracting the authoritative
    /// `tenant_id` from coord's `DeviceStateRow` response. Verify the pure
    /// parse: a real response shape yields the UUID; junk / missing /
    /// non-UUID values yield `None` (caller treats as "nothing to heal").
    #[test]
    fn response_tenant_id_extracts_from_device_state_row() {
        // Minimal DeviceStateRow-shaped body (coord serializes more fields,
        // but the heal only reads tenant_id).
        let body = r#"{
            "device_id": "00000000-0000-0000-0000-000000000001",
            "hostname": "spaceship",
            "state": "healthy",
            "last_seen_at": "2026-05-30T00:00:00Z",
            "tenant_id": "c231d9da-1111-2222-3333-444455556666"
        }"#;
        assert_eq!(
            response_tenant_id(body),
            uuid::Uuid::parse_str("c231d9da-1111-2222-3333-444455556666").ok(),
            "valid response tenant_id must parse"
        );

        assert_eq!(response_tenant_id("not json"), None, "non-JSON → None");
        assert_eq!(
            response_tenant_id(r#"{"hostname":"x"}"#),
            None,
            "missing tenant_id → None"
        );
        assert_eq!(
            response_tenant_id(r#"{"tenant_id":"not-a-uuid"}"#),
            None,
            "non-UUID tenant_id → None"
        );
    }
}
