//! Background health monitor for secondary runner instances.
//!
//! Spawned by the primary runner on startup. Periodically checks the
//! `runner_instances` table for stale heartbeats, probes unhealthy instances
//! via HTTP, and cleans up dead entries.

use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;
use tracing::{debug, info, warn};

use crate::database::pg::PgDb;
use crate::instance_manager::InstanceManager;

/// How often the health monitor runs (seconds).
const CHECK_INTERVAL_SECS: u64 = 30;
/// Heartbeat older than this → mark unhealthy (seconds).
const STALE_THRESHOLD_SECS: i64 = 45;
/// Unhealthy for longer than this → remove from registry (seconds).
const DEAD_THRESHOLD_SECS: i64 = 120;

/// Start the instance health monitor background task.
///
/// Only the primary runner should call this. Secondaries do not monitor other instances.
pub fn start_instance_health_monitor(
    pg_db: Arc<PgDb>,
    instance_manager: Arc<InstanceManager>,
) {
    // Only run on the primary
    if crate::instance::is_secondary() {
        return;
    }

    info!("Starting instance health monitor (check every {}s, stale after {}s, dead after {}s)",
        CHECK_INTERVAL_SECS, STALE_THRESHOLD_SECS, DEAD_THRESHOLD_SECS);

    tauri::async_runtime::spawn(async move {
        // Wait for startup to settle
        tokio::time::sleep(Duration::from_secs(15)).await;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap_or_default();

        let mut ticker = interval(Duration::from_secs(CHECK_INTERVAL_SECS));

        loop {
            ticker.tick().await;

            // 1. Mark instances with stale heartbeats as unhealthy
            match pg_db.mark_stale_runner_instances(STALE_THRESHOLD_SECS).await {
                Ok(count) if count > 0 => {
                    info!("Marked {} instance(s) as unhealthy (heartbeat stale)", count);
                }
                Err(e) => {
                    debug!("Failed to mark stale instances: {}", e);
                }
                _ => {}
            }

            // 2. Probe unhealthy instances — if they respond, mark healthy again
            if let Ok(instances) = pg_db.get_all_runner_instances().await {
                for inst in &instances {
                    if inst.status != "unhealthy" || inst.is_primary {
                        continue;
                    }
                    let url = format!("http://localhost:{}/health", inst.port);
                    match client.get(&url).send().await {
                        Ok(resp) if resp.status().is_success() => {
                            // Instance recovered — mark healthy
                            let _ = pg_db
                                .update_runner_instance_heartbeat(&inst.id, None, "healthy")
                                .await;
                            debug!("Instance '{}' (port {}) recovered", inst.name, inst.port);
                        }
                        _ => {
                            debug!(
                                "Instance '{}' (port {}) still unhealthy",
                                inst.name, inst.port
                            );
                        }
                    }
                }
            }

            // 3. Clean up dead instances (unhealthy beyond threshold)
            match pg_db.cleanup_dead_runner_instances(DEAD_THRESHOLD_SECS).await {
                Ok(count) if count > 0 => {
                    info!("Cleaned up {} dead instance(s)", count);
                }
                Err(e) => {
                    debug!("Failed to cleanup dead instances: {}", e);
                }
                _ => {}
            }

            // 4. Clean up in-memory child process handles for dead processes
            let _ = instance_manager.get_running_ids().await;
        }
    });
}

/// Purge all stale/dead instances immediately.
///
/// Called by `POST /instances/purge-stale` for on-demand cleanup.
pub async fn purge_stale_instances(
    pg_db: &PgDb,
    instance_manager: &InstanceManager,
) -> (u64, u64) {
    let marked = pg_db
        .mark_stale_runner_instances(STALE_THRESHOLD_SECS)
        .await
        .unwrap_or(0);

    // For on-demand purge: first probe unhealthy instances to give them a
    // chance to respond before deletion.
    if let Ok(instances) = pg_db.get_all_runner_instances().await {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap_or_default();
        for inst in &instances {
            if inst.status != "unhealthy" || inst.is_primary {
                continue;
            }
            let url = format!("http://localhost:{}/health", inst.port);
            if let Ok(resp) = client.get(&url).send().await {
                if resp.status().is_success() {
                    let _ = pg_db
                        .update_runner_instance_heartbeat(&inst.id, None, "healthy")
                        .await;
                }
            }
        }
    }

    // Remove instances that are still unhealthy/stopped after probing
    let cleaned = pg_db
        .cleanup_dead_runner_instances(0)
        .await
        .unwrap_or(0);

    // Also clean up in-memory: remove registered instances whose ports are gone
    let in_mem_purged = instance_manager.purge_unreachable_registered().await;
    let _ = instance_manager.get_running_ids().await;

    (marked, cleaned + in_mem_purged as u64)
}

/// Startup cleanup: remove stale DB entries from a previous session.
pub async fn startup_cleanup(pg_db: &PgDb) {
    // Probe all non-primary instances and remove dead ones
    let instances = match pg_db.get_all_runner_instances().await {
        Ok(i) => i,
        Err(e) => {
            warn!("Startup cleanup: failed to read instances: {}", e);
            return;
        }
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap_or_default();

    let mut removed = 0u32;
    for inst in &instances {
        if inst.is_primary {
            continue;
        }
        let url = format!("http://localhost:{}/health", inst.port);
        let alive = client
            .get(&url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);

        if !alive {
            let _ = pg_db.remove_runner_instance(&inst.id).await;
            removed += 1;
            debug!(
                "Startup cleanup: removed dead instance '{}' (port {})",
                inst.name, inst.port
            );
        }
    }

    if removed > 0 {
        info!(
            "Startup cleanup: removed {} dead instance(s) from registry",
            removed
        );
    }
}
