//! Exploration depth modes for State Explorer
//!
//! This module provides configurable exploration depths that control:
//! - How many states to visit
//! - Whether to stop on first failure
//! - Which transitions to explore
//! - Screenshot capture behavior
//!
//! # Critical Path Determination
//!
//! The critical path is determined by:
//! 1. Initial states (always included)
//! 2. States marked as `is_critical: true` in config
//! 3. States with `priority: 0` (highest priority)
//! 4. States with AI descriptions (documented = important)
//! 5. Goal states (explicitly marked endpoints)
//!
//! For QuickScan mode, we find the shortest path through critical states.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

/// Exploration depth presets
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplorationDepth {
    /// Quick scan - critical path only, stop on first failure
    /// Best for: CI/CD pipelines, quick smoke tests
    QuickScan,
    /// Standard exploration - balanced coverage
    /// Best for: Regular testing, development
    Standard,
    /// Deep exploration - thorough coverage
    /// Best for: Release testing, finding edge cases
    Deep,
    /// Exhaustive - every permutation
    /// Best for: Stress testing, complete coverage reports
    Exhaustive,
}

impl Default for ExplorationDepth {
    fn default() -> Self {
        Self::Standard
    }
}

impl ExplorationDepth {
    /// Get human-readable name
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::QuickScan => "Quick Scan",
            Self::Standard => "Standard",
            Self::Deep => "Deep",
            Self::Exhaustive => "Exhaustive",
        }
    }

    /// Get description of what this depth does
    pub fn description(&self) -> &'static str {
        match self {
            Self::QuickScan => "Critical path only, stops on first failure. Fast CI/CD check.",
            Self::Standard => {
                "Balanced exploration with reasonable coverage. Default for development."
            }
            Self::Deep => "Thorough exploration of all states and transitions. Good for releases.",
            Self::Exhaustive => {
                "Complete coverage including all permutations. Use for stress testing."
            }
        }
    }

    /// Maximum number of states to visit (0 = unlimited)
    pub fn max_states(&self) -> u32 {
        match self {
            Self::QuickScan => 5,
            Self::Standard => 20,
            Self::Deep => 0,       // unlimited
            Self::Exhaustive => 0, // unlimited
        }
    }

    /// Whether to stop exploration on first failure
    pub fn stop_on_first_failure(&self) -> bool {
        match self {
            Self::QuickScan => true,
            Self::Standard => false,
            Self::Deep => false,
            Self::Exhaustive => false,
        }
    }

    /// Whether to explore all transitions or just primary ones
    pub fn explore_all_transitions(&self) -> bool {
        match self {
            Self::QuickScan => false,
            Self::Standard => true,
            Self::Deep => true,
            Self::Exhaustive => true,
        }
    }

    /// Whether to capture screenshots for all states or just failures
    pub fn screenshot_mode(&self) -> ScreenshotMode {
        match self {
            Self::QuickScan => ScreenshotMode::FailuresOnly,
            Self::Standard => ScreenshotMode::AllStates,
            Self::Deep => ScreenshotMode::AllStatesAndTransitions,
            Self::Exhaustive => ScreenshotMode::AllStatesAndTransitions,
        }
    }

    /// Whether to include transition screenshots
    pub fn capture_transition_screenshots(&self) -> bool {
        match self {
            Self::QuickScan => false,
            Self::Standard => false,
            Self::Deep => true,
            Self::Exhaustive => true,
        }
    }

    /// Parse from string
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "quick" | "quick_scan" | "quickscan" => Self::QuickScan,
            "standard" | "normal" | "default" => Self::Standard,
            "deep" | "thorough" => Self::Deep,
            "exhaustive" | "full" | "complete" => Self::Exhaustive,
            _ => Self::Standard,
        }
    }
}

/// Screenshot capture modes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotMode {
    /// No screenshots
    None,
    /// Only capture on failures
    FailuresOnly,
    /// Capture at all states
    AllStates,
    /// Capture at all states and during transitions
    AllStatesAndTransitions,
}

/// Configuration derived from exploration depth
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepthConfig {
    pub max_states: u32,
    pub stop_on_first_failure: bool,
    pub explore_all_transitions: bool,
    pub capture_screenshots: bool,
    pub capture_transition_screenshots: bool,
    pub critical_states_only: bool,
}

impl From<ExplorationDepth> for DepthConfig {
    fn from(depth: ExplorationDepth) -> Self {
        Self {
            max_states: depth.max_states(),
            stop_on_first_failure: depth.stop_on_first_failure(),
            explore_all_transitions: depth.explore_all_transitions(),
            capture_screenshots: depth.screenshot_mode() != ScreenshotMode::None,
            capture_transition_screenshots: depth.capture_transition_screenshots(),
            critical_states_only: depth == ExplorationDepth::QuickScan,
        }
    }
}

/// Identifies critical states and computes critical paths
pub struct CriticalPathFinder {
    /// All states in the graph
    states: HashMap<String, CriticalStateInfo>,
    /// Adjacency list for transitions
    adjacency: HashMap<String, Vec<String>>,
    /// Initial states
    initial_states: Vec<String>,
    /// Goal states (if defined)
    goal_states: Vec<String>,
}

/// Information about a state for critical path analysis
#[derive(Debug, Clone)]
struct CriticalStateInfo {
    id: String,
    is_critical: bool,
    priority: u32,
    has_description: bool,
    is_goal: bool,
}

impl CriticalPathFinder {
    /// Create from state machine graph
    pub fn from_graph(graph: &super::exploration::StateMachineGraph) -> Self {
        let mut states = HashMap::new();
        let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();

        for (state_id, state_info) in &graph.states {
            states.insert(
                state_id.clone(),
                CriticalStateInfo {
                    id: state_id.clone(),
                    is_critical: state_info.is_critical,
                    priority: state_info.priority,
                    has_description: state_info.has_description,
                    is_goal: false, // Will be set if state has no outgoing transitions
                },
            );
            adjacency.insert(state_id.clone(), Vec::new());
        }

        // Build adjacency from transitions
        for transition_info in graph.transitions.values() {
            adjacency
                .entry(transition_info.from_state_id.clone())
                .or_default()
                .push(transition_info.to_state_id.clone());
        }

        // Identify goal states (states with no outgoing transitions)
        let mut goal_states = Vec::new();
        for (state_id, neighbors) in &adjacency {
            if neighbors.is_empty() {
                goal_states.push(state_id.clone());
                if let Some(state) = states.get_mut(state_id) {
                    state.is_goal = true;
                }
            }
        }

        Self {
            states,
            adjacency,
            initial_states: graph.initial_states.clone(),
            goal_states,
        }
    }

    /// Get all critical states (based on criticality criteria)
    pub fn get_critical_states(&self) -> Vec<String> {
        let mut critical: Vec<_> = self
            .states
            .values()
            .filter(|s| s.is_critical || s.priority == 0 || s.has_description || s.is_goal)
            .map(|s| s.id.clone())
            .collect();

        // Always include initial states
        for initial in &self.initial_states {
            if !critical.contains(initial) {
                critical.push(initial.clone());
            }
        }

        // Sort by priority
        critical.sort_by(|a, b| {
            let priority_a = self.states.get(a).map(|s| s.priority).unwrap_or(99);
            let priority_b = self.states.get(b).map(|s| s.priority).unwrap_or(99);
            priority_a.cmp(&priority_b)
        });

        critical
    }

    /// Find shortest path through all critical states
    ///
    /// Uses a greedy approach: from each state, go to the nearest unvisited critical state.
    /// This isn't optimal but is fast and gives reasonable results.
    pub fn find_critical_path(&self) -> Vec<String> {
        let critical_states: HashSet<_> = self.get_critical_states().into_iter().collect();

        if critical_states.is_empty() {
            return Vec::new();
        }

        let mut path = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();

        // Start from first initial state
        let start = self.initial_states.first().cloned().unwrap_or_default();
        if start.is_empty() {
            return Vec::new();
        }

        let mut current = start;

        loop {
            // Add current to path if it's critical and not visited
            if critical_states.contains(&current) && !visited.contains(&current) {
                path.push(current.clone());
                visited.insert(current.clone());
            }

            // Find next unvisited critical state
            let next = self.find_nearest_critical(&current, &critical_states, &visited);

            match next {
                Some((next_state, intermediate_path)) => {
                    // Add intermediate states to path (they connect critical states)
                    for state in intermediate_path {
                        if !path.contains(&state) {
                            path.push(state);
                        }
                    }
                    current = next_state;
                }
                None => break, // No more reachable critical states
            }
        }

        path
    }

    /// Find nearest unvisited critical state using BFS
    fn find_nearest_critical(
        &self,
        from: &str,
        critical_states: &HashSet<String>,
        visited: &HashSet<String>,
    ) -> Option<(String, Vec<String>)> {
        let mut queue: VecDeque<(String, Vec<String>)> = VecDeque::new();
        let mut seen: HashSet<String> = HashSet::new();

        queue.push_back((from.to_string(), Vec::new()));
        seen.insert(from.to_string());

        while let Some((current, path)) = queue.pop_front() {
            // Check neighbors
            if let Some(neighbors) = self.adjacency.get(&current) {
                for neighbor in neighbors {
                    if seen.contains(neighbor) {
                        continue;
                    }
                    seen.insert(neighbor.clone());

                    let mut new_path = path.clone();
                    new_path.push(neighbor.clone());

                    // Found an unvisited critical state
                    if critical_states.contains(neighbor) && !visited.contains(neighbor) {
                        return Some((neighbor.clone(), new_path));
                    }

                    queue.push_back((neighbor.clone(), new_path));
                }
            }
        }

        None
    }

    /// Get ordered list of states for quick scan (critical path)
    pub fn get_quick_scan_order(&self) -> Vec<String> {
        self.find_critical_path()
    }

    /// Check if a state is on the critical path
    pub fn is_on_critical_path(&self, state_id: &str) -> bool {
        self.find_critical_path().contains(&state_id.to_string())
    }

    /// Get summary of critical path analysis
    pub fn summary(&self) -> String {
        let critical = self.get_critical_states();
        let path = self.find_critical_path();

        format!(
            "Critical Path: {} critical states, {} states in path, {} initial, {} goal",
            critical.len(),
            path.len(),
            self.initial_states.len(),
            self.goal_states.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_depth_presets() {
        assert_eq!(ExplorationDepth::QuickScan.max_states(), 5);
        assert!(ExplorationDepth::QuickScan.stop_on_first_failure());
        assert!(!ExplorationDepth::Standard.stop_on_first_failure());
        assert_eq!(ExplorationDepth::Deep.max_states(), 0);
    }

    #[test]
    fn test_depth_from_string() {
        assert_eq!(
            ExplorationDepth::from_str("quick"),
            ExplorationDepth::QuickScan
        );
        assert_eq!(ExplorationDepth::from_str("DEEP"), ExplorationDepth::Deep);
        assert_eq!(
            ExplorationDepth::from_str("unknown"),
            ExplorationDepth::Standard
        );
    }

    #[test]
    fn test_depth_config_conversion() {
        let config: DepthConfig = ExplorationDepth::QuickScan.into();
        assert_eq!(config.max_states, 5);
        assert!(config.stop_on_first_failure);
        assert!(config.critical_states_only);
    }

    #[test]
    fn test_screenshot_modes() {
        assert_eq!(
            ExplorationDepth::QuickScan.screenshot_mode(),
            ScreenshotMode::FailuresOnly
        );
        assert_eq!(
            ExplorationDepth::Deep.screenshot_mode(),
            ScreenshotMode::AllStatesAndTransitions
        );
    }
}
