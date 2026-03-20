//! Spec versioning — tracks spec changes over time with content hashing and diffs.

use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::info;

use crate::database::CheckpointDb;

// ── Types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecVersion {
    pub id: String,
    pub spec_id: String,
    pub version_number: i64,
    pub content_hash: String,
    pub spec_json: String,
    pub change_summary: Option<String>,
    pub change_type: String,
    pub parent_version_id: Option<String>,
    pub assertion_count: i64,
    pub group_count: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecDiff {
    pub added_groups: Vec<GroupSummary>,
    pub removed_groups: Vec<GroupSummary>,
    pub modified_groups: Vec<GroupModification>,
    pub added_assertions: Vec<AssertionSummary>,
    pub removed_assertions: Vec<AssertionSummary>,
    pub modified_assertions: Vec<AssertionModification>,
    pub has_changes: bool,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupSummary {
    pub id: String,
    pub name: String,
    pub assertion_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupModification {
    pub id: String,
    pub name: String,
    pub added_assertions: Vec<String>,
    pub removed_assertions: Vec<String>,
    pub modified_assertions: Vec<AssertionModification>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionSummary {
    pub id: String,
    pub description: String,
    pub group_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionModification {
    pub id: String,
    pub field: String,
    pub from: String,
    pub to: String,
}

// ── Core functions ────────────────────────────────────────────────────

/// Compute SHA-256 content hash of spec JSON (normalized).
fn compute_content_hash(spec_json: &str) -> String {
    // Normalize by parsing and re-serializing to remove formatting differences
    let normalized = match serde_json::from_str::<serde_json::Value>(spec_json) {
        Ok(v) => serde_json::to_string(&v).unwrap_or_else(|_| spec_json.to_string()),
        Err(_) => spec_json.to_string(),
    };
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Count assertions and groups in a spec JSON.
fn count_spec_stats(spec_json: &str) -> (i64, i64) {
    let parsed: serde_json::Value = match serde_json::from_str(spec_json) {
        Ok(v) => v,
        Err(_) => return (0, 0),
    };

    let groups = parsed
        .get("groups")
        .and_then(|g| g.as_array())
        .map(|g| g.len())
        .unwrap_or(0) as i64;

    let assertions: i64 = parsed
        .get("groups")
        .and_then(|g| g.as_array())
        .map(|groups| {
            groups
                .iter()
                .map(|g| {
                    g.get("assertions")
                        .and_then(|a| a.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0)
                })
                .sum::<usize>() as i64
        })
        .unwrap_or(0);

    (assertions, groups)
}

/// Snapshot a spec's current state into the version history.
/// Skips if content_hash matches the latest version (no changes).
pub fn snapshot_spec_version(
    db: &CheckpointDb,
    spec_id: &str,
    spec_json: &str,
    change_summary: Option<&str>,
    change_type: &str,
) -> Result<SpecVersion, String> {
    let content_hash = compute_content_hash(spec_json);
    let (assertion_count, group_count) = count_spec_stats(spec_json);

    let spec_id_owned = spec_id.to_string();
    let content_hash_clone = content_hash.clone();

    // Check if latest version has same hash
    let latest = get_latest_version(db, spec_id)?;
    if let Some(ref latest) = latest {
        if latest.content_hash == content_hash {
            return Ok(latest.clone());
        }
    }

    let id = format!("sv-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();
    let version_number = latest.as_ref().map(|v| v.version_number + 1).unwrap_or(1);
    let parent_version_id = latest.as_ref().map(|v| v.id.clone());

    let version = SpecVersion {
        id: id.clone(),
        spec_id: spec_id_owned.clone(),
        version_number,
        content_hash: content_hash_clone.clone(),
        spec_json: spec_json.to_string(),
        change_summary: change_summary.map(|s| s.to_string()),
        change_type: change_type.to_string(),
        parent_version_id: parent_version_id.clone(),
        assertion_count,
        group_count,
        created_at: now.clone(),
    };

    db.with_conn({
        let id = id.clone();
        let spec_id = spec_id_owned.clone();
        let hash = content_hash_clone.clone();
        let json = spec_json.to_string();
        let summary = change_summary.map(|s| s.to_string());
        let ctype = change_type.to_string();
        let parent = parent_version_id.clone();
        let now = now.clone();
        move |conn| {
            conn.execute(
                r#"INSERT INTO spec_versions (
                    id, spec_id, version_number, content_hash, spec_json,
                    change_summary, change_type, parent_version_id,
                    assertion_count, group_count, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"#,
                params![
                    id,
                    spec_id,
                    version_number,
                    hash,
                    json,
                    summary,
                    ctype,
                    parent,
                    assertion_count,
                    group_count,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to insert spec version: {}", e))?;
            Ok(())
        }
    })?;

    info!(
        "Versioned spec '{}' → v{} (hash={}, {} groups, {} assertions)",
        spec_id_owned, version_number, &content_hash_clone[..8], group_count, assertion_count
    );

    Ok(version)
}

/// Get the latest version of a spec.
pub fn get_latest_version(
    db: &CheckpointDb,
    spec_id: &str,
) -> Result<Option<SpecVersion>, String> {
    let sid = spec_id.to_string();
    db.with_conn(move |conn| {
        let result = conn.query_row(
            r#"SELECT id, spec_id, version_number, content_hash, spec_json,
                      change_summary, change_type, parent_version_id,
                      assertion_count, group_count, created_at
               FROM spec_versions
               WHERE spec_id = ?1
               ORDER BY version_number DESC
               LIMIT 1"#,
            params![sid],
            |row| row_to_spec_version(row),
        );
        match result {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to query latest spec version: {}", e)),
        }
    })
}

/// Get version history for a spec.
pub fn get_version_history(
    db: &CheckpointDb,
    spec_id: &str,
    limit: Option<i64>,
) -> Result<Vec<SpecVersion>, String> {
    let sid = spec_id.to_string();
    let limit = limit.unwrap_or(50);
    db.with_conn(move |conn| {
        let mut stmt = conn
            .prepare(
                r#"SELECT id, spec_id, version_number, content_hash, spec_json,
                          change_summary, change_type, parent_version_id,
                          assertion_count, group_count, created_at
                   FROM spec_versions
                   WHERE spec_id = ?1
                   ORDER BY version_number DESC
                   LIMIT ?2"#,
            )
            .map_err(|e| format!("Failed to prepare version history query: {}", e))?;

        let rows = stmt
            .query_map(params![sid, limit], |row| row_to_spec_version(row))
            .map_err(|e| format!("Failed to query version history: {}", e))?;

        Ok(rows.filter_map(|r| r.ok()).collect())
    })
}

/// Compute a structured diff between two spec JSON strings.
pub fn diff_specs(old_json: &str, new_json: &str) -> Result<SpecDiff, String> {
    let old: serde_json::Value = serde_json::from_str(old_json)
        .map_err(|e| format!("Failed to parse old spec: {}", e))?;
    let new: serde_json::Value = serde_json::from_str(new_json)
        .map_err(|e| format!("Failed to parse new spec: {}", e))?;

    let old_groups = extract_groups(&old);
    let new_groups = extract_groups(&new);

    let mut added_groups = Vec::new();
    let mut removed_groups = Vec::new();
    let mut modified_groups = Vec::new();
    let mut added_assertions = Vec::new();
    let mut removed_assertions = Vec::new();
    let mut modified_assertions = Vec::new();

    // Find added and modified groups
    for (gid, new_group) in &new_groups {
        if let Some(old_group) = old_groups.get(gid) {
            // Group exists in both — diff assertions
            let old_assertions = extract_assertions(old_group);
            let new_assertions = extract_assertions(new_group);

            let mut g_added = Vec::new();
            let mut g_removed = Vec::new();
            let mut g_modified = Vec::new();

            for (aid, new_a) in &new_assertions {
                if let Some(old_a) = old_assertions.get(aid) {
                    // Assertion exists in both — check for modifications
                    let mods = diff_assertion(old_a, new_a, aid);
                    if !mods.is_empty() {
                        g_modified.extend(mods.clone());
                        modified_assertions.extend(mods);
                    }
                } else {
                    g_added.push(aid.clone());
                    added_assertions.push(AssertionSummary {
                        id: aid.clone(),
                        description: new_a
                            .get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("")
                            .to_string(),
                        group_id: gid.clone(),
                    });
                }
            }

            for (aid, old_a) in &old_assertions {
                if !new_assertions.contains_key(aid) {
                    g_removed.push(aid.clone());
                    removed_assertions.push(AssertionSummary {
                        id: aid.clone(),
                        description: old_a
                            .get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("")
                            .to_string(),
                        group_id: gid.clone(),
                    });
                }
            }

            if !g_added.is_empty() || !g_removed.is_empty() || !g_modified.is_empty() {
                modified_groups.push(GroupModification {
                    id: gid.clone(),
                    name: new_group
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string(),
                    added_assertions: g_added,
                    removed_assertions: g_removed,
                    modified_assertions: g_modified,
                });
            }
        } else {
            // New group
            let assertions = extract_assertions(new_group);
            added_groups.push(GroupSummary {
                id: gid.clone(),
                name: new_group
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string(),
                assertion_count: assertions.len(),
            });
            for (aid, a) in &assertions {
                added_assertions.push(AssertionSummary {
                    id: aid.clone(),
                    description: a
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string(),
                    group_id: gid.clone(),
                });
            }
        }
    }

    // Find removed groups
    for (gid, old_group) in &old_groups {
        if !new_groups.contains_key(gid) {
            let assertions = extract_assertions(old_group);
            removed_groups.push(GroupSummary {
                id: gid.clone(),
                name: old_group
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string(),
                assertion_count: assertions.len(),
            });
            for (aid, a) in &assertions {
                removed_assertions.push(AssertionSummary {
                    id: aid.clone(),
                    description: a
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string(),
                    group_id: gid.clone(),
                });
            }
        }
    }

    let has_changes = !added_groups.is_empty()
        || !removed_groups.is_empty()
        || !modified_groups.is_empty()
        || !added_assertions.is_empty()
        || !removed_assertions.is_empty()
        || !modified_assertions.is_empty();

    let mut summary_parts = Vec::new();
    if !added_groups.is_empty() {
        summary_parts.push(format!("Added {} group(s)", added_groups.len()));
    }
    if !removed_groups.is_empty() {
        summary_parts.push(format!("Removed {} group(s)", removed_groups.len()));
    }
    if !modified_groups.is_empty() {
        summary_parts.push(format!("Modified {} group(s)", modified_groups.len()));
    }
    if !added_assertions.is_empty() {
        summary_parts.push(format!("{} assertion(s) added", added_assertions.len()));
    }
    if !removed_assertions.is_empty() {
        summary_parts.push(format!("{} assertion(s) removed", removed_assertions.len()));
    }
    let summary = if summary_parts.is_empty() {
        "No changes".to_string()
    } else {
        summary_parts.join(", ")
    };

    Ok(SpecDiff {
        added_groups,
        removed_groups,
        modified_groups,
        added_assertions,
        removed_assertions,
        modified_assertions,
        has_changes,
        summary,
    })
}

/// Diff two spec versions by their version numbers.
pub fn diff_spec_versions(
    db: &CheckpointDb,
    spec_id: &str,
    from_version: i64,
    to_version: i64,
) -> Result<SpecDiff, String> {
    let sid = spec_id.to_string();
    let (old_json, new_json) = db.with_conn(move |conn| {
        let old: String = conn
            .query_row(
                "SELECT spec_json FROM spec_versions WHERE spec_id = ?1 AND version_number = ?2",
                params![sid, from_version],
                |row| row.get(0),
            )
            .map_err(|e| format!("Version {} not found: {}", from_version, e))?;
        let new: String = conn
            .query_row(
                "SELECT spec_json FROM spec_versions WHERE spec_id = ?1 AND version_number = ?2",
                params![sid, to_version],
                |row| row.get(0),
            )
            .map_err(|e| format!("Version {} not found: {}", to_version, e))?;
        Ok((old, new))
    })?;

    diff_specs(&old_json, &new_json)
}

// ── Helpers ───────────────────────────────────────────────────────────

fn row_to_spec_version(row: &rusqlite::Row) -> rusqlite::Result<SpecVersion> {
    Ok(SpecVersion {
        id: row.get(0)?,
        spec_id: row.get(1)?,
        version_number: row.get(2)?,
        content_hash: row.get(3)?,
        spec_json: row.get(4)?,
        change_summary: row.get(5)?,
        change_type: row.get(6)?,
        parent_version_id: row.get(7)?,
        assertion_count: row.get(8)?,
        group_count: row.get(9)?,
        created_at: row.get(10)?,
    })
}

fn extract_groups(
    spec: &serde_json::Value,
) -> std::collections::HashMap<String, &serde_json::Value> {
    let mut map = std::collections::HashMap::new();
    if let Some(groups) = spec.get("groups").and_then(|g| g.as_array()) {
        for group in groups {
            if let Some(id) = group.get("id").and_then(|i| i.as_str()) {
                map.insert(id.to_string(), group);
            }
        }
    }
    map
}

fn extract_assertions(
    group: &serde_json::Value,
) -> std::collections::HashMap<String, &serde_json::Value> {
    let mut map = std::collections::HashMap::new();
    if let Some(assertions) = group.get("assertions").and_then(|a| a.as_array()) {
        for assertion in assertions {
            if let Some(id) = assertion.get("id").and_then(|i| i.as_str()) {
                map.insert(id.to_string(), assertion);
            }
        }
    }
    map
}

fn diff_assertion(
    old: &serde_json::Value,
    new: &serde_json::Value,
    assertion_id: &str,
) -> Vec<AssertionModification> {
    let mut mods = Vec::new();
    let fields = ["description", "assertionType", "severity", "enabled"];

    for field in &fields {
        let old_val = old.get(field).map(|v| v.to_string()).unwrap_or_default();
        let new_val = new.get(field).map(|v| v.to_string()).unwrap_or_default();
        if old_val != new_val {
            mods.push(AssertionModification {
                id: assertion_id.to_string(),
                field: field.to_string(),
                from: old_val,
                to: new_val,
            });
        }
    }

    // Check target criteria changes
    let old_criteria = old
        .get("target")
        .and_then(|t| t.get("criteria"))
        .map(|c| c.to_string())
        .unwrap_or_default();
    let new_criteria = new
        .get("target")
        .and_then(|t| t.get("criteria"))
        .map(|c| c.to_string())
        .unwrap_or_default();
    if old_criteria != new_criteria {
        mods.push(AssertionModification {
            id: assertion_id.to_string(),
            field: "target.criteria".to_string(),
            from: old_criteria,
            to: new_criteria,
        });
    }

    mods
}
