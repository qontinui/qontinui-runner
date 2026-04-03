//! In-memory knowledge graph engine backed by petgraph.
//!
//! The KnowledgeGraph materializes SQLite data into a traversable directed graph.
//! It queries 16+ tables to build nodes and edges, then provides traversal queries
//! for causal reasoning, impact analysis, pattern detection, and unified search.

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use std::collections::{HashMap, HashSet, VecDeque};

use super::graph_types::*;

// =============================================================================
// KnowledgeGraph
// =============================================================================

pub struct KnowledgeGraph {
    graph: DiGraph<GraphNode, GraphEdge>,
    pub(crate) node_index: HashMap<NodeKey, NodeIndex>,
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
    pub fn build_from_db(workflow_name: Option<&str>) -> Result<Self, String> {
        Err("SQLite removed".to_string())
    }

    // -------------------------------------------------------------------------
    // Node management helpers
    // -------------------------------------------------------------------------

    /// Insert a node if its key does not already exist; return the NodeIndex either way.
    pub(crate) fn get_or_insert_node(&mut self, node: GraphNode) -> NodeIndex {
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
    pub(crate) fn add_edge_by_key(
        &mut self,
        from_key: &str,
        to_key: &str,
        edge: GraphEdge,
    ) -> bool {
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

    fn load_workflows(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    fn load_workflow_versions(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    fn load_task_runs(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    fn load_findings(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    fn load_fixes(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    fn load_errors(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    fn load_components(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    fn load_rules(&mut self) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    fn load_patterns(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    fn load_knowledge(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    fn load_step_defs(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// Load UI Bridge elements that were interacted with during automation.
    /// Capped at 100 elements per workflow to respect memory budgets.
    fn load_ui_elements(&mut self, _workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// Load skills from user_skills table into the graph as Skill nodes.
    ///
    /// Each skill becomes a Skill node with properties for category, source,
    /// usage_count, version, and approval_status. Skills with source="auto"
    /// were procedurally generated from cross-run learning.
    fn load_skills(&mut self) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// Link skills to task runs, findings, and components based on skill_origin
    /// data stored in step provenance and fix applications.
    fn link_skills(&mut self) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// Load pre-fetched observations from PostgreSQL into the graph.
    ///
    /// Observations live in PG (not SQLite), so callers must fetch them async
    /// before passing them here. Each observation becomes an Observation node,
    /// with LearnedFrom edges to any linked task_run and InformedBy edges to
    /// any linked workflow.
    pub fn load_observations_from_pg(
        &mut self,
        observations: &[crate::database::types::ObservationSearchResult],
    ) {
        for obs in observations {
            let id_str = obs.id.to_string();
            let mut node = GraphNode::new(GraphNodeKind::Observation, &id_str, &obs.title)
                .with_property("observation_type", serde_json::json!(obs.observation_type))
                .with_property("scope", serde_json::json!(obs.scope))
                .with_property("revision_count", serde_json::json!(obs.revision_count))
                .with_property("content_preview", serde_json::json!(obs.content_preview));

            if let Some(ref tk) = obs.topic_key {
                node = node.with_property("topic_key", serde_json::json!(tk));
            }
            if let Some(ref pid) = obs.project_id {
                node = node.with_property("project_id", serde_json::json!(pid));
            }
            node = node.with_created_at(&obs.created_at);
            self.get_or_insert_node(node);
        }
    }

    /// Link observations to task runs (LearnedFrom) and workflows (InformedBy).
    ///
    /// Must be called after load_observations_from_pg and after task_run/workflow
    /// nodes have been loaded.
    pub fn link_observations(&mut self, observations: &[crate::database::types::Observation]) {
        for obs in observations {
            let obs_key = format!("observation:{}", obs.id);

            // LearnedFrom: observation → task_run
            if let Some(ref tr_id) = obs.task_run_id {
                let tr_key = format!("task_run:{}", tr_id);
                self.add_edge_by_key(
                    &obs_key,
                    &tr_key,
                    GraphEdge::new(GraphEdgeKind::LearnedFrom).with_label(&obs.observation_type),
                );
            }

            // InformedBy: workflow → observation (workflow was informed by this observation)
            if let Some(ref wf_id) = obs.workflow_id {
                let wf_key = format!("workflow:{}", wf_id);
                self.add_edge_by_key(
                    &wf_key,
                    &obs_key,
                    GraphEdge::new(GraphEdgeKind::InformedBy).with_label(&obs.observation_type),
                );
            }

            // Link skill-mirrored observations to their corresponding Skill node.
            // Skill observations use topic_key="skill:<slug>" format.
            if let Some(ref tk) = obs.topic_key {
                if let Some(slug) = tk.strip_prefix("skill:") {
                    // Find the skill node by matching the slug in user_skills
                    let skill_key = format!("skill:auto:{}", slug);
                    self.add_edge_by_key(
                        &obs_key,
                        &skill_key,
                        GraphEdge::new(GraphEdgeKind::InformedBy)
                            .with_label("observation mirrors skill"),
                    );
                }
            }
        }
    }

    /// Link task runs to UI elements via InteractedWith edges.
    fn link_ui_interactions(&mut self, _workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    // =========================================================================
    // UI Bridge failure chain tracing
    // =========================================================================

    /// Trace the causal chain behind a UI element's failures.
    ///
    /// Traverses: UIElement ←InteractedWith← TaskRun →BelongsTo→ Workflow,
    /// collecting connected findings, fixes, and rules along the way.
    /// Returns paths showing element → task_run → workflow → related entities.
    pub fn trace_ui_failure_chain(&self, element_key: &str) -> Vec<GraphPath> {
        let full_key = if element_key.starts_with("ui_element:") {
            element_key.to_string()
        } else {
            format!("ui_element:{}", element_key)
        };

        let start_idx = match self.node_index.get(&full_key) {
            Some(&idx) => idx,
            None => return vec![],
        };

        let mut results: Vec<GraphPath> = Vec::new();
        let mut visited = std::collections::HashSet::new();
        visited.insert(start_idx);

        // Step 1: find all TaskRuns that interacted with this element
        for edge_ref in self.graph.edges_directed(start_idx, Direction::Incoming) {
            if !matches!(edge_ref.weight().kind, GraphEdgeKind::InteractedWith) {
                continue;
            }

            let task_run_idx = edge_ref.source();
            if !visited.insert(task_run_idx) {
                continue;
            }

            // Step 2: from TaskRun, follow BelongsTo → Workflow
            for edge2 in self.graph.edges_directed(task_run_idx, Direction::Outgoing) {
                let target = edge2.target();
                if !matches!(
                    edge2.weight().kind,
                    GraphEdgeKind::BelongsTo
                        | GraphEdgeKind::DetectedDuring
                        | GraphEdgeKind::AppliedIn
                ) {
                    continue;
                }

                if results.len() >= 10 {
                    break;
                }

                let path_nodes = vec![start_idx, task_run_idx, target];
                results.push(self.materialize_path(&path_nodes));
            }

            // Step 3: from TaskRun, find connected findings (DetectedDuring edges incoming)
            for edge3 in self.graph.edges_directed(task_run_idx, Direction::Incoming) {
                if matches!(edge3.weight().kind, GraphEdgeKind::DetectedDuring) {
                    let finding_idx = edge3.source();
                    if visited.insert(finding_idx) && results.len() < 10 {
                        let path_nodes = vec![start_idx, task_run_idx, finding_idx];
                        results.push(self.materialize_path(&path_nodes));
                    }
                }
            }
        }

        results
    }

    /// Score fix effectiveness for UI-related fixes.
    ///
    /// For each Fix node connected to UIElement interactions, compute:
    /// score = (resolved_count * 2 - regression_count * 3) / total_applications
    pub fn score_ui_fix_effectiveness(&self) -> Vec<(String, f64, u32, u32)> {
        let mut scores: Vec<(String, f64, u32, u32)> = Vec::new();

        for (key, idx) in self
            .node_index
            .iter()
            .filter(|(k, _)| k.starts_with("fix:"))
        {
            let _ = key;
            let mut resolved = 0u32;
            let mut regressions = 0u32;
            let mut total = 0u32;

            for edge in self.graph.edges_directed(*idx, Direction::Outgoing) {
                total += 1;
                match edge.weight().kind {
                    GraphEdgeKind::Resolved => resolved += 1,
                    GraphEdgeKind::Caused => regressions += 1,
                    _ => {}
                }
            }

            if total > 0 {
                let node = &self.graph[*idx];
                let score = (resolved as f64 * 2.0 - regressions as f64 * 3.0) / total as f64;
                scores.push((node.key.clone(), score, resolved, regressions));
            }
        }

        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(20);
        scores
    }

    // =========================================================================
    // Private loaders — Edges
    // =========================================================================

    /// task_runs.workflow_name → BelongsTo → workflow
    fn link_task_runs_to_workflows(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// task_run_findings.task_run_id → DetectedDuring → task_run
    fn link_findings_to_task_runs(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// reflection_fixes.source_finding_id → Caused (finding → fix)
    /// reflection_fixes where effective → Resolved (fix → finding)
    fn link_fixes_to_findings(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// causal_events → Caused / Resolved edges based on relationship type
    fn link_causal_events(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// workflow_versions parent → EvolvedFrom, generation_task_run_id → GeneratedBy
    fn link_workflow_versions(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// step_provenance → BuiltBy (step → pipeline_agent)
    fn link_step_provenance(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// step_finding_links → DetectedDuring (step → finding)
    fn link_step_finding_links(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// rule_influence_log → InfluencedBy (rule → workflow)
    fn link_rule_influence(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// component_relationships → ImpactsComponent
    fn link_component_relationships(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// fix_applications → AppliedIn (fix → task_run)
    fn link_fix_applications(&mut self, workflow_name: Option<&str>) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    // =========================================================================
    // Utility helpers
    // =========================================================================

    /// Find a component node key by its path label (since component_relationships
    /// reference by path, not by DB id).
    pub(crate) fn find_component_by_path(&self, path: &str) -> Option<String> {
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
    pub fn ingest_task_run(&mut self, task_run_id: &str) -> Result<u32, String> {
        Err("SQLite removed".to_string())
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
    pub fn find_paths(&self, from_key: &str, to_key: &str, max_depth: u32) -> Vec<GraphPath> {
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

                    let is_other_run = run_indices.contains(&neighbor) && neighbor != start_idx;
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
                (resolved_count as f64 * 2.0 - regression_count as f64 * 3.0) / total_edges as f64
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
                node.label.to_lowercase().contains(&q) || node.entity_id.to_lowercase().contains(&q)
            })
            .take(limit)
            .collect()
    }

    // =========================================================================
    // Similar fix finding
    // =========================================================================

    /// For a fix node, find other fix nodes that share neighbors (Jaccard similarity
    /// on neighbor sets). Returns fixes with similarity >= min_similarity.
    pub fn find_similar_fixes(&self, fix_key: &str, min_similarity: f64) -> Vec<(String, f64)> {
        let &fix_idx = match self.node_index.get(fix_key) {
            Some(idx) => idx,
            None => return vec![],
        };

        // Collect the neighbor set for the target fix (both directions)
        let target_neighbors: HashSet<NodeIndex> =
            self.graph.neighbors_undirected(fix_idx).collect();

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

            let other_neighbors: HashSet<NodeIndex> =
                self.graph.neighbors_undirected(idx).collect();

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
        // SQLite removed - no-op
    }

    #[test]
    fn test_build_from_db_with_workflow() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_build_from_db_with_task_run() {
        // SQLite removed - no-op
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
        let result =
            kg.add_edge_by_key("bogus:1", "bogus:2", GraphEdge::new(GraphEdgeKind::Caused));
        assert!(!result, "Should return false when both nodes missing");

        // From exists, to missing
        let result = kg.add_edge_by_key(
            "workflow:wf-1",
            "bogus:2",
            GraphEdge::new(GraphEdgeKind::Caused),
        );
        assert!(!result, "Should return false when target node missing");

        // From missing, to exists
        let result = kg.add_edge_by_key(
            "bogus:1",
            "workflow:wf-1",
            GraphEdge::new(GraphEdgeKind::Caused),
        );
        assert!(!result, "Should return false when source node missing");

        // Both exist
        let node2 = GraphNode::new(GraphNodeKind::Fix, "fix-1", "A Fix");
        kg.get_or_insert_node(node2);
        let result = kg.add_edge_by_key(
            "workflow:wf-1",
            "fix:fix-1",
            GraphEdge::new(GraphEdgeKind::Caused),
        );
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
        kg.add_edge_by_key(
            "finding:f-1",
            "fix:fix-1",
            GraphEdge::new(GraphEdgeKind::Caused),
        );
        kg.add_edge_by_key(
            "fix:fix-1",
            "finding:f-1",
            GraphEdge::new(GraphEdgeKind::Resolved),
        );

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
        kg.get_or_insert_node(GraphNode::new(
            GraphNodeKind::Finding,
            "f-1",
            "error in login flow",
        ));
        kg.get_or_insert_node(GraphNode::new(
            GraphNodeKind::Finding,
            "f-2",
            "timeout in API call",
        ));
        kg.get_or_insert_node(GraphNode::new(
            GraphNodeKind::Fix,
            "fix-1",
            "fix for login error",
        ));

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
        kg.get_or_insert_node(GraphNode::new(
            GraphNodeKind::Finding,
            "f-1",
            "error in login flow",
        ));
        kg.get_or_insert_node(GraphNode::new(
            GraphNodeKind::Finding,
            "f-2",
            "timeout issue",
        ));

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
        kg.add_edge_by_key(
            "finding:f-1",
            "fix:fix-1",
            GraphEdge::new(GraphEdgeKind::Caused),
        );

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

        kg.add_edge_by_key(
            "finding:a",
            "finding:b",
            GraphEdge::new(GraphEdgeKind::Caused),
        );
        kg.add_edge_by_key(
            "finding:b",
            "finding:c",
            GraphEdge::new(GraphEdgeKind::Caused),
        );
        kg.add_edge_by_key(
            "finding:c",
            "finding:d",
            GraphEdge::new(GraphEdgeKind::Caused),
        );
        kg.add_edge_by_key(
            "finding:d",
            "finding:e",
            GraphEdge::new(GraphEdgeKind::Caused),
        );
        kg.add_edge_by_key("finding:e", "fix:f", GraphEdge::new(GraphEdgeKind::Caused));

        // Path exists at depth 5 but we limit to 3 — the BFS expands up to
        // max_depth+1 nodes in the path before checking the target, so
        // max_depth=3 can reach at most 4 edges. 5 edges should be unreachable.
        let paths = kg.find_paths("finding:a", "fix:f", 3);
        assert!(
            paths.is_empty(),
            "Path at depth 5 should not be found with max_depth=3"
        );

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
        kg.add_edge_by_key(
            "fix:fix-1",
            "finding:f-1",
            GraphEdge::new(GraphEdgeKind::Resolved),
        );
        kg.add_edge_by_key(
            "fix:fix-1",
            "finding:f-2",
            GraphEdge::new(GraphEdgeKind::Resolved),
        );
        // Incoming to center
        kg.add_edge_by_key(
            "task_run:run-1",
            "fix:fix-1",
            GraphEdge::new(GraphEdgeKind::AppliedIn),
        );

        let hood = kg
            .neighborhood("fix:fix-1", 1)
            .expect("Should find neighborhood");
        assert_eq!(hood.center.entity_id, "fix-1");
        assert_eq!(
            hood.neighbors.len(),
            3,
            "Should have 3 neighbors at depth 1"
        );

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

        kg.add_edge_by_key(
            "finding:a",
            "finding:b",
            GraphEdge::new(GraphEdgeKind::Caused),
        );
        kg.add_edge_by_key(
            "finding:b",
            "finding:c",
            GraphEdge::new(GraphEdgeKind::Caused),
        );

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
        kg.get_or_insert_node(GraphNode::new(
            GraphNodeKind::Finding,
            "f-1",
            "Intermediate Finding",
        ));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-1", "Leaf Fix"));

        // error caused finding, finding caused fix
        kg.add_edge_by_key(
            "error:err-1",
            "finding:f-1",
            GraphEdge::new(GraphEdgeKind::Caused),
        );
        kg.add_edge_by_key(
            "finding:f-1",
            "fix:fix-1",
            GraphEdge::new(GraphEdgeKind::Caused),
        );

        let traces = kg.trace_root_causes("fix:fix-1", 5);
        assert!(
            !traces.is_empty(),
            "Should find at least one root cause path"
        );

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
        kg.get_or_insert_node(GraphNode::new(
            GraphNodeKind::Finding,
            "f-1",
            "Resolved Finding",
        ));

        kg.add_edge_by_key(
            "fix:fix-1",
            "finding:f-1",
            GraphEdge::new(GraphEdgeKind::Resolved),
        );

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
        kg.add_edge_by_key(
            "fix:fix-good",
            "finding:f-1",
            GraphEdge::new(GraphEdgeKind::Resolved),
        );
        kg.add_edge_by_key(
            "fix:fix-good",
            "finding:f-2",
            GraphEdge::new(GraphEdgeKind::Resolved),
        );

        // fix-bad: 0 Resolved edges, just 1 BelongsTo (non-scoring)
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-bad", "Bad Fix"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::TaskRun, "run-1", "Run"));
        kg.add_edge_by_key(
            "fix:fix-bad",
            "task_run:run-1",
            GraphEdge::new(GraphEdgeKind::AppliedIn),
        );

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
        kg.get_or_insert_node(GraphNode::new(
            GraphNodeKind::Finding,
            "f-shared-1",
            "Shared1",
        ));
        kg.get_or_insert_node(GraphNode::new(
            GraphNodeKind::Finding,
            "f-shared-2",
            "Shared2",
        ));
        // Unique neighbor
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-unique", "Unique"));

        // fix-a connects to shared-1, shared-2, unique (3 neighbors)
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-a", "Fix A"));
        kg.add_edge_by_key(
            "fix:fix-a",
            "finding:f-shared-1",
            GraphEdge::new(GraphEdgeKind::Resolved),
        );
        kg.add_edge_by_key(
            "fix:fix-a",
            "finding:f-shared-2",
            GraphEdge::new(GraphEdgeKind::Resolved),
        );
        kg.add_edge_by_key(
            "fix:fix-a",
            "finding:f-unique",
            GraphEdge::new(GraphEdgeKind::Resolved),
        );

        // fix-b connects to shared-1, shared-2 (2 neighbors, both shared)
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-b", "Fix B"));
        kg.add_edge_by_key(
            "fix:fix-b",
            "finding:f-shared-1",
            GraphEdge::new(GraphEdgeKind::Resolved),
        );
        kg.add_edge_by_key(
            "fix:fix-b",
            "finding:f-shared-2",
            GraphEdge::new(GraphEdgeKind::Resolved),
        );

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
        kg.add_edge_by_key(
            "fix:fix-a",
            "finding:f-1",
            GraphEdge::new(GraphEdgeKind::Resolved),
        );

        // fix-b has completely different neighbors
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Fix, "fix-b", "Fix B"));
        kg.get_or_insert_node(GraphNode::new(GraphNodeKind::Finding, "f-2", "Finding 2"));
        kg.add_edge_by_key(
            "fix:fix-b",
            "finding:f-2",
            GraphEdge::new(GraphEdgeKind::Resolved),
        );

        // Jaccard = 0/2 = 0.0, so nothing should meet min_similarity 0.1
        let similar = kg.find_similar_fixes("fix:fix-a", 0.1);
        assert!(
            similar.is_empty(),
            "Disjoint neighbor sets should produce no similar fixes"
        );
    }
}
