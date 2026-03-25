//! In-memory knowledge graph engine backed by petgraph.
//!
//! The KnowledgeGraph materializes SQLite data into a traversable directed graph.
//! It queries 16+ tables to build nodes and edges, then provides traversal queries
//! for causal reasoning, impact analysis, pattern detection, and unified search.

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet, VecDeque};

use super::graph_types::*;

// =============================================================================
// KnowledgeGraph
// =============================================================================

pub struct KnowledgeGraph {
    graph: DiGraph<GraphNode, GraphEdge>,
    node_index: HashMap<NodeKey, NodeIndex>,
    pub built_at: String,
    pub workflow_scope: Option<String>,
}

impl KnowledgeGraph {
    // -------------------------------------------------------------------------
    // Constructor
    // -------------------------------------------------------------------------

    pub fn new(workflow_scope: Option<String>) -> Self {
        Self {
            graph: DiGraph::new(),
            node_index: HashMap::new(),
            built_at: chrono::Utc::now().to_rfc3339(),
            workflow_scope,
        }
    }

    // -------------------------------------------------------------------------
    // Build from database
    // -------------------------------------------------------------------------

    /// Build the full knowledge graph from SQLite data.
    ///
    /// Queries 16+ tables, creates nodes for each entity type, then wires up
    /// directed edges representing relationships (causal, structural, temporal).
    pub fn build_from_db(conn: &Connection, workflow_name: Option<&str>) -> Result<Self, String> {
        let mut kg = Self::new(workflow_name.map(|s| s.to_string()));

        // --- Nodes ---
        kg.load_workflows(conn, workflow_name)?;
        kg.load_workflow_versions(conn, workflow_name)?;
        kg.load_task_runs(conn, workflow_name)?;
        kg.load_findings(conn, workflow_name)?;
        kg.load_fixes(conn, workflow_name)?;
        kg.load_errors(conn, workflow_name)?;
        kg.load_components(conn, workflow_name)?;
        kg.load_rules(conn)?;
        kg.load_patterns(conn, workflow_name)?;
        kg.load_knowledge(conn, workflow_name)?;
        kg.load_step_defs(conn, workflow_name)?;
        kg.load_ui_elements(conn, workflow_name)?;

        // --- Edges ---
        kg.link_task_runs_to_workflows(conn, workflow_name)?;
        kg.link_findings_to_task_runs(conn, workflow_name)?;
        kg.link_fixes_to_findings(conn, workflow_name)?;
        kg.link_causal_events(conn, workflow_name)?;
        kg.link_workflow_versions(conn, workflow_name)?;
        kg.link_step_provenance(conn, workflow_name)?;
        kg.link_step_finding_links(conn, workflow_name)?;
        kg.link_rule_influence(conn, workflow_name)?;
        kg.link_component_relationships(conn, workflow_name)?;
        kg.link_fix_applications(conn, workflow_name)?;
        kg.link_ui_interactions(conn, workflow_name)?;

        Ok(kg)
    }

    // -------------------------------------------------------------------------
    // Node management helpers
    // -------------------------------------------------------------------------

    /// Insert a node if its key does not already exist; return the NodeIndex either way.
    fn get_or_insert_node(&mut self, node: GraphNode) -> NodeIndex {
        if let Some(&idx) = self.node_index.get(&node.key) {
            return idx;
        }
        let key = node.key.clone();
        let idx = self.graph.add_node(node);
        self.node_index.insert(key, idx);
        idx
    }

    /// Add an edge between two nodes looked up by their string keys.
    /// Returns `false` if either endpoint is missing from the graph.
    fn add_edge_by_key(&mut self, from_key: &str, to_key: &str, edge: GraphEdge) -> bool {
        let from_idx = match self.node_index.get(from_key) {
            Some(&idx) => idx,
            None => return false,
        };
        let to_idx = match self.node_index.get(to_key) {
            Some(&idx) => idx,
            None => return false,
        };
        self.graph.add_edge(from_idx, to_idx, edge);
        true
    }

    // =========================================================================
    // Private loaders — Nodes
    // =========================================================================

    fn load_workflows(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT id, name, description, category, created_at
               FROM unified_workflows WHERE name = ?1"#
        } else {
            r#"SELECT id, name, description, category, created_at
               FROM unified_workflows"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare workflows query: {}", e))?;

        let rows: Vec<(String, String, Option<String>, Option<String>, Option<String>)> =
            if let Some(wn) = workflow_name {
                stmt.query_map(params![wn], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                })
                .map_err(|e| format!("Failed to query workflows: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            } else {
                stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                })
                .map_err(|e| format!("Failed to query workflows: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            };

        for (id, name, desc, category, created_at) in rows {
            let mut node = GraphNode::new(GraphNodeKind::Workflow, &id, &name);
            if let Some(d) = desc {
                node = node.with_property("description", serde_json::json!(d));
            }
            if let Some(c) = category {
                node = node.with_property("category", serde_json::json!(c));
            }
            if let Some(ref ts) = created_at {
                node = node.with_created_at(ts);
            }
            self.get_or_insert_node(node);
        }
        Ok(())
    }

    fn load_workflow_versions(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT wv.id, wv.workflow_id, wv.version_number, wv.parent_version_id,
                      wv.generation_task_run_id, wv.trigger, wv.created_at
               FROM workflow_versions wv
               INNER JOIN unified_workflows uw ON uw.id = wv.workflow_id
               WHERE uw.name = ?1"#
        } else {
            r#"SELECT id, workflow_id, version_number, parent_version_id,
                      generation_task_run_id, trigger, created_at
               FROM workflow_versions"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare workflow_versions query: {}", e))?;

        let rows: Vec<(
            String,
            String,
            i64,
            Option<String>,
            Option<String>,
            String,
            String,
        )> = if let Some(wn) = workflow_name {
            stmt.query_map(params![wn], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(|e| format!("Failed to query workflow_versions: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(|e| format!("Failed to query workflow_versions: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        };

        for (id, _wf_id, version_number, _parent, _gen_run, trigger, created_at) in rows {
            let label = format!("v{} ({})", version_number, trigger);
            let node = GraphNode::new(GraphNodeKind::WorkflowVersion, &id, &label)
                .with_property("version_number", serde_json::json!(version_number))
                .with_property("trigger", serde_json::json!(trigger))
                .with_created_at(&created_at);
            self.get_or_insert_node(node);
        }
        Ok(())
    }

    fn load_task_runs(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT id, task_name, workflow_name, status, created_at
               FROM task_runs WHERE workflow_name = ?1"#
        } else {
            r#"SELECT id, task_name, workflow_name, status, created_at
               FROM task_runs WHERE workflow_name IS NOT NULL"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare task_runs query: {}", e))?;

        let rows: Vec<(String, String, Option<String>, String, String)> =
            if let Some(wn) = workflow_name {
                stmt.query_map(params![wn], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })
                .map_err(|e| format!("Failed to query task_runs: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            } else {
                stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })
                .map_err(|e| format!("Failed to query task_runs: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            };

        for (id, task_name, _wf_name, status, created_at) in rows {
            let node = GraphNode::new(GraphNodeKind::TaskRun, &id, &task_name)
                .with_property("status", serde_json::json!(status))
                .with_created_at(&created_at);
            self.get_or_insert_node(node);
        }
        Ok(())
    }

    fn load_findings(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT f.id, f.title, f.category, f.severity, f.status, f.created_at
               FROM task_run_findings f
               INNER JOIN task_runs tr ON tr.id = f.task_run_id
               WHERE tr.workflow_name = ?1"#
        } else {
            r#"SELECT f.id, f.title, f.category, f.severity, f.status, f.created_at
               FROM task_run_findings f
               INNER JOIN task_runs tr ON tr.id = f.task_run_id
               WHERE tr.workflow_name IS NOT NULL"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare findings query: {}", e))?;

        let rows: Vec<(String, String, String, String, String, String)> =
            if let Some(wn) = workflow_name {
                stmt.query_map(params![wn], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .map_err(|e| format!("Failed to query findings: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            } else {
                stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .map_err(|e| format!("Failed to query findings: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            };

        for (id, title, category, severity, status, created_at) in rows {
            let weight = match severity.as_str() {
                "critical" => 4.0,
                "high" => 3.0,
                "medium" => 2.0,
                "low" => 1.0,
                _ => 1.0,
            };
            let node = GraphNode::new(GraphNodeKind::Finding, &id, &title)
                .with_weight(weight)
                .with_property("category", serde_json::json!(category))
                .with_property("severity", serde_json::json!(severity))
                .with_property("status", serde_json::json!(status))
                .with_created_at(&created_at);
            self.get_or_insert_node(node);
        }
        Ok(())
    }

    fn load_fixes(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT rf.id, rf.fix_type, rf.fix_description, rf.effectiveness,
                      rf.confidence, rf.status, rf.created_at
               FROM reflection_fixes rf
               INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
               WHERE tr.workflow_name = ?1"#
        } else {
            r#"SELECT rf.id, rf.fix_type, rf.fix_description, rf.effectiveness,
                      rf.confidence, rf.status, rf.created_at
               FROM reflection_fixes rf
               INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
               WHERE tr.workflow_name IS NOT NULL"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare fixes query: {}", e))?;

        let rows: Vec<(
            String,
            String,
            String,
            Option<String>,
            String,
            String,
            String,
        )> = if let Some(wn) = workflow_name {
            stmt.query_map(params![wn], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(|e| format!("Failed to query fixes: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(|e| format!("Failed to query fixes: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        };

        for (id, fix_type, description, effectiveness, confidence, status, created_at) in rows {
            let label = if description.len() > 80 {
                let truncated: String = description.chars().take(77).collect();
                format!("{}...", truncated)
            } else {
                description.clone()
            };
            let weight = match effectiveness.as_deref() {
                Some("effective") => 3.0,
                Some("ineffective") => 0.5,
                Some("caused_regression") => -1.0,
                _ => 1.0,
            };
            let node = GraphNode::new(GraphNodeKind::Fix, &id, &label)
                .with_weight(weight)
                .with_property("fix_type", serde_json::json!(fix_type))
                .with_property("effectiveness", serde_json::json!(effectiveness))
                .with_property("confidence", serde_json::json!(confidence))
                .with_property("status", serde_json::json!(status))
                .with_created_at(&created_at);
            self.get_or_insert_node(node);
        }
        Ok(())
    }

    fn load_errors(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        // Group by signature_hash, use MIN(id) as the representative error.
        let sql = if workflow_name.is_some() {
            r#"SELECT MIN(e.id), e.signature_hash, e.error_type, e.message,
                      e.severity, SUM(e.occurrence_count), MIN(e.first_seen_at)
               FROM error_events e
               INNER JOIN task_runs tr ON tr.id = e.task_run_id
               WHERE tr.workflow_name = ?1
               GROUP BY e.signature_hash"#
        } else {
            r#"SELECT MIN(e.id), e.signature_hash, e.error_type, e.message,
                      e.severity, SUM(e.occurrence_count), MIN(e.first_seen_at)
               FROM error_events e
               INNER JOIN task_runs tr ON tr.id = e.task_run_id
               WHERE tr.workflow_name IS NOT NULL
               GROUP BY e.signature_hash"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare errors query: {}", e))?;

        let rows: Vec<(i64, String, Option<String>, String, String, i64, String)> =
            if let Some(wn) = workflow_name {
                stmt.query_map(params![wn], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                })
                .map_err(|e| format!("Failed to query errors: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            } else {
                stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                })
                .map_err(|e| format!("Failed to query errors: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            };

        for (id, sig_hash, error_type, message, severity, occurrences, first_seen) in rows {
            let entity_id = format!("err_{}", id);
            let label = if message.len() > 80 {
                let truncated: String = message.chars().take(77).collect();
                format!("{}...", truncated)
            } else {
                message.clone()
            };
            let weight = match severity.as_str() {
                "critical" => 4.0,
                "error" => 3.0,
                "warning" => 1.5,
                _ => 1.0,
            };
            let node = GraphNode::new(GraphNodeKind::Error, &entity_id, &label)
                .with_weight(weight)
                .with_property("signature_hash", serde_json::json!(sig_hash))
                .with_property("error_type", serde_json::json!(error_type))
                .with_property("occurrences", serde_json::json!(occurrences))
                .with_property("severity", serde_json::json!(severity))
                .with_created_at(&first_seen);
            self.get_or_insert_node(node);
        }
        Ok(())
    }

    fn load_components(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT id, component_path, component_type, health_score, created_at
               FROM architecture_components WHERE workflow_name = ?1"#
        } else {
            r#"SELECT id, component_path, component_type, health_score, created_at
               FROM architecture_components"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare components query: {}", e))?;

        let rows: Vec<(String, String, String, f64, String)> =
            if let Some(wn) = workflow_name {
                stmt.query_map(params![wn], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, f64>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })
                .map_err(|e| format!("Failed to query components: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            } else {
                stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, f64>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })
                .map_err(|e| format!("Failed to query components: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            };

        for (id, path, comp_type, health_score, created_at) in rows {
            let node = GraphNode::new(GraphNodeKind::Component, &id, &path)
                .with_weight(health_score)
                .with_property("component_type", serde_json::json!(comp_type))
                .with_property("health_score", serde_json::json!(health_score))
                .with_created_at(&created_at);
            self.get_or_insert_node(node);
        }
        Ok(())
    }

    fn load_rules(&mut self, conn: &Connection) -> Result<(), String> {
        let mut stmt = conn
            .prepare(
                r#"SELECT id, agent, section, title, severity, created_at
                   FROM generation_rules WHERE status = 'active'"#,
            )
            .map_err(|e| format!("Failed to prepare rules query: {}", e))?;

        let rows: Vec<(String, String, String, String, String, String)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .map_err(|e| format!("Failed to query rules: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        for (id, agent, section, title, severity, created_at) in rows {
            let weight = match severity.as_str() {
                "critical" => 4.0,
                "important" => 3.0,
                "normal" => 2.0,
                "hint" => 1.0,
                _ => 1.0,
            };
            let node = GraphNode::new(GraphNodeKind::Rule, &id, &title)
                .with_weight(weight)
                .with_property("agent", serde_json::json!(agent))
                .with_property("section", serde_json::json!(section))
                .with_property("severity", serde_json::json!(severity))
                .with_created_at(&created_at);
            self.get_or_insert_node(node);
        }
        Ok(())
    }

    fn load_patterns(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT id, pattern_type, signature_hash, occurrence_count, status, created_at
               FROM cross_run_patterns
               WHERE status = 'active' AND workflow_name = ?1"#
        } else {
            r#"SELECT id, pattern_type, signature_hash, occurrence_count, status, created_at
               FROM cross_run_patterns WHERE status = 'active'"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare patterns query: {}", e))?;

        let rows: Vec<(String, String, String, i64, String, String)> =
            if let Some(wn) = workflow_name {
                stmt.query_map(params![wn], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .map_err(|e| format!("Failed to query patterns: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            } else {
                stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .map_err(|e| format!("Failed to query patterns: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            };

        for (id, pattern_type, sig_hash, occurrence_count, _status, created_at) in rows {
            let label = format!("{} ({}x)", pattern_type, occurrence_count);
            let node = GraphNode::new(GraphNodeKind::Pattern, &id, &label)
                .with_weight(occurrence_count as f64)
                .with_property("pattern_type", serde_json::json!(pattern_type))
                .with_property("signature_hash", serde_json::json!(sig_hash))
                .with_property("occurrence_count", serde_json::json!(occurrence_count))
                .with_created_at(&created_at);
            self.get_or_insert_node(node);
        }
        Ok(())
    }

    fn load_knowledge(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT tk.id, tk.category, tk.content, tk.confidence, tk.created_at
               FROM task_knowledge tk
               INNER JOIN task_runs tr ON tr.id = tk.task_run_id
               WHERE tr.workflow_name = ?1"#
        } else {
            r#"SELECT tk.id, tk.category, tk.content, tk.confidence, tk.created_at
               FROM task_knowledge tk
               INNER JOIN task_runs tr ON tr.id = tk.task_run_id
               WHERE tr.workflow_name IS NOT NULL"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare knowledge query: {}", e))?;

        let rows: Vec<(String, String, String, Option<String>, String)> =
            if let Some(wn) = workflow_name {
                stmt.query_map(params![wn], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })
                .map_err(|e| format!("Failed to query knowledge: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            } else {
                stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })
                .map_err(|e| format!("Failed to query knowledge: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            };

        for (id, category, content, confidence, created_at) in rows {
            let label = if content.len() > 80 {
                let truncated: String = content.chars().take(77).collect();
                format!("{}...", truncated)
            } else {
                content.clone()
            };
            let node = GraphNode::new(GraphNodeKind::Knowledge, &id, &label)
                .with_property("category", serde_json::json!(category))
                .with_property("confidence", serde_json::json!(confidence))
                .with_created_at(&created_at);
            self.get_or_insert_node(node);
        }
        Ok(())
    }

    fn load_step_defs(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT DISTINCT sp.step_name, sp.phase, sp.generating_agent
               FROM step_provenance sp
               INNER JOIN unified_workflows uw ON uw.id = sp.workflow_id
               WHERE uw.name = ?1"#
        } else {
            r#"SELECT DISTINCT step_name, phase, generating_agent
               FROM step_provenance"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare step_defs query: {}", e))?;

        let rows: Vec<(String, String, String)> = if let Some(wn) = workflow_name {
            stmt.query_map(params![wn], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })
            .map_err(|e| format!("Failed to query step_defs: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })
            .map_err(|e| format!("Failed to query step_defs: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        };

        for (step_name, phase, agent) in rows {
            let entity_id = format!("{}:{}", phase, step_name);
            let label = format!("{} [{}]", step_name, phase);
            let node = GraphNode::new(GraphNodeKind::StepDef, &entity_id, &label)
                .with_property("phase", serde_json::json!(phase))
                .with_property("generating_agent", serde_json::json!(&agent));
            self.get_or_insert_node(node);

            // Also ensure the pipeline agent node exists.
            let agent_node =
                GraphNode::new(GraphNodeKind::PipelineAgent, &agent, &agent);
            self.get_or_insert_node(agent_node);
        }
        Ok(())
    }

    /// Load UI Bridge elements that were interacted with during automation.
    /// Capped at 100 elements per workflow to respect memory budgets.
    fn load_ui_elements(
        &mut self,
        conn: &Connection,
        _workflow_name: Option<&str>,
    ) -> Result<(), String> {
        // Get distinct element IDs with interaction counts, capped at 100
        let mut stmt = conn
            .prepare(
                r#"SELECT element_id, COUNT(*) as interaction_count,
                          CAST(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) AS REAL) / COUNT(*) as success_rate
                   FROM ui_bridge_events
                   WHERE element_id IS NOT NULL AND event_type = 'action_executed'
                   GROUP BY element_id
                   ORDER BY interaction_count DESC
                   LIMIT 100"#,
            )
            .map_err(|e| format!("Failed to prepare ui_elements query: {}", e))?;

        let rows: Vec<(String, i64, f64)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, f64>(2)?,
                ))
            })
            .map_err(|e| format!("Failed to query ui_elements: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        for (element_id, count, rate) in rows {
            let node = GraphNode::new(GraphNodeKind::UIElement, &element_id, &element_id)
                .with_weight(count as f64)
                .with_property("interaction_count", serde_json::json!(count))
                .with_property("success_rate", serde_json::json!(rate))
                .with_property("flaky", serde_json::json!(rate < 0.95));
            self.get_or_insert_node(node);
        }

        Ok(())
    }

    /// Link task runs to UI elements via InteractedWith edges.
    fn link_ui_interactions(
        &mut self,
        conn: &Connection,
        _workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let mut stmt = conn
            .prepare(
                r#"SELECT DISTINCT
                     CAST(task_run_id AS TEXT) as tr_id,
                     element_id,
                     COUNT(*) as count,
                     CAST(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) AS REAL) / COUNT(*) as rate
                   FROM ui_bridge_events
                   WHERE element_id IS NOT NULL
                     AND task_run_id IS NOT NULL
                     AND event_type = 'action_executed'
                   GROUP BY task_run_id, element_id
                   LIMIT 500"#,
            )
            .map_err(|e| format!("Failed to prepare ui_interactions query: {}", e))?;

        let rows: Vec<(String, String, i64, f64)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, f64>(3)?,
                ))
            })
            .map_err(|e| format!("Failed to query ui_interactions: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        for (task_run_id, element_id, count, rate) in rows {
            let from_key = format!("task_run:{}", task_run_id);
            let to_key = format!("ui_element:{}", element_id);
            let edge = GraphEdge::new(GraphEdgeKind::InteractedWith)
                .with_weight(rate * count as f64)
                .with_label(&format!("{}x ({}% success)", count, (rate * 100.0) as u32));
            self.add_edge_by_key(&from_key, &to_key, edge);
        }

        Ok(())
    }

    // =========================================================================
    // Private loaders — Edges
    // =========================================================================

    /// task_runs.workflow_name → BelongsTo → workflow
    fn link_task_runs_to_workflows(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT tr.id, uw.id
               FROM task_runs tr
               INNER JOIN unified_workflows uw ON uw.name = tr.workflow_name
               WHERE tr.workflow_name = ?1"#
        } else {
            r#"SELECT tr.id, uw.id
               FROM task_runs tr
               INNER JOIN unified_workflows uw ON uw.name = tr.workflow_name
               WHERE tr.workflow_name IS NOT NULL"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare task_run→workflow edge query: {}", e))?;

        let rows: Vec<(String, String)> = if let Some(wn) = workflow_name {
            stmt.query_map(params![wn], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                .map_err(|e| format!("Failed to query task_run→workflow edges: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                .map_err(|e| format!("Failed to query task_run→workflow edges: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        };

        for (run_id, wf_id) in rows {
            let from_key = format!("task_run:{}", run_id);
            let to_key = format!("workflow:{}", wf_id);
            self.add_edge_by_key(&from_key, &to_key, GraphEdge::new(GraphEdgeKind::BelongsTo));
        }
        Ok(())
    }

    /// task_run_findings.task_run_id → DetectedDuring → task_run
    fn link_findings_to_task_runs(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT f.id, f.task_run_id
               FROM task_run_findings f
               INNER JOIN task_runs tr ON tr.id = f.task_run_id
               WHERE tr.workflow_name = ?1"#
        } else {
            r#"SELECT f.id, f.task_run_id
               FROM task_run_findings f
               INNER JOIN task_runs tr ON tr.id = f.task_run_id
               WHERE tr.workflow_name IS NOT NULL"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare finding→task_run edge query: {}", e))?;

        let rows: Vec<(String, String)> = if let Some(wn) = workflow_name {
            stmt.query_map(params![wn], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                .map_err(|e| format!("Failed to query finding→task_run edges: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                .map_err(|e| format!("Failed to query finding→task_run edges: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        };

        for (finding_id, run_id) in rows {
            let from_key = format!("finding:{}", finding_id);
            let to_key = format!("task_run:{}", run_id);
            self.add_edge_by_key(
                &from_key,
                &to_key,
                GraphEdge::new(GraphEdgeKind::DetectedDuring),
            );
        }
        Ok(())
    }

    /// reflection_fixes.source_finding_id → Caused (finding → fix)
    /// reflection_fixes where effective → Resolved (fix → finding)
    fn link_fixes_to_findings(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT rf.id, rf.source_finding_id, rf.effectiveness
               FROM reflection_fixes rf
               INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
               WHERE tr.workflow_name = ?1
                 AND rf.source_finding_id IS NOT NULL"#
        } else {
            r#"SELECT rf.id, rf.source_finding_id, rf.effectiveness
               FROM reflection_fixes rf
               INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
               WHERE tr.workflow_name IS NOT NULL
                 AND rf.source_finding_id IS NOT NULL"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare fix→finding edge query: {}", e))?;

        let rows: Vec<(String, String, Option<String>)> = if let Some(wn) = workflow_name {
            stmt.query_map(params![wn], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?))
            })
            .map_err(|e| format!("Failed to query fix→finding edges: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?))
            })
            .map_err(|e| format!("Failed to query fix→finding edges: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        };

        for (fix_id, finding_id, effectiveness) in rows {
            let finding_key = format!("finding:{}", finding_id);
            let fix_key = format!("fix:{}", fix_id);

            // Finding caused the fix to be created
            self.add_edge_by_key(
                &finding_key,
                &fix_key,
                GraphEdge::new(GraphEdgeKind::Caused),
            );

            // If effective, the fix resolved the finding
            if effectiveness.as_deref() == Some("effective") {
                self.add_edge_by_key(
                    &fix_key,
                    &finding_key,
                    GraphEdge::new(GraphEdgeKind::Resolved),
                );
            }
        }
        Ok(())
    }

    /// causal_events → Caused / Resolved edges based on relationship type
    fn link_causal_events(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT cause_event_type, cause_event_id, effect_event_type, effect_event_id,
                      relationship, confidence
               FROM causal_events WHERE workflow_name = ?1"#
        } else {
            r#"SELECT cause_event_type, cause_event_id, effect_event_type, effect_event_id,
                      relationship, confidence
               FROM causal_events WHERE workflow_name IS NOT NULL"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare causal_events edge query: {}", e))?;

        let rows: Vec<(String, String, String, String, String, String)> =
            if let Some(wn) = workflow_name {
                stmt.query_map(params![wn], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .map_err(|e| format!("Failed to query causal_events edges: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            } else {
                stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .map_err(|e| format!("Failed to query causal_events edges: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            };

        for (cause_type, cause_id, effect_type, effect_id, relationship, confidence) in rows {
            let from_key = causal_event_to_node_key(&cause_type, &cause_id);
            let to_key = causal_event_to_node_key(&effect_type, &effect_id);

            let edge_kind = match relationship.as_str() {
                "resolved" | "prevented" => GraphEdgeKind::Resolved,
                _ => GraphEdgeKind::Caused,
            };

            let weight = match confidence.as_str() {
                "high" => 1.0,
                "medium" => 0.7,
                "low" => 0.4,
                _ => 0.5,
            };

            self.add_edge_by_key(
                &from_key,
                &to_key,
                GraphEdge::new(edge_kind)
                    .with_weight(weight)
                    .with_label(&relationship),
            );
        }
        Ok(())
    }

    /// workflow_versions parent → EvolvedFrom, generation_task_run_id → GeneratedBy
    fn link_workflow_versions(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT wv.id, wv.parent_version_id, wv.generation_task_run_id
               FROM workflow_versions wv
               INNER JOIN unified_workflows uw ON uw.id = wv.workflow_id
               WHERE uw.name = ?1"#
        } else {
            r#"SELECT id, parent_version_id, generation_task_run_id
               FROM workflow_versions"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare version edge query: {}", e))?;

        let rows: Vec<(String, Option<String>, Option<String>)> =
            if let Some(wn) = workflow_name {
                stmt.query_map(params![wn], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?))
                })
                .map_err(|e| format!("Failed to query version edges: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            } else {
                stmt.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?))
                })
                .map_err(|e| format!("Failed to query version edges: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
            };

        for (version_id, parent_id, gen_run_id) in rows {
            let ver_key = format!("workflow_version:{}", version_id);

            if let Some(pid) = parent_id {
                let parent_key = format!("workflow_version:{}", pid);
                self.add_edge_by_key(
                    &ver_key,
                    &parent_key,
                    GraphEdge::new(GraphEdgeKind::EvolvedFrom),
                );
            }

            if let Some(run_id) = gen_run_id {
                let run_key = format!("task_run:{}", run_id);
                self.add_edge_by_key(
                    &ver_key,
                    &run_key,
                    GraphEdge::new(GraphEdgeKind::GeneratedBy),
                );
            }
        }
        Ok(())
    }

    /// step_provenance → BuiltBy (step → pipeline_agent)
    fn link_step_provenance(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT DISTINCT sp.step_name, sp.phase, sp.generating_agent
               FROM step_provenance sp
               INNER JOIN unified_workflows uw ON uw.id = sp.workflow_id
               WHERE uw.name = ?1"#
        } else {
            r#"SELECT DISTINCT step_name, phase, generating_agent
               FROM step_provenance"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare step_provenance edge query: {}", e))?;

        let rows: Vec<(String, String, String)> = if let Some(wn) = workflow_name {
            stmt.query_map(params![wn], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })
            .map_err(|e| format!("Failed to query step_provenance edges: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })
            .map_err(|e| format!("Failed to query step_provenance edges: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        };

        for (step_name, phase, agent) in rows {
            let step_key = format!("step_def:{}:{}", phase, step_name);
            let agent_key = format!("pipeline_agent:{}", agent);
            self.add_edge_by_key(
                &step_key,
                &agent_key,
                GraphEdge::new(GraphEdgeKind::BuiltBy),
            );
        }
        Ok(())
    }

    /// step_finding_links → DetectedDuring (step → finding)
    fn link_step_finding_links(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT sfl.step_name, sfl.finding_id
               FROM step_finding_links sfl
               INNER JOIN task_runs tr ON tr.id = sfl.task_run_id
               WHERE tr.workflow_name = ?1"#
        } else {
            r#"SELECT sfl.step_name, sfl.finding_id
               FROM step_finding_links sfl
               INNER JOIN task_runs tr ON tr.id = sfl.task_run_id
               WHERE tr.workflow_name IS NOT NULL"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare step_finding edge query: {}", e))?;

        let rows: Vec<(String, String)> = if let Some(wn) = workflow_name {
            stmt.query_map(params![wn], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                .map_err(|e| format!("Failed to query step_finding edges: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                .map_err(|e| format!("Failed to query step_finding edges: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        };

        for (step_name, finding_id) in rows {
            // We don't know the phase from step_finding_links, try to find the step node
            // by checking all possible phase prefixes.
            let finding_key = format!("finding:{}", finding_id);
            for phase in &["setup", "verification", "agentic", "completion"] {
                let step_key = format!("step_def:{}:{}", phase, step_name);
                if self.node_index.contains_key(&step_key) {
                    self.add_edge_by_key(
                        &step_key,
                        &finding_key,
                        GraphEdge::new(GraphEdgeKind::DetectedDuring),
                    );
                    break;
                }
            }
        }
        Ok(())
    }

    /// rule_influence_log → InfluencedBy (rule → workflow)
    fn link_rule_influence(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT ril.rule_id, ril.workflow_id
               FROM rule_influence_log ril
               WHERE ril.workflow_id IS NOT NULL
                 AND ril.workflow_id IN (
                     SELECT id FROM unified_workflows WHERE name = ?1
                 )"#
        } else {
            r#"SELECT rule_id, workflow_id
               FROM rule_influence_log WHERE workflow_id IS NOT NULL"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare rule_influence edge query: {}", e))?;

        let rows: Vec<(String, String)> = if let Some(wn) = workflow_name {
            stmt.query_map(params![wn], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                .map_err(|e| format!("Failed to query rule_influence edges: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                .map_err(|e| format!("Failed to query rule_influence edges: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        };

        for (rule_id, workflow_id) in rows {
            let rule_key = format!("rule:{}", rule_id);
            let wf_key = format!("workflow:{}", workflow_id);
            self.add_edge_by_key(
                &rule_key,
                &wf_key,
                GraphEdge::new(GraphEdgeKind::InfluencedBy),
            );
        }
        Ok(())
    }

    /// component_relationships → ImpactsComponent
    fn link_component_relationships(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT cr.source_component, cr.target_component, cr.relationship_type, cr.strength
               FROM component_relationships cr
               WHERE cr.workflow_name = ?1"#
        } else {
            r#"SELECT source_component, target_component, relationship_type, strength
               FROM component_relationships"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare component_rel edge query: {}", e))?;

        let rows: Vec<(String, String, String, i64)> = if let Some(wn) = workflow_name {
            stmt.query_map(params![wn], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?))
            })
            .map_err(|e| format!("Failed to query component_rel edges: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?))
            })
            .map_err(|e| format!("Failed to query component_rel edges: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        };

        for (source, target, rel_type, strength) in rows {
            // Component nodes are keyed by their DB id, but component_relationships
            // reference by path. We need to find the matching component node.
            let from_key = self.find_component_by_path(&source);
            let to_key = self.find_component_by_path(&target);
            if let (Some(fk), Some(tk)) = (from_key, to_key) {
                self.add_edge_by_key(
                    &fk,
                    &tk,
                    GraphEdge::new(GraphEdgeKind::ImpactsComponent)
                        .with_weight(strength as f64)
                        .with_label(&rel_type),
                );
            }
        }
        Ok(())
    }

    /// fix_applications → AppliedIn (fix → task_run)
    fn link_fix_applications(
        &mut self,
        conn: &Connection,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let sql = if workflow_name.is_some() {
            r#"SELECT fa.fix_id, fa.task_run_id, fa.outcome
               FROM fix_applications fa
               INNER JOIN task_runs tr ON tr.id = fa.task_run_id
               WHERE tr.workflow_name = ?1"#
        } else {
            r#"SELECT fa.fix_id, fa.task_run_id, fa.outcome
               FROM fix_applications fa
               INNER JOIN task_runs tr ON tr.id = fa.task_run_id
               WHERE tr.workflow_name IS NOT NULL"#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare fix_application edge query: {}", e))?;

        let rows: Vec<(String, String, Option<String>)> = if let Some(wn) = workflow_name {
            stmt.query_map(params![wn], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?))
            })
            .map_err(|e| format!("Failed to query fix_application edges: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?))
            })
            .map_err(|e| format!("Failed to query fix_application edges: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        };

        for (fix_id, run_id, outcome) in rows {
            let fix_key = format!("fix:{}", fix_id);
            let run_key = format!("task_run:{}", run_id);
            let mut edge = GraphEdge::new(GraphEdgeKind::AppliedIn);
            if let Some(ref o) = outcome {
                edge = edge.with_label(o);
                let o_str: &str = o;
                edge = edge.with_weight(match o_str {
                    "resolved" => 2.0,
                    "ineffective" => 0.5,
                    _ => 1.0,
                });
            }
            self.add_edge_by_key(&fix_key, &run_key, edge);
        }
        Ok(())
    }

    // =========================================================================
    // Utility helpers
    // =========================================================================

    /// Find a component node key by its path label (since component_relationships
    /// reference by path, not by DB id).
    fn find_component_by_path(&self, path: &str) -> Option<String> {
        for (key, &idx) in &self.node_index {
            if key.starts_with("component:") {
                if let Some(node) = self.graph.node_weight(idx) {
                    if node.label == path {
                        return Some(key.clone());
                    }
                }
            }
        }
        None
    }

    // =========================================================================
    // Incremental update
    // =========================================================================

    /// Ingest a single task run and its related entities into the graph.
    /// Returns the count of new nodes added.
    pub fn ingest_task_run(
        &mut self,
        conn: &Connection,
        task_run_id: &str,
    ) -> Result<u32, String> {
        let before_count = self.graph.node_count();

        // 1. Load the task run itself
        let row: (String, String, Option<String>, String, String) = conn
            .query_row(
                r#"SELECT id, task_name, workflow_name, status, created_at
                   FROM task_runs WHERE id = ?1"#,
                params![task_run_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .map_err(|e| format!("Task run not found: {}", e))?;

        let run_node = GraphNode::new(GraphNodeKind::TaskRun, &row.0, &row.1)
            .with_property("status", serde_json::json!(row.3))
            .with_created_at(&row.4);
        self.get_or_insert_node(run_node);

        // Link to workflow if we have it
        if let Some(ref wf_name) = row.2 {
            let wf_row: Option<String> = conn
                .query_row(
                    "SELECT id FROM unified_workflows WHERE name = ?1",
                    params![wf_name],
                    |r| r.get(0),
                )
                .ok();
            if let Some(wf_id) = wf_row {
                let run_key = format!("task_run:{}", task_run_id);
                let wf_key = format!("workflow:{}", wf_id);
                self.add_edge_by_key(
                    &run_key,
                    &wf_key,
                    GraphEdge::new(GraphEdgeKind::BelongsTo),
                );
            }
        }

        // 2. Load findings for this run
        {
            let mut stmt = conn
                .prepare(
                    r#"SELECT id, title, category, severity, status, created_at
                       FROM task_run_findings WHERE task_run_id = ?1"#,
                )
                .map_err(|e| format!("Failed to prepare ingest findings query: {}", e))?;

            let findings: Vec<(String, String, String, String, String, String)> = stmt
                .query_map(params![task_run_id], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                })
                .map_err(|e| format!("Failed to query ingest findings: {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            for (fid, title, category, severity, status, created_at) in findings {
                let weight = match severity.as_str() {
                    "critical" => 4.0,
                    "high" => 3.0,
                    "medium" => 2.0,
                    _ => 1.0,
                };
                let node = GraphNode::new(GraphNodeKind::Finding, &fid, &title)
                    .with_weight(weight)
                    .with_property("category", serde_json::json!(category))
                    .with_property("severity", serde_json::json!(severity))
                    .with_property("status", serde_json::json!(status))
                    .with_created_at(&created_at);
                self.get_or_insert_node(node);

                let finding_key = format!("finding:{}", fid);
                let run_key = format!("task_run:{}", task_run_id);
                self.add_edge_by_key(
                    &finding_key,
                    &run_key,
                    GraphEdge::new(GraphEdgeKind::DetectedDuring),
                );
            }
        }

        // 3. Load fixes sourced from this run
        {
            let mut stmt = conn
                .prepare(
                    r#"SELECT id, fix_type, fix_description, effectiveness,
                              source_finding_id, created_at
                       FROM reflection_fixes WHERE source_task_run_id = ?1"#,
                )
                .map_err(|e| format!("Failed to prepare ingest fixes query: {}", e))?;

            let fixes: Vec<(String, String, String, Option<String>, Option<String>, String)> = stmt
                .query_map(params![task_run_id], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                })
                .map_err(|e| format!("Failed to query ingest fixes: {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            for (fix_id, fix_type, desc, effectiveness, finding_id, created_at) in fixes {
                let label = if desc.len() > 80 {
                    let truncated: String = desc.chars().take(77).collect();
                    format!("{}...", truncated)
                } else {
                    desc.clone()
                };
                let weight = match effectiveness.as_deref() {
                    Some("effective") => 3.0,
                    Some("ineffective") => 0.5,
                    Some("caused_regression") => -1.0,
                    _ => 1.0,
                };
                let node = GraphNode::new(GraphNodeKind::Fix, &fix_id, &label)
                    .with_weight(weight)
                    .with_property("fix_type", serde_json::json!(fix_type))
                    .with_property("effectiveness", serde_json::json!(effectiveness))
                    .with_created_at(&created_at);
                self.get_or_insert_node(node);

                if let Some(ref fid) = finding_id {
                    let finding_key = format!("finding:{}", fid);
                    let fix_key = format!("fix:{}", fix_id);
                    self.add_edge_by_key(
                        &finding_key,
                        &fix_key,
                        GraphEdge::new(GraphEdgeKind::Caused),
                    );
                    if effectiveness.as_deref() == Some("effective") {
                        self.add_edge_by_key(
                            &fix_key,
                            &finding_key,
                            GraphEdge::new(GraphEdgeKind::Resolved),
                        );
                    }
                }
            }
        }

        let new_count = self.graph.node_count() - before_count;
        Ok(new_count as u32)
    }

    // =========================================================================
    // Summary
    // =========================================================================

    /// Produce aggregate statistics about the graph.
    pub fn summary(&self) -> GraphSummary {
        let mut nodes_by_kind: HashMap<String, usize> = HashMap::new();
        let mut edges_by_kind: HashMap<String, usize> = HashMap::new();

        // Count nodes by kind
        for node in self.graph.node_weights() {
            *nodes_by_kind
                .entry(node.kind.as_str().to_string())
                .or_insert(0) += 1;
        }

        // Count edges by kind
        for edge in self.graph.edge_weights() {
            *edges_by_kind
                .entry(edge.kind.as_str().to_string())
                .or_insert(0) += 1;
        }

        // Most-connected nodes (by total degree = incoming + outgoing)
        let mut degree_list: Vec<(String, usize)> = self
            .node_index
            .iter()
            .map(|(key, &idx)| {
                let in_deg = self
                    .graph
                    .neighbors_directed(idx, Direction::Incoming)
                    .count();
                let out_deg = self
                    .graph
                    .neighbors_directed(idx, Direction::Outgoing)
                    .count();
                (key.clone(), in_deg + out_deg)
            })
            .collect();
        degree_list.sort_by(|a, b| b.1.cmp(&a.1));
        degree_list.truncate(10);

        GraphSummary {
            total_nodes: self.graph.node_count(),
            total_edges: self.graph.edge_count(),
            nodes_by_kind,
            edges_by_kind,
            most_connected: degree_list,
            built_at: self.built_at.clone(),
            workflow_scope: self.workflow_scope.clone(),
        }
    }

    // =========================================================================
    // Path finding
    // =========================================================================

    /// BFS to find all paths (up to 10) between two nodes, bounded by max_depth.
    pub fn find_paths(
        &self,
        from_key: &str,
        to_key: &str,
        max_depth: u32,
    ) -> Vec<GraphPath> {
        let from_idx = match self.node_index.get(from_key) {
            Some(&idx) => idx,
            None => return vec![],
        };
        let to_idx = match self.node_index.get(to_key) {
            Some(&idx) => idx,
            None => return vec![],
        };

        let mut results: Vec<GraphPath> = Vec::new();
        // BFS with path tracking: (current_node, path_of_node_indices, path_of_edge_indices)
        let mut queue: VecDeque<(NodeIndex, Vec<NodeIndex>)> = VecDeque::new();
        queue.push_back((from_idx, vec![from_idx]));

        while let Some((current, path)) = queue.pop_front() {
            if results.len() >= 10 {
                break;
            }
            if path.len() as u32 > max_depth + 1 {
                continue;
            }

            for edge_ref in self.graph.edges_directed(current, Direction::Outgoing) {
                let neighbor = edge_ref.target();

                // Avoid cycles within this path
                if path.contains(&neighbor) {
                    continue;
                }

                let mut new_path = path.clone();
                new_path.push(neighbor);

                if neighbor == to_idx {
                    // Found a complete path — materialize it
                    let gp = self.materialize_path(&new_path);
                    results.push(gp);
                    if results.len() >= 10 {
                        break;
                    }
                } else if (new_path.len() as u32) <= max_depth + 1 {
                    queue.push_back((neighbor, new_path));
                }
            }
        }

        results
    }

    // =========================================================================
    // Neighborhood
    // =========================================================================

    /// BFS outward from a node, collecting all neighbors within `depth` hops
    /// (both incoming and outgoing directions).
    pub fn neighborhood(&self, node_key: &str, depth: u32) -> Option<GraphNeighborhood> {
        let &center_idx = self.node_index.get(node_key)?;
        let center_node = self.graph.node_weight(center_idx)?.clone();

        let mut visited: HashSet<NodeIndex> = HashSet::new();
        visited.insert(center_idx);

        let mut neighbors: Vec<NeighborEntry> = Vec::new();
        // (node_index, current_distance)
        let mut queue: VecDeque<(NodeIndex, u32)> = VecDeque::new();
        queue.push_back((center_idx, 0));

        while let Some((current, dist)) = queue.pop_front() {
            if dist >= depth {
                continue;
            }

            // Outgoing edges
            for edge_ref in self.graph.edges_directed(current, Direction::Outgoing) {
                let neighbor_idx = edge_ref.target();
                if visited.contains(&neighbor_idx) {
                    continue;
                }
                visited.insert(neighbor_idx);

                if let Some(neighbor_node) = self.graph.node_weight(neighbor_idx) {
                    neighbors.push(NeighborEntry {
                        node: neighbor_node.clone(),
                        edge: edge_ref.weight().clone(),
                        direction: EdgeDirection::Outgoing,
                        distance: dist + 1,
                    });
                }
                queue.push_back((neighbor_idx, dist + 1));
            }

            // Incoming edges
            for edge_ref in self.graph.edges_directed(current, Direction::Incoming) {
                let neighbor_idx = edge_ref.source();
                if visited.contains(&neighbor_idx) {
                    continue;
                }
                visited.insert(neighbor_idx);

                if let Some(neighbor_node) = self.graph.node_weight(neighbor_idx) {
                    neighbors.push(NeighborEntry {
                        node: neighbor_node.clone(),
                        edge: edge_ref.weight().clone(),
                        direction: EdgeDirection::Incoming,
                        distance: dist + 1,
                    });
                }
                queue.push_back((neighbor_idx, dist + 1));
            }
        }

        Some(GraphNeighborhood {
            center: center_node,
            neighbors,
            depth,
        })
    }

    // =========================================================================
    // Root cause tracing
    // =========================================================================

    /// Follow incoming Caused/DetectedDuring edges backward to find root causes.
    pub fn trace_root_causes(&self, node_key: &str, max_depth: u32) -> Vec<GraphPath> {
        let start_idx = match self.node_index.get(node_key) {
            Some(&idx) => idx,
            None => return vec![],
        };

        let mut results: Vec<GraphPath> = Vec::new();

        // DFS backward along Caused / DetectedDuring edges
        let mut stack: Vec<(NodeIndex, Vec<NodeIndex>)> = vec![(start_idx, vec![start_idx])];

        while let Some((current, path)) = stack.pop() {
            if results.len() >= 10 {
                break;
            }
            if path.len() as u32 > max_depth + 1 {
                continue;
            }

            let mut found_predecessor = false;
            for edge_ref in self.graph.edges_directed(current, Direction::Incoming) {
                let edge_kind = &edge_ref.weight().kind;
                if !matches!(
                    edge_kind,
                    GraphEdgeKind::Caused | GraphEdgeKind::DetectedDuring
                ) {
                    continue;
                }

                let predecessor = edge_ref.source();
                if path.contains(&predecessor) {
                    continue;
                }

                found_predecessor = true;
                let mut new_path = path.clone();
                new_path.push(predecessor);

                if new_path.len() as u32 <= max_depth + 1 {
                    stack.push((predecessor, new_path));
                }
            }

            // If no predecessors found and we've moved beyond the start, this is a root cause
            if !found_predecessor && path.len() > 1 {
                // Reverse the path so it goes root → ... → target
                let mut reversed = path;
                reversed.reverse();
                results.push(self.materialize_path(&reversed));
            }
        }

        results
    }

    // =========================================================================
    // Impact tracing
    // =========================================================================

    /// Follow outgoing Resolved/ImpactsComponent/AppliedIn edges forward to
    /// trace the downstream impact of a node.
    pub fn trace_impact(&self, node_key: &str, max_depth: u32) -> Vec<GraphPath> {
        let start_idx = match self.node_index.get(node_key) {
            Some(&idx) => idx,
            None => return vec![],
        };

        let mut results: Vec<GraphPath> = Vec::new();

        let mut stack: Vec<(NodeIndex, Vec<NodeIndex>)> = vec![(start_idx, vec![start_idx])];

        while let Some((current, path)) = stack.pop() {
            if results.len() >= 10 {
                break;
            }
            if path.len() as u32 > max_depth + 1 {
                continue;
            }

            let mut found_successor = false;
            for edge_ref in self.graph.edges_directed(current, Direction::Outgoing) {
                let edge_kind = &edge_ref.weight().kind;
                if !matches!(
                    edge_kind,
                    GraphEdgeKind::Resolved
                        | GraphEdgeKind::ImpactsComponent
                        | GraphEdgeKind::AppliedIn
                ) {
                    continue;
                }

                let successor = edge_ref.target();
                if path.contains(&successor) {
                    continue;
                }

                found_successor = true;
                let mut new_path = path.clone();
                new_path.push(successor);

                if new_path.len() as u32 <= max_depth + 1 {
                    stack.push((successor, new_path));
                }
            }

            // Terminal nodes of impact chains (leaf nodes with no further impact edges)
            if !found_successor && path.len() > 1 {
                results.push(self.materialize_path(&path));
            }
        }

        results
    }

    // =========================================================================
    // Cross-run causal chains
    // =========================================================================

    /// Find causal chains that span multiple TaskRun nodes for a given workflow.
    pub fn cross_run_causal_chains(&self, workflow_name: &str) -> Vec<GraphPath> {
        // Collect all TaskRun nodes that belong to this workflow
        let run_indices: HashSet<NodeIndex> = self
            .node_index
            .iter()
            .filter(|(key, _)| key.starts_with("task_run:"))
            .filter_map(|(_, &idx)| {
                // Check if this run belongs to the target workflow by following BelongsTo edges
                for edge_ref in self.graph.edges_directed(idx, Direction::Outgoing) {
                    if edge_ref.weight().kind == GraphEdgeKind::BelongsTo {
                        if let Some(wf_node) = self.graph.node_weight(edge_ref.target()) {
                            if wf_node.label == workflow_name {
                                return Some(idx);
                            }
                        }
                    }
                }
                None
            })
            .collect();

        if run_indices.len() < 2 {
            return vec![];
        }

        let mut results: Vec<GraphPath> = Vec::new();

        // For each TaskRun, BFS forward along Caused edges looking for paths
        // that pass through at least one other TaskRun in our set.
        for &start_idx in &run_indices {
            if results.len() >= 10 {
                break;
            }

            let mut visited: HashSet<NodeIndex> = HashSet::new();
            visited.insert(start_idx);
            let mut queue: VecDeque<(NodeIndex, Vec<NodeIndex>, bool)> = VecDeque::new();
            queue.push_back((start_idx, vec![start_idx], false));

            while let Some((current, path, crossed_run)) = queue.pop_front() {
                if results.len() >= 10 {
                    break;
                }
                if path.len() > 10 {
                    continue;
                }

                for edge_ref in self.graph.edges_directed(current, Direction::Outgoing) {
                    let neighbor = edge_ref.target();
                    if path.contains(&neighbor) {
                        continue;
                    }

                    let is_other_run =
                        run_indices.contains(&neighbor) && neighbor != start_idx;
                    let now_crossed = crossed_run || is_other_run;

                    let mut new_path = path.clone();
                    new_path.push(neighbor);

                    if now_crossed && is_other_run {
                        // We've found a cross-run chain
                        results.push(self.materialize_path(&new_path));
                    }

                    if !visited.contains(&neighbor) {
                        visited.insert(neighbor);
                        queue.push_back((neighbor, new_path, now_crossed));
                    }
                }
            }
        }

        results
    }

    // =========================================================================
    // Effectiveness ranking
    // =========================================================================

    /// For fix/rule nodes, compute an effectiveness score based on outgoing edge weights.
    ///
    /// Score = (resolved_count * 2 - caused_regression_count * 3) / total_edges
    /// Returns sorted descending by score.
    pub fn rank_effectiveness(&self, node_kind_filter: &str) -> Vec<(String, f64)> {
        let mut scores: Vec<(String, f64)> = Vec::new();

        for (key, &idx) in &self.node_index {
            let node = match self.graph.node_weight(idx) {
                Some(n) => n,
                None => continue,
            };

            if node.kind.as_str() != node_kind_filter {
                continue;
            }

            let mut resolved_count = 0i64;
            let mut regression_count = 0i64;
            let mut total_edges = 0i64;

            for edge_ref in self.graph.edges_directed(idx, Direction::Outgoing) {
                total_edges += 1;
                match edge_ref.weight().kind {
                    GraphEdgeKind::Resolved => resolved_count += 1,
                    GraphEdgeKind::Caused => {
                        // Check if this is a regression (negative weight or label)
                        if edge_ref.weight().weight < 0.0
                            || edge_ref.weight().label.as_deref() == Some("regression")
                        {
                            regression_count += 1;
                        }
                    }
                    _ => {}
                }
            }

            let score = if total_edges > 0 {
                (resolved_count as f64 * 2.0 - regression_count as f64 * 3.0)
                    / total_edges as f64
            } else {
                0.0
            };

            scores.push((key.clone(), score));
        }

        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores
    }

    // =========================================================================
    // Node search
    // =========================================================================

    /// Case-insensitive substring search on node labels and entity_ids.
    pub fn search_nodes(&self, query: &str, limit: usize) -> Vec<&GraphNode> {
        let q = query.to_lowercase();
        self.graph
            .node_weights()
            .filter(|node| {
                node.label.to_lowercase().contains(&q)
                    || node.entity_id.to_lowercase().contains(&q)
            })
            .take(limit)
            .collect()
    }

    // =========================================================================
    // Similar fix finding
    // =========================================================================

    /// For a fix node, find other fix nodes that share neighbors (Jaccard similarity
    /// on neighbor sets). Returns fixes with similarity >= min_similarity.
    pub fn find_similar_fixes(
        &self,
        fix_key: &str,
        min_similarity: f64,
    ) -> Vec<(String, f64)> {
        let &fix_idx = match self.node_index.get(fix_key) {
            Some(idx) => idx,
            None => return vec![],
        };

        // Collect the neighbor set for the target fix (both directions)
        let target_neighbors: HashSet<NodeIndex> = self
            .graph
            .neighbors_undirected(fix_idx)
            .collect();

        if target_neighbors.is_empty() {
            return vec![];
        }

        let mut results: Vec<(String, f64)> = Vec::new();

        for (key, &idx) in &self.node_index {
            if idx == fix_idx {
                continue;
            }
            // Only compare against other fix nodes
            if !key.starts_with("fix:") {
                continue;
            }

            let other_neighbors: HashSet<NodeIndex> = self
                .graph
                .neighbors_undirected(idx)
                .collect();

            if other_neighbors.is_empty() {
                continue;
            }

            // Jaccard similarity = |intersection| / |union|
            let intersection = target_neighbors.intersection(&other_neighbors).count();
            let union = target_neighbors.union(&other_neighbors).count();

            let similarity = if union > 0 {
                intersection as f64 / union as f64
            } else {
                0.0
            };

            if similarity >= min_similarity {
                results.push((key.clone(), similarity));
            }
        }

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    // =========================================================================
    // Internal path materialization
    // =========================================================================

    /// Convert a sequence of NodeIndex values into a GraphPath with cloned nodes
    /// and the edges between consecutive pairs.
    fn materialize_path(&self, indices: &[NodeIndex]) -> GraphPath {
        let mut nodes: Vec<GraphNode> = Vec::new();
        let mut edges: Vec<GraphEdge> = Vec::new();
        let mut total_weight = 0.0;

        for (i, &idx) in indices.iter().enumerate() {
            if let Some(node) = self.graph.node_weight(idx) {
                nodes.push(node.clone());
            }

            if i + 1 < indices.len() {
                // Find the edge between indices[i] and indices[i+1]
                if let Some(edge_idx) = self.graph.find_edge(idx, indices[i + 1]) {
                    if let Some(edge) = self.graph.edge_weight(edge_idx) {
                        total_weight += edge.weight;
                        edges.push(edge.clone());
                    }
                } else {
                    // Edge might be in the reverse direction (for backward traces)
                    if let Some(edge_idx) = self.graph.find_edge(indices[i + 1], idx) {
                        if let Some(edge) = self.graph.edge_weight(edge_idx) {
                            total_weight += edge.weight;
                            edges.push(edge.clone());
                        }
                    }
                }
            }
        }

        GraphPath {
            nodes,
            edges,
            total_weight,
        }
    }
}

// =============================================================================
// Free helpers
// =============================================================================

/// Map a causal_events event type + id to a graph node key.
/// causal_events uses types like "finding_detected", "error_occurred", "fix_applied",
/// "code_change" — we map these to graph node kind prefixes.
fn causal_event_to_node_key(event_type: &str, event_id: &str) -> String {
    let prefix = match event_type {
        "finding_detected" => "finding",
        "error_occurred" => "error",
        "fix_applied" | "fix_effective" => "fix",
        "code_change" => "component",
        _ => event_type,
    };
    format!("{}:{}", prefix, event_id)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    // -------------------------------------------------------------------------
    // Helper: create in-memory DB with all tables the graph engine queries
    // -------------------------------------------------------------------------

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().expect("Failed to open in-memory DB");
        conn.execute_batch(
            r#"
            CREATE TABLE unified_workflows (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                category TEXT,
                created_at TEXT
            );

            CREATE TABLE workflow_versions (
                id TEXT PRIMARY KEY,
                workflow_id TEXT NOT NULL,
                version_number INTEGER NOT NULL,
                parent_version_id TEXT,
                generation_task_run_id TEXT,
                trigger TEXT NOT NULL DEFAULT 'manual',
                created_at TEXT NOT NULL DEFAULT '2026-01-01T00:00:00Z'
            );

            CREATE TABLE task_runs (
                id TEXT PRIMARY KEY,
                task_name TEXT NOT NULL,
                workflow_name TEXT,
                status TEXT NOT NULL DEFAULT 'completed',
                created_at TEXT NOT NULL DEFAULT '2026-01-01T00:00:00Z'
            );

            CREATE TABLE task_run_findings (
                id TEXT PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                title TEXT NOT NULL,
                category TEXT NOT NULL DEFAULT 'quality',
                severity TEXT NOT NULL DEFAULT 'medium',
                status TEXT NOT NULL DEFAULT 'open',
                created_at TEXT NOT NULL DEFAULT '2026-01-01T00:00:00Z'
            );

            CREATE TABLE reflection_fixes (
                id TEXT PRIMARY KEY,
                source_task_run_id TEXT NOT NULL,
                source_finding_id TEXT,
                fix_type TEXT NOT NULL DEFAULT 'prompt',
                fix_description TEXT NOT NULL DEFAULT '',
                effectiveness TEXT,
                confidence TEXT NOT NULL DEFAULT 'medium',
                status TEXT NOT NULL DEFAULT 'pending',
                created_at TEXT NOT NULL DEFAULT '2026-01-01T00:00:00Z'
            );

            CREATE TABLE error_events (
                id INTEGER PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                signature_hash TEXT NOT NULL,
                error_type TEXT,
                message TEXT NOT NULL DEFAULT '',
                severity TEXT NOT NULL DEFAULT 'error',
                occurrence_count INTEGER NOT NULL DEFAULT 1,
                first_seen_at TEXT NOT NULL DEFAULT '2026-01-01T00:00:00Z'
            );

            CREATE TABLE architecture_components (
                id TEXT PRIMARY KEY,
                workflow_name TEXT,
                component_path TEXT NOT NULL,
                component_type TEXT NOT NULL DEFAULT 'file',
                health_score REAL NOT NULL DEFAULT 1.0,
                created_at TEXT NOT NULL DEFAULT '2026-01-01T00:00:00Z'
            );

            CREATE TABLE generation_rules (
                id TEXT PRIMARY KEY,
                agent TEXT NOT NULL DEFAULT 'default',
                section TEXT NOT NULL DEFAULT 'general',
                title TEXT NOT NULL,
                severity TEXT NOT NULL DEFAULT 'normal',
                status TEXT NOT NULL DEFAULT 'active',
                created_at TEXT NOT NULL DEFAULT '2026-01-01T00:00:00Z'
            );

            CREATE TABLE cross_run_patterns (
                id TEXT PRIMARY KEY,
                workflow_name TEXT,
                pattern_type TEXT NOT NULL DEFAULT 'recurring_error',
                signature_hash TEXT NOT NULL DEFAULT '',
                occurrence_count INTEGER NOT NULL DEFAULT 1,
                status TEXT NOT NULL DEFAULT 'active',
                created_at TEXT NOT NULL DEFAULT '2026-01-01T00:00:00Z'
            );

            CREATE TABLE task_knowledge (
                id TEXT PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                category TEXT NOT NULL DEFAULT 'insight',
                content TEXT NOT NULL DEFAULT '',
                confidence TEXT,
                created_at TEXT NOT NULL DEFAULT '2026-01-01T00:00:00Z'
            );

            CREATE TABLE step_provenance (
                id INTEGER PRIMARY KEY,
                workflow_id TEXT NOT NULL,
                step_name TEXT NOT NULL,
                phase TEXT NOT NULL DEFAULT 'setup',
                generating_agent TEXT NOT NULL DEFAULT 'unknown'
            );

            CREATE TABLE causal_events (
                id INTEGER PRIMARY KEY,
                workflow_name TEXT,
                cause_event_type TEXT NOT NULL,
                cause_event_id TEXT NOT NULL,
                effect_event_type TEXT NOT NULL,
                effect_event_id TEXT NOT NULL,
                relationship TEXT NOT NULL DEFAULT 'caused',
                confidence TEXT NOT NULL DEFAULT 'medium'
            );

            CREATE TABLE rule_influence_log (
                id INTEGER PRIMARY KEY,
                rule_id TEXT NOT NULL,
                workflow_id TEXT
            );

            CREATE TABLE component_relationships (
                id INTEGER PRIMARY KEY,
                workflow_name TEXT,
                source_component TEXT NOT NULL,
                target_component TEXT NOT NULL,
                relationship_type TEXT NOT NULL DEFAULT 'depends_on',
                strength INTEGER NOT NULL DEFAULT 1
            );

            CREATE TABLE fix_applications (
                id INTEGER PRIMARY KEY,
                fix_id TEXT NOT NULL,
                task_run_id TEXT NOT NULL,
                outcome TEXT
            );

            CREATE TABLE step_finding_links (
                id INTEGER PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                step_name TEXT NOT NULL,
                finding_id TEXT NOT NULL
            );
            "#,
        )
        .expect("Failed to create test tables");
        conn
    }

    // =========================================================================
    // Construction tests
    // =========================================================================

    #[test]
    fn test_new_empty_graph() {
        let kg = KnowledgeGraph::new(None);
        assert_eq!(kg.graph.node_count(), 0);
        assert_eq!(kg.graph.edge_count(), 0);
        assert!(kg.workflow_scope.is_none());
    }

    #[test]
    fn test_build_from_empty_db() {
        let conn = setup_test_db();
        let kg = KnowledgeGraph::build_from_db(&conn, None).expect("build_from_db failed");
        assert_eq!(kg.graph.node_count(), 0);
        assert_eq!(kg.graph.edge_count(), 0);
    }

    #[test]
    fn test_build_from_db_with_workflow() {
        let conn = setup_test_db();
        conn.execute(
            "INSERT INTO unified_workflows (id, name, description, category, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params!["wf-1", "my-workflow", "A test workflow", "testing", "2026-01-01T00:00:00Z"],
        )
        .unwrap();

        let kg = KnowledgeGraph::build_from_db(&conn, None).expect("build_from_db failed");
        assert_eq!(kg.graph.node_count(), 1);

        let summary = kg.summary();
        assert_eq!(summary.nodes_by_kind.get("workflow"), Some(&1));
    }

    #[test]
    fn test_build_from_db_with_task_run() {
        let conn = setup_test_db();
        conn.execute(
            "INSERT INTO unified_workflows (id, name) VALUES (?1, ?2)",
            params!["wf-1", "my-workflow"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params!["run-1", "build-task", "my-workflow", "completed", "2026-01-01T00:00:00Z"],
        )
        .unwrap();

        let kg = KnowledgeGraph::build_from_db(&conn, None).expect("build_from_db failed");

        let summary = kg.summary();
        assert_eq!(summary.nodes_by_kind.get("workflow"), Some(&1));
        assert_eq!(summary.nodes_by_kind.get("task_run"), Some(&1));
        // BelongsTo edge from task_run -> workflow
        assert_eq!(summary.edges_by_kind.get("belongs_to"), Some(&1));
        assert_eq!(summary.total_edges, 1);
    }

    // =========================================================================
    // Node management tests
    // =========================================================================

    #[test]
    fn test_get_or_insert_node_dedup() {
        let mut kg = KnowledgeGraph::new(None);
        let node1 = GraphNode::new(GraphNodeKind::Workflow, "wf-1", "Workflow One");
        let node2 = GraphNode::new(GraphNodeKind::Workflow, "wf-1", "Workflow One Duplicate");

        let idx1 = kg.get_or_insert_node(node1);
        let idx2 = kg.get_or_insert_node(node2);

        assert_eq!(idx1, idx2, "Same key should return same NodeIndex");
        assert_eq!(kg.graph.node_count(), 1, "Should not create duplicate node");

        // The label should be the FIRST insert's label (dedup keeps original)
        let node = kg.graph.node_weight(idx1).unwrap();
        assert_eq!(node.label, "Workflow One");
    }

    #[test]
    fn test_add_edge_by_key_missing_node() {
        let mut kg = KnowledgeGraph::new(None);
        let node = GraphNode::new(GraphNodeKind::Workflow, "wf-1", "Workflow");
        kg.get_or_insert_node(node);

        // Both missing
        let result = kg.add_edge_by_key("bogus:1", "bogus:2", GraphEdge::new(GraphEdgeKind::Caused));
        assert!(!result, "Should return false when both nodes missing");

        // From exists, to missing
        let result = kg.add_edge_by_key("workflow:wf-1", "bogus:2", GraphEdge::new(GraphEdgeKind::Caused));
        assert!(!result, "Should return false when target node missing");

        // From missing, to exists
        let result = kg.add_edge_by_key("bogus:1", "workflow:wf-1", GraphEdge::new(GraphEdgeKind::Caused));
        assert!(!result, "Should return false when source node missing");

        // Both exist
        let node2 = GraphNode::new(GraphNodeKind::Fix, "fix-1", "A Fix");
        kg.get_or_insert_node(node2);
        let result = kg.add_edge_by_key("workflow:wf-1", "fix:fix-1", GraphEdge::new(GraphEdgeKind::Caused));
        assert!(result, "Should return true when both nodes exist");
        assert_eq!(kg.graph.edge_count(), 1);
    }

    // =========================================================================
    // Graph query tests
    // =========================================================================

    #[test]
    fn test_summary() {
        let mut kg = KnowledgeGraph::new(None);

        // Insert 2 workflows, 1 fix, 1 finding
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Workflow, "wf-1", "WF1"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Workflow, "wf-2", "WF2"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-1", "Fix1"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-1", "Finding1"));

        // Add 2 edges
        kg.add_edge_by_key("finding:f-1", "fix:fix-1", GraphEdge::new(GraphEdgeKind::Caused));
        kg.add_edge_by_key("fix:fix-1", "finding:f-1", GraphEdge::new(GraphEdgeKind::Resolved));

        let summary = kg.summary();
        assert_eq!(summary.total_nodes, 4);
        assert_eq!(summary.total_edges, 2);
        assert_eq!(summary.nodes_by_kind.get("workflow"), Some(&2));
        assert_eq!(summary.nodes_by_kind.get("fix"), Some(&1));
        assert_eq!(summary.nodes_by_kind.get("finding"), Some(&1));
        assert_eq!(summary.edges_by_kind.get("caused"), Some(&1));
        assert_eq!(summary.edges_by_kind.get("resolved"), Some(&1));
    }

    #[test]
    fn test_search_nodes() {
        let mut kg = KnowledgeGraph::new(None);
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-1", "error in login flow"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-2", "timeout in API call"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-1", "fix for login error"));

        let results = kg.search_nodes("login", 10);
        assert_eq!(results.len(), 2);
        // Both the finding and the fix mention "login"
        let labels: Vec<&str> = results.iter().map(|n| n.label.as_str()).collect();
        assert!(labels.contains(&"error in login flow"));
        assert!(labels.contains(&"fix for login error"));
    }

    #[test]
    fn test_search_nodes_case_insensitive() {
        let mut kg = KnowledgeGraph::new(None);
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-1", "error in login flow"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-2", "timeout issue"));

        // Search uppercase "ERROR" should match lowercase "error"
        let results = kg.search_nodes("ERROR", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].label, "error in login flow");
    }

    #[test]
    fn test_find_paths_direct() {
        let mut kg = KnowledgeGraph::new(None);
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-1", "Finding"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-1", "Fix"));
        kg.add_edge_by_key("finding:f-1", "fix:fix-1", GraphEdge::new(GraphEdgeKind::Caused));

        let paths = kg.find_paths("finding:f-1", "fix:fix-1", 5);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].len(), 1, "Path should have 1 edge");
        assert_eq!(paths[0].nodes.len(), 2, "Path should have 2 nodes");
        assert_eq!(paths[0].edges[0].kind, GraphEdgeKind::Caused);
    }

    #[test]
    fn test_find_paths_no_path() {
        let mut kg = KnowledgeGraph::new(None);
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-1", "Finding"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-1", "Fix"));
        // No edge between them

        let paths = kg.find_paths("finding:f-1", "fix:fix-1", 5);
        assert!(paths.is_empty(), "Disconnected nodes should yield no paths");
    }

    #[test]
    fn test_find_paths_bounded_depth() {
        // Build chain: A -> B -> C -> D -> E -> F (depth 5)
        let mut kg = KnowledgeGraph::new(None);
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "a", "A"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "b", "B"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "c", "C"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "d", "D"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "e", "E"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "f", "F"));

        kg.add_edge_by_key("finding:a", "finding:b", GraphEdge::new(GraphEdgeKind::Caused));
        kg.add_edge_by_key("finding:b", "finding:c", GraphEdge::new(GraphEdgeKind::Caused));
        kg.add_edge_by_key("finding:c", "finding:d", GraphEdge::new(GraphEdgeKind::Caused));
        kg.add_edge_by_key("finding:d", "finding:e", GraphEdge::new(GraphEdgeKind::Caused));
        kg.add_edge_by_key("finding:e", "fix:f", GraphEdge::new(GraphEdgeKind::Caused));

        // Path exists at depth 5 but we limit to 3 — the BFS expands up to
        // max_depth+1 nodes in the path before checking the target, so
        // max_depth=3 can reach at most 4 edges. 5 edges should be unreachable.
        let paths = kg.find_paths("finding:a", "fix:f", 3);
        assert!(paths.is_empty(), "Path at depth 5 should not be found with max_depth=3");

        // With depth 5, it should be found
        let paths = kg.find_paths("finding:a", "fix:f", 5);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].len(), 5);
    }

    #[test]
    fn test_neighborhood() {
        let mut kg = KnowledgeGraph::new(None);
        let center = GraphNode::new(GraphNodeKind::Fix, "fix-1", "Center Fix");
        let n1 = GraphNode::new(GraphNodeKind::Finding, "f-1", "Finding 1");
        let n2 = GraphNode::new(GraphNodeKind::Finding, "f-2", "Finding 2");
        let n3 = GraphNode::new(GraphNodeKind::TaskRun, "run-1", "Run 1");

        kg.get_or_insert_node(center);
        kg.get_or_insert_node(n1);
        kg.get_or_insert_node(n2);
        kg.get_or_insert_node(n3);

        // Outgoing from center
        kg.add_edge_by_key("fix:fix-1", "finding:f-1", GraphEdge::new(GraphEdgeKind::Resolved));
        kg.add_edge_by_key("fix:fix-1", "finding:f-2", GraphEdge::new(GraphEdgeKind::Resolved));
        // Incoming to center
        kg.add_edge_by_key("task_run:run-1", "fix:fix-1", GraphEdge::new(GraphEdgeKind::AppliedIn));

        let hood = kg.neighborhood("fix:fix-1", 1).expect("Should find neighborhood");
        assert_eq!(hood.center.entity_id, "fix-1");
        assert_eq!(hood.neighbors.len(), 3, "Should have 3 neighbors at depth 1");

        let neighbor_ids: HashSet<String> = hood
            .neighbors
            .iter()
            .map(|n| n.node.entity_id.clone())
            .collect();
        assert!(neighbor_ids.contains("f-1"));
        assert!(neighbor_ids.contains("f-2"));
        assert!(neighbor_ids.contains("run-1"));
    }

    #[test]
    fn test_neighborhood_depth_2() {
        // Chain: A -> B -> C
        let mut kg = KnowledgeGraph::new(None);
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "a", "A"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "b", "B"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "c", "C"));

        kg.add_edge_by_key("finding:a", "finding:b", GraphEdge::new(GraphEdgeKind::Caused));
        kg.add_edge_by_key("finding:b", "finding:c", GraphEdge::new(GraphEdgeKind::Caused));

        // depth=1 from A should only reach B
        let hood1 = kg.neighborhood("finding:a", 1).unwrap();
        assert_eq!(hood1.neighbors.len(), 1);
        assert_eq!(hood1.neighbors[0].node.entity_id, "b");
        assert_eq!(hood1.neighbors[0].distance, 1);

        // depth=2 from A should reach both B and C
        let hood2 = kg.neighborhood("finding:a", 2).unwrap();
        assert_eq!(hood2.neighbors.len(), 2);

        let neighbor_ids: HashSet<String> = hood2
            .neighbors
            .iter()
            .map(|n| n.node.entity_id.clone())
            .collect();
        assert!(neighbor_ids.contains("b"));
        assert!(neighbor_ids.contains("c"));

        // Verify distances
        let c_entry = hood2
            .neighbors
            .iter()
            .find(|n| n.node.entity_id == "c")
            .unwrap();
        assert_eq!(c_entry.distance, 2);
    }

    // =========================================================================
    // Traversal tests
    // =========================================================================

    #[test]
    fn test_trace_root_causes() {
        // Chain: error -[Caused]-> finding -[Caused]-> fix
        // Tracing root causes from fix should find error at the root.
        let mut kg = KnowledgeGraph::new(None);
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Error, "err-1", "Root Error"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-1", "Intermediate Finding"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-1", "Leaf Fix"));

        // error caused finding, finding caused fix
        kg.add_edge_by_key("error:err-1", "finding:f-1", GraphEdge::new(GraphEdgeKind::Caused));
        kg.add_edge_by_key("finding:f-1", "fix:fix-1", GraphEdge::new(GraphEdgeKind::Caused));

        let traces = kg.trace_root_causes("fix:fix-1", 5);
        assert!(!traces.is_empty(), "Should find at least one root cause path");

        // The path should go from root (error) to the fix
        let first = &traces[0];
        assert_eq!(first.nodes.first().unwrap().entity_id, "err-1");
        assert_eq!(first.nodes.last().unwrap().entity_id, "fix-1");
    }

    #[test]
    fn test_trace_impact() {
        // Chain: fix -[Resolved]-> finding
        // Tracing impact from fix should find finding.
        let mut kg = KnowledgeGraph::new(None);
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-1", "Applied Fix"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-1", "Resolved Finding"));

        kg.add_edge_by_key("fix:fix-1", "finding:f-1", GraphEdge::new(GraphEdgeKind::Resolved));

        let traces = kg.trace_impact("fix:fix-1", 5);
        assert_eq!(traces.len(), 1);

        let path = &traces[0];
        assert_eq!(path.nodes.first().unwrap().entity_id, "fix-1");
        assert_eq!(path.nodes.last().unwrap().entity_id, "f-1");
        assert_eq!(path.edges[0].kind, GraphEdgeKind::Resolved);
    }

    #[test]
    fn test_rank_effectiveness() {
        let mut kg = KnowledgeGraph::new(None);

        // fix-good: 2 Resolved edges, no regressions
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-good", "Good Fix"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-1", "F1"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-2", "F2"));
        kg.add_edge_by_key("fix:fix-good", "finding:f-1", GraphEdge::new(GraphEdgeKind::Resolved));
        kg.add_edge_by_key("fix:fix-good", "finding:f-2", GraphEdge::new(GraphEdgeKind::Resolved));

        // fix-bad: 0 Resolved edges, just 1 BelongsTo (non-scoring)
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-bad", "Bad Fix"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::TaskRun, "run-1", "Run"));
        kg.add_edge_by_key("fix:fix-bad", "task_run:run-1", GraphEdge::new(GraphEdgeKind::AppliedIn));

        let rankings = kg.rank_effectiveness("fix");
        assert_eq!(rankings.len(), 2);

        // fix-good should be ranked first (score = 2*2/2 = 2.0)
        assert_eq!(rankings[0].0, "fix:fix-good");
        assert!(
            rankings[0].1 > rankings[1].1,
            "Good fix should rank higher than bad fix: {} vs {}",
            rankings[0].1,
            rankings[1].1
        );
    }

    // =========================================================================
    // Similarity tests
    // =========================================================================

    #[test]
    fn test_find_similar_fixes() {
        let mut kg = KnowledgeGraph::new(None);

        // Shared neighbors
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-shared-1", "Shared1"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-shared-2", "Shared2"));
        // Unique neighbor
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-unique", "Unique"));

        // fix-a connects to shared-1, shared-2, unique (3 neighbors)
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-a", "Fix A"));
        kg.add_edge_by_key("fix:fix-a", "finding:f-shared-1", GraphEdge::new(GraphEdgeKind::Resolved));
        kg.add_edge_by_key("fix:fix-a", "finding:f-shared-2", GraphEdge::new(GraphEdgeKind::Resolved));
        kg.add_edge_by_key("fix:fix-a", "finding:f-unique", GraphEdge::new(GraphEdgeKind::Resolved));

        // fix-b connects to shared-1, shared-2 (2 neighbors, both shared)
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-b", "Fix B"));
        kg.add_edge_by_key("fix:fix-b", "finding:f-shared-1", GraphEdge::new(GraphEdgeKind::Resolved));
        kg.add_edge_by_key("fix:fix-b", "finding:f-shared-2", GraphEdge::new(GraphEdgeKind::Resolved));

        // Jaccard(fix-a, fix-b) = |{shared1, shared2}| / |{shared1, shared2, unique}| = 2/3 ≈ 0.667
        let similar = kg.find_similar_fixes("fix:fix-a", 0.5);
        assert_eq!(similar.len(), 1);
        assert_eq!(similar[0].0, "fix:fix-b");
        assert!(
            similar[0].1 > 0.5 && similar[0].1 < 0.8,
            "Jaccard should be ~0.667, got {}",
            similar[0].1
        );
    }

    #[test]
    fn test_find_similar_fixes_no_match() {
        let mut kg = KnowledgeGraph::new(None);

        // fix-a has its own neighbors
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-a", "Fix A"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-1", "Finding 1"));
        kg.add_edge_by_key("fix:fix-a", "finding:f-1", GraphEdge::new(GraphEdgeKind::Resolved));

        // fix-b has completely different neighbors
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-b", "Fix B"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-2", "Finding 2"));
        kg.add_edge_by_key("fix:fix-b", "finding:f-2", GraphEdge::new(GraphEdgeKind::Resolved));

        // Jaccard = 0/2 = 0.0, so nothing should meet min_similarity 0.1
        let similar = kg.find_similar_fixes("fix:fix-a", 0.1);
        assert!(
            similar.is_empty(),
            "Disjoint neighbor sets should produce no similar fixes"
        );
    }
}
