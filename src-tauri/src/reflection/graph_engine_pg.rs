//! PostgreSQL-backed knowledge graph builder.
//!
//! Mirrors the SQLite `KnowledgeGraph::build_from_db` method but queries
//! PostgreSQL via `PgDb`. Each loader method performs a raw SQL query
//! against the PG pool, converting rows into the same graph nodes/edges.
//!
//! All timestamps use `::TEXT` casts since tokio-postgres does not have
//! the `with-chrono-0_4` feature enabled in this workspace.

use super::graph_engine::KnowledgeGraph;
use super::graph_types::*;
use crate::database::pg::PgDb;

impl KnowledgeGraph {
    /// Build the full knowledge graph from PostgreSQL data.
    ///
    /// Async equivalent of `build_from_db`. Queries 16+ tables, creates nodes
    /// for each entity type, then wires up directed edges.
    pub async fn build_from_pg(pg: &PgDb, workflow_name: Option<&str>) -> Result<Self, String> {
        let mut kg = Self::new(workflow_name.map(|s| s.to_string()));

        // --- Nodes ---
        kg.pg_load_workflows(pg, workflow_name).await?;
        kg.pg_load_workflow_versions(pg, workflow_name).await?;
        kg.pg_load_task_runs(pg, workflow_name).await?;
        kg.pg_load_findings(pg, workflow_name).await?;
        kg.pg_load_fixes(pg, workflow_name).await?;
        kg.pg_load_errors(pg, workflow_name).await?;
        kg.pg_load_components(pg, workflow_name).await?;
        kg.pg_load_rules(pg).await?;
        kg.pg_load_patterns(pg, workflow_name).await?;
        kg.pg_load_knowledge(pg, workflow_name).await?;
        kg.pg_load_step_defs(pg, workflow_name).await?;
        kg.pg_load_ui_elements(pg).await?;
        kg.pg_load_skills(pg).await?;
        kg.pg_load_entity_profiles(pg).await?;

        // --- Edges ---
        kg.pg_link_task_runs_to_workflows(pg, workflow_name).await?;
        kg.pg_link_findings_to_task_runs(pg, workflow_name).await?;
        kg.pg_link_fixes_to_findings(pg, workflow_name).await?;
        kg.pg_link_causal_events(pg, workflow_name).await?;
        kg.pg_link_workflow_versions(pg, workflow_name).await?;
        kg.pg_link_step_provenance(pg, workflow_name).await?;
        kg.pg_link_step_finding_links(pg, workflow_name).await?;
        kg.pg_link_rule_influence(pg, workflow_name).await?;
        kg.pg_link_component_relationships(pg, workflow_name)
            .await?;
        kg.pg_link_fix_applications(pg, workflow_name).await?;
        kg.pg_link_ui_interactions(pg).await?;
        kg.pg_link_skills(pg).await?;

        Ok(kg)
    }

    // =========================================================================
    // PG Node loaders
    // =========================================================================

    async fn pg_load_workflows(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT id, name, description, category, created_at::TEXT
                 FROM unified_workflows WHERE name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT id, name, description, category, created_at::TEXT
                 FROM unified_workflows",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG load_workflows: {}", e))?;

        for row in &rows {
            let id: String = row.get(0);
            let name: String = row.get(1);
            let desc: Option<String> = row.get(2);
            let category: Option<String> = row.get(3);
            let created_at: Option<String> = row.get(4);

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

    async fn pg_load_workflow_versions(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT wv.id, wv.workflow_id, wv.version_number, wv.parent_version_id,
                        wv.generation_task_run_id, wv.trigger, wv.created_at::TEXT
                 FROM workflow_versions wv
                 INNER JOIN unified_workflows uw ON uw.id = wv.workflow_id
                 WHERE uw.name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT id, workflow_id, version_number, parent_version_id,
                        generation_task_run_id, trigger, created_at::TEXT
                 FROM workflow_versions",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG load_workflow_versions: {}", e))?;

        for row in &rows {
            let id: String = row.get(0);
            let version_number: i32 = row.get(2);
            let trigger: String = row.get(5);
            let created_at: String = row.get(6);

            let label = format!("v{} ({})", version_number, trigger);
            let node = GraphNode::new(GraphNodeKind::WorkflowVersion, &id, &label)
                .with_property("version_number", serde_json::json!(version_number))
                .with_property("trigger", serde_json::json!(trigger))
                .with_created_at(&created_at);
            self.get_or_insert_node(node);
        }
        Ok(())
    }

    async fn pg_load_task_runs(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT id, task_name, workflow_name, status, created_at::TEXT
                 FROM task_runs WHERE workflow_name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT id, task_name, workflow_name, status, created_at::TEXT
                 FROM task_runs WHERE workflow_name IS NOT NULL",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG load_task_runs: {}", e))?;

        for row in &rows {
            let id: String = row.get(0);
            let task_name: String = row.get(1);
            let status: String = row.get(3);
            let created_at: String = row.get(4);

            let node = GraphNode::new(GraphNodeKind::TaskRun, &id, &task_name)
                .with_property("status", serde_json::json!(status))
                .with_created_at(&created_at);
            self.get_or_insert_node(node);
        }
        Ok(())
    }

    async fn pg_load_findings(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT f.id, f.title, f.category, f.severity, f.status, f.detected_at::TEXT
                 FROM task_run_findings f
                 INNER JOIN task_runs tr ON tr.id = f.task_run_id
                 WHERE tr.workflow_name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT f.id, f.title, f.category, f.severity, f.status, f.detected_at::TEXT
                 FROM task_run_findings f
                 INNER JOIN task_runs tr ON tr.id = f.task_run_id
                 WHERE tr.workflow_name IS NOT NULL",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG load_findings: {}", e))?;

        for row in &rows {
            let id: String = row.get(0);
            let title: String = row.get(1);
            let category: String = row.get(2);
            let severity: String = row.get(3);
            let status: String = row.get(4);
            let detected_at: String = row.get(5);

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
                .with_created_at(&detected_at);
            self.get_or_insert_node(node);
        }
        Ok(())
    }

    async fn pg_load_fixes(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT rf.id, rf.fix_type, rf.fix_description, rf.effectiveness,
                        rf.confidence, rf.status, rf.created_at::TEXT
                 FROM reflection_fixes rf
                 INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
                 WHERE tr.workflow_name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT rf.id, rf.fix_type, rf.fix_description, rf.effectiveness,
                        rf.confidence, rf.status, rf.created_at::TEXT
                 FROM reflection_fixes rf
                 INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
                 WHERE tr.workflow_name IS NOT NULL",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG load_fixes: {}", e))?;

        for row in &rows {
            let id: String = row.get(0);
            let fix_type: String = row.get(1);
            let description: String = row.get(2);
            let effectiveness: Option<String> = row.get(3);
            let confidence: String = row.get(4);
            let status: String = row.get(5);
            let created_at: String = row.get(6);

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

    async fn pg_load_errors(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT MIN(e.id), e.signature_hash, e.error_type, e.message,
                        e.severity, SUM(e.occurrence_count), MIN(e.first_seen_at)::TEXT
                 FROM error_events e
                 INNER JOIN task_runs tr ON tr.id = e.task_run_id
                 WHERE tr.workflow_name = $1
                 GROUP BY e.signature_hash, e.error_type, e.message, e.severity",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT MIN(e.id), e.signature_hash, e.error_type, e.message,
                        e.severity, SUM(e.occurrence_count), MIN(e.first_seen_at)::TEXT
                 FROM error_events e
                 INNER JOIN task_runs tr ON tr.id = e.task_run_id
                 WHERE tr.workflow_name IS NOT NULL
                 GROUP BY e.signature_hash, e.error_type, e.message, e.severity",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG load_errors: {}", e))?;

        for row in &rows {
            let id: i64 = row.get(0);
            let sig_hash: String = row.get(1);
            let error_type: Option<String> = row.get(2);
            let message: String = row.get(3);
            let severity: String = row.get(4);
            let occurrences: i64 = row.get(5);
            let first_seen: String = row.get(6);

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

    async fn pg_load_components(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT id, component_path, component_type, health_score, created_at::TEXT
                 FROM architecture_components WHERE workflow_name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT id, component_path, component_type, health_score, created_at::TEXT
                 FROM architecture_components",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG load_components: {}", e))?;

        for row in &rows {
            let id: String = row.get(0);
            let path: String = row.get(1);
            let comp_type: String = row.get(2);
            let health_score: f64 = row.get(3);
            let created_at: String = row.get(4);

            let node = GraphNode::new(GraphNodeKind::Component, &id, &path)
                .with_weight(health_score)
                .with_property("component_type", serde_json::json!(comp_type))
                .with_property("health_score", serde_json::json!(health_score))
                .with_created_at(&created_at);
            self.get_or_insert_node(node);
        }
        Ok(())
    }

    async fn pg_load_rules(&mut self, pg: &PgDb) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT id, agent, section, title, severity, created_at::TEXT
                 FROM generation_rules WHERE status = 'active'",
                &[],
            )
            .await
            .map_err(|e| format!("PG load_rules: {}", e))?;

        for row in &rows {
            let id: String = row.get(0);
            let agent: String = row.get(1);
            let section: String = row.get(2);
            let title: String = row.get(3);
            let severity: String = row.get(4);
            let created_at: String = row.get(5);

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

    async fn pg_load_patterns(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT id, pattern_type, signature_hash, occurrence_count, status, created_at::TEXT
                 FROM cross_run_patterns
                 WHERE status = 'active' AND workflow_name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT id, pattern_type, signature_hash, occurrence_count, status, created_at::TEXT
                 FROM cross_run_patterns WHERE status = 'active'",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG load_patterns: {}", e))?;

        for row in &rows {
            let id: String = row.get(0);
            let pattern_type: String = row.get(1);
            let sig_hash: String = row.get(2);
            let occurrence_count: i64 = row.get(3);
            let created_at: String = row.get(5);

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

    async fn pg_load_knowledge(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT tk.id, tk.category, tk.content, tk.confidence, tk.created_at::TEXT
                 FROM task_knowledge tk
                 INNER JOIN task_runs tr ON tr.id = tk.task_run_id
                 WHERE tr.workflow_name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT tk.id, tk.category, tk.content, tk.confidence, tk.created_at::TEXT
                 FROM task_knowledge tk
                 INNER JOIN task_runs tr ON tr.id = tk.task_run_id
                 WHERE tr.workflow_name IS NOT NULL",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG load_knowledge: {}", e))?;

        for row in &rows {
            let id: String = row.get(0);
            let category: String = row.get(1);
            let content: String = row.get(2);
            let confidence: Option<String> = row.get(3);
            let created_at: String = row.get(4);

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

    async fn pg_load_step_defs(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT DISTINCT sp.step_name, sp.phase, sp.generating_agent
                 FROM step_provenance sp
                 INNER JOIN unified_workflows uw ON uw.id = sp.workflow_id
                 WHERE uw.name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT DISTINCT step_name, phase, generating_agent
                 FROM step_provenance",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG load_step_defs: {}", e))?;

        for row in &rows {
            let step_name: String = row.get(0);
            let phase: String = row.get(1);
            let agent: String = row.get(2);

            let entity_id = format!("{}:{}", phase, step_name);
            let label = format!("{} [{}]", step_name, phase);
            let node = GraphNode::new(GraphNodeKind::StepDef, &entity_id, &label)
                .with_property("phase", serde_json::json!(phase))
                .with_property("generating_agent", serde_json::json!(&agent));
            self.get_or_insert_node(node);

            let agent_node = GraphNode::new(GraphNodeKind::PipelineAgent, &agent, &agent);
            self.get_or_insert_node(agent_node);
        }
        Ok(())
    }

    async fn pg_load_ui_elements(&mut self, pg: &PgDb) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT element_id, COUNT(*) as interaction_count,
                        CAST(SUM(CASE WHEN success = true THEN 1 ELSE 0 END) AS DOUBLE PRECISION) / COUNT(*) as success_rate
                 FROM ui_bridge_events
                 WHERE element_id IS NOT NULL AND event_type = 'action_executed'
                 GROUP BY element_id
                 ORDER BY interaction_count DESC
                 LIMIT 100",
                &[],
            )
            .await
            .map_err(|e| format!("PG load_ui_elements: {}", e))?;

        for row in &rows {
            let element_id: String = row.get(0);
            let count: i64 = row.get(1);
            let rate: f64 = row.get(2);

            let node = GraphNode::new(GraphNodeKind::UIElement, &element_id, &element_id)
                .with_weight(count as f64)
                .with_property("interaction_count", serde_json::json!(count))
                .with_property("success_rate", serde_json::json!(rate))
                .with_property("flaky", serde_json::json!(rate < 0.95));
            self.get_or_insert_node(node);
        }
        Ok(())
    }

    async fn pg_load_skills(&mut self, pg: &PgDb) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT id, name, slug, category, source, usage_count,
                        version, approval_status, forked_from, created_at::TEXT
                 FROM user_skills
                 ORDER BY usage_count DESC NULLS LAST
                 LIMIT 200",
                &[],
            )
            .await
            .map_err(|e| format!("PG load_skills: {}", e))?;

        for row in &rows {
            let id: String = row.get(0);
            let name: String = row.get(1);
            let slug: String = row.get(2);
            let category: Option<String> = row.get(3);
            let source: Option<String> = row.get(4);
            let usage_count: Option<i64> = row.get(5);
            let version: Option<String> = row.get(6);
            let approval: Option<String> = row.get(7);
            let forked_from: Option<String> = row.get(8);
            let created_at: String = row.get(9);

            let category = category.unwrap_or_else(|| "custom".to_string());
            let source = source.unwrap_or_else(|| "user".to_string());
            let usage_count = usage_count.unwrap_or(0);

            let mut node = GraphNode::new(GraphNodeKind::Skill, &id, &name)
                .with_weight(usage_count as f64 + 1.0)
                .with_property("slug", serde_json::json!(slug))
                .with_property("category", serde_json::json!(category))
                .with_property("source", serde_json::json!(source))
                .with_property("usage_count", serde_json::json!(usage_count))
                .with_created_at(&created_at);

            if let Some(v) = &version {
                node = node.with_property("version", serde_json::json!(v));
            }
            if let Some(a) = &approval {
                node = node.with_property("approval_status", serde_json::json!(a));
            }
            if let Some(f) = &forked_from {
                node = node.with_property("forked_from", serde_json::json!(f));
            }
            self.get_or_insert_node(node);

            if let Some(ref parent_id) = forked_from {
                let parent_key = format!("skill:{}", parent_id);
                let child_key = format!("skill:{}", id);
                self.add_edge_by_key(
                    &child_key,
                    &parent_key,
                    GraphEdge::new(GraphEdgeKind::Supersedes).with_label("forked and improved"),
                );
            }
        }
        Ok(())
    }

    // =========================================================================
    // PG Edge linkers
    // =========================================================================

    async fn pg_link_task_runs_to_workflows(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT tr.id, uw.id
                 FROM task_runs tr
                 INNER JOIN unified_workflows uw ON uw.name = tr.workflow_name
                 WHERE tr.workflow_name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT tr.id, uw.id
                 FROM task_runs tr
                 INNER JOIN unified_workflows uw ON uw.name = tr.workflow_name
                 WHERE tr.workflow_name IS NOT NULL",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG link_task_runs_to_workflows: {}", e))?;

        for row in &rows {
            let run_id: String = row.get(0);
            let wf_id: String = row.get(1);
            let from_key = format!("task_run:{}", run_id);
            let to_key = format!("workflow:{}", wf_id);
            self.add_edge_by_key(&from_key, &to_key, GraphEdge::new(GraphEdgeKind::BelongsTo));
        }
        Ok(())
    }

    async fn pg_link_findings_to_task_runs(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT f.id, f.task_run_id
                 FROM task_run_findings f
                 INNER JOIN task_runs tr ON tr.id = f.task_run_id
                 WHERE tr.workflow_name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT f.id, f.task_run_id
                 FROM task_run_findings f
                 INNER JOIN task_runs tr ON tr.id = f.task_run_id
                 WHERE tr.workflow_name IS NOT NULL",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG link_findings_to_task_runs: {}", e))?;

        for row in &rows {
            let finding_id: String = row.get(0);
            let run_id: String = row.get(1);
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

    async fn pg_link_fixes_to_findings(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT rf.id, rf.source_finding_id, rf.effectiveness
                 FROM reflection_fixes rf
                 INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
                 WHERE tr.workflow_name = $1
                   AND rf.source_finding_id IS NOT NULL",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT rf.id, rf.source_finding_id, rf.effectiveness
                 FROM reflection_fixes rf
                 INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
                 WHERE tr.workflow_name IS NOT NULL
                   AND rf.source_finding_id IS NOT NULL",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG link_fixes_to_findings: {}", e))?;

        for row in &rows {
            let fix_id: String = row.get(0);
            let finding_id: String = row.get(1);
            let effectiveness: Option<String> = row.get(2);

            let finding_key = format!("finding:{}", finding_id);
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
        Ok(())
    }

    async fn pg_link_causal_events(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT cause_event_type, cause_event_id, effect_event_type, effect_event_id,
                        relationship, confidence
                 FROM causal_events WHERE workflow_name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT cause_event_type, cause_event_id, effect_event_type, effect_event_id,
                        relationship, confidence
                 FROM causal_events WHERE workflow_name IS NOT NULL",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG link_causal_events: {}", e))?;

        for row in &rows {
            let cause_type: String = row.get(0);
            let cause_id: String = row.get(1);
            let effect_type: String = row.get(2);
            let effect_id: String = row.get(3);
            let relationship: String = row.get(4);
            let confidence: String = row.get(5);

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

    async fn pg_link_workflow_versions(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT wv.id, wv.parent_version_id, wv.generation_task_run_id
                 FROM workflow_versions wv
                 INNER JOIN unified_workflows uw ON uw.id = wv.workflow_id
                 WHERE uw.name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT id, parent_version_id, generation_task_run_id
                 FROM workflow_versions",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG link_workflow_versions: {}", e))?;

        for row in &rows {
            let version_id: String = row.get(0);
            let parent_id: Option<String> = row.get(1);
            let gen_run_id: Option<String> = row.get(2);

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

    async fn pg_link_step_provenance(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT DISTINCT sp.step_name, sp.phase, sp.generating_agent
                 FROM step_provenance sp
                 INNER JOIN unified_workflows uw ON uw.id = sp.workflow_id
                 WHERE uw.name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT DISTINCT step_name, phase, generating_agent
                 FROM step_provenance",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG link_step_provenance: {}", e))?;

        for row in &rows {
            let step_name: String = row.get(0);
            let phase: String = row.get(1);
            let agent: String = row.get(2);

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

    async fn pg_link_step_finding_links(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT sfl.step_name, sfl.finding_id
                 FROM step_finding_links sfl
                 INNER JOIN task_runs tr ON tr.id = sfl.task_run_id
                 WHERE tr.workflow_name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT sfl.step_name, sfl.finding_id
                 FROM step_finding_links sfl
                 INNER JOIN task_runs tr ON tr.id = sfl.task_run_id
                 WHERE tr.workflow_name IS NOT NULL",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG link_step_finding_links: {}", e))?;

        for row in &rows {
            let step_name: String = row.get(0);
            let finding_id: String = row.get(1);

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

    async fn pg_link_rule_influence(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT ril.rule_id, ril.workflow_id
                 FROM rule_influence_log ril
                 WHERE ril.workflow_id IS NOT NULL
                   AND ril.workflow_id IN (
                       SELECT id FROM unified_workflows WHERE name = $1
                   )",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT rule_id, workflow_id
                 FROM rule_influence_log WHERE workflow_id IS NOT NULL",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG link_rule_influence: {}", e))?;

        for row in &rows {
            let rule_id: String = row.get(0);
            let workflow_id: String = row.get(1);

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

    async fn pg_link_component_relationships(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT source_component, target_component, relationship_type, strength
                 FROM component_relationships WHERE workflow_name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT source_component, target_component, relationship_type, strength
                 FROM component_relationships",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG link_component_relationships: {}", e))?;

        for row in &rows {
            let source: String = row.get(0);
            let target: String = row.get(1);
            let rel_type: String = row.get(2);
            let strength: i64 = row.get(3);

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

    async fn pg_link_fix_applications(
        &mut self,
        pg: &PgDb,
        workflow_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(wn) = workflow_name {
            conn.query(
                "SELECT fa.fix_id, fa.task_run_id, fa.outcome
                 FROM fix_applications fa
                 INNER JOIN task_runs tr ON tr.id = fa.task_run_id
                 WHERE tr.workflow_name = $1",
                &[&wn],
            )
            .await
        } else {
            conn.query(
                "SELECT fa.fix_id, fa.task_run_id, fa.outcome
                 FROM fix_applications fa
                 INNER JOIN task_runs tr ON tr.id = fa.task_run_id
                 WHERE tr.workflow_name IS NOT NULL",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG link_fix_applications: {}", e))?;

        for row in &rows {
            let fix_id: String = row.get(0);
            let run_id: String = row.get(1);
            let outcome: Option<String> = row.get(2);

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

    async fn pg_link_ui_interactions(&mut self, pg: &PgDb) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT DISTINCT
                   CAST(task_run_id AS TEXT) as tr_id,
                   element_id,
                   COUNT(*) as count,
                   CAST(SUM(CASE WHEN success = true THEN 1 ELSE 0 END) AS DOUBLE PRECISION) / COUNT(*) as rate
                 FROM ui_bridge_events
                 WHERE element_id IS NOT NULL
                   AND task_run_id IS NOT NULL
                   AND event_type = 'action_executed'
                 GROUP BY task_run_id, element_id
                 LIMIT 500",
                &[],
            )
            .await
            .map_err(|e| format!("PG link_ui_interactions: {}", e))?;

        for row in &rows {
            let task_run_id: String = row.get(0);
            let element_id: String = row.get(1);
            let count: i64 = row.get(2);
            let rate: f64 = row.get(3);

            let from_key = format!("task_run:{}", task_run_id);
            let to_key = format!("ui_element:{}", element_id);
            let edge = GraphEdge::new(GraphEdgeKind::InteractedWith)
                .with_weight(rate * count as f64)
                .with_label(&format!("{}x ({}% success)", count, (rate * 100.0) as u32));
            self.add_edge_by_key(&from_key, &to_key, edge);
        }
        Ok(())
    }

    async fn pg_link_skills(&mut self, pg: &PgDb) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Link skills to workflows via step_provenance
        let rows = conn
            .query(
                "SELECT sp.workflow_id, us.id as skill_id
                 FROM step_provenance sp
                 INNER JOIN user_skills us ON sp.final_step_json LIKE '%' || us.id || '%'
                 GROUP BY sp.workflow_id, us.id
                 LIMIT 500",
                &[],
            )
            .await
            .map_err(|e| format!("PG link_skills (provenance): {}", e))?;

        for row in &rows {
            let workflow_id: String = row.get(0);
            let skill_id: String = row.get(1);

            let skill_key = format!("skill:{}", skill_id);
            let wf_key = format!("workflow:{}", workflow_id);
            self.add_edge_by_key(
                &skill_key,
                &wf_key,
                GraphEdge::new(GraphEdgeKind::UsedIn).with_label("skill template used in workflow"),
            );
        }

        // Link auto-generated skills to source fixes
        let rows = conn
            .query(
                "SELECT us.id as skill_id, trf.id as finding_id
                 FROM user_skills us
                 INNER JOIN reflection_fixes rf ON us.source_fix_id = rf.id
                 INNER JOIN task_run_findings trf ON rf.source_finding_id = trf.id
                 WHERE us.source = 'auto' AND us.source_fix_id IS NOT NULL
                 LIMIT 200",
                &[],
            )
            .await
            .map_err(|e| format!("PG link_skills (fix): {}", e))?;

        for row in &rows {
            let skill_id: String = row.get(0);
            let finding_id: String = row.get(1);

            let skill_key = format!("skill:{}", skill_id);
            let finding_key = format!("finding:{}", finding_id);
            self.add_edge_by_key(
                &skill_key,
                &finding_key,
                GraphEdge::new(GraphEdgeKind::DerivedFrom)
                    .with_label("auto-extracted from effective fix"),
            );
        }

        // Link skills to cross_run_patterns via source_pattern_id
        let rows = conn
            .query(
                "SELECT us.id, us.source_pattern_id
                 FROM user_skills us
                 WHERE us.source = 'auto' AND us.source_pattern_id IS NOT NULL
                 LIMIT 200",
                &[],
            )
            .await
            .map_err(|e| format!("PG link_skills (pattern): {}", e))?;

        for row in &rows {
            let skill_id: String = row.get(0);
            let pattern_id: String = row.get(1);

            let skill_key = format!("skill:{}", skill_id);
            let pattern_key = format!("pattern:{}", pattern_id);
            self.add_edge_by_key(
                &skill_key,
                &pattern_key,
                GraphEdge::new(GraphEdgeKind::DerivedFrom)
                    .with_label("auto-extracted from recurring pattern"),
            );
        }

        Ok(())
    }
}

/// Map causal_event type + id to a graph node key.
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

impl KnowledgeGraph {
    /// Load entity profiles from PostgreSQL and insert as EntityProfile nodes.
    ///
    /// Also creates ProfileDescribes edges linking each profile to its target
    /// entity node (if that node exists in the graph).
    async fn pg_load_entity_profiles(&mut self, pg: &PgDb) -> Result<(), String> {
        let conn = pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                "SELECT id, entity_kind, entity_id, entity_label, profile_summary, profile_detail,
                        topic_key, content_hash, importance, decay_rate, access_count,
                        last_accessed_at::TEXT, revision_count,
                        source_observation_ids, source_finding_ids, source_fix_ids,
                        source_cross_run_pattern_ids,
                        valid_from::TEXT, valid_until::TEXT, superseded_by,
                        is_deleted, created_at::TEXT, updated_at::TEXT
                 FROM entity_profiles
                 WHERE NOT is_deleted
                 ORDER BY importance DESC
                 LIMIT 500",
                &[],
            )
            .await
            .map_err(|e| format!("PG load entity_profiles: {}", e))?;

        let profiles: Vec<crate::database::types::EntityProfile> = rows
            .iter()
            .map(|r| crate::database::types::EntityProfile {
                id: r.get(0),
                entity_kind: r.get(1),
                entity_id: r.get(2),
                entity_label: r.get(3),
                profile_summary: r.get(4),
                profile_detail: r.get(5),
                topic_key: r.get(6),
                content_hash: r.get(7),
                importance: r.get(8),
                decay_rate: r.get(9),
                access_count: r.get(10),
                last_accessed_at: r.get(11),
                revision_count: r.get(12),
                source_observation_ids: r.get(13),
                source_finding_ids: r.get(14),
                source_fix_ids: r.get(15),
                source_cross_run_pattern_ids: r.get(16),
                valid_from: r.get(17),
                valid_until: r.get(18),
                superseded_by: r.get(19),
                is_deleted: r.get(20),
                created_at: r.get(21),
                updated_at: r.get(22),
            })
            .collect();

        self.load_entity_profiles(&profiles);
        Ok(())
    }
}
