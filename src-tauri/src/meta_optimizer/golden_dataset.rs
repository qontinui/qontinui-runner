//! Golden dataset management for regression testing.
//!
//! Curates test cases from successful historical runs to serve as regression
//! baselines when evaluating new prompt variants.

use rusqlite::params;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::database::CheckpointDb;

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
pub fn save_golden_dataset(db: &CheckpointDb, dataset: &GoldenDataset) -> Result<(), String> {
    let id = dataset.id.clone();
    let agent_type = dataset.agent_type.clone();
    let name = dataset.name.clone();
    let entries_json = serde_json::to_string(&dataset.entries)
        .map_err(|e| format!("Failed to serialize entries: {}", e))?;
    let entry_count = dataset.entries.len() as i64;
    let now = chrono::Utc::now().to_rfc3339();

    db.with_conn(move |conn| {
        conn.execute(
            r#"INSERT INTO golden_datasets (id, agent_type, name, entries_json, entry_count, created_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
               ON CONFLICT(id) DO UPDATE SET
                   name = excluded.name,
                   entries_json = excluded.entries_json,
                   entry_count = excluded.entry_count,
                   updated_at = excluded.updated_at"#,
            params![id, agent_type, name, entries_json, entry_count, now],
        )
        .map_err(|e| format!("Failed to save golden dataset: {}", e))?;

        info!("Saved golden dataset {} ({} entries)", id, entry_count);
        Ok(())
    })
}

/// List golden datasets, optionally filtered by agent type.
pub fn list_golden_datasets(
    db: &CheckpointDb,
    agent_type: Option<&str>,
) -> Result<Vec<GoldenDataset>, String> {
    let agent = agent_type.map(|s| s.to_string());

    db.with_conn(move |conn| {
        let (sql, has_filter) = if agent.is_some() {
            (
                r#"SELECT id, agent_type, name, entries_json, created_at, updated_at
                   FROM golden_datasets WHERE agent_type = ?1 ORDER BY updated_at DESC"#,
                true,
            )
        } else {
            (
                r#"SELECT id, agent_type, name, entries_json, created_at, updated_at
                   FROM golden_datasets ORDER BY updated_at DESC"#,
                false,
            )
        };

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<GoldenDataset> {
            let entries_json: String = row.get(3)?;
            let entries: Vec<GoldenEntry> = serde_json::from_str(&entries_json).unwrap_or_default();
            Ok(GoldenDataset {
                id: row.get(0)?,
                agent_type: row.get(1)?,
                name: row.get(2)?,
                entries,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        };

        let rows: Vec<GoldenDataset> = if has_filter {
            stmt.query_map(params![agent.unwrap()], map_row)
                .map_err(|e| format!("Failed to query: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map([], map_row)
                .map_err(|e| format!("Failed to query: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        };

        Ok(rows)
    })
}

/// Delete a golden dataset.
pub fn delete_golden_dataset(db: &CheckpointDb, dataset_id: &str) -> Result<(), String> {
    let id = dataset_id.to_string();
    db.with_conn(move |conn| {
        conn.execute("DELETE FROM golden_datasets WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete golden dataset: {}", e))?;
        Ok(())
    })
}

/// Build a golden dataset from recent successful pipeline runs for a given agent.
///
/// Selects up to `max_entries` successful runs from the last 30 days and captures
/// their task summaries and metrics as golden entries.
pub fn build_from_history(
    db: &CheckpointDb,
    agent_type: &str,
    max_entries: usize,
) -> Result<GoldenDataset, String> {
    let agent = agent_type.to_string();
    let limit = max_entries as i64;

    let entries: Vec<GoldenEntry> = db.with_conn(move |conn| {
        let period_start = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();

        let mut stmt = conn
            .prepare(
                r#"SELECT pat.task_run_id, tr.name, pat.duration_ms, pat.success
                   FROM pipeline_agent_traces pat
                   JOIN task_runs tr ON pat.task_run_id = tr.id
                   WHERE pat.agent_type = ?1
                     AND pat.success = 1
                     AND pat.created_at > ?2
                   ORDER BY pat.created_at DESC
                   LIMIT ?3"#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows: Vec<GoldenEntry> = stmt
            .query_map(params![agent, period_start, limit], |row| {
                let task_run_id: String = row.get(0)?;
                let task_name: String = row.get(1)?;
                let duration_ms: i64 = row.get(2)?;
                let success: bool = row.get(3)?;

                // Use a simple hash of task name for deduplication
                let input_hash = format!("{:x}", md5_hash(&task_name));

                Ok(GoldenEntry {
                    input_hash,
                    input_summary: task_name,
                    expected_success: true,
                    source_task_run_id: Some(task_run_id),
                    baseline_metrics: Some(GoldenEntryMetrics {
                        iterations: 0, // Not available from pipeline traces directly
                        duration_ms: duration_ms as u64,
                        success,
                    }),
                })
            })
            .map_err(|e| format!("Failed to query traces: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows)
    })?;

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

    save_golden_dataset(db, &dataset)?;

    Ok(dataset)
}

/// Simple string hash for deduplication (not cryptographic).
fn md5_hash(s: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}
