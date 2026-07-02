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
pub(crate) struct DeviceFile {
    #[serde(alias = "machine_id")]
    pub(crate) device_id: String,
    hostname: String,
}

fn device_file_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("machine.json"))
}

pub(crate) fn load_device_file() -> Option<DeviceFile> {
    let path = device_file_path()?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Canonical hostname: read from `~/.qontinui/machine.json`, falling
/// back to the OS hostname. All device-registration and heartbeat
/// surfaces should use this single source so the dashboard never shows
/// a stale or mismatched hostname for temp runners that share the
/// primary's `settings.json`.
pub fn canonical_hostname() -> Option<String> {
    load_device_file().map(|d| d.hostname).or_else(|| {
        hostname::get()
            .ok()
            .map(|h| h.to_string_lossy().to_string())
    })
}

/// The device's locally-held tenant bindings, as resolved for coord-side
/// requests. Phase 8a (plan 2026-07-02, D3/D4): the register heartbeat
/// sends the WHOLE set (`tenant_ids`) while the legacy single `tenant_id`
/// field keeps carrying the DEFAULT binding for older coords.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalBindingSet {
    /// The default binding — what the legacy `access_token` slot holds a
    /// JWT for, and what device-level single-tenant surfaces use.
    pub(crate) default_tenant: uuid::Uuid,
    /// Every locally-held binding (deduped; always contains
    /// `default_tenant`).
    pub(crate) tenant_ids: Vec<uuid::Uuid>,
}

/// Resolve the device's binding set for coord-side requests
/// (`POST /coord/devices/register`). Returns the first hit:
///
/// 1. `paired_user.json` — the v2-migrated binding set (legacy files
///    yield their one synthesized entry); the default is
///    `default_tenant_id`/`tenant_id`.
/// 2. JWT-claim fallback — decode `tenant_id` from the cached
///    device-token JWT via
///    [`qontinui_runner_lib::pair::tenant_id_from_oauth_claim`]; the set
///    is that single tenant. (The pre-8a opportunistic disk backfill is
///    gone — the register response's `tenant_ids` reconciliation is the
///    file's healer now, and per-tick resolution works without the
///    write-back.)
/// 3. `None` — no source has a usable tenant. Callers must skip the
///    request (coord rejects with `400 tenant_id_required`).
fn resolve_binding_set() -> Option<LocalBindingSet> {
    // Branch 1 — paired_user.json (v2-migrated view)
    if let Some(s) = qontinui_runner_lib::pair::read_paired_tenant_id_from_disk() {
        if let Ok(default_tenant) = uuid::Uuid::parse_str(s.trim()) {
            let mut tenant_ids = qontinui_runner_lib::pair::read_paired_binding_tenant_ids();
            if !tenant_ids.contains(&default_tenant) {
                tenant_ids.insert(0, default_tenant);
            }
            return Some(LocalBindingSet {
                default_tenant,
                tenant_ids,
            });
        }
    }

    // Branch 2 — cached device-token JWT (the legacy/default slot)
    let token = crate::auth::AuthManager::new()
        .get_access_token()
        .ok()
        .unwrap_or_default();
    if token.is_empty() {
        return None;
    }
    let claim = qontinui_runner_lib::pair::tenant_id_from_oauth_claim(&token)?;
    let parsed = uuid::Uuid::parse_str(claim.trim()).ok()?;
    Some(LocalBindingSet {
        default_tenant: parsed,
        tenant_ids: vec![parsed],
    })
}

/// Single-tenant convenience over [`resolve_binding_set`]: the DEFAULT
/// binding, for callers that stamp one tenant on a device-scoped write
/// (e.g. the git-ops fleet feed, the tree publisher's explicit
/// `tenant_id`, the WIP-attribution walker). Session-scoped callers get
/// their tenant from the owning session instead (Phase 8b).
pub(crate) fn resolve_tenant_id() -> Option<uuid::Uuid> {
    resolve_binding_set().map(|b| b.default_tenant)
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

/// Parse the authoritative binding set (`tenant_ids: ["<uuid>", …]`) out
/// of a register response, if coord sent one (Phase 3 coord — D3).
///
/// FAIL-SOFT is the contract: `None` (→ the caller must NOT reconcile)
/// when the body isn't JSON, the field is ABSENT (today's production
/// coord), it isn't an array, or ANY element fails to parse as a UUID
/// string — a partially-parseable set must never drive binding drops.
/// `Some(vec![])` (present-but-empty) IS meaningful: coord says the
/// device has zero bindings. Pure (no IO) for unit-testability.
fn response_tenant_ids(body: &str) -> Option<Vec<uuid::Uuid>> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let arr = json.get("tenant_ids")?.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let s = v.as_str()?;
        out.push(uuid::Uuid::parse_str(s.trim()).ok()?);
    }
    Some(out)
}

/// Dedupe the "coord reports a binding this runner holds no JWT for"
/// warning per tenant per process — the heartbeat ticks every 30s and
/// the heal (pair for that tenant) is operator-paced.
static COORD_ONLY_BINDINGS_WARNED: std::sync::OnceLock<
    std::sync::Mutex<std::collections::BTreeSet<uuid::Uuid>>,
> = std::sync::OnceLock::new();

fn warn_coord_only_binding_once(tenant: uuid::Uuid) {
    let set = COORD_ONLY_BINDINGS_WARNED
        .get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()));
    let fresh = set.lock().map(|mut g| g.insert(tenant)).unwrap_or(false);
    if fresh {
        warn!(
            "fleet::heartbeat: coord reports this device bound to tenant {tenant} but the \
             runner holds no device-JWT for it — pair for that tenant to enable its \
             sessions. Logging once per tenant per process."
        );
    }
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
pub(crate) fn coord_http_base() -> Option<String> {
    // Delegates to the shared resolver, preserving the deliberate `None`
    // (rather than localhost) when nothing is configured, so the heartbeat
    // cleanly skips instead of spamming connection errors every tick.
    match qontinui_runner_lib::profiles::resolve_coord_base() {
        qontinui_runner_lib::profiles::CoordBase::Configured(base) => Some(base),
        _ => None,
    }
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

// =============================================================================
// Capture-backend telemetry bridge (plan 2026-06-07-fleet-capture-backend-
// telemetry.md, work item 1).
//
// The capture-backend counters live on `ApiState`
// (`vision_capture_preview_count` / `vision_monitor_crop_count` /
// `vision_last_fallback`, `mcp/types.rs:282/285/292`), constructed in
// `mcp_api::start_server` deep inside the Tauri setup. The 30s device
// heartbeat (`heartbeat_to_coord`) is a free fn on its own dedicated OS
// thread (`main.rs`), spawned BEFORE `ApiState` exists — so it can't hold
// an `Arc<ApiState>`. Mirror the `CLAUDE_PROBE_CACHE` static pattern: publish
// clones of just the three telemetry handles into a process-global `OnceLock`
// when `ApiState` is built; the heartbeat reads them per-tick. Clones of
// `Arc<Atomic*>`/`Arc<Mutex<_>>` share the live counters the capture path
// bumps, so the heartbeat always reports the current cumulative values.
// =============================================================================

/// Process-global clones of the capture-backend telemetry handles, published
/// by [`publish_capture_telemetry_handles`] when `ApiState` is constructed.
/// `None` until then (e.g. early boot, or unit tests that never build the MCP
/// state) — the heartbeat then reports zero/absent, which is the honest
/// "no captures observed" value.
static CAPTURE_TELEMETRY: std::sync::OnceLock<CaptureTelemetryHandles> = std::sync::OnceLock::new();

/// Read-only clones of the three `ApiState` capture-backend telemetry handles
/// the device heartbeat reports. Shares the live `Arc`s the capture path bumps.
#[derive(Clone)]
pub struct CaptureTelemetryHandles {
    /// Cumulative WebView2 CapturePreview frames since process start.
    pub capture_preview_count: Arc<std::sync::atomic::AtomicU64>,
    /// Cumulative monitor-crop fallback frames since process start.
    pub monitor_crop_count: Arc<std::sync::atomic::AtomicU64>,
    /// Reason + timestamp of the most recent fallback this session, or `None`.
    pub last_fallback: Arc<std::sync::Mutex<Option<(String, chrono::DateTime<chrono::Utc>)>>>,
}

/// Publish the capture-backend telemetry handles for the device heartbeat to
/// read. Called once from `ApiState` construction (`mcp_api::start_server`).
/// Idempotent: a second call (e.g. a re-init) is silently ignored — the first
/// set of live handles wins, which is correct since `ApiState` is built once.
pub fn publish_capture_telemetry_handles(handles: CaptureTelemetryHandles) {
    let _ = CAPTURE_TELEMETRY.set(handles);
}

/// Snapshot the current capture-backend telemetry for one heartbeat tick.
/// Returns `(preview_count, crop_count, last_fallback_at)`; all zero/`None`
/// before `ApiState` publishes its handles. `last_fallback_at` is the RFC3339
/// timestamp of the most recent fallback (the reason string stays runner-local
/// — only the timestamp rides the wire, per the privacy posture: no content).
fn capture_telemetry_snapshot() -> (u64, u64, Option<chrono::DateTime<chrono::Utc>>) {
    use std::sync::atomic::Ordering;
    match CAPTURE_TELEMETRY.get() {
        Some(h) => {
            let preview = h.capture_preview_count.load(Ordering::Relaxed);
            let crop = h.monitor_crop_count.load(Ordering::Relaxed);
            let last = h
                .last_fallback
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|(_, at)| *at));
            (preview, crop, last)
        }
        None => (0, 0, None),
    }
}

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
    /// `routes_phase3.rs:257-269`). Resolved via [`resolve_binding_set`]
    /// (the DEFAULT binding) before the payload is constructed; if
    /// `None` there, the heartbeat is skipped rather than 400-spamming
    /// coord. Phase 2 of the default-tenant-propagation plan.
    tenant_id: uuid::Uuid,
    /// Phase 8a (plan 2026-07-02, D3): the runner's WHOLE locally-held
    /// binding set (always includes `tenant_id`). Phase-3 coord touches
    /// `tenant_devices.last_active_at` for the intersection with real
    /// bindings (never inserts); today's production coord simply ignores
    /// the field (`DeviceRegisterRequest` has no `deny_unknown_fields`).
    tenant_ids: Vec<uuid::Uuid>,
    /// Capture-backend telemetry (plan 2026-06-07-fleet-capture-backend-
    /// telemetry.md D1) — cumulative WebView2 CapturePreview frames served
    /// since this process started. Straight-write coord-side into
    /// `coord.devices.capture_preview_count`. Always serialized (even 0) so a
    /// device with zero captures still reports an honest baseline; coord
    /// ingest tolerates absence via COALESCE so legacy/older coord ignores it.
    capture_preview_count: u64,
    /// Cumulative monitor-crop fallback frames since process start. Coord-side
    /// `coord.devices.monitor_crop_count`. A nonzero value here is the fleet's
    /// "this device is silently on the fallback" signal.
    monitor_crop_count: u64,
    /// RFC3339 timestamp of the most recent CapturePreview→monitor-crop
    /// fallback this session, or absent if none yet. Straight-write coord-side
    /// into `coord.devices.last_capture_fallback_at` when present, COALESCE
    /// (preserve) when absent — so a runner restart with no fresh fallback
    /// doesn't erase the last observed one. Reason string is deliberately NOT
    /// sent (privacy: timestamp only, no content).
    #[serde(skip_serializing_if = "Option::is_none")]
    last_capture_fallback_at: Option<chrono::DateTime<chrono::Utc>>,
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

    let bindings = match resolve_binding_set() {
        Some(b) => b,
        None => {
            warn_tenant_id_unresolvable_once();
            return Ok(());
        }
    };

    let claude_code_available = claude_code_probe();

    let (capture_preview_count, monitor_crop_count, last_capture_fallback_at) =
        capture_telemetry_snapshot();

    let payload = HeartbeatPayload {
        device_id,
        hostname: device.hostname.clone(),
        claude_code_available,
        tenant_id: bindings.default_tenant,
        tenant_ids: bindings.tenant_ids,
        capture_preview_count,
        monitor_crop_count,
        last_capture_fallback_at,
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
        // Binding reconciliation (Phase 8a, D3). When coord's response
        // carries the authoritative `tenant_ids` set (Phase 3 coord), the
        // runner reconciles its local binding state against it: drop
        // entries (+ their JWT slots) coord no longer has, warn-once for
        // bindings coord has that this runner holds no JWT for. When the
        // field is ABSENT — today's production coord — this is a strict
        // no-op on binding state, and only the LEGACY single-value
        // echo-heal runs (itself a no-op on v2 multi-entry files: see
        // `pair::backfill_paired_tenant_id`), preserving the pre-8a
        // stale-single-tenant convergence for legacy-shape installs.
        // Best-effort throughout: a parse/IO miss just retries next tick.
        let body = resp.text().await.unwrap_or_default();
        if let Some(coord_set) = response_tenant_ids(&body) {
            match qontinui_runner_lib::pair::reconcile_paired_bindings(&coord_set) {
                Ok(report) => {
                    if report.changed() {
                        info!(
                            "fleet::heartbeat: reconciled bindings against coord \
                             (dropped={:?} dropped_slots={:?} default_repointed={:?})",
                            report.dropped, report.dropped_slots, report.default_repointed
                        );
                    }
                    for t in report.coord_only {
                        warn_coord_only_binding_once(t);
                    }
                }
                Err(e) => {
                    tracing::debug!("fleet::heartbeat: binding reconcile non-fatal: {e}");
                }
            }
        } else if let Some(resp_tenant) = response_tenant_id(&body) {
            // Legacy echo-heal (single-value; v2-file-aware no-op).
            // Scheduled for deletion in Phase 10 item 4.
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
    /// Commits HEAD is behind `origin/<default_branch>` — the distance
    /// from the canonical default ref, computed ONLY when the tree is
    /// parked on a NON-default named branch (`branch` known,
    /// `default_branch` known, and they differ). `None` otherwise: on the
    /// default branch the existing `behind_count` (vs `origin/<branch>`,
    /// which == `origin/<default>`) already covers it, and detached/
    /// unknown-branch states can't name a meaningful comparison branch.
    /// A checkout parked on a squash-merged feature branch reads `0` for
    /// `behind_count` (its own remote ref hasn't advanced) — the distance
    /// from `origin/<default>` is the signal that catches that stale tree.
    /// Reflects last-fetched remote state — no network fetch. `None` if
    /// the `rev-list` fails (e.g. `origin/<default>` unresolved locally).
    #[serde(skip_serializing_if = "Option::is_none")]
    behind_default_count: Option<i32>,
    /// Total count of dirty (porcelain) entries BEFORE the
    /// MAX_DIRTY_FILES_REPORTED truncation applied to `dirty_files`. Lets
    /// coord's WIP-attribution clear path distinguish a COMPLETE dirty_files
    /// list (dirty_total <= reported len) from a capped sample (dirty_total >
    /// reported len) so it never wrongly clears a still-dirty file dropped from
    /// the truncated sample. `None` only if status couldn't be determined.
    #[serde(skip_serializing_if = "Option::is_none")]
    dirty_total: Option<i32>,
    /// Phase 8b (plan 2026-07-02-session-scoped-multi-tenant-device-binding,
    /// Phase 8 item 7 / D2 site 13): EXPLICIT tenant attribution — the
    /// device DEFAULT binding, since primary trees are canonical
    /// device-scoped checkouts (agent worktrees are coord-stamped at
    /// allocate and never published here). Explicit-wins coord-side once
    /// Phase 5's resolver deploys; today's coord ignores the extra field.
    /// Omitted when the runner has no resolvable default binding. Filled by
    /// the publisher alongside `device_id` (identity-side, not per-repo).
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant_id: Option<uuid::Uuid>,
    /// Application ID associated with this repository (fleet-fresh test-target
    /// routing). Stamped by the publisher: `Some(app.app_id)` for app-repo
    /// trees (from `project.apps` registry), `None` for plain `qontinui-*`
    /// repos. Used by dispatcher to route tests to fresh app instances.
    #[serde(skip_serializing_if = "Option::is_none")]
    app_id: Option<String>,
}

/// Maximum number of dirty paths included per row (the column is unbounded
/// but operator triage doesn't benefit from a 10k-row dump). Anything past
/// this is silently truncated; `dirty=true` still flags the tree.
const MAX_DIRTY_FILES_REPORTED: usize = 50;

/// Machine-local artifacts the runner drops into a managed repo's working
/// tree: the per-session coord-mcp proxy `.mcp.json` (holds device-scoped
/// proxy keys — must never be committed) and the `agent-worktrees/` /
/// `.agent-worktrees/` checkout roots. Left untracked they make `capture_tree`
/// report `dirty=true`, which on a default branch yields a permanent
/// `wip_on_default` Hold from the pull-decision watcher — silently wedging the
/// repo-pull executor. We exclude them per-repo via `.git/info/exclude` (see
/// [`ensure_repo_info_exclude`]) rather than a committed `.gitignore`, so the
/// heal is machine-local (no per-repo PR) and a secrets file never enters git
/// history at all.
const MANAGED_REPO_EXCLUDES: &[&str] = &[".mcp.json", "agent-worktrees/", ".agent-worktrees/"];

/// Header line bracketing the runner-managed block in a repo's
/// `.git/info/exclude` — lets the operator see who added the entries.
const MANAGED_EXCLUDE_MARKER: &str = "# qontinui-runner: machine-local artifacts (auto-excluded)";

/// Idempotently ensure `<repo>/.git/info/exclude` ignores the runner's
/// machine-local artifacts ([`MANAGED_REPO_EXCLUDES`]). `.git/info/exclude` is
/// git's per-repo, NON-tracked ignore file: it hides matching UNTRACKED paths
/// exactly like `.gitignore` but without a tracked edit (which would itself
/// dirty the tree). Because exclusion applies to any matching untracked path
/// regardless of when it appeared, calling this every publish cycle also heals
/// repos that ALREADY have the stray files — so no `.gitignore` sweep is needed.
///
/// Only PRIMARY checkouts (`.git` is a directory) are handled; a linked
/// worktree's `.git` is a file pointing elsewhere, so we skip it (the publisher
/// walks primary trees anyway). Best-effort: any IO error is swallowed — a
/// missed exclude just means the tree reads dirty for one more cycle, never a
/// panic.
fn ensure_repo_info_exclude(repo_path: &std::path::Path) {
    // `.git` as a directory == primary checkout. Skip worktree `.git` files
    // and bare/odd layouts.
    if !repo_path.join(".git").is_dir() {
        return;
    }
    let info_dir = repo_path.join(".git").join("info");
    let exclude_path = info_dir.join("exclude");
    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();

    // Match on whole (trimmed) lines so a substring like `.mcp.json.bak` never
    // counts as already-covered.
    let present: std::collections::HashSet<&str> = existing.lines().map(|l| l.trim()).collect();
    let missing: Vec<&str> = MANAGED_REPO_EXCLUDES
        .iter()
        .copied()
        .filter(|pat| !present.contains(*pat))
        .collect();
    if missing.is_empty() {
        return;
    }
    // Capture marker presence before `existing` is moved into `out` below;
    // `missing` holds 'static patterns so it doesn't borrow `existing`.
    let marker_present = present.contains(MANAGED_EXCLUDE_MARKER);

    if let Err(e) = std::fs::create_dir_all(&info_dir) {
        debug!("fleet::ensure_repo_info_exclude: create_dir_all {info_dir:?} failed: {e}");
        return;
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !marker_present {
        out.push_str(MANAGED_EXCLUDE_MARKER);
        out.push('\n');
    }
    for pat in missing {
        out.push_str(pat);
        out.push('\n');
    }
    if let Err(e) = std::fs::write(&exclude_path, out) {
        debug!("fleet::ensure_repo_info_exclude: write {exclude_path:?} failed: {e}");
    }
}

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

/// Decide which branch (if any) the `behind_default_count` rev-list should
/// compare against. Returns `Some(default_branch)` ONLY when the tree is on
/// a known, non-default named branch — i.e. `branch` is a real branch (not
/// `(detached)`), and it differs from `default_branch`. Returns `None` on
/// the default branch (where `behind_count` already covers the distance) or
/// when the branch is detached/unknown (no meaningful comparison). Pure so
/// the gating is unit-testable without shelling to git.
fn behind_default_compare_branch<'a>(branch: &str, default_branch: &'a str) -> Option<&'a str> {
    if branch == "(detached)" || branch.is_empty() {
        return None;
    }
    if branch == default_branch {
        return None;
    }
    Some(default_branch)
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
    // Total dirty entries BEFORE the MAX_DIRTY_FILES_REPORTED truncation,
    // counted with the SAME entry-recognition predicate (len >= 4) used to
    // build `dirty_files`. coord compares this against the reported sample
    // length to know whether the list is complete or capped.
    let dirty_total_count = status_str.lines().filter(|line| line.len() >= 4).count() as i32;
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
    // `origin/main` when detached/no branch). Uses remote-tracking refs
    // (`origin/*`) — `capture_tree` itself performs no `git fetch`, so it
    // stays pure and fast. Freshness is the publisher's job: it runs an
    // explicit periodic `git fetch origin` (no refspec — touches only
    // `origin/*`, never the working tree) BEFORE this capture, gated by
    // `COORD_TREE_FETCH_INTERVAL_SECS` (see `publish_tree_state`). On a
    // no-manual-coding machine that explicit fetch is what keeps these
    // behind-counts honest.
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

    // behind_default_count: commits HEAD is behind `origin/<default>` —
    // ONLY when parked on a non-default named branch (see
    // `behind_default_compare_branch`). On the default branch the existing
    // `behind_count` (vs `origin/<branch>` == `origin/<default>`) already
    // covers it, so we emit `None` there. The signal that matters: a tree
    // sitting on a squash-merged feature branch reads `behind_count == 0`
    // (its own remote ref is stale) yet is many commits behind
    // `origin/<default>`. Same no-fetch posture as `behind_count` above; a
    // failed rev-list (e.g. `origin/<default>` unresolved) → `None`, never
    // an error.
    let behind_default_count: Option<i32> =
        match behind_default_compare_branch(&branch, &default_branch) {
            Some(cmp) => Command::new("git")
                .args([
                    "-C",
                    repo_path.to_str()?,
                    "rev-list",
                    "--count",
                    &format!("HEAD..origin/{cmp}"),
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
                }),
            None => None,
        };

    // device_id is filled in by the caller (it's identity-side, not
    // per-repo). Punch in a placeholder; the publisher overwrites it.
    // app_id is populated by the publisher if the repo is in project.apps.
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
        behind_default_count,
        dirty_total: Some(dirty_total_count),
        tenant_id: None,
        app_id: None,
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
// ON by default since 2026-06-12 (operator granted standing autonomous-pull
// authorization fleet-wide after the live-verified rollout): set
// `COORD_PULL_EXECUTOR_ENABLED=0` to opt a machine out locally. The runtime
// control plane is coord's per-tenant `repo_pull` autonomy dial — dialing it
// to `guidance_only` makes every executor surface recommendations without
// applying them, fleet-wide, without touching machines.
// =============================================================================

/// Is the auto-pull executor enabled? On unless `COORD_PULL_EXECUTOR_ENABLED`
/// is an explicit falsy value (`0`/`false`/`no`, case-insensitive).
fn pull_executor_enabled() -> bool {
    std::env::var("COORD_PULL_EXECUTOR_ENABLED")
        .ok()
        .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no"))
        .unwrap_or(true)
}

// =============================================================================
// Always-on safe remote-ref refresh (repo-freshness plan, Layer 1)
//
// `capture_tree`'s behind-counts are computed against remote-tracking refs
// (`origin/*`). On a machine where nobody runs `git fetch`/`pull-all` by hand,
// those refs go stale and every behind-count silently reads 0. The publisher
// therefore runs a best-effort `git fetch origin` (NO refspec — updates only
// `origin/*` remote-tracking refs; never touches the working tree or any local
// branch ref) before each repo's `capture_tree`. `--prune` drops deleted remote
// branches so stale `origin/<merged-feature>` refs don't linger.
//
// Frequency is gated by `COORD_TREE_FETCH_INTERVAL_SECS` (defaults to the
// publish interval `COORD_TREE_PUBLISH_INTERVAL_SECS`, default 60, floor 5) so
// an operator can throttle fetch independently of publication. The gate is a
// real throttle, not a kill switch — the default fetches every publish cycle.
// =============================================================================

/// Resolve the per-repo fetch interval. Reads `COORD_TREE_FETCH_INTERVAL_SECS`,
/// falling back to `COORD_TREE_PUBLISH_INTERVAL_SECS`, then 60; floored at 5s.
fn tree_fetch_interval() -> Duration {
    let secs: u64 = std::env::var("COORD_TREE_FETCH_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .or_else(|| {
            std::env::var("COORD_TREE_PUBLISH_INTERVAL_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(60)
        .max(5);
    Duration::from_secs(secs)
}

/// Process-global per-repo last-fetch timestamps for the interval gate.
fn last_fetch_at(
) -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, std::time::Instant>> {
    static LAST_FETCH: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, std::time::Instant>>,
    > = std::sync::OnceLock::new();
    LAST_FETCH.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// True when `repo_path` is due a fetch (never fetched, or last fetch is at
/// least `tree_fetch_interval()` ago). Records `now` as the attempt time when
/// it returns true, so concurrent/back-to-back ticks don't double-fetch.
fn fetch_due(repo_path: &std::path::Path, now: std::time::Instant) -> bool {
    let interval = tree_fetch_interval();
    let mut map = match last_fetch_at().lock() {
        Ok(m) => m,
        Err(p) => p.into_inner(),
    };
    let due = match map.get(repo_path) {
        Some(prev) => now.duration_since(*prev) >= interval,
        None => true,
    };
    if due {
        map.insert(repo_path.to_path_buf(), now);
    }
    due
}

/// Best-effort `git -C <path> fetch origin --prune` (no refspec). Blocking git;
/// call from a blocking context. Non-zero/spawn errors `warn!` and return — a
/// stale ref is no worse than skipping the fetch entirely.
fn fetch_remote_refs_blocking(repo_path: &std::path::Path) {
    use std::process::Command;
    let path_str = match repo_path.to_str() {
        Some(s) => s,
        None => return,
    };
    match Command::new("git")
        .args(["-C", path_str, "fetch", "origin", "--prune"])
        .output()
    {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            let excerpt: String = stderr.trim().chars().take(200).collect();
            warn!(
                "fleet::tree_publisher: git fetch origin in {} failed ({}): {excerpt}",
                repo_path.display(),
                o.status
            );
        }
        Err(e) => {
            warn!(
                "fleet::tree_publisher: git fetch origin in {} could not spawn: {e}",
                repo_path.display()
            );
        }
    }
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

/// The `restore_default` verdict's payload fields (chkguard Phase 3 — restore
/// a clean primary checkout parked on a merged-PR branch). Parsed from the
/// coord Decision; every field is re-verified against live git at apply time.
struct RestoreParams {
    parked_branch: String,
    /// The merged PR's head SHA — the local HEAD must STILL equal it.
    expected_head_sha: String,
    /// The merged PR number (log/audit context only).
    pr_number: Option<i64>,
}

/// Apply one safe verdict to one repo's working tree. Blocking git via the
/// caller's `spawn_blocking`. Returns the outcome to record. NEVER performs an
/// unsafe op regardless of the verdict (defense in depth, plan §5).
fn apply_pull_verdict_blocking(
    repo_path: &std::path::Path,
    verdict_kind: &str,
    timing_now: bool,
    hold_reason: Option<&str>,
    restore: Option<RestoreParams>,
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
        "restore_default" => {
            // chkguard Phase 3 — restore a clean primary checkout parked on a
            // merged-PR branch: switch <default> && merge --ff-only
            // origin/<default> && delete the parked branch. Every predicate
            // coord evaluated is re-verified against LIVE git first; any
            // mismatch → no action (the staleness alert keeps paging).
            if !timing_now {
                return PullOutcome {
                    chosen_option: "deferred".to_string(),
                    reasoning:
                        "coord verdict RestoreDefault but timing=Defer — re-evaluate next tick"
                            .to_string(),
                    git_op: None,
                };
            }
            let Some(p) = restore else {
                return PullOutcome {
                    chosen_option: "restore_malformed".to_string(),
                    reasoning: "restore_default verdict missing parked_branch/expected_head_sha"
                        .to_string(),
                    git_op: None,
                };
            };
            apply_restore_default_blocking(repo_path, repo_str, &default_branch, &p)
        }
        other => PullOutcome {
            chosen_option: "unknown_verdict".to_string(),
            reasoning: format!("unrecognized verdict kind `{other}` — no action"),
            git_op: None,
        },
    }
}

/// Execute one verified `restore_default`: apply-time re-verification, then
/// fetch → switch → ff-only merge → delete the parked branch. Each step
/// aborts on failure with a distinct outcome (idempotent — the next publish
/// tick re-evaluates whatever state the tree was left in; every intermediate
/// state is a valid git state strictly no staler than the parked one).
fn apply_restore_default_blocking(
    repo_path: &std::path::Path,
    repo_str: &str,
    default_branch: &str,
    p: &RestoreParams,
) -> PullOutcome {
    use std::process::Command;
    let pr = p
        .pr_number
        .map(|n| format!("#{n}"))
        .unwrap_or_else(|| "(unknown)".to_string());

    // Apply-time re-verification — the verdict may be a tick stale.
    // (1) PRIMARY checkout only: a linked worktree's `.git` is a file.
    if !repo_path.join(".git").is_dir() {
        return PullOutcome {
            chosen_option: "restore_skipped_recheck".to_string(),
            reasoning: "not a primary checkout (.git is not a directory) — refusing restore"
                .to_string(),
            git_op: None,
        };
    }
    // (2) Still on the parked branch coord decided about.
    let cur_branch = Command::new("git")
        .args(["-C", repo_str, "symbolic-ref", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if cur_branch.as_deref() != Some(p.parked_branch.as_str()) {
        return PullOutcome {
            chosen_option: "restore_skipped_recheck".to_string(),
            reasoning: format!(
                "tree moved since the verdict (on {:?}, expected parked branch `{}`) — not restoring",
                cur_branch, p.parked_branch
            ),
            git_op: None,
        };
    }
    // (3) Still porcelain-clean (includes untracked).
    let clean = Command::new("git")
        .args(["-C", repo_str, "status", "--porcelain=v1"])
        .output()
        .ok()
        .map(|o| o.status.success() && o.stdout.is_empty())
        .unwrap_or(false);
    if !clean {
        return PullOutcome {
            chosen_option: "restore_skipped_recheck".to_string(),
            reasoning: "tree dirty at apply time — not restoring (never touch WIP)".to_string(),
            git_op: None,
        };
    }
    // (4) Local HEAD still equals the merged PR's head SHA — the zero-work-loss
    //     proof. A new local commit since the verdict fails this and aborts.
    let head = Command::new("git")
        .args(["-C", repo_str, "rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if head.as_deref() != Some(p.expected_head_sha.as_str()) {
        return PullOutcome {
            chosen_option: "restore_skipped_recheck".to_string(),
            reasoning: format!(
                "HEAD {:?} != merged PR {pr} head {} — local work may exist; not restoring",
                head, p.expected_head_sha
            ),
            git_op: None,
        };
    }

    // Fetch the default ref first so the ff-only merge lands on CURRENT main
    // (the parked tree's origin/<default> may be arbitrarily stale).
    let fetch = Command::new("git")
        .args(["-C", repo_str, "fetch", "origin", default_branch])
        .output();
    if !fetch.as_ref().map(|o| o.status.success()).unwrap_or(false) {
        let err = fetch
            .map(|o| {
                String::from_utf8_lossy(&o.stderr)
                    .trim()
                    .chars()
                    .take(200)
                    .collect()
            })
            .unwrap_or_else(|e| format!("spawn failed: {e}"));
        return PullOutcome {
            chosen_option: "restore_fetch_failed".to_string(),
            reasoning: format!("git fetch origin {default_branch} failed: {err}"),
            git_op: None,
        };
    }
    let switch = Command::new("git")
        .args(["-C", repo_str, "switch", default_branch])
        .output();
    if !switch.as_ref().map(|o| o.status.success()).unwrap_or(false) {
        let err = switch
            .map(|o| {
                String::from_utf8_lossy(&o.stderr)
                    .trim()
                    .chars()
                    .take(200)
                    .collect()
            })
            .unwrap_or_else(|e| format!("spawn failed: {e}"));
        return PullOutcome {
            chosen_option: "restore_switch_failed".to_string(),
            reasoning: format!("git switch {default_branch} failed: {err}"),
            git_op: None,
        };
    }
    let merge = Command::new("git")
        .args([
            "-C",
            repo_str,
            "merge",
            "--ff-only",
            &format!("origin/{default_branch}"),
        ])
        .output();
    if !merge.as_ref().map(|o| o.status.success()).unwrap_or(false) {
        // Switched but couldn't ff (local default diverged). The tree is on
        // the default branch at its old position — strictly less stale than
        // parked; the ordinary repo_pull ladder (Diverged → escalate) takes
        // over next tick. The parked branch is kept for the audit trail.
        let err = merge
            .map(|o| {
                String::from_utf8_lossy(&o.stderr)
                    .trim()
                    .chars()
                    .take(200)
                    .collect()
            })
            .unwrap_or_else(|e| format!("spawn failed: {e}"));
        return PullOutcome {
            chosen_option: "restored_ff_failed".to_string(),
            reasoning: format!(
                "switched to {default_branch} but ff-only merge failed (local default \
                 diverged?): {err} — parked branch `{}` kept",
                p.parked_branch
            ),
            git_op: Some((
                "restore".to_string(),
                Some(format!("switch {default_branch} (ff failed)")),
            )),
        };
    }
    // Delete the parked branch. -D, not -d: a squash-merged branch tip is not
    // an ancestor of the default branch, so -d always refuses it — and the
    // (re-verified) HEAD == merged-PR-head equality above is the actual
    // safety proof (the content is on GitHub as the PR head regardless).
    let del = Command::new("git")
        .args(["-C", repo_str, "branch", "-D", &p.parked_branch])
        .output();
    let del_note = if del.as_ref().map(|o| o.status.success()).unwrap_or(false) {
        format!("deleted parked branch `{}`", p.parked_branch)
    } else {
        format!(
            "branch -D `{}` failed (left in place): {}",
            p.parked_branch,
            del.map(|o| String::from_utf8_lossy(&o.stderr)
                .trim()
                .chars()
                .take(120)
                .collect())
                .unwrap_or_else(|e| format!("spawn failed: {e}"))
        )
    };
    let new_head = Command::new("git")
        .args(["-C", repo_str, "rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    PullOutcome {
        chosen_option: "restored".to_string(),
        reasoning: format!(
            "restored from merged PR {pr} branch `{}` to {default_branch} \
             (HEAD={}); {del_note}",
            p.parked_branch,
            &new_head[..new_head.len().min(12)],
        ),
        git_op: Some((
            "restore".to_string(),
            Some(format!(
                "parked merged branch `{}` -> {default_branch}",
                p.parked_branch
            )),
        )),
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
    // 1. Request the decision (device-scoped; the executor's fresh git state
    //    rides in `context` so the verdict can fall back to it if coord's
    //    row lags). Phase 8b item 7: `tenant_id` goes EXPLICIT (device
    //    DEFAULT binding, mirrored from the tree upsert) — coord's D2
    //    resolver prefers it once Phase 5 deploys and stamps it into the
    //    policy_rule_resolutions row that `pull-decision/record` later
    //    resolves through; today's coord ignores the extra field.
    let context = serde_json::json!({
        "repo": payload.repo,
        "branch": payload.branch,
        "behind": payload.behind_count,
        "dirty": payload.dirty,
        "untracked": payload.untracked_count,
        "detached": payload.head_detached,
        "local_ahead": payload.local_ahead,
    });
    let mut body = serde_json::json!({
        "device_id": device_id,
        "repo": payload.repo,
        "surface": "infra",
        "context": context,
    });
    if let Some(t) = payload.tenant_id {
        body["tenant_id"] = serde_json::json!(t);
    }
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
    // chkguard Phase 3: the restore_default verdict's payload. Both fields
    // required — a partial payload parses to None and the apply arm reports
    // restore_malformed instead of guessing.
    let restore_params = if verdict_kind == "restore_default" {
        match (
            verdict.get("parked_branch").and_then(|v| v.as_str()),
            verdict.get("expected_head_sha").and_then(|v| v.as_str()),
        ) {
            (Some(b), Some(sha)) if !b.is_empty() && !sha.is_empty() => Some(RestoreParams {
                parked_branch: b.to_string(),
                expected_head_sha: sha.to_string(),
                pr_number: verdict.get("pr_number").and_then(|v| v.as_i64()),
            }),
            _ => None,
        }
    } else {
        None
    };

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
        apply_pull_verdict_blocking(&rp, &vk, timing_now, hr.as_deref(), restore_params)
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

    if outcome.chosen_option == "pulled"
        || outcome.chosen_option == "default_ref_sync"
        || outcome.chosen_option == "restored"
    {
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

    // Phase 8b item 7 — resolve the explicit publisher tenant (device
    // DEFAULT binding) once per pass. `None` (unpaired) omits the field and
    // coord resolves device-side, exactly the pre-8b behavior.
    let publisher_tenant = resolve_tenant_id();

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
        // Always-on safe remote-ref refresh: refresh `origin/*` (no refspec,
        // working tree untouched) before capturing so behind-counts are honest
        // on machines with no manual `git fetch` cadence. Interval-gated by
        // COORD_TREE_FETCH_INTERVAL_SECS; best-effort (failures warn, never
        // fail the cycle). Runs on the blocking pool like capture_tree below.
        if fetch_due(&path, std::time::Instant::now()) {
            let fetch_path = path.clone();
            let _ =
                tokio::task::spawn_blocking(move || fetch_remote_refs_blocking(&fetch_path)).await;
        }

        let capture_path = path.clone();
        let mut payload = match tokio::task::spawn_blocking(move || {
            // Heal machine-local artifacts into `.git/info/exclude` BEFORE
            // capturing, so this cycle's dirty/verdict already reflects it —
            // otherwise the stray `.mcp.json` would pin the tree to a
            // `wip_on_default` Hold and the pull below would never fire.
            ensure_repo_info_exclude(&capture_path);
            capture_tree(&capture_path)
        })
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
        // Phase 8b item 7 — explicit tenant on the upsert wire (device
        // DEFAULT binding; canonical trees are device-scoped). Identity-side
        // like `device_id`, so stamped here rather than in capture_tree.
        payload.tenant_id = publisher_tenant;

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
        // apply the safe action. ON by default (opt out via
        // COORD_PULL_EXECUTOR_ENABLED=0) — the apply mutates the working tree.
        //
        // chkguard Phase 3: ALSO request a verdict for a clean PRIMARY checkout
        // parked on a non-default named branch — the restore-candidate shape.
        // Such a tree reads behind_count 0 against its own (merged) branch's
        // static remote ref, so the behind>0 trigger alone never consults
        // coord about it. Primary-only (`.git` is a DIRECTORY): a linked
        // worktree shares the branch namespace with the primary, so a restore
        // switch there could collide with the primary's checked-out branch.
        let restore_candidate = !payload.dirty
            && !payload.head_detached.unwrap_or(false)
            && payload.branch != "main"
            && payload.branch != "master"
            && path.join(".git").is_dir();
        // behind_default_count covers the feature-branch-behind-origin/main
        // case: a checkout parked on a (possibly merged) named branch reads
        // behind_count 0 against its own static remote ref yet may be far
        // behind origin/<default>. coord answers that with a safe DefaultRefSync
        // (`git fetch origin <default>:<default>`, working tree untouched).
        if upsert_ok
            && pull_executor_enabled()
            && (payload.behind_count.unwrap_or(0) > 0
                || payload.behind_default_count.unwrap_or(0) > 0
                || restore_candidate)
        {
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

/// P3 — Fleet-Wide Auto-Fresh Engine (fleet-fresh test-target routing)
///
/// Polls coord for designated test-targets and auto-refreshes app repositories
/// to keep their deployed/built versions at upstream HEAD. Runs on a background
/// loop similar to `spawn_tree_publisher`.
pub fn spawn_auto_fresh_engine() {
    let secs: u64 = std::env::var("AUTO_FRESH_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300)
        .max(60);

    info!(
        "fleet::auto_fresh_engine: starting periodic auto-fresh loop, interval={}s",
        secs
    );

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(e) = run_auto_fresh_cycle().await {
                warn!("fleet::auto_fresh_engine: {e}");
            }
        }
    });
}

/// Single cycle of the auto-fresh engine: poll coord for test-targets,
/// check freshness, pull+build+restart as needed.
async fn run_auto_fresh_cycle() -> Result<(), String> {
    let device = match load_device_file() {
        Some(d) => d,
        None => {
            return Ok(()); // Device not initialized, skip silently
        }
    };

    let device_id = uuid::Uuid::parse_str(&device.device_id)
        .map_err(|e| format!("invalid device_id UUID: {e}"))?;

    let coord_base = coord_http_base()
        .ok_or_else(|| "no coord endpoint available".to_string())?;

    // Poll coord for test-targets designated for this device + auto_fresh enabled
    let url = format!(
        "{}/coord/trees/test-targets/by-device/{}",
        coord_base, device_id
    );

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET {}: {e}", url))?;

    if !response.status().is_success() {
        return Err(format!(
            "GET {} returned {}",
            url,
            response.status()
        ));
    }

    #[derive(serde::Deserialize)]
    struct TestTarget {
        app_id: String,
        auto_fresh: bool,
    }

    #[derive(serde::Deserialize)]
    struct TestTargetsResponse {
        test_targets: Vec<TestTarget>,
    }

    let data: TestTargetsResponse = response
        .json()
        .await
        .map_err(|e| format!("parsing test-targets response: {e}"))?;

    // For each auto_fresh app, check if refresh is needed
    for target in data.test_targets {
        if !target.auto_fresh {
            continue;
        }

        // Spawn async task to process this app (non-blocking iteration)
        let app_id = target.app_id.clone();
        tokio::spawn(async move {
            if let Err(e) = process_auto_fresh_app(&app_id, device_id).await {
                warn!(
                    "fleet::auto_fresh_engine: failed to process app_id={}: {}",
                    app_id, e
                );
            }
        });
    }

    Ok(())
}

/// Check if THIS runner instance has active task-runs (idle-aware guard).
///
/// Asks our own HTTP API (`get_mcp_api_port()`, NOT a hardcoded 9876 — temp
/// runners bind 9877+) whether `/task-runs/running` has entries. Best-effort
/// with a 2s ceiling; any failure reads as idle so a wedged API can't
/// permanently starve auto-fresh.
async fn runner_has_active_tasks() -> bool {
    let port = crate::mcp::types::get_mcp_api_port();
    let probe = async move {
        let resp = reqwest::Client::new()
            .get(format!("http://localhost:{port}/task-runs/running"))
            .send()
            .await
            .ok()?;
        let body: serde_json::Value = resp.json().await.ok()?;
        // Handler returns a bare array; tolerate an envelope like the
        // discovery_tools consumer does.
        body.get("data")
            .and_then(|d| d.as_array())
            .or_else(|| body.as_array())
            .map(|a| !a.is_empty())
    };
    match tokio::time::timeout(Duration::from_secs(2), probe).await {
        Ok(Some(has_tasks)) => has_tasks,
        _ => false, // timeout, HTTP error, or unexpected shape: assume idle
    }
}

/// Get current HEAD SHA via git rev-parse (blocking operation).
/// Returns Option<String> with last 12 chars of SHA, or None on error.
fn get_current_head_sha(repo_path: &std::path::Path) -> Option<String> {
    use std::process::Command;

    let output = Command::new("git")
        .args(["-C", repo_path.to_str().unwrap_or("."), "rev-parse", "HEAD"])
        .output()
        .ok()?;

    if output.status.success() {
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Some(sha.chars().take(12).collect())
    } else {
        None
    }
}

/// Process a single auto_fresh app: check if behind upstream, pull, build if
/// configured, and restart via start_command. Updates app_deploy_state table.
async fn process_auto_fresh_app(app_id: &str, device_id: uuid::Uuid) -> Result<(), String> {
    // Phase P3: lookup app from project.apps, check freshness, pull+build
    let pg = match PgDb::try_global() {
        Some(db) => db,
        None => {
            debug!(
                "fleet::auto_fresh_engine::process_app: app_id={}: no runner DB available",
                app_id
            );
            return Ok(());
        }
    };

    // Get app from registry
    let app = match pg.get_app(app_id).await {
        Ok(Some(a)) => a,
        Ok(None) => {
            warn!(
                "fleet::auto_fresh_engine::process_app: app_id={} not found in project.apps",
                app_id
            );
            return Ok(());
        }
        Err(e) => {
            return Err(format!("get_app({}): {}", app_id, e));
        }
    };

    let repo_path = std::path::Path::new(&app.repo_root);
    if !repo_path.exists() {
        warn!(
            "fleet::auto_fresh_engine::process_app: app_id={} repo_root does not exist: {}",
            app_id, app.repo_root
        );
        return Ok(());
    }

    // Step 1: Idle-aware guard — never interrupt running tests
    if runner_has_active_tasks().await {
        debug!(
            "fleet::auto_fresh_engine::process_app: app_id={} skipping (active task-runs)",
            app_id
        );
        return Ok(());
    }

    // Step 2: Check if tree is behind upstream via git
    let is_behind = match check_if_behind(repo_path) {
        Ok(behind) => behind,
        Err(e) => {
            warn!(
                "fleet::auto_fresh_engine::process_app: app_id={} \
                 failed to check upstream: {}",
                app_id, e
            );
            return Ok(());
        }
    };

    if !is_behind {
        debug!(
            "fleet::auto_fresh_engine::process_app: app_id={} is up to date",
            app_id
        );
        return Ok(());
    }

    info!(
        "fleet::auto_fresh_engine::process_app: app_id={} is behind upstream, \
         update_strategy={}",
        app_id, app.update_strategy
    );

    // Step 3: Pull updated source code
    match pull_and_update_app(repo_path).await {
        Ok((success, message)) => {
            if success {
                let deployed_sha = get_current_head_sha(repo_path);
                info!(
                    "fleet::auto_fresh_engine::process_app: app_id={} \
                     pull succeeded, update_strategy={}, deployed_sha={}",
                    app_id, app.update_strategy,
                    deployed_sha.as_deref().unwrap_or("unknown")
                );

                // Step 4: On pull_build strategy, run build and restart
                if app.update_strategy == "pull_build" {
                    if let Err(e) = execute_build_and_restart(&app, app_id).await {
                        warn!(
                            "fleet::auto_fresh_engine::process_app: app_id={} \
                             build/restart failed: {}",
                            app_id, e
                        );
                        // Record failure to app_deploy_state (best-effort)
                        if let Some(pg) = PgDb::try_global() {
                            pg.update_app_deploy_state_best_effort(
                                device_id,
                                app_id,
                                None,
                                "failed",
                                Some(&e),
                            )
                            .await;
                        }
                        return Ok(());
                    }

                    // Record success to app_deploy_state (best-effort)
                    if let Some(pg) = PgDb::try_global() {
                        pg.update_app_deploy_state_best_effort(
                            device_id,
                            app_id,
                            deployed_sha.as_deref(),
                            "fresh",
                            None,
                        )
                        .await;
                    }

                    info!(
                        "fleet::auto_fresh_engine::process_app: app_id={} \
                         build and restart complete",
                        app_id
                    );
                } else {
                    // pull_only: record fresh state (best-effort)
                    if let Some(pg) = PgDb::try_global() {
                        pg.update_app_deploy_state_best_effort(
                            device_id,
                            app_id,
                            deployed_sha.as_deref(),
                            "fresh",
                            None,
                        )
                        .await;
                    }

                    info!(
                        "fleet::auto_fresh_engine::process_app: app_id={} \
                         pull_only strategy complete",
                        app_id
                    );
                }
            } else {
                warn!(
                    "fleet::auto_fresh_engine::process_app: app_id={} \
                     pull failed: {}",
                    app_id, message
                );
                // Record failure to app_deploy_state (best-effort)
                if let Some(pg) = PgDb::try_global() {
                    pg.update_app_deploy_state_best_effort(
                        device_id,
                        app_id,
                        None,
                        "failed",
                        Some(&message),
                    )
                    .await;
                }
            }
        }
        Err(e) => {
            return Err(format!(
                "fleet::auto_fresh_engine::process_app: app_id={} pull error: {}",
                app_id, e
            ));
        }
    }

    Ok(())
}

/// Check if a git repository is behind upstream (origin/<default>).
fn check_if_behind(repo_path: &std::path::Path) -> Result<bool, String> {
    use std::process::Command;

    // First resolve the default branch
    let default_branch = resolve_default_branch(repo_path);

    // Check behind_count via git rev-list
    let output = Command::new("git")
        .args([
            "-C",
            repo_path.to_str().unwrap_or("."),
            "rev-list",
            "--count",
            &format!("HEAD..origin/{}", default_branch),
        ])
        .output()
        .map_err(|e| format!("git rev-list failed: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let count_str = String::from_utf8_lossy(&output.stdout);
    let behind_count: i32 = count_str
        .trim()
        .parse()
        .map_err(|e| format!("parse behind_count: {}", e))?;

    Ok(behind_count > 0)
}

/// Pull updated source code from origin. Returns (success, message).
///
/// Enforces the /pull-scoped hard rules the repo's pull executor lives by:
/// the tree must be ON the default branch AND clean, or we refuse — an
/// auto-fresh engine must never disturb operator WIP or a parked feature
/// branch. `--ff-only` additionally refuses diverged history.
async fn pull_and_update_app(repo_path: &std::path::Path) -> Result<(bool, String), String> {
    use std::process::Command;

    let repo_str = repo_path.to_str().unwrap_or(".");
    let default_branch = resolve_default_branch(repo_path);

    // Guard 1: must be parked on the default branch.
    let head = Command::new("git")
        .args(["-C", repo_str, "symbolic-ref", "--short", "-q", "HEAD"])
        .output()
        .map_err(|e| format!("git symbolic-ref failed: {}", e))?;
    let on_branch = String::from_utf8_lossy(&head.stdout).trim().to_string();
    if on_branch != default_branch {
        return Ok((
            false,
            format!(
                "refused: tree is on '{}' not default '{}' — auto-fresh never \
                 switches or pulls a non-default branch",
                on_branch, default_branch
            ),
        ));
    }

    // Guard 2: tree must be clean (uncommitted WIP is an implicit claim).
    let porcelain = Command::new("git")
        .args(["-C", repo_str, "status", "--porcelain"])
        .output()
        .map_err(|e| format!("git status failed: {}", e))?;
    if !porcelain.stdout.is_empty() {
        return Ok((
            false,
            "refused: working tree has uncommitted changes — auto-fresh never \
             pulls over WIP"
                .to_string(),
        ));
    }

    let output = Command::new("git")
        .args(["-C", repo_str, "pull", "--ff-only", "origin", &default_branch])
        .output()
        .map_err(|e| format!("git pull failed: {}", e))?;

    let success = output.status.success();
    let message = String::from_utf8_lossy(if success {
        &output.stdout
    } else {
        &output.stderr
    });

    Ok((success, message.to_string()))
}

/// Run a configured shell command in `cwd` via the platform shell —
/// `cmd /C` on Windows, `sh -c` elsewhere (same split as agent_runtime's
/// spawn path).
fn run_shell_command(cwd: &str, command: &str) -> std::io::Result<std::process::Output> {
    use std::process::Command;
    if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/C", command])
            .current_dir(cwd)
            .output()
    } else {
        Command::new("sh")
            .args(["-c", command])
            .current_dir(cwd)
            .output()
    }
}

/// Execute build_command and start_command for pull_build strategy.
async fn execute_build_and_restart(
    app: &qontinui_types::apps::App,
    app_id: &str,
) -> Result<(), String> {
    // Execute build_command if present
    if let Some(ref build_cmd) = app.build_command {
        info!(
            "fleet::auto_fresh_engine::execute_build: app_id={} \
             running build_command: {}",
            app_id, build_cmd
        );

        let output = run_shell_command(&app.repo_root, build_cmd)
            .map_err(|e| format!("build_command failed: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("build_command error: {}", stderr));
        }

        debug!("fleet::auto_fresh_engine::execute_build: build_command succeeded");
    }

    // Execute start_command if present
    if let Some(ref start_cmd) = app.start_command {
        info!(
            "fleet::auto_fresh_engine::execute_start: app_id={} \
             running start_command: {}",
            app_id, start_cmd
        );

        let output = run_shell_command(&app.repo_root, start_cmd)
            .map_err(|e| format!("start_command failed: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("start_command error: {}", stderr));
        }

        debug!("fleet::auto_fresh_engine::execute_start: start_command succeeded");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- repo-freshness Layer 1 — fetch interval gate ----

    #[test]
    fn fetch_due_first_call_true_then_immediate_false() {
        // Unique path so the process-global last-fetch map can't collide with
        // another test. The interval is >= 5s for any env config, so an
        // immediately-following call is never due regardless of the knob.
        let p = std::path::PathBuf::from(format!(
            "/tmp/freshness-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let t0 = std::time::Instant::now();
        // Never-fetched repo is due, and records the attempt.
        assert!(fetch_due(&p, t0), "first call should be due");
        // A call right after (well inside the >=5s interval) is not due.
        assert!(
            !fetch_due(&p, std::time::Instant::now()),
            "immediate re-call should not be due"
        );
    }

    #[test]
    fn tree_fetch_interval_floored_at_5s() {
        // Even with no env set the default is 60s; the floor guarantees >= 5s.
        assert!(tree_fetch_interval() >= Duration::from_secs(5));
    }

    // ---- chkguard Phase 3 — restore_default executor (real temp git) ----

    /// Run git in `dir`, asserting success. Identity pinned per-invocation so
    /// the temp repos commit without global config.
    fn tgit(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} in {dir:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn ensure_repo_info_exclude_heals_existing_untracked_artifacts() {
        let dir =
            std::env::temp_dir().join(format!("fleet-exclude-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        tgit(&dir, &["init", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        tgit(&dir, &["add", "."]);
        tgit(&dir, &["commit", "-m", "A"]);

        // The stray machine artifacts already present BEFORE we exclude — the
        // retroactive-heal case that obviates a `.gitignore` sweep.
        std::fs::write(dir.join(".mcp.json"), "{}").unwrap();
        std::fs::create_dir_all(dir.join("agent-worktrees")).unwrap();
        std::fs::write(dir.join("agent-worktrees").join("x"), "x").unwrap();
        assert!(
            tgit(&dir, &["status", "--porcelain"]).contains(".mcp.json"),
            "precondition: artifacts make the tree dirty before excluding"
        );

        ensure_repo_info_exclude(&dir);

        assert!(
            tgit(&dir, &["status", "--porcelain"]).is_empty(),
            "tree must read clean once artifacts are excluded"
        );
        let exclude =
            std::fs::read_to_string(dir.join(".git").join("info").join("exclude")).unwrap();
        assert!(exclude.contains(".mcp.json") && exclude.contains("agent-worktrees/"));

        // Idempotent: a second pass changes nothing (no duplicate lines/marker).
        let first = exclude;
        ensure_repo_info_exclude(&dir);
        let second =
            std::fs::read_to_string(dir.join(".git").join("info").join("exclude")).unwrap();
        assert_eq!(first, second, "second call must be a no-op");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ensure_repo_info_exclude_skips_linked_worktree() {
        // A dir whose `.git` is a FILE (linked-worktree shape) is skipped so we
        // never write into a shared/foreign git dir.
        let dir = std::env::temp_dir().join(format!(
            "fleet-exclude-wt-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".git"), "gitdir: /somewhere/else").unwrap();
        ensure_repo_info_exclude(&dir); // must not panic
        assert!(!dir.join(".git").join("info").join("exclude").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A parked-on-squash-merged-branch fixture: a bare origin whose `main`
    /// advanced past the merge while the work tree sits clean on `feat/x`.
    /// Returns `(tempdir, work_path, feat_head_sha)`.
    fn parked_fixture(name: &str) -> (std::path::PathBuf, std::path::PathBuf, String) {
        let base = std::env::temp_dir().join(format!(
            "chkguard-restore-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let origin = base.join("origin.git");
        let work = base.join("work");
        std::fs::create_dir_all(&origin).unwrap();
        tgit(&base, &["init", "--bare", "-b", "main", "origin.git"]);
        std::fs::create_dir_all(&work).unwrap();
        tgit(
            &base,
            &["clone", origin.to_str().unwrap(), work.to_str().unwrap()],
        );
        std::fs::write(work.join("a.txt"), "a").unwrap();
        tgit(&work, &["add", "."]);
        tgit(&work, &["commit", "-m", "A"]);
        tgit(&work, &["push", "origin", "HEAD:main"]);
        // Feature branch with one commit (the PR head).
        tgit(&work, &["switch", "-c", "feat/x"]);
        std::fs::write(work.join("b.txt"), "b").unwrap();
        tgit(&work, &["add", "."]);
        tgit(&work, &["commit", "-m", "B"]);
        let feat_head = tgit(&work, &["rev-parse", "HEAD"]);
        // Simulate the squash-merge landing on origin/main (content of B as a
        // single new commit C), pushed from a scratch clone so the work tree
        // stays parked on feat/x with a stale origin/main ref.
        let scratch = base.join("scratch");
        tgit(
            &base,
            &["clone", origin.to_str().unwrap(), scratch.to_str().unwrap()],
        );
        std::fs::write(scratch.join("b.txt"), "b").unwrap();
        tgit(&scratch, &["add", "."]);
        tgit(&scratch, &["commit", "-m", "C (squash of feat/x)"]);
        tgit(&scratch, &["push", "origin", "HEAD:main"]);
        (base, work, feat_head)
    }

    fn restore_params(feat_head: &str) -> RestoreParams {
        RestoreParams {
            parked_branch: "feat/x".to_string(),
            expected_head_sha: feat_head.to_string(),
            pr_number: Some(42),
        }
    }

    #[test]
    fn restore_default_happy_path_switches_ffs_and_deletes_branch() {
        let (base, work, feat_head) = parked_fixture("happy");
        let out = apply_restore_default_blocking(
            &work,
            work.to_str().unwrap(),
            "main",
            &restore_params(&feat_head),
        );
        assert_eq!(
            out.chosen_option, "restored",
            "reasoning: {}",
            out.reasoning
        );
        // On main, fast-forwarded to the squash-merge commit.
        assert_eq!(tgit(&work, &["symbolic-ref", "--short", "HEAD"]), "main");
        assert_eq!(
            tgit(&work, &["rev-parse", "HEAD"]),
            tgit(&work, &["rev-parse", "origin/main"])
        );
        // Parked branch safe-deleted (-D: squash-merged tips never pass -d).
        let branches = tgit(&work, &["branch", "--list", "feat/x"]);
        assert!(
            branches.is_empty(),
            "feat/x must be deleted, got {branches:?}"
        );
        // The squashed content is present.
        assert!(work.join("b.txt").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_default_refuses_on_head_sha_mismatch() {
        // The 4/26 calibration shape: local head != merged PR head (e.g. a
        // commit landed on the parked branch after the PR merged).
        let (base, work, _feat_head) = parked_fixture("shamismatch");
        let p = restore_params("0000000000000000000000000000000000000000");
        let out = apply_restore_default_blocking(&work, work.to_str().unwrap(), "main", &p);
        assert_eq!(out.chosen_option, "restore_skipped_recheck");
        // Untouched: still parked on feat/x.
        assert_eq!(tgit(&work, &["symbolic-ref", "--short", "HEAD"]), "feat/x");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_default_refuses_dirty_tree() {
        let (base, work, feat_head) = parked_fixture("dirty");
        std::fs::write(work.join("wip.txt"), "uncommitted").unwrap();
        let out = apply_restore_default_blocking(
            &work,
            work.to_str().unwrap(),
            "main",
            &restore_params(&feat_head),
        );
        assert_eq!(out.chosen_option, "restore_skipped_recheck");
        assert_eq!(tgit(&work, &["symbolic-ref", "--short", "HEAD"]), "feat/x");
        assert!(work.join("wip.txt").exists(), "WIP must never be touched");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_default_refuses_branch_moved_since_verdict() {
        let (base, work, feat_head) = parked_fixture("moved");
        tgit(&work, &["switch", "-c", "other-branch"]);
        let out = apply_restore_default_blocking(
            &work,
            work.to_str().unwrap(),
            "main",
            &restore_params(&feat_head),
        );
        assert_eq!(out.chosen_option, "restore_skipped_recheck");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_default_refuses_non_primary_checkout() {
        // A linked worktree's `.git` is a FILE — restore must refuse before
        // any git call (branch switches there collide with the primary).
        let base = std::env::temp_dir().join(format!(
            "chkguard-restore-wt-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join(".git"), "gitdir: ../somewhere/.git/worktrees/x").unwrap();
        let p = restore_params("abc");
        let out = apply_restore_default_blocking(&base, base.to_str().unwrap(), "main", &p);
        assert_eq!(out.chosen_option, "restore_skipped_recheck");
        assert!(out.reasoning.contains("not a primary checkout"));
        let _ = std::fs::remove_dir_all(&base);
    }

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
            tenant_ids: vec![uuid::Uuid::nil()],
            capture_preview_count: 0,
            monitor_crop_count: 0,
            last_capture_fallback_at: None,
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
            tenant_ids: vec![uuid::Uuid::nil()],
            capture_preview_count: 0,
            monitor_crop_count: 0,
            last_capture_fallback_at: None,
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
            tenant_ids: vec![tenant],
            capture_preview_count: 0,
            monitor_crop_count: 0,
            last_capture_fallback_at: None,
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
            tenant_ids: vec![uuid::Uuid::nil()],
            capture_preview_count: 0,
            monitor_crop_count: 0,
            last_capture_fallback_at: None,
        };
        let body = serde_json::to_value(&p).unwrap();
        assert_eq!(
            body.get("claude_code_available").and_then(|v| v.as_bool()),
            Some(true),
            "claude_code_available must remain on the heartbeat wire (PR #216 shape)"
        );
    }

    /// Capture-backend telemetry (plan 2026-06-07-fleet-capture-backend-
    /// telemetry.md D1): the two counters always serialize (even at 0, the
    /// honest "no captures yet" baseline) so coord can straight-write them;
    /// `last_capture_fallback_at` is omitted when `None` (skip_serializing_if)
    /// so coord COALESCE-preserves the last observed fallback. Coord ingest
    /// tolerates absent fields via COALESCE for legacy compatibility.
    #[test]
    fn heartbeat_payload_serializes_capture_backend_counters() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-06-07T12:34:56Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let p = HeartbeatPayload {
            device_id: uuid::Uuid::nil(),
            hostname: "test".into(),
            claude_code_available: false,
            tenant_id: uuid::Uuid::nil(),
            tenant_ids: vec![uuid::Uuid::nil()],
            capture_preview_count: 7,
            monitor_crop_count: 3,
            last_capture_fallback_at: Some(at),
        };
        let body = serde_json::to_value(&p).unwrap();
        assert_eq!(
            body.get("capture_preview_count").and_then(|v| v.as_u64()),
            Some(7),
            "capture_preview_count must ride the heartbeat wire"
        );
        assert_eq!(
            body.get("monitor_crop_count").and_then(|v| v.as_u64()),
            Some(3),
            "monitor_crop_count must ride the heartbeat wire"
        );
        assert!(
            body.get("last_capture_fallback_at")
                .and_then(|v| v.as_str())
                .is_some(),
            "last_capture_fallback_at must serialize when present"
        );

        // Absent fallback timestamp is omitted (COALESCE-preserve coord-side);
        // counters still present at 0.
        let p0 = HeartbeatPayload {
            device_id: uuid::Uuid::nil(),
            hostname: "test".into(),
            claude_code_available: false,
            tenant_id: uuid::Uuid::nil(),
            tenant_ids: vec![uuid::Uuid::nil()],
            capture_preview_count: 0,
            monitor_crop_count: 0,
            last_capture_fallback_at: None,
        };
        let body0 = serde_json::to_value(&p0).unwrap();
        assert!(
            body0.get("last_capture_fallback_at").is_none(),
            "absent last_capture_fallback_at must be omitted from the wire"
        );
        assert_eq!(
            body0.get("capture_preview_count").and_then(|v| v.as_u64()),
            Some(0),
            "capture_preview_count must serialize even at 0 (honest baseline)"
        );
        assert_eq!(
            body0.get("monitor_crop_count").and_then(|v| v.as_u64()),
            Some(0),
            "monitor_crop_count must serialize even at 0 (honest baseline)"
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

    /// Phase 8a reconciliation gate: `response_tenant_ids` must be
    /// strictly fail-soft. `None` (→ NO reconciliation, the no-op path
    /// against today's production coord) for: absent field, non-JSON
    /// body, non-array field, or ANY malformed element. Present-but-empty
    /// is `Some(vec![])` — a meaningful "zero bindings" statement.
    #[test]
    fn response_tenant_ids_is_fail_soft() {
        let a = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let b = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

        // Phase-3 coord shape: full set parses.
        let body = format!(r#"{{"tenant_id":"{a}","tenant_ids":["{a}","{b}"]}}"#);
        assert_eq!(
            response_tenant_ids(&body),
            Some(vec![
                uuid::Uuid::parse_str(a).unwrap(),
                uuid::Uuid::parse_str(b).unwrap()
            ])
        );

        // Today's production coord: field ABSENT → None → caller no-ops.
        assert_eq!(
            response_tenant_ids(&format!(r#"{{"tenant_id":"{a}"}}"#)),
            None,
            "absent tenant_ids (today's coord) must be None — reconciliation no-op"
        );
        assert_eq!(response_tenant_ids("not json"), None);
        assert_eq!(
            response_tenant_ids(r#"{"tenant_ids":"not-an-array"}"#),
            None,
            "non-array tenant_ids → None"
        );
        assert_eq!(
            response_tenant_ids(&format!(r#"{{"tenant_ids":["{a}","junk"]}}"#)),
            None,
            "ANY malformed element poisons the set — a partial set must never drive drops"
        );
        assert_eq!(
            response_tenant_ids(&format!(r#"{{"tenant_ids":["{a}",42]}}"#)),
            None,
            "non-string element → None"
        );

        // Present-but-empty IS meaningful (zero server-side bindings).
        assert_eq!(
            response_tenant_ids(r#"{"tenant_ids":[]}"#),
            Some(vec![]),
            "empty array → Some(empty): coord says zero bindings"
        );
    }

    /// Phase 8a wire shape: the register heartbeat carries the whole
    /// binding set as `tenant_ids` alongside the legacy single
    /// `tenant_id` (today's coord ignores the new field — its
    /// `DeviceRegisterRequest` has no `deny_unknown_fields`).
    #[test]
    fn heartbeat_payload_serializes_binding_set() {
        let a = uuid::Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let b = uuid::Uuid::parse_str("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
        let p = HeartbeatPayload {
            device_id: uuid::Uuid::nil(),
            hostname: "test".into(),
            claude_code_available: false,
            tenant_id: a,
            tenant_ids: vec![a, b],
            capture_preview_count: 0,
            monitor_crop_count: 0,
            last_capture_fallback_at: None,
        };
        let body = serde_json::to_value(&p).unwrap();
        assert_eq!(
            body.get("tenant_id").and_then(|v| v.as_str()),
            Some(a.to_string().as_str()),
            "legacy single tenant_id (the DEFAULT binding) must stay on the wire"
        );
        let ids: Vec<String> = body
            .get("tenant_ids")
            .and_then(|v| v.as_array())
            .expect("tenant_ids array on the wire")
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        assert_eq!(ids, vec![a.to_string(), b.to_string()]);
    }

    // ---- behind_default_count (stale-primary-checkout guard, Phase 1c) ----

    /// The gating helper: `behind_default_count` is only meaningful when
    /// parked on a non-default named branch. On the default branch
    /// `behind_count` already covers the distance, and detached/empty
    /// branch states can't name a comparison branch.
    #[test]
    fn behind_default_compare_branch_only_on_nondefault_named_branch() {
        // Non-default named branch → compare against the default.
        assert_eq!(
            behind_default_compare_branch("chkguard/foo", "main"),
            Some("main"),
            "a feature branch must compare against origin/<default>"
        );
        // On the default branch → None (behind_count already covers it).
        assert_eq!(
            behind_default_compare_branch("main", "main"),
            None,
            "on the default branch there's nothing extra to compute"
        );
        // Detached HEAD → None (no meaningful comparison branch).
        assert_eq!(
            behind_default_compare_branch("(detached)", "main"),
            None,
            "detached HEAD has no comparison branch"
        );
        // Empty branch (defensive) → None.
        assert_eq!(
            behind_default_compare_branch("", "main"),
            None,
            "empty branch name has no comparison branch"
        );
        // Non-`main` default is honored.
        assert_eq!(
            behind_default_compare_branch("topic", "develop"),
            Some("develop"),
            "the configured default branch is what we compare against"
        );
    }

    /// `behind_default_count` is `#[serde(skip_serializing_if = Option::is_none)]`
    /// so the default-branch case (where it's `None`) keeps the same wire
    /// shape as before this field existed — no regression for coord ingest.
    #[test]
    fn tree_payload_omits_none_behind_default_count() {
        let p = TreeStatePayload {
            device_id: uuid::Uuid::nil(),
            repo: "qontinui-runner".into(),
            branch: "main".into(),
            head_sha: "deadbeef".into(),
            dirty: false,
            dirty_files: None,
            last_edit_at: None,
            behind_count: Some(0),
            head_detached: Some(false),
            untracked_count: Some(0),
            local_ahead: Some(0),
            behind_default_count: None,
            dirty_total: None,
            tenant_id: None,
            app_id: None,
        };
        let body = serde_json::to_value(&p).unwrap();
        assert!(
            body.get("behind_default_count").is_none(),
            "None behind_default_count must be omitted from the upsert wire"
        );
    }

    /// When set (the stale-feature-branch case), `behind_default_count`
    /// appears on the wire as a number so coord's watcher can consume it.
    #[test]
    fn tree_payload_serializes_behind_default_count() {
        let p = TreeStatePayload {
            device_id: uuid::Uuid::nil(),
            repo: "qontinui-runner".into(),
            branch: "chkguard/stale".into(),
            head_sha: "deadbeef".into(),
            dirty: false,
            dirty_files: None,
            last_edit_at: None,
            behind_count: Some(0),
            head_detached: Some(false),
            untracked_count: Some(0),
            local_ahead: Some(0),
            behind_default_count: Some(42),
            dirty_total: None,
            tenant_id: None,
            app_id: None,
        };
        let body = serde_json::to_value(&p).unwrap();
        assert_eq!(
            body.get("behind_default_count").and_then(|v| v.as_i64()),
            Some(42),
            "a set behind_default_count must appear on the upsert wire"
        );
        // Phase 8b item 7 — a None tenant_id is omitted (pre-8b wire shape
        // preserved for unpaired runners)…
        assert!(
            body.get("tenant_id").is_none(),
            "None tenant_id must be omitted from the upsert wire"
        );
    }

    /// Phase 8b item 7 — the tree upsert goes EXPLICIT: a resolved default
    /// binding appears as `tenant_id` on the wire.
    #[test]
    fn tree_payload_serializes_explicit_tenant_id() {
        let t = uuid::Uuid::from_bytes([0x5A; 16]);
        let p = TreeStatePayload {
            device_id: uuid::Uuid::nil(),
            repo: "qontinui-runner".into(),
            branch: "main".into(),
            head_sha: "deadbeef".into(),
            dirty: false,
            dirty_files: None,
            last_edit_at: None,
            behind_count: Some(0),
            head_detached: Some(false),
            untracked_count: Some(0),
            local_ahead: Some(0),
            behind_default_count: None,
            dirty_total: None,
            tenant_id: Some(t),
        };
        let body = serde_json::to_value(&p).unwrap();
        assert_eq!(
            body.get("tenant_id").and_then(|v| v.as_str()),
            Some(t.to_string().as_str()),
            "an explicit tenant_id must appear on the upsert wire"
        );
    }
}
