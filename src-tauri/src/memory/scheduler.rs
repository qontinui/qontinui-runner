//! Background scheduler for memory consolidation, decay, and importance recalculation.
//!
//! Periodically runs the 4-phase consolidation pipeline (orient, gather, consolidate, prune)
//! and persists high-confidence learnings to PostgreSQL for cross-session survival.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::interval;
use tracing::{debug, info, warn};

use super::consolidation::{self, ConsolidationConfig};
use crate::database::pg::PgDb;

/// Configuration for the memory consolidation scheduler.
pub struct MemorySchedulerConfig {
    /// How often to run consolidation (default: 10 minutes).
    pub interval_secs: u64,
    /// Initial delay before first run (default: 60 seconds).
    pub initial_delay_secs: u64,
    /// Consolidation pipeline configuration.
    pub consolidation: ConsolidationConfig,
    /// How often to run the dreamer (default: 8 hours).
    pub dreamer_interval_secs: u64,
    /// Initial delay before first dreamer run (default: 30 minutes).
    pub dreamer_initial_delay_secs: u64,
}

impl Default for MemorySchedulerConfig {
    fn default() -> Self {
        Self {
            interval_secs: 600, // 10 minutes
            initial_delay_secs: 60,
            consolidation: ConsolidationConfig::default(),
            dreamer_interval_secs: 28800,     // 8 hours
            dreamer_initial_delay_secs: 1800, // 30 minutes
        }
    }
}

/// Start the memory consolidation scheduler as a background task.
///
/// Spawns via `tauri::async_runtime::spawn` to integrate with Tauri's runtime,
/// following the same pattern as `start_embedding_job`.
pub fn start_memory_scheduler(
    pg: Arc<PgDb>,
    config: MemorySchedulerConfig,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        // Initial delay to let the app finish starting up
        tokio::time::sleep(Duration::from_secs(config.initial_delay_secs)).await;
        info!(
            "Memory consolidation scheduler started (interval: {}s)",
            config.interval_secs
        );

        let mut tick = interval(Duration::from_secs(config.interval_secs));

        loop {
            tick.tick().await;

            // Check cooldown before running expensive consolidation
            if !consolidation::can_run_consolidation(&pg, config.consolidation.cooldown_hours).await
            {
                debug!("Memory consolidation: cooldown active, skipping cycle");
                continue;
            }

            debug!("Running memory consolidation cycle");
            match consolidation::run_consolidation(&pg, &config.consolidation).await {
                Ok(stats) => {
                    info!(
                        "Memory consolidation complete: scanned={}, groups={}, models_created={}, merged={}, archived={}",
                        stats.observations_scanned,
                        stats.groups_found,
                        stats.models_created,
                        stats.observations_merged,
                        stats.observations_archived,
                    );
                }
                Err(e) => warn!("Memory consolidation failed: {}", e),
            }

            // Cleanup expired working representations
            if let Err(e) = pg.cleanup_expired_representations().await {
                debug!("WR cleanup failed: {}", e);
            }
        }
    })
}

/// Start the dreamer scheduler as a background task.
///
/// Runs the formal reasoning cycle (inductive, deductive, abductive) on a
/// longer interval than standard consolidation. Includes its own cooldown check.
pub fn start_dreamer_scheduler(
    pg: Arc<PgDb>,
    config: MemorySchedulerConfig,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(config.dreamer_initial_delay_secs)).await;
        info!(
            "Dreamer scheduler started (interval: {}s)",
            config.dreamer_interval_secs
        );

        let mut tick = interval(Duration::from_secs(config.dreamer_interval_secs));

        loop {
            tick.tick().await;

            // Check cooldown (dreamer-specific)
            match pg.get_last_dreamer_time().await {
                Ok(Some(last)) => {
                    let hours_since = (chrono::Utc::now() - last).num_seconds() as f64 / 3600.0;
                    if hours_since < (config.dreamer_interval_secs as f64 / 3600.0) * 0.9 {
                        debug!(
                            "Dreamer: cooldown active ({:.1}h since last run), skipping",
                            hours_since
                        );
                        continue;
                    }
                }
                Ok(None) => {} // First run ever
                Err(e) => {
                    warn!("Dreamer: failed to check last run time: {}", e);
                    continue;
                }
            }

            debug!("Running dreamer cycle");
            match consolidation::run_dreamer(&pg, &config.consolidation).await {
                Ok(stats) => {
                    info!(
                        "Dreamer complete: inductive={}, deductive={}, abductive={}, observations={}",
                        stats.inductive_traces,
                        stats.deductive_traces,
                        stats.abductive_traces,
                        stats.observations_created,
                    );
                }
                Err(e) => warn!("Dreamer failed: {}", e),
            }
        }
    })
}
