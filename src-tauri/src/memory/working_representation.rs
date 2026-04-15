//! Working Representations — pre-computed context snapshots for worker prompts.
//!
//! At task-run start the builder fans out parallel queries to collect observations,
//! cross-run patterns, entity profiles, findings, fixes, and skills. The result
//! is cached in memory (per task-run) and persisted to PG for resume.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::database::pg::PgDb;
use crate::database::types::*;

// ── Global cache instance ───────────────────────────────────────────────────

/// Global WorkingRepresentationCache instance, set once during app initialization.
/// Allows code that cannot thread Arc through (e.g. consolidation service) to
/// access the cache without plumbing it through every call chain.
static GLOBAL_WR_CACHE: OnceLock<Arc<WorkingRepresentationCache>> = OnceLock::new();

impl WorkingRepresentationCache {
    /// Set the global cache instance. Call once during app initialization.
    pub fn set_global(cache: Arc<WorkingRepresentationCache>) {
        GLOBAL_WR_CACHE.set(cache).unwrap_or_else(|_| {
            warn!("WorkingRepresentationCache::set_global called more than once (ignored)")
        });
    }

    /// Get the global cache instance. Returns None if not initialized.
    pub fn try_global() -> Option<Arc<WorkingRepresentationCache>> {
        GLOBAL_WR_CACHE.get().cloned()
    }
}

// ── In-memory cache ──────────────────────────────────────────────────────────

/// In-memory cache of working representations keyed by task_run_id.
pub struct WorkingRepresentationCache {
    cache: RwLock<HashMap<String, WorkingRepresentation>>,
}

impl WorkingRepresentationCache {
    pub fn new() -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Get or build a working representation for a task run.
    pub async fn get_or_build(
        &self,
        task_run_id: &str,
        workflow_id: Option<&str>,
        workflow_name: Option<&str>,
        pg: &PgDb,
    ) -> Result<WorkingRepresentation, String> {
        // Fast path: check in-memory cache
        {
            let cache = self.cache.read().await;
            if let Some(wr) = cache.get(task_run_id) {
                if !wr.is_stale {
                    debug!("Working representation cache hit for {}", task_run_id);
                    return Ok(wr.clone());
                }
            }
        }

        // Check PG before building fresh (in case of restart)
        if let Some(global_pg) = crate::database::pg::PgDb::try_global() {
            if let Ok(Some(persisted)) = global_pg.get_working_representation(task_run_id).await {
                if !persisted.is_stale {
                    // Cache it in memory for future hits
                    let mut cache = self.cache.write().await;
                    cache.insert(task_run_id.to_string(), persisted.clone());
                    return Ok(persisted);
                }
            }
        }

        // Build fresh
        let wr = build_working_representation(task_run_id, workflow_id, workflow_name, pg).await?;

        // Store in cache
        {
            let mut cache = self.cache.write().await;
            cache.insert(task_run_id.to_string(), wr.clone());
        }

        // After caching in memory, persist to PG in background
        if let Some(global_pg) = crate::database::pg::PgDb::try_global() {
            let wr_clone = wr.clone();
            tokio::spawn(async move {
                if let Err(e) = global_pg.save_working_representation(&wr_clone).await {
                    tracing::debug!("Failed to persist working representation: {}", e);
                }
            });
        }

        Ok(wr)
    }

    /// Remove a task run's cached representation.
    pub async fn evict(&self, task_run_id: &str) {
        let mut cache = self.cache.write().await;
        cache.remove(task_run_id);
    }

    /// Mark all cached representations as stale so they are rebuilt on next access.
    pub async fn invalidate_all(&self) {
        let mut cache = self.cache.write().await;
        for wr in cache.values_mut() {
            wr.is_stale = true;
        }
    }
}

// ── Builder ──────────────────────────────────────────────────────────────────

/// Build a fresh working representation by querying all relevant sources in parallel.
///
/// This is the public entry point used by orchestrator integration when the
/// in-memory cache is not wired up yet.
pub async fn build_working_representation(
    task_run_id: &str,
    workflow_id: Option<&str>,
    workflow_name: Option<&str>,
    pg: &PgDb,
) -> Result<WorkingRepresentation, String> {
    let start = Instant::now();

    // Fan out parallel queries — each returns Ok(vec) or Err; we default on error
    let (observations, patterns, profiles, findings, fixes, skills) = tokio::join!(
        pg.fetch_observation_snapshots(workflow_name, 20),
        pg.fetch_active_pattern_snapshots(workflow_name, 10),
        pg.fetch_entity_profile_snapshots(15),
        pg.fetch_finding_snapshots(workflow_name, 10),
        pg.fetch_fix_snapshots(workflow_name, 10),
        pg.fetch_skill_snapshots(10),
    );

    let obs = observations.unwrap_or_else(|e| {
        warn!("WR: observations fetch failed: {}", e);
        Vec::new()
    });
    let pats = patterns.unwrap_or_else(|e| {
        warn!("WR: patterns fetch failed: {}", e);
        Vec::new()
    });
    let profs = profiles.unwrap_or_else(|e| {
        warn!("WR: profiles fetch failed: {}", e);
        Vec::new()
    });
    let finds = findings.unwrap_or_else(|e| {
        warn!("WR: findings fetch failed: {}", e);
        Vec::new()
    });
    let fixs = fixes.unwrap_or_else(|e| {
        warn!("WR: fixes fetch failed: {}", e);
        Vec::new()
    });
    let skls = skills.unwrap_or_else(|e| {
        warn!("WR: skills fetch failed: {}", e);
        Vec::new()
    });

    let total = obs.len() + pats.len() + profs.len() + finds.len() + fixs.len() + skls.len();
    let duration_ms = start.elapsed().as_millis() as i64;

    info!(
        "Built working representation for {} in {}ms ({} items: {} obs, {} patterns, {} profiles, {} findings, {} fixes, {} skills)",
        task_run_id, duration_ms, total,
        obs.len(), pats.len(), profs.len(), finds.len(), fixs.len(), skls.len()
    );

    Ok(WorkingRepresentation {
        id: 0,
        task_run_id: task_run_id.to_string(),
        observations: obs,
        cross_run_patterns: pats,
        entity_profiles: profs,
        recent_findings: finds,
        recent_fixes: fixs,
        applicable_skills: skls,
        workflow_id: workflow_id.map(|s| s.to_string()),
        workflow_name: workflow_name.map(|s| s.to_string()),
        total_items: total as i32,
        build_duration_ms: Some(duration_ms),
        built_at: chrono::Utc::now().to_rfc3339(),
        expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
        is_stale: false,
    })
}

// ── Formatter ────────────────────────────────────────────────────────────────

/// Format a working representation as markdown for injection into worker prompts.
///
/// Returns an empty string when the representation contains no items, so callers
/// can simply check `!ctx.is_empty()` before appending.
pub fn format_working_representation(wr: &WorkingRepresentation) -> String {
    if wr.total_items == 0 {
        return String::new();
    }

    let mut ctx = String::from("## Working Context (Pre-computed)\n\n");

    if !wr.entity_profiles.is_empty() {
        ctx.push_str("### Entity Profiles\n");
        for p in &wr.entity_profiles {
            ctx.push_str(&format!(
                "- **{}** ({}): {}\n",
                p.entity_label, p.entity_kind, p.profile_summary
            ));
        }
        ctx.push('\n');
    }

    if !wr.cross_run_patterns.is_empty() {
        ctx.push_str("### Active Patterns\n");
        for p in &wr.cross_run_patterns {
            ctx.push_str(&format!(
                "- [{}] {} (seen {}x)\n",
                p.pattern_type, p.description, p.occurrence_count
            ));
        }
        ctx.push('\n');
    }

    if !wr.recent_findings.is_empty() {
        ctx.push_str("### Recent Findings (Prior Runs)\n");
        for f in &wr.recent_findings {
            ctx.push_str(&format!("- [{}] {} ({})\n", f.severity, f.title, f.status));
        }
        ctx.push('\n');
    }

    if !wr.recent_fixes.is_empty() {
        ctx.push_str("### Recent Fixes (Prior Runs)\n");
        for f in &wr.recent_fixes {
            ctx.push_str(&format!("- {} ({})\n", f.title, f.status));
        }
        ctx.push('\n');
    }

    if !wr.observations.is_empty() {
        ctx.push_str("### Key Observations\n");
        for o in &wr.observations {
            ctx.push_str(&format!(
                "- [{}] {}: {}\n",
                o.observation_type, o.title, o.summary
            ));
        }
        ctx.push('\n');
    }

    if !wr.applicable_skills.is_empty() {
        ctx.push_str("### Available Skills\n");
        for s in &wr.applicable_skills {
            ctx.push_str(&format!(
                "- **{}** ({}): {} [used {}x]\n",
                s.name, s.category, s.description, s.usage_count
            ));
        }
        ctx.push('\n');
    }

    ctx
}
