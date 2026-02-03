//! Dependency graph for intelligent exploration ordering
//!
//! This module provides dependency-aware exploration that:
//! 1. Tracks which states depend on other states
//! 2. Generates optimal exploration order via topological sort
//! 3. Prunes dependent states when dependencies fail
//!
//! # Usage
//!
//! ```ignore
//! let dep_graph = DependencyGraph::from_state_machine(&graph);
//! let order = dep_graph.optimal_order();
//!
//! // If state fails, prune its dependents
//! if result.status == Failed {
//!     let skipped = dep_graph.mark_failed(&state_id);
//!     for s in skipped {
//!         // Mark as skipped due to dependency failure
//!     }
//! }

#![allow(dead_code)]
//! ```

use std::collections::{HashMap, HashSet, VecDeque};

/// A node in the dependency graph representing a state
#[derive(Debug, Clone)]
pub struct StateNode {
    /// State ID
    pub id: String,
    /// State name for display
    pub name: String,
    /// Distance from initial state (for ordering)
    pub depth: usize,
    /// States that must be reached before this one
    pub predecessors: HashSet<String>,
    /// States that depend on this one
    pub successors: HashSet<String>,
    /// Whether this state is currently reachable
    pub is_reachable: bool,
    /// Reason if not reachable
    pub unreachable_reason: Option<String>,
}

impl StateNode {
    /// Create a new state node
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            depth: 0,
            predecessors: HashSet::new(),
            successors: HashSet::new(),
            is_reachable: true,
            unreachable_reason: None,
        }
    }
}

/// Dependency edge connecting two states
#[derive(Debug, Clone)]
pub struct DependencyEdge {
    /// Source state ID
    pub from_state: String,
    /// Target state ID
    pub to_state: String,
    /// Transition ID (if from a specific transition)
    pub transition_id: Option<String>,
}

/// Dependency graph for state machine exploration
#[derive(Debug, Clone)]
pub struct DependencyGraph {
    /// States in the graph
    pub nodes: HashMap<String, StateNode>,
    /// Directed edges representing dependencies
    pub edges: Vec<DependencyEdge>,
    /// Initial states (no predecessors required)
    pub initial_states: Vec<String>,
    /// States that have been marked as failed
    pub failed_states: HashSet<String>,
}

impl Default for DependencyGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl DependencyGraph {
    /// Create a new empty dependency graph
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            edges: Vec::new(),
            initial_states: Vec::new(),
            failed_states: HashSet::new(),
        }
    }

    /// Build dependency graph from state machine graph
    pub fn from_state_machine(graph: &super::exploration::StateMachineGraph) -> Self {
        let mut dep_graph = Self::new();

        // Add all states as nodes
        for (state_id, state_info) in &graph.states {
            let mut node = StateNode::new(state_id.clone(), state_info.name.clone());
            node.is_reachable = true;
            dep_graph.nodes.insert(state_id.clone(), node);
        }

        // Copy initial states
        dep_graph.initial_states = graph.initial_states.clone();

        // Add edges from transitions
        for (transition_id, transition_info) in &graph.transitions {
            dep_graph.edges.push(DependencyEdge {
                from_state: transition_info.from_state_id.clone(),
                to_state: transition_info.to_state_id.clone(),
                transition_id: Some(transition_id.clone()),
            });

            // Update predecessor/successor relationships
            if let Some(from_node) = dep_graph.nodes.get_mut(&transition_info.from_state_id) {
                from_node
                    .successors
                    .insert(transition_info.to_state_id.clone());
            }
            if let Some(to_node) = dep_graph.nodes.get_mut(&transition_info.to_state_id) {
                to_node
                    .predecessors
                    .insert(transition_info.from_state_id.clone());
            }
        }

        // Calculate depths via BFS from initial states
        dep_graph.calculate_depths();

        dep_graph
    }

    /// Calculate depths for all nodes using BFS from initial states
    fn calculate_depths(&mut self) {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();

        // Start from initial states at depth 0
        for initial in &self.initial_states {
            queue.push_back((initial.clone(), 0));
            visited.insert(initial.clone());
        }

        while let Some((state_id, depth)) = queue.pop_front() {
            if let Some(node) = self.nodes.get_mut(&state_id) {
                node.depth = depth;

                // Add successors to queue
                for successor in node.successors.clone() {
                    if !visited.contains(&successor) {
                        visited.insert(successor.clone());
                        queue.push_back((successor, depth + 1));
                    }
                }
            }
        }
    }

    /// Get optimal exploration order (topological sort with priority)
    ///
    /// Returns states in order such that:
    /// 1. Initial states come first
    /// 2. A state comes before its dependents
    /// 3. Within same level, prefer critical states
    pub fn optimal_order(&self) -> Vec<String> {
        let mut result = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut in_degree: HashMap<String, usize> = HashMap::new();

        // Calculate in-degrees (number of predecessors)
        for (state_id, node) in &self.nodes {
            // Only count reachable predecessors
            let count = node
                .predecessors
                .iter()
                .filter(|p| self.nodes.get(*p).map(|n| n.is_reachable).unwrap_or(false))
                .count();
            in_degree.insert(state_id.clone(), count);
        }

        // Start with initial states (in-degree 0 and marked as initial)
        let mut queue: VecDeque<String> = self.initial_states.clone().into();

        while let Some(state_id) = queue.pop_front() {
            if visited.contains(&state_id) {
                continue;
            }

            // Check if node is reachable
            if let Some(node) = self.nodes.get(&state_id) {
                if !node.is_reachable {
                    continue;
                }
            }

            visited.insert(state_id.clone());
            result.push(state_id.clone());

            // Process successors
            if let Some(node) = self.nodes.get(&state_id) {
                // Sort successors by depth then by name for deterministic order
                let mut successors: Vec<_> = node.successors.iter().collect();
                successors.sort_by(|a, b| {
                    let depth_a = self.nodes.get(*a).map(|n| n.depth).unwrap_or(0);
                    let depth_b = self.nodes.get(*b).map(|n| n.depth).unwrap_or(0);
                    depth_a.cmp(&depth_b).then_with(|| a.cmp(b))
                });

                for successor in successors {
                    if visited.contains(successor) {
                        continue;
                    }

                    // Decrement in-degree
                    if let Some(deg) = in_degree.get_mut(successor) {
                        *deg = deg.saturating_sub(1);
                        if *deg == 0 {
                            queue.push_back(successor.clone());
                        }
                    }
                }
            }
        }

        // Add any remaining unvisited reachable states (handles disconnected components)
        for (state_id, node) in &self.nodes {
            if !visited.contains(state_id) && node.is_reachable {
                result.push(state_id.clone());
            }
        }

        result
    }

    /// Mark a state as failed and prune dependents
    ///
    /// Returns the list of states that were marked as unreachable due to this failure
    pub fn mark_failed(&mut self, state_id: &str) -> Vec<String> {
        self.failed_states.insert(state_id.to_string());

        // Mark the failed state as unreachable
        if let Some(node) = self.nodes.get_mut(state_id) {
            node.is_reachable = false;
            node.unreachable_reason = Some("State exploration failed".to_string());
        }

        // Find and mark all dependent states as unreachable
        let dependents = self.get_all_dependents(state_id);

        for dependent_id in &dependents {
            if let Some(node) = self.nodes.get_mut(dependent_id) {
                node.is_reachable = false;
                node.unreachable_reason = Some(format!("Depends on failed state: {}", state_id));
            }
        }

        dependents
    }

    /// Get all states that depend on the given state (directly or transitively)
    pub fn get_all_dependents(&self, state_id: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();

        // Start with direct successors
        if let Some(node) = self.nodes.get(state_id) {
            for successor in &node.successors {
                queue.push_back(successor.clone());
            }
        }

        // BFS to find all transitive dependents
        while let Some(current) = queue.pop_front() {
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());
            result.push(current.clone());

            // Add successors of this state
            if let Some(node) = self.nodes.get(&current) {
                for successor in &node.successors {
                    if !visited.contains(successor) {
                        queue.push_back(successor.clone());
                    }
                }
            }
        }

        result
    }

    /// Get states that this state depends on (directly or transitively)
    pub fn get_all_dependencies(&self, state_id: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();

        // Start with direct predecessors
        if let Some(node) = self.nodes.get(state_id) {
            for predecessor in &node.predecessors {
                queue.push_back(predecessor.clone());
            }
        }

        // BFS to find all transitive dependencies
        while let Some(current) = queue.pop_front() {
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());
            result.push(current.clone());

            // Add predecessors of this state
            if let Some(node) = self.nodes.get(&current) {
                for predecessor in &node.predecessors {
                    if !visited.contains(predecessor) {
                        queue.push_back(predecessor.clone());
                    }
                }
            }
        }

        result
    }

    /// Check if a state is reachable
    pub fn is_reachable(&self, state_id: &str) -> bool {
        self.nodes
            .get(state_id)
            .map(|n| n.is_reachable)
            .unwrap_or(false)
    }

    /// Get reason why a state is not reachable
    pub fn unreachable_reason(&self, state_id: &str) -> Option<String> {
        self.nodes
            .get(state_id)
            .and_then(|n| n.unreachable_reason.clone())
    }

    /// Reset all states to reachable
    pub fn reset(&mut self) {
        self.failed_states.clear();
        for node in self.nodes.values_mut() {
            node.is_reachable = true;
            node.unreachable_reason = None;
        }
    }

    /// Get summary of graph structure
    pub fn summary(&self) -> String {
        let total = self.nodes.len();
        let reachable = self.nodes.values().filter(|n| n.is_reachable).count();
        let failed = self.failed_states.len();

        format!(
            "Dependency Graph: {} states ({} reachable, {} failed), {} edges",
            total,
            reachable,
            failed,
            self.edges.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_graph() -> DependencyGraph {
        let mut graph = DependencyGraph::new();

        // Create states: A -> B -> C, A -> D
        graph
            .nodes
            .insert("A".to_string(), StateNode::new("A", "State A"));
        graph
            .nodes
            .insert("B".to_string(), StateNode::new("B", "State B"));
        graph
            .nodes
            .insert("C".to_string(), StateNode::new("C", "State C"));
        graph
            .nodes
            .insert("D".to_string(), StateNode::new("D", "State D"));

        graph.initial_states.push("A".to_string());

        // Add edges
        graph.edges.push(DependencyEdge {
            from_state: "A".to_string(),
            to_state: "B".to_string(),
            transition_id: Some("t1".to_string()),
        });
        graph.edges.push(DependencyEdge {
            from_state: "B".to_string(),
            to_state: "C".to_string(),
            transition_id: Some("t2".to_string()),
        });
        graph.edges.push(DependencyEdge {
            from_state: "A".to_string(),
            to_state: "D".to_string(),
            transition_id: Some("t3".to_string()),
        });

        // Update predecessor/successor relationships
        graph
            .nodes
            .get_mut("A")
            .unwrap()
            .successors
            .insert("B".to_string());
        graph
            .nodes
            .get_mut("A")
            .unwrap()
            .successors
            .insert("D".to_string());
        graph
            .nodes
            .get_mut("B")
            .unwrap()
            .predecessors
            .insert("A".to_string());
        graph
            .nodes
            .get_mut("B")
            .unwrap()
            .successors
            .insert("C".to_string());
        graph
            .nodes
            .get_mut("C")
            .unwrap()
            .predecessors
            .insert("B".to_string());
        graph
            .nodes
            .get_mut("D")
            .unwrap()
            .predecessors
            .insert("A".to_string());

        graph.calculate_depths();

        graph
    }

    #[test]
    fn test_optimal_order() {
        let graph = make_test_graph();
        let order = graph.optimal_order();

        // A should come first
        assert_eq!(order[0], "A");

        // B and D should come before C (C depends on B)
        let b_pos = order.iter().position(|s| s == "B").unwrap();
        let c_pos = order.iter().position(|s| s == "C").unwrap();
        assert!(b_pos < c_pos);
    }

    #[test]
    fn test_mark_failed_prunes_dependents() {
        let mut graph = make_test_graph();

        // Mark B as failed
        let skipped = graph.mark_failed("B");

        // C should be skipped (depends on B)
        assert!(skipped.contains(&"C".to_string()));

        // D should still be reachable (doesn't depend on B)
        assert!(graph.is_reachable("D"));

        // B and C should not be reachable
        assert!(!graph.is_reachable("B"));
        assert!(!graph.is_reachable("C"));
    }

    #[test]
    fn test_get_all_dependents() {
        let graph = make_test_graph();

        let a_deps = graph.get_all_dependents("A");
        assert!(a_deps.contains(&"B".to_string()));
        assert!(a_deps.contains(&"C".to_string()));
        assert!(a_deps.contains(&"D".to_string()));

        let b_deps = graph.get_all_dependents("B");
        assert!(b_deps.contains(&"C".to_string()));
        assert!(!b_deps.contains(&"D".to_string()));
    }

    #[test]
    fn test_depth_calculation() {
        let graph = make_test_graph();

        // A is initial state - depth 0
        assert_eq!(graph.nodes.get("A").unwrap().depth, 0);

        // B and D are direct successors of A - depth 1
        assert_eq!(graph.nodes.get("B").unwrap().depth, 1);
        assert_eq!(graph.nodes.get("D").unwrap().depth, 1);

        // C is successor of B - depth 2
        assert_eq!(graph.nodes.get("C").unwrap().depth, 2);
    }

    #[test]
    fn test_reset() {
        let mut graph = make_test_graph();

        graph.mark_failed("B");
        assert!(!graph.is_reachable("C"));

        graph.reset();
        assert!(graph.is_reachable("C"));
        assert!(graph.failed_states.is_empty());
    }
}
