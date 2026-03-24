//! Architecture Model Engine
//!
//! Aggregates component-level data from reflection fixes, causal events, and
//! knowledge into a queryable graph of components and their relationships.
//! Provides health scoring, impact analysis, and component detail queries.

use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use tracing::{info, warn};
use uuid::Uuid;

use crate::reflection::prediction;

// =============================================================================
// Types
// =============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct ComponentNode {
    pub id: String,
    pub component_path: String,
    pub component_type: String,
    pub fix_count: u32,
    pub error_count: u32,
    pub causal_involvement_count: u32,
    pub effective_fix_count: u32,
    pub ineffective_fix_count: u32,
    pub health_score: f64,
    pub change_velocity: f64,
    pub last_activity_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentEdge {
    pub source: String,
    pub target: String,
    pub relationship_type: String,
    pub strength: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphStats {
    pub total_components: u32,
    pub total_relationships: u32,
    pub avg_health_score: f64,
    pub most_volatile: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentGraph {
    pub nodes: Vec<ComponentNode>,
    pub edges: Vec<ComponentEdge>,
    pub stats: GraphStats,
}

#[derive(Debug, Clone, Serialize)]
pub struct RebuildResult {
    pub components_count: u32,
    pub relationships_count: u32,
    pub workflow_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FixSummary {
    pub id: String,
    pub fix_type: String,
    pub fix_description: String,
    pub effectiveness: Option<String>,
    pub applied_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactEntry {
    pub component_path: String,
    pub relationship_type: String,
    pub strength: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentDetails {
    pub node: ComponentNode,
    pub recent_fixes: Vec<FixSummary>,
    pub impacted_by: Vec<ImpactEntry>,
    pub impacts: Vec<ImpactEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactAnalysis {
    pub component: String,
    pub direct_impacts: Vec<ImpactEntry>,
    pub transitive_impacts: Vec<ImpactEntry>,
    pub total_impact_radius: u32,
    pub highest_risk_path: Vec<String>,
}

// =============================================================================
// Path Normalization
// =============================================================================

/// Normalize a raw file path: lowercase, forward slashes, strip `./` prefix and drive letters.
pub fn normalize_component_path(raw: &str) -> String {
    let mut path = raw.replace('\\', "/").to_lowercase();
    // Strip drive letters like C:/
    if path.len() >= 3 && path.as_bytes()[1] == b':' && path.as_bytes()[2] == b'/' {
        path = path[3..].to_string();
    }
    // Strip leading ./
    if let Some(stripped) = path.strip_prefix("./") {
        path = stripped.to_string();
    }
    path
}

/// Infer component type from a path.
pub fn infer_component_type(path: &str) -> &str {
    if path.contains("/services/") || path.contains("/service/") {
        return "service";
    }
    if path.ends_with('/') || !path.contains('.') {
        return "module";
    }
    "file"
}

// =============================================================================
// Rebuild
// =============================================================================

/// Full rebuild of the architecture model for a workflow.
///
/// Deletes existing data and re-extracts from reflection_fixes, causal_events,
/// and task_knowledge tables.
pub fn rebuild_architecture_model(
    conn: &Connection,
    workflow_name: &str,
) -> Result<RebuildResult, String> {
    // 1. Delete existing rows for this workflow
    conn.execute(
        "DELETE FROM architecture_components WHERE workflow_name = ?1",
        params![workflow_name],
    )
    .map_err(|e| format!("Failed to clear architecture_components: {}", e))?;

    conn.execute(
        "DELETE FROM component_relationships WHERE workflow_name = ?1",
        params![workflow_name],
    )
    .map_err(|e| format!("Failed to clear component_relationships: {}", e))?;

    // 2. Extract component nodes from reflection_fixes
    // Tracks: component_path -> (fix_count, effective, ineffective, last_activity)
    let mut components: HashMap<String, ComponentData> = HashMap::new();

    // 2a. From file_changed
    {
        let mut stmt = conn
            .prepare(
                r#"SELECT rf.file_changed, rf.effectiveness, rf.applied_at
                   FROM reflection_fixes rf
                   INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
                   WHERE tr.workflow_name = ?1
                     AND rf.file_changed IS NOT NULL
                     AND rf.file_changed != ''"#,
            )
            .map_err(|e| format!("Failed to prepare file_changed query: {}", e))?;

        let rows = stmt
            .query_map(params![workflow_name], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|e| format!("Failed to query file_changed: {}", e))?;

        for row in rows.flatten() {
            let path = normalize_component_path(&row.0);
            let entry = components.entry(path).or_default();
            entry.fix_count += 1;
            match row.1.as_deref() {
                Some("effective") => entry.effective_fix_count += 1,
                Some("ineffective") | Some("regression") => entry.ineffective_fix_count += 1,
                _ => {}
            }
            if let Some(ref ts) = row.2 {
                if entry.last_activity_at.is_none()
                    || entry.last_activity_at.as_deref() < Some(ts.as_str())
                {
                    entry.last_activity_at = Some(ts.clone());
                }
            }
        }
    }

    // 2b. From target_component (preferred, more stable identifier)
    {
        let mut stmt = conn
            .prepare(
                r#"SELECT rf.target_component, rf.effectiveness, rf.applied_at
                   FROM reflection_fixes rf
                   INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
                   WHERE tr.workflow_name = ?1
                     AND rf.target_component IS NOT NULL
                     AND rf.target_component != ''"#,
            )
            .map_err(|e| format!("Failed to prepare target_component query: {}", e))?;

        let rows = stmt
            .query_map(params![workflow_name], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|e| format!("Failed to query target_component: {}", e))?;

        for row in rows.flatten() {
            let path = normalize_component_path(&row.0);
            let entry = components.entry(path).or_default();
            entry.fix_count += 1;
            match row.1.as_deref() {
                Some("effective") => entry.effective_fix_count += 1,
                Some("ineffective") | Some("regression") => entry.ineffective_fix_count += 1,
                _ => {}
            }
            if let Some(ref ts) = row.2 {
                if entry.last_activity_at.is_none()
                    || entry.last_activity_at.as_deref() < Some(ts.as_str())
                {
                    entry.last_activity_at = Some(ts.clone());
                }
            }
        }
    }

    // 3. Extract causal involvement from causal_events where type=code_change
    {
        let mut stmt = conn
            .prepare(
                r#"SELECT cause_event_id
                   FROM causal_events
                   WHERE workflow_name = ?1
                     AND cause_event_type = 'code_change'"#,
            )
            .map_err(|e| format!("Failed to prepare causal code_change query: {}", e))?;

        let rows = stmt
            .query_map(params![workflow_name], |row| row.get::<_, String>(0))
            .map_err(|e| format!("Failed to query causal code_change: {}", e))?;

        for row in rows.flatten() {
            let path = normalize_component_path(&row);
            let entry = components.entry(path).or_default();
            entry.causal_involvement_count += 1;
        }
    }

    // 4. Extract error_count from task_knowledge related_files
    {
        let result = conn.prepare(
            r#"SELECT tk.related_files
               FROM task_knowledge tk
               INNER JOIN task_runs tr ON tr.id = tk.task_run_id
               WHERE tr.workflow_name = ?1
                 AND tk.related_files IS NOT NULL
                 AND tk.related_files != ''
                 AND tk.related_files != '[]'"#,
        );
        if let Ok(mut stmt) = result {
            let rows = stmt
                .query_map(params![workflow_name], |row| row.get::<_, String>(0))
                .map_err(|e| format!("Failed to query task_knowledge: {}", e))?;

            for row in rows.flatten() {
                // related_files is a JSON array of file paths
                if let Ok(files) = serde_json::from_str::<Vec<String>>(&row) {
                    for file in files {
                        let path = normalize_component_path(&file);
                        let entry = components.entry(path).or_default();
                        entry.error_count += 1;
                    }
                }
            }
        }
        // Table may not exist in all databases — skip silently
    }

    // 5. Build co_changes_with edges from fixes in same reflection run
    let mut edges: HashMap<(String, String, String), u32> = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                r#"SELECT rf.reflection_task_run_id, rf.file_changed, rf.target_component
                   FROM reflection_fixes rf
                   INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
                   WHERE tr.workflow_name = ?1
                     AND rf.reflection_task_run_id IS NOT NULL
                   ORDER BY rf.reflection_task_run_id"#,
            )
            .map_err(|e| format!("Failed to prepare co-change query: {}", e))?;

        let rows = stmt
            .query_map(params![workflow_name], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|e| format!("Failed to query co-changes: {}", e))?;

        // Group files by task_run_id
        let mut run_files: HashMap<String, HashSet<String>> = HashMap::new();
        for row in rows.flatten() {
            let run_id = row.0;
            if let Some(f) = row.1 {
                let path = normalize_component_path(&f);
                if !path.is_empty() {
                    run_files.entry(run_id.clone()).or_default().insert(path);
                }
            }
            if let Some(t) = row.2 {
                let path = normalize_component_path(&t);
                if !path.is_empty() {
                    run_files.entry(run_id).or_default().insert(path);
                }
            }
        }

        // Create co_changes_with edges for all pairs within same run
        for files in run_files.values() {
            let sorted: Vec<&String> = {
                let mut v: Vec<&String> = files.iter().collect();
                v.sort();
                v
            };
            for i in 0..sorted.len() {
                for j in (i + 1)..sorted.len() {
                    let key = (
                        sorted[i].clone(),
                        sorted[j].clone(),
                        "co_changes_with".to_string(),
                    );
                    *edges.entry(key).or_insert(0) += 1;
                }
            }
        }
    }

    // 6. Build impacts edges from causal events linking different files
    {
        let mut stmt = conn
            .prepare(
                r#"SELECT cause_event_id, effect_event_id
                   FROM causal_events
                   WHERE workflow_name = ?1
                     AND cause_event_type = 'code_change'"#,
            )
            .map_err(|e| format!("Failed to prepare impacts query: {}", e))?;

        let rows = stmt
            .query_map(params![workflow_name], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("Failed to query impacts: {}", e))?;

        for row in rows.flatten() {
            let source = normalize_component_path(&row.0);
            let target = normalize_component_path(&row.1);
            if !source.is_empty() && !target.is_empty() && source != target {
                let key = (source, target, "impacts".to_string());
                *edges.entry(key).or_insert(0) += 1;
            }
        }
    }

    // 7. Compute health scores and insert components
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut components_count = 0u32;
    let mut snapshot_data: Vec<(String, f64, i32, i32, f64)> = Vec::new();

    for (path, data) in &components {
        let comp_type = infer_component_type(path);
        let velocity = prediction::compute_change_velocity(conn, path, 5).unwrap_or(0.0);
        let effectiveness_rate = data.effective_fix_count as f64 / (data.fix_count.max(1) as f64);
        let health_score = (effectiveness_rate * (1.0 / (1.0 + velocity))).clamp(0.0, 1.0);

        let id = Uuid::new_v4().to_string();
        conn.execute(
            r#"INSERT INTO architecture_components
               (id, workflow_name, component_path, component_type,
                fix_count, error_count, causal_involvement_count,
                effective_fix_count, ineffective_fix_count,
                health_score, change_velocity, last_activity_at, created_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)"#,
            params![
                id,
                workflow_name,
                path,
                comp_type,
                data.fix_count,
                data.error_count,
                data.causal_involvement_count,
                data.effective_fix_count,
                data.ineffective_fix_count,
                health_score,
                velocity,
                data.last_activity_at,
                now,
            ],
        )
        .map_err(|e| format!("Failed to insert architecture_component: {}", e))?;
        components_count += 1;

        // Capture for health snapshot (reuse already-computed values)
        snapshot_data.push((
            path.clone(),
            health_score,
            data.fix_count as i32,
            data.effective_fix_count as i32,
            velocity,
        ));
    }

    // 7b. Snapshot component health for temporal trend tracking
    if let Err(e) =
        super::trends::store_component_health_snapshots(conn, workflow_name, &snapshot_data)
    {
        warn!("Failed to store component health snapshots: {}", e);
    }

    // 8. Insert edges
    let mut relationships_count = 0u32;
    for ((source, target, rel_type), strength) in &edges {
        let id = Uuid::new_v4().to_string();
        conn.execute(
            r#"INSERT INTO component_relationships
               (id, workflow_name, source_component, target_component,
                relationship_type, strength, last_seen_at, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"#,
            params![
                id,
                workflow_name,
                source,
                target,
                rel_type,
                strength,
                now,
                now
            ],
        )
        .map_err(|e| format!("Failed to insert component_relationship: {}", e))?;
        relationships_count += 1;
    }

    info!(
        "Architecture model rebuilt for '{}': {} components, {} relationships",
        workflow_name, components_count, relationships_count
    );

    Ok(RebuildResult {
        components_count,
        relationships_count,
        workflow_name: workflow_name.to_string(),
    })
}

/// Internal accumulator for component data during rebuild.
#[derive(Default)]
struct ComponentData {
    fix_count: u32,
    error_count: u32,
    causal_involvement_count: u32,
    effective_fix_count: u32,
    ineffective_fix_count: u32,
    last_activity_at: Option<String>,
}

// =============================================================================
// Queries
// =============================================================================

/// Get the full component graph for a workflow.
pub fn get_component_graph(
    conn: &Connection,
    workflow_name: &str,
) -> Result<ComponentGraph, String> {
    // Nodes
    let mut stmt = conn
        .prepare(
            r#"SELECT id, component_path, component_type,
                      fix_count, error_count, causal_involvement_count,
                      effective_fix_count, ineffective_fix_count,
                      health_score, change_velocity, last_activity_at
               FROM architecture_components
               WHERE workflow_name = ?1
               ORDER BY health_score ASC"#,
        )
        .map_err(|e| format!("Failed to prepare component query: {}", e))?;

    let nodes: Vec<ComponentNode> = stmt
        .query_map(params![workflow_name], |row| {
            Ok(ComponentNode {
                id: row.get(0)?,
                component_path: row.get(1)?,
                component_type: row.get(2)?,
                fix_count: row.get(3)?,
                error_count: row.get(4)?,
                causal_involvement_count: row.get(5)?,
                effective_fix_count: row.get(6)?,
                ineffective_fix_count: row.get(7)?,
                health_score: row.get(8)?,
                change_velocity: row.get(9)?,
                last_activity_at: row.get(10)?,
            })
        })
        .map_err(|e| format!("Failed to query components: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    // Edges
    let mut stmt = conn
        .prepare(
            r#"SELECT source_component, target_component, relationship_type, strength
               FROM component_relationships
               WHERE workflow_name = ?1"#,
        )
        .map_err(|e| format!("Failed to prepare relationship query: {}", e))?;

    let edges: Vec<ComponentEdge> = stmt
        .query_map(params![workflow_name], |row| {
            Ok(ComponentEdge {
                source: row.get(0)?,
                target: row.get(1)?,
                relationship_type: row.get(2)?,
                strength: row.get(3)?,
            })
        })
        .map_err(|e| format!("Failed to query relationships: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    // Stats
    let total_components = nodes.len() as u32;
    let total_relationships = edges.len() as u32;
    let avg_health_score = if nodes.is_empty() {
        1.0
    } else {
        nodes.iter().map(|n| n.health_score).sum::<f64>() / nodes.len() as f64
    };
    // Most volatile: top 5 by fix_count + causal_involvement_count
    let mut volatile: Vec<(&str, u32)> = nodes
        .iter()
        .map(|n| {
            (
                n.component_path.as_str(),
                n.fix_count + n.causal_involvement_count,
            )
        })
        .collect();
    volatile.sort_by(|a, b| b.1.cmp(&a.1));
    let most_volatile: Vec<String> = volatile
        .iter()
        .take(5)
        .map(|(p, _)| p.to_string())
        .collect();

    Ok(ComponentGraph {
        nodes,
        edges,
        stats: GraphStats {
            total_components,
            total_relationships,
            avg_health_score,
            most_volatile,
        },
    })
}

/// Get detailed info for a single component.
pub fn get_component_details(
    conn: &Connection,
    workflow_name: &str,
    component_path: &str,
) -> Result<ComponentDetails, String> {
    let normalized = normalize_component_path(component_path);

    // Node
    let node = conn
        .query_row(
            r#"SELECT id, component_path, component_type,
                      fix_count, error_count, causal_involvement_count,
                      effective_fix_count, ineffective_fix_count,
                      health_score, change_velocity, last_activity_at
               FROM architecture_components
               WHERE workflow_name = ?1 AND component_path = ?2"#,
            params![workflow_name, normalized],
            |row| {
                Ok(ComponentNode {
                    id: row.get(0)?,
                    component_path: row.get(1)?,
                    component_type: row.get(2)?,
                    fix_count: row.get(3)?,
                    error_count: row.get(4)?,
                    causal_involvement_count: row.get(5)?,
                    effective_fix_count: row.get(6)?,
                    ineffective_fix_count: row.get(7)?,
                    health_score: row.get(8)?,
                    change_velocity: row.get(9)?,
                    last_activity_at: row.get(10)?,
                })
            },
        )
        .map_err(|e| format!("Component not found: {}", e))?;

    // Recent fixes
    let mut stmt = conn
        .prepare(
            r#"SELECT rf.id, rf.fix_type, rf.fix_description, rf.effectiveness, rf.applied_at
               FROM reflection_fixes rf
               INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
               WHERE tr.workflow_name = ?1
                 AND (rf.file_changed = ?2 OR rf.target_component = ?2
                      OR LOWER(REPLACE(rf.file_changed, '\', '/')) = ?2
                      OR LOWER(REPLACE(rf.target_component, '\', '/')) = ?2)
               ORDER BY rf.applied_at DESC
               LIMIT 10"#,
        )
        .map_err(|e| format!("Failed to prepare recent fixes query: {}", e))?;

    let recent_fixes: Vec<FixSummary> = stmt
        .query_map(params![workflow_name, normalized], |row| {
            Ok(FixSummary {
                id: row.get(0)?,
                fix_type: row.get(1)?,
                fix_description: row.get(2)?,
                effectiveness: row.get(3)?,
                applied_at: row.get(4)?,
            })
        })
        .map_err(|e| format!("Failed to query recent fixes: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    // Relationships: impacted by (this component is the target)
    let mut stmt = conn
        .prepare(
            r#"SELECT source_component, relationship_type, strength
               FROM component_relationships
               WHERE workflow_name = ?1 AND target_component = ?2"#,
        )
        .map_err(|e| format!("Failed to prepare impacted_by query: {}", e))?;

    let impacted_by: Vec<ImpactEntry> = stmt
        .query_map(params![workflow_name, normalized], |row| {
            Ok(ImpactEntry {
                component_path: row.get(0)?,
                relationship_type: row.get(1)?,
                strength: row.get(2)?,
            })
        })
        .map_err(|e| format!("Failed to query impacted_by: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    // Relationships: impacts (this component is the source)
    let mut stmt = conn
        .prepare(
            r#"SELECT target_component, relationship_type, strength
               FROM component_relationships
               WHERE workflow_name = ?1 AND source_component = ?2"#,
        )
        .map_err(|e| format!("Failed to prepare impacts query: {}", e))?;

    let impacts: Vec<ImpactEntry> = stmt
        .query_map(params![workflow_name, normalized], |row| {
            Ok(ImpactEntry {
                component_path: row.get(0)?,
                relationship_type: row.get(1)?,
                strength: row.get(2)?,
            })
        })
        .map_err(|e| format!("Failed to query impacts: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(ComponentDetails {
        node,
        recent_fixes,
        impacted_by,
        impacts,
    })
}

/// BFS impact analysis from a component (max 3 hops).
pub fn get_impact_analysis(
    conn: &Connection,
    workflow_name: &str,
    component_path: &str,
) -> Result<ImpactAnalysis, String> {
    let normalized = normalize_component_path(component_path);

    // Build adjacency list from component_relationships
    let mut stmt = conn
        .prepare(
            r#"SELECT source_component, target_component, relationship_type, strength
               FROM component_relationships
               WHERE workflow_name = ?1"#,
        )
        .map_err(|e| format!("Failed to prepare adjacency query: {}", e))?;

    let all_edges: Vec<(String, String, String, u32)> = stmt
        .query_map(params![workflow_name], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, u32>(3)?,
            ))
        })
        .map_err(|e| format!("Failed to query adjacency: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    // Adjacency map: source -> [(target, type, strength)]
    let mut adj: HashMap<String, Vec<(String, String, u32)>> = HashMap::new();
    for (src, tgt, rel, str_) in &all_edges {
        adj.entry(src.clone())
            .or_default()
            .push((tgt.clone(), rel.clone(), *str_));
    }

    // BFS from normalized component, max 3 hops
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(normalized.clone());
    let mut queue: VecDeque<(String, u32, Vec<String>)> = VecDeque::new();
    queue.push_back((normalized.clone(), 0, vec![normalized.clone()]));

    let mut direct_impacts: Vec<ImpactEntry> = Vec::new();
    let mut transitive_impacts: Vec<ImpactEntry> = Vec::new();
    let mut highest_risk_path: Vec<String> = vec![normalized.clone()];
    let mut max_path_risk = 0u32;

    while let Some((current, depth, path)) = queue.pop_front() {
        if depth >= 3 {
            continue;
        }
        if let Some(neighbors) = adj.get(&current) {
            for (target, rel_type, strength) in neighbors {
                if visited.contains(target) {
                    continue;
                }
                visited.insert(target.clone());

                let entry = ImpactEntry {
                    component_path: target.clone(),
                    relationship_type: rel_type.clone(),
                    strength: *strength,
                };

                let mut new_path = path.clone();
                new_path.push(target.clone());

                if depth == 0 {
                    direct_impacts.push(entry);
                } else {
                    transitive_impacts.push(entry);
                }

                // Track highest risk path (by sum of strengths)
                let path_risk: u32 = new_path.len() as u32 * strength;
                if path_risk > max_path_risk {
                    max_path_risk = path_risk;
                    highest_risk_path = new_path.clone();
                }

                queue.push_back((target.clone(), depth + 1, new_path));
            }
        }
    }

    let total_impact_radius = (direct_impacts.len() + transitive_impacts.len()) as u32;

    Ok(ImpactAnalysis {
        component: normalized,
        direct_impacts,
        transitive_impacts,
        total_impact_radius,
        highest_risk_path,
    })
}

// =============================================================================
// Graph-Enhanced Impact Analysis
// =============================================================================

/// Extended impact analysis that supplements the BFS-on-component_relationships approach
/// with additional relationship data from step_finding_links and step_provenance tables.
pub fn rebuild_architecture_with_graph(
    conn: &Connection,
    workflow_name: &str,
) -> Result<RebuildResult, String> {
    let base_result = rebuild_architecture_model(conn, workflow_name)?;

    let cross_finding_edges: Vec<(String, String)> = conn
        .prepare(
            r#"SELECT DISTINCT
                   sfl.step_name,
                   rf.file_changed
               FROM step_finding_links sfl
               JOIN task_run_findings trf ON sfl.finding_id = trf.id
               JOIN task_runs t ON sfl.task_run_id = t.id
               JOIN reflection_fixes rf ON rf.source_finding_id = trf.id
               WHERE t.workflow_name = ?1
               AND rf.file_changed IS NOT NULL"#,
        )
        .and_then(|mut stmt| {
            stmt.query_map(params![workflow_name], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();

    let mut new_edges = 0u32;
    let now = chrono::Utc::now().to_rfc3339();

    for (step_name, file_changed) in &cross_finding_edges {
        let source = normalize_component_path(step_name);
        let target = normalize_component_path(file_changed);
        if source == target {
            continue;
        }

        let id = Uuid::new_v4().to_string();
        match conn.execute(
            r#"INSERT OR IGNORE INTO component_relationships
               (id, workflow_name, source_component, target_component, relationship_type, strength, last_seen_at, created_at)
               VALUES (?1, ?2, ?3, ?4, 'finding_link', 1, ?5, ?5)"#,
            params![id, workflow_name, source, target, now],
        ) {
            Ok(1) => new_edges += 1,
            Ok(_) => {}
            Err(e) => warn!("Failed to insert finding_link edge: {}", e),
        }
    }

    if new_edges > 0 {
        info!(
            "Graph-enhanced rebuild added {} finding_link edges for '{}'",
            new_edges, workflow_name
        );
    }

    Ok(RebuildResult {
        components_count: base_result.components_count,
        relationships_count: base_result.relationships_count + new_edges,
        workflow_name: workflow_name.to_string(),
    })
}

/// Graph-enhanced impact analysis.
pub fn get_impact_analysis_with_graph(
    conn: &Connection,
    workflow_name: &str,
    component_path: &str,
) -> Result<ImpactAnalysis, String> {
    let _ = rebuild_architecture_with_graph(conn, workflow_name);
    get_impact_analysis(conn, workflow_name, component_path)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();

        // Create tables
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS task_runs (
                id TEXT PRIMARY KEY,
                task_name TEXT NOT NULL DEFAULT '',
                task_type TEXT NOT NULL DEFAULT 'task',
                status TEXT NOT NULL DEFAULT 'running',
                sessions_count INTEGER NOT NULL DEFAULT 0,
                auto_continue BOOLEAN NOT NULL DEFAULT 1,
                workflow_name TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS architecture_components (
                id TEXT PRIMARY KEY,
                workflow_name TEXT NOT NULL,
                component_path TEXT NOT NULL,
                component_type TEXT NOT NULL DEFAULT 'file',
                fix_count INTEGER NOT NULL DEFAULT 0,
                error_count INTEGER NOT NULL DEFAULT 0,
                causal_involvement_count INTEGER NOT NULL DEFAULT 0,
                effective_fix_count INTEGER NOT NULL DEFAULT 0,
                ineffective_fix_count INTEGER NOT NULL DEFAULT 0,
                health_score REAL NOT NULL DEFAULT 1.0,
                change_velocity REAL NOT NULL DEFAULT 0.0,
                last_activity_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(workflow_name, component_path)
            );
            CREATE TABLE IF NOT EXISTS component_relationships (
                id TEXT PRIMARY KEY,
                workflow_name TEXT NOT NULL,
                source_component TEXT NOT NULL,
                target_component TEXT NOT NULL,
                relationship_type TEXT NOT NULL,
                strength INTEGER NOT NULL DEFAULT 1,
                last_seen_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(workflow_name, source_component, target_component, relationship_type)
            );
            CREATE TABLE IF NOT EXISTS reflection_fixes (
                id TEXT PRIMARY KEY,
                source_task_run_id TEXT NOT NULL,
                reflection_task_run_id TEXT NOT NULL,
                source_finding_id TEXT,
                source_knowledge_id TEXT,
                fix_type TEXT NOT NULL DEFAULT 'code_change',
                fix_description TEXT NOT NULL DEFAULT '',
                file_changed TEXT,
                old_value TEXT,
                new_value TEXT,
                confidence TEXT NOT NULL DEFAULT 'medium',
                content_hash TEXT,
                status TEXT NOT NULL DEFAULT 'applied',
                effectiveness TEXT,
                effectiveness_evidence TEXT,
                applied_at TEXT NOT NULL,
                evaluated_at TEXT,
                created_at TEXT NOT NULL,
                source_agent TEXT,
                reflection_scope TEXT DEFAULT 'workflow',
                project_path TEXT,
                target_component TEXT,
                reuse_count INTEGER DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS causal_events (
                id TEXT PRIMARY KEY,
                cause_event_type TEXT NOT NULL,
                cause_event_id TEXT NOT NULL,
                effect_event_type TEXT NOT NULL,
                effect_event_id TEXT NOT NULL,
                relationship TEXT NOT NULL,
                confidence TEXT NOT NULL DEFAULT 'high',
                source TEXT NOT NULL DEFAULT 'automated',
                task_run_id TEXT,
                workflow_name TEXT,
                description TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            "#,
        )
        .unwrap();

        conn
    }

    /// Insert a test task_run and return its ID.
    fn insert_test_run(conn: &Connection, id: &str, workflow_name: &str) {
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name) VALUES (?1, ?2, ?3)",
            params![id, workflow_name, workflow_name],
        )
        .unwrap();
    }

    #[test]
    fn test_normalize_component_path() {
        assert_eq!(
            normalize_component_path("C:\\Users\\test\\src\\main.rs"),
            "users/test/src/main.rs"
        );
        assert_eq!(normalize_component_path("./src/lib.rs"), "src/lib.rs");
        assert_eq!(
            normalize_component_path("src/Auth/Middleware.rs"),
            "src/auth/middleware.rs"
        );
    }

    #[test]
    fn test_infer_component_type() {
        assert_eq!(infer_component_type("src/services/auth.rs"), "service");
        assert_eq!(infer_component_type("src/utils"), "module");
        assert_eq!(infer_component_type("src/main.rs"), "file");
    }

    #[test]
    fn test_rebuild_empty() {
        let conn = setup_test_db();
        let result = rebuild_architecture_model(&conn, "TestWorkflow").unwrap();
        assert_eq!(result.components_count, 0);
        assert_eq!(result.relationships_count, 0);
    }

    #[test]
    fn test_rebuild_with_fixes() {
        let conn = setup_test_db();

        // Insert task runs first
        insert_test_run(&conn, "source1", "TestWorkflow");
        insert_test_run(&conn, "refl1", "Reflection: TestWorkflow");

        // Insert test fixes (using source_task_run_id and reflection_task_run_id)
        conn.execute(
            r#"INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id, fix_type, fix_description, file_changed, effectiveness, applied_at, created_at)
               VALUES ('fix1', 'source1', 'refl1', 'code_change', 'Fix auth', 'src/auth/middleware.rs', 'effective', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z')"#,
            [],
        ).unwrap();

        conn.execute(
            r#"INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id, fix_type, fix_description, file_changed, effectiveness, applied_at, created_at)
               VALUES ('fix2', 'source1', 'refl1', 'code_change', 'Fix handler', 'src/auth/handler.rs', 'ineffective', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z')"#,
            [],
        ).unwrap();

        let result = rebuild_architecture_model(&conn, "TestWorkflow").unwrap();
        assert_eq!(result.components_count, 2);
        // Same reflection_task_run_id + different files = co_changes_with edge
        assert_eq!(result.relationships_count, 1);

        // Verify graph
        let graph = get_component_graph(&conn, "TestWorkflow").unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].relationship_type, "co_changes_with");
        assert_eq!(graph.stats.total_components, 2);
    }

    #[test]
    fn test_get_component_details() {
        let conn = setup_test_db();

        insert_test_run(&conn, "source1", "TestWorkflow");
        insert_test_run(&conn, "refl1", "Reflection: TestWorkflow");

        conn.execute(
            r#"INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id, fix_type, fix_description, file_changed, effectiveness, applied_at, created_at)
               VALUES ('fix1', 'source1', 'refl1', 'code_change', 'Fix auth bug', 'src/auth.rs', 'effective', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z')"#,
            [],
        ).unwrap();

        rebuild_architecture_model(&conn, "TestWorkflow").unwrap();

        let details = get_component_details(&conn, "TestWorkflow", "src/auth.rs").unwrap();
        assert_eq!(details.node.fix_count, 1);
        assert_eq!(details.node.effective_fix_count, 1);
        assert!(details.node.health_score > 0.0);
    }

    #[test]
    fn test_impact_analysis() {
        let conn = setup_test_db();

        insert_test_run(&conn, "source1", "TestWorkflow");
        insert_test_run(&conn, "refl1", "Reflection: TestWorkflow");

        // Insert fixes and causal events for a chain: A -> B -> C
        conn.execute(
            r#"INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id, fix_type, fix_description, file_changed, applied_at, created_at)
               VALUES ('f1', 'source1', 'refl1', 'code_change', 'Fix A', 'src/a.rs', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z')"#,
            [],
        ).unwrap();
        conn.execute(
            r#"INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id, fix_type, fix_description, file_changed, applied_at, created_at)
               VALUES ('f2', 'source1', 'refl1', 'code_change', 'Fix B', 'src/b.rs', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z')"#,
            [],
        ).unwrap();
        conn.execute(
            r#"INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id, fix_type, fix_description, file_changed, applied_at, created_at)
               VALUES ('f3', 'source1', 'refl1', 'code_change', 'Fix C', 'src/c.rs', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z')"#,
            [],
        ).unwrap();

        // Causal: A impacts B, B impacts C
        conn.execute(
            r#"INSERT INTO causal_events (id, cause_event_type, cause_event_id, effect_event_type, effect_event_id, relationship, workflow_name)
               VALUES ('ce1', 'code_change', 'src/a.rs', 'code_change', 'src/b.rs', 'caused', 'TestWorkflow')"#,
            [],
        ).unwrap();
        conn.execute(
            r#"INSERT INTO causal_events (id, cause_event_type, cause_event_id, effect_event_type, effect_event_id, relationship, workflow_name)
               VALUES ('ce2', 'code_change', 'src/b.rs', 'code_change', 'src/c.rs', 'caused', 'TestWorkflow')"#,
            [],
        ).unwrap();

        rebuild_architecture_model(&conn, "TestWorkflow").unwrap();

        let impact = get_impact_analysis(&conn, "TestWorkflow", "src/a.rs").unwrap();
        assert_eq!(impact.component, "src/a.rs");
        assert!(!impact.direct_impacts.is_empty());
        assert!(impact.total_impact_radius >= 2);
    }

    #[test]
    fn test_health_score_computation() {
        let conn = setup_test_db();

        insert_test_run(&conn, "source1", "TestWorkflow");
        insert_test_run(&conn, "refl1", "Reflection: TestWorkflow");

        // Insert 4 fixes: 3 effective, 1 ineffective
        for i in 0..4 {
            let effectiveness = if i < 3 { "effective" } else { "ineffective" };
            conn.execute(
                &format!(
                    r#"INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id, fix_type, fix_description, file_changed, effectiveness, applied_at, created_at)
                       VALUES ('fix{}', 'source1', 'refl1', 'code_change', 'Fix {}', 'src/auth.rs', '{}', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z')"#,
                    i, i, effectiveness
                ),
                [],
            ).unwrap();
        }

        rebuild_architecture_model(&conn, "TestWorkflow").unwrap();

        let graph = get_component_graph(&conn, "TestWorkflow").unwrap();
        assert_eq!(graph.nodes.len(), 1);
        let node = &graph.nodes[0];
        assert_eq!(node.fix_count, 4);
        assert_eq!(node.effective_fix_count, 3);
        assert_eq!(node.ineffective_fix_count, 1);
        // effectiveness_rate = 3/4 = 0.75
        // health_score = 0.75 * (1.0 / (1.0 + velocity))
        assert!(node.health_score > 0.0 && node.health_score <= 1.0);
    }
}
