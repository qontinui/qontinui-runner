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

use serde::Deserialize;
use tracing::{info, warn};

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
    let memory_gb: u32 = (sys.total_memory() / (1024 * 1024 * 1024))
        .min(u32::MAX as u64) as u32;

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

    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool: {e}"))?;

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
        warn!(
            "fleet::publish_on_startup failed (non-fatal — runner still boots): {e}"
        );
    }
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
        assert!(r.cpu_cores >= 1, "expected ≥1 cpu_core, got {}", r.cpu_cores);
        assert!(r.memory_gb >= 1, "expected ≥1 GiB RAM, got {}", r.memory_gb);
        assert!(
            r.disk_total_gb >= 1,
            "expected ≥1 GiB disk, got {}",
            r.disk_total_gb
        );
    }
}
