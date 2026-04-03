//! Golden dataset management for regression testing.
//!
//! Curates test cases from successful historical runs to serve as regression
//! baselines when evaluating new prompt variants.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::runtime::Handle;
use tracing::info;

use crate::database::pg::PgDb;

// =============================================================================
// Types
// =============================================================================

/// A curated set of test cases for regression testing a specific agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenDataset {
    pub id: String,
    pub agent_type: String,
    pub name: String,
    pub entries: Vec<GoldenEntry>,
    pub created_at: String,
    pub updated_at: String,
}

/// A single entry in a golden dataset — represents a known-good input/outcome pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenEntry {
    /// Hash of the input for deduplication.
    pub input_hash: String,
    /// The workflow or task description that was successfully handled.
    pub input_summary: String,
    /// Whether the original run succeeded.
    pub expected_success: bool,
    /// Source task run ID (for provenance).
    pub source_task_run_id: Option<String>,
    /// Key characteristics of the successful run (iterations, duration).
    pub baseline_metrics: Option<GoldenEntryMetrics>,
}

/// Baseline metrics from the golden entry's source run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenEntryMetrics {
    pub iterations: u32,
    pub duration_ms: u64,
    pub success: bool,
}

// =============================================================================
// Database CRUD
// =============================================================================

/// Save or update a golden dataset.
pub fn save_golden_dataset(pg_db: &Arc<PgDb>, dataset: &GoldenDataset) -> Result<(), String> {
    let id = dataset.id.clone();
    let agent_type = dataset.agent_type.clone();
    let name = dataset.name.clone();
    let entries_json = serde_json::to_string(&dataset.entries)
        .map_err(|e| format!("Failed to serialize entries: {}", e))?;
    let entry_count = dataset.entries.len() as i64;

    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.save_golden_dataset(
            &id,
            &agent_type,
            &name,
            &entries_json,
            entry_count,
        ))
    })?;
    info!("Saved golden dataset {} ({} entries)", id, entry_count);
    Ok(())
}

/// List golden datasets, optionally filtered by agent type.
pub fn list_golden_datasets(
    pg_db: &Arc<PgDb>,
    agent_type: Option<&str>,
) -> Result<Vec<GoldenDataset>, String> {
    let tuples = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.list_golden_datasets(agent_type))
    })?;
    let datasets: Vec<GoldenDataset> = tuples
        .into_iter()
        .map(
            |(id, agent_type, name, entries_json, created_at, updated_at)| {
                let entries: Vec<GoldenEntry> =
                    serde_json::from_str(&entries_json).unwrap_or_default();
                GoldenDataset {
                    id,
                    agent_type,
                    name,
                    entries,
                    created_at,
                    updated_at,
                }
            },
        )
        .collect();
    Ok(datasets)
}

/// Delete a golden dataset.
pub fn delete_golden_dataset(pg_db: &Arc<PgDb>, dataset_id: &str) -> Result<(), String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.delete_golden_dataset(dataset_id))
    })
}

/// Build a golden dataset from recent successful pipeline runs for a given agent.
///
/// Selects up to `max_entries` successful runs from the last 30 days and captures
/// their task summaries and metrics as golden entries.
pub fn build_from_history(
    pg_db: &Arc<PgDb>,
    agent_type: &str,
    max_entries: usize,
) -> Result<GoldenDataset, String> {
    let limit = max_entries as i64;

    let raw_entries = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.build_golden_entries_from_history(agent_type, limit))
    })?;

    let entries: Vec<GoldenEntry> = raw_entries
        .into_iter()
        .map(|(task_run_id, task_name, duration_ms, success)| {
            let input_hash = format!("{:x}", md5_hash(&task_name));
            GoldenEntry {
                input_hash,
                input_summary: task_name,
                expected_success: true,
                source_task_run_id: Some(task_run_id),
                baseline_metrics: Some(GoldenEntryMetrics {
                    iterations: 0,
                    duration_ms: duration_ms as u64,
                    success,
                }),
            }
        })
        .collect();

    // Deduplicate by input_hash
    let mut seen = std::collections::HashSet::new();
    let deduped: Vec<GoldenEntry> = entries
        .into_iter()
        .filter(|e| seen.insert(e.input_hash.clone()))
        .collect();

    let id = format!("gd-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();

    let dataset = GoldenDataset {
        id,
        agent_type: agent_type.to_string(),
        name: format!("Auto-built from {} history (30d)", agent_type),
        entries: deduped,
        created_at: now.clone(),
        updated_at: now,
    };

    // Save to PG (primary storage for golden datasets)
    save_golden_dataset(pg_db, &dataset)?;

    Ok(dataset)
}

// ── PG dual-write wrappers ─────────────────────────────────────────────

/// Save a golden dataset with PG dual-write (fire-and-forget).
#[deprecated(note = "Use save_golden_dataset directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn save_golden_dataset_with_pg(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    dataset: &GoldenDataset,
) -> Result<(), String> {
    save_golden_dataset(pg_db, dataset)
}

/// Delete a golden dataset with PG dual-write (fire-and-forget).
#[deprecated(note = "Use delete_golden_dataset directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn delete_golden_dataset_with_pg(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    dataset_id: &str,
) -> Result<(), String> {
    delete_golden_dataset(pg_db, dataset_id)
}

/// Build a golden dataset from history with PG dual-write (fire-and-forget).
#[deprecated(note = "Use build_from_history directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn build_from_history_with_pg(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    agent_type: &str,
    max_entries: usize,
) -> Result<GoldenDataset, String> {
    build_from_history(pg_db, agent_type, max_entries)
}

// ── PG-primary read wrappers ─────────────────────────────────────────────

/// List golden datasets with PG-primary read.
#[deprecated(note = "Use list_golden_datasets directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn list_golden_datasets_with_pg(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    agent_type: Option<&str>,
) -> Result<Vec<GoldenDataset>, String> {
    list_golden_datasets(pg_db, agent_type)
}

/// Simple string hash for deduplication (not cryptographic).
fn md5_hash(s: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}
