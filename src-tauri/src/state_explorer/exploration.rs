//! Exploration strategies for state machine traversal
//!
//! This module implements different strategies for exploring the state machine
//! during state exploration. Each strategy has different trade-offs between coverage,
//! speed, and focus areas.

#![allow(dead_code)]

use crate::config::{StateDescription, TransitionDescription};
use rand::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

/// Exploration strategy for state machine exploration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ExplorationStrategy {
    /// Visit every state and transition - complete but slow
    #[default]
    Exhaustive,
    /// Quick path through critical states marked with high priority
    SmokeTest,
    /// Focus on previously-failed areas (requires failure history)
    Regression,
    /// Random walk to discover unexpected behaviors
    RandomWalk,
    /// Explore only specific states/transitions provided in config
    Targeted,
    /// Pre-seeded exploration: skip known states, flag flaky element transitions
    Seeded,
}

impl ExplorationStrategy {
    /// Parse a strategy from a string
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "exhaustive" => Self::Exhaustive,
            "smoke_test" | "smoketest" | "smoke" => Self::SmokeTest,
            "regression" => Self::Regression,
            "random_walk" | "randomwalk" | "random" => Self::RandomWalk,
            "targeted" => Self::Targeted,
            "seeded" => Self::Seeded,
            _ => Self::Exhaustive,
        }
    }
}

/// Represents a visit to a state during exploration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateVisit {
    /// State ID being visited
    pub state_id: String,
    /// State name for display
    pub state_name: String,
    /// How we arrived at this state (transition ID, or "initial" for starting states)
    pub arrived_via: Option<String>,
    /// Depth in the exploration tree
    pub depth: u32,
    /// Index in the exploration sequence
    pub sequence_index: u32,
    /// Whether this is a critical state for smoke tests
    pub is_critical: bool,
    /// Priority for exploration order
    pub priority: u32,
}

/// A planned path through the state machine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationPath {
    /// Ordered list of states to visit
    pub states: Vec<StateVisit>,
    /// Transitions to take between states
    pub transitions: Vec<TransitionStep>,
    /// Total estimated cost/time
    pub estimated_cost: f64,
    /// Strategy that generated this path
    pub strategy: ExplorationStrategy,
}

/// A transition step in the exploration path
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionStep {
    /// Transition ID
    pub transition_id: String,
    /// Source state ID
    pub from_state_id: String,
    /// Target state ID
    pub to_state_id: String,
    /// Sequence index in the path
    pub sequence_index: u32,
    /// Whether to verify this transition
    pub verify: bool,
}

/// State machine structure for exploration planning
#[derive(Debug, Clone, Default)]
pub struct StateMachineGraph {
    /// Map of state ID to state info
    pub states: HashMap<String, StateInfo>,
    /// Map of transition ID to transition info
    pub transitions: HashMap<String, TransitionInfo>,
    /// Adjacency list: state ID -> list of outgoing transition IDs
    pub adjacency: HashMap<String, Vec<String>>,
    /// Initial state IDs
    pub initial_states: Vec<String>,
}

/// Information about a state for exploration planning
#[derive(Debug, Clone)]
pub struct StateInfo {
    pub id: String,
    pub name: String,
    pub is_initial: bool,
    pub is_critical: bool,
    pub priority: u32,
    pub has_description: bool,
    /// Elements expected to be visible in this state
    pub expected_elements: Vec<String>,
    /// Elements that should NOT be visible in this state
    pub unexpected_elements: Vec<String>,
    /// Rich AI description for verification
    pub ai_description: Option<StateDescription>,
    /// Optional explicit assertions for this state
    pub assertions: Vec<super::assertions::StateAssertion>,
}

/// Information about a transition for exploration planning
#[derive(Debug, Clone)]
pub struct TransitionInfo {
    pub id: String,
    pub from_state_id: String,
    pub to_state_id: String,
    pub cost: f64,
    pub has_description: bool,
    pub expected_duration_ms: Option<u64>,
    /// Rich AI description for verification
    pub ai_description: Option<TransitionDescription>,
}

impl StateMachineGraph {
    /// Create a new empty graph
    pub fn new() -> Self {
        Self::default()
    }

    /// Build graph from loaded config
    pub fn from_config(config: &serde_json::Value) -> Self {
        let mut graph = Self::new();

        // Parse states
        if let Some(states) = config.get("states").and_then(|s| s.as_array()) {
            for state in states {
                if let Some(id) = state.get("id").and_then(|v| v.as_str()) {
                    let name = state
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(id)
                        .to_string();
                    let is_initial = state
                        .get("isInitial")
                        .or_else(|| state.get("is_initial"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    // Parse AI description (new field)
                    let ai_description: Option<StateDescription> = state
                        .get("aiDescription")
                        .and_then(|d| serde_json::from_value(d.clone()).ok());

                    // Check for simple description or AI description
                    let has_description =
                        state.get("description").is_some() || ai_description.is_some();

                    // Get expected elements from AI description or legacy description field
                    let expected_elements = ai_description
                        .as_ref()
                        .and_then(|d| d.expected_elements.clone())
                        .or_else(|| {
                            // Fallback to legacy description.expected_elements
                            state
                                .get("description")
                                .and_then(|d| d.get("expected_elements"))
                                .and_then(|e| e.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|v| v.as_str().map(String::from))
                                        .collect()
                                })
                        })
                        .unwrap_or_default();

                    // Get unexpected elements from AI description or legacy description field
                    let unexpected_elements = ai_description
                        .as_ref()
                        .and_then(|d| d.unexpected_elements.clone())
                        .or_else(|| {
                            // Fallback to legacy description.unexpected_elements
                            state
                                .get("description")
                                .and_then(|d| d.get("unexpected_elements"))
                                .and_then(|e| e.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|v| v.as_str().map(String::from))
                                        .collect()
                                })
                        })
                        .unwrap_or_default();

                    // Determine priority from AI description or default
                    let priority = if ai_description.is_some() {
                        0 // Highest priority for states with AI descriptions
                    } else if has_description {
                        1
                    } else {
                        2
                    };
                    let is_critical = is_initial || has_description;

                    // Parse explicit assertions if defined
                    let assertions: Vec<super::assertions::StateAssertion> = state
                        .get("assertions")
                        .and_then(|a| a.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                                .collect()
                        })
                        .unwrap_or_default();

                    let state_info = StateInfo {
                        id: id.to_string(),
                        name,
                        is_initial,
                        is_critical,
                        priority,
                        has_description,
                        expected_elements,
                        unexpected_elements,
                        ai_description,
                        assertions,
                    };

                    if is_initial {
                        graph.initial_states.push(id.to_string());
                    }

                    graph.states.insert(id.to_string(), state_info);
                    graph.adjacency.insert(id.to_string(), Vec::new());
                }
            }
        }

        // Parse transitions
        if let Some(transitions) = config.get("transitions").and_then(|t| t.as_array()) {
            for transition in transitions {
                if let (Some(id), Some(from), Some(to)) = (
                    transition.get("id").and_then(|v| v.as_str()),
                    transition.get("fromStateId").and_then(|v| v.as_str()),
                    transition.get("toStateId").and_then(|v| v.as_str()),
                ) {
                    // Parse AI description (new field)
                    let ai_description: Option<TransitionDescription> = transition
                        .get("aiDescription")
                        .and_then(|d| serde_json::from_value(d.clone()).ok());

                    let has_description =
                        transition.get("description").is_some() || ai_description.is_some();

                    // Get expected duration from AI description or legacy field
                    let expected_duration_ms = ai_description
                        .as_ref()
                        .and_then(|d| d.expected_duration_ms)
                        .or_else(|| {
                            // Fallback to legacy description.expected_duration_ms
                            transition
                                .get("description")
                                .and_then(|d| d.get("expected_duration_ms"))
                                .and_then(|v| v.as_u64())
                        });

                    let transition_info = TransitionInfo {
                        id: id.to_string(),
                        from_state_id: from.to_string(),
                        to_state_id: to.to_string(),
                        cost: 1.0,
                        has_description,
                        expected_duration_ms,
                        ai_description,
                    };

                    graph.transitions.insert(id.to_string(), transition_info);

                    // Add to adjacency list
                    graph
                        .adjacency
                        .entry(from.to_string())
                        .or_default()
                        .push(id.to_string());
                }
            }
        }

        graph
    }

    /// Get outgoing transitions from a state
    pub fn get_outgoing_transitions(&self, state_id: &str) -> Vec<&TransitionInfo> {
        self.adjacency
            .get(state_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|tid| self.transitions.get(tid))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Explorer that generates paths through the state machine
pub struct StateExplorer {
    graph: StateMachineGraph,
    strategy: ExplorationStrategy,
    max_states: u32,
    target_states: HashSet<String>,
    target_transitions: HashSet<String>,
    rng: Option<StdRng>,
    failure_history: HashSet<String>, // States that previously failed
    known_states: HashSet<String>,    // States discovered in prior runs (for Seeded strategy)
    flaky_elements: HashSet<String>,  // Element IDs with high failure rates
}

impl StateExplorer {
    /// Create a new explorer
    pub fn new(graph: StateMachineGraph, strategy: ExplorationStrategy) -> Self {
        Self {
            graph,
            strategy,
            max_states: 0,
            target_states: HashSet::new(),
            target_transitions: HashSet::new(),
            rng: None,
            failure_history: HashSet::new(),
            known_states: HashSet::new(),
            flaky_elements: HashSet::new(),
        }
    }

    /// Set maximum number of states to visit
    pub fn with_max_states(mut self, max_states: u32) -> Self {
        self.max_states = max_states;
        self
    }

    /// Set target states for targeted strategy
    pub fn with_targets(mut self, states: Vec<String>, transitions: Vec<String>) -> Self {
        self.target_states = states.into_iter().collect();
        self.target_transitions = transitions.into_iter().collect();
        self
    }

    /// Set random seed for reproducible random walks
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.rng = Some(StdRng::seed_from_u64(seed));
        self
    }

    /// Set failure history for regression testing
    pub fn with_failure_history(mut self, failures: HashSet<String>) -> Self {
        self.failure_history = failures;
        self
    }

    /// Set known states from prior runs (for Seeded strategy).
    /// Known states are skipped during exploration, reducing time.
    pub fn with_known_states(mut self, states: HashSet<String>) -> Self {
        self.known_states = states;
        self
    }

    /// Set flaky element IDs (for Seeded strategy).
    /// Transitions involving flaky elements get higher path costs.
    pub fn with_flaky_elements(mut self, elements: HashSet<String>) -> Self {
        self.flaky_elements = elements;
        self
    }

    /// Generate an exploration path based on the strategy
    pub fn generate_path(&mut self) -> ExplorationPath {
        match self.strategy {
            ExplorationStrategy::Exhaustive => self.exhaustive_path(),
            ExplorationStrategy::SmokeTest => self.smoke_test_path(),
            ExplorationStrategy::Regression => self.regression_path(),
            ExplorationStrategy::RandomWalk => self.random_walk_path(),
            ExplorationStrategy::Targeted => self.targeted_path(),
            ExplorationStrategy::Seeded => self.seeded_path(),
        }
    }

    /// Exhaustive BFS traversal - visits all reachable states
    fn exhaustive_path(&self) -> ExplorationPath {
        let mut path = ExplorationPath {
            states: Vec::new(),
            transitions: Vec::new(),
            estimated_cost: 0.0,
            strategy: ExplorationStrategy::Exhaustive,
        };

        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, Option<String>, u32)> = VecDeque::new();
        let mut sequence = 0u32;
        let mut transition_sequence = 0u32;

        // Start from initial states
        for initial in &self.graph.initial_states {
            if !visited.contains(initial) {
                queue.push_back((initial.clone(), None, 0));
            }
        }

        // If no initial states, start from first state
        if queue.is_empty() {
            if let Some(first) = self.graph.states.keys().next() {
                queue.push_back((first.clone(), None, 0));
            }
        }

        while let Some((state_id, arrived_via, depth)) = queue.pop_front() {
            if visited.contains(&state_id) {
                continue;
            }

            if self.max_states > 0 && visited.len() >= self.max_states as usize {
                break;
            }

            visited.insert(state_id.clone());

            if let Some(state_info) = self.graph.states.get(&state_id) {
                path.states.push(StateVisit {
                    state_id: state_id.clone(),
                    state_name: state_info.name.clone(),
                    arrived_via: arrived_via.clone(),
                    depth,
                    sequence_index: sequence,
                    is_critical: state_info.is_critical,
                    priority: state_info.priority,
                });
                sequence += 1;
            }

            // Add outgoing transitions
            for transition in self.graph.get_outgoing_transitions(&state_id) {
                if !visited.contains(&transition.to_state_id) {
                    path.transitions.push(TransitionStep {
                        transition_id: transition.id.clone(),
                        from_state_id: state_id.clone(),
                        to_state_id: transition.to_state_id.clone(),
                        sequence_index: transition_sequence,
                        verify: true,
                    });
                    transition_sequence += 1;

                    queue.push_back((
                        transition.to_state_id.clone(),
                        Some(transition.id.clone()),
                        depth + 1,
                    ));
                }
            }
        }

        path.estimated_cost = path.states.len() as f64;
        path
    }

    /// Smoke test - prioritize critical states and shortest paths
    fn smoke_test_path(&self) -> ExplorationPath {
        let mut path = ExplorationPath {
            states: Vec::new(),
            transitions: Vec::new(),
            estimated_cost: 0.0,
            strategy: ExplorationStrategy::SmokeTest,
        };

        // Collect critical states (states with descriptions, initial states)
        let mut critical_states: Vec<&StateInfo> = self
            .graph
            .states
            .values()
            .filter(|s| s.is_critical || s.has_description)
            .collect();

        // Sort by priority (lower is higher priority)
        critical_states.sort_by_key(|s| s.priority);

        let mut visited: HashSet<String> = HashSet::new();
        let mut sequence = 0u32;

        // Add initial states first
        for initial in &self.graph.initial_states {
            if let Some(state_info) = self.graph.states.get(initial) {
                if !visited.contains(initial) {
                    visited.insert(initial.clone());
                    path.states.push(StateVisit {
                        state_id: initial.clone(),
                        state_name: state_info.name.clone(),
                        arrived_via: None,
                        depth: 0,
                        sequence_index: sequence,
                        is_critical: true,
                        priority: 0,
                    });
                    sequence += 1;
                }
            }
        }

        // Add other critical states
        for state in critical_states {
            if !visited.contains(&state.id) {
                if self.max_states > 0 && visited.len() >= self.max_states as usize {
                    break;
                }
                visited.insert(state.id.clone());
                path.states.push(StateVisit {
                    state_id: state.id.clone(),
                    state_name: state.name.clone(),
                    arrived_via: None,
                    depth: 1,
                    sequence_index: sequence,
                    is_critical: state.is_critical,
                    priority: state.priority,
                });
                sequence += 1;
            }
        }

        path.estimated_cost = path.states.len() as f64 * 0.5; // Smoke tests are faster
        path
    }

    /// Regression - focus on previously failed states
    fn regression_path(&self) -> ExplorationPath {
        let mut path = ExplorationPath {
            states: Vec::new(),
            transitions: Vec::new(),
            estimated_cost: 0.0,
            strategy: ExplorationStrategy::Regression,
        };

        let mut sequence = 0u32;

        // If we have failure history, prioritize those states
        if !self.failure_history.is_empty() {
            for state_id in &self.failure_history {
                if let Some(state_info) = self.graph.states.get(state_id) {
                    if self.max_states > 0 && sequence >= self.max_states {
                        break;
                    }
                    path.states.push(StateVisit {
                        state_id: state_id.clone(),
                        state_name: state_info.name.clone(),
                        arrived_via: None,
                        depth: 0,
                        sequence_index: sequence,
                        is_critical: true,
                        priority: 0,
                    });
                    sequence += 1;
                }
            }
        } else {
            // No failure history, fall back to smoke test
            return self.smoke_test_path();
        }

        path.estimated_cost = path.states.len() as f64;
        path
    }

    /// Random walk - explore randomly for unexpected behaviors
    fn random_walk_path(&mut self) -> ExplorationPath {
        let mut path = ExplorationPath {
            states: Vec::new(),
            transitions: Vec::new(),
            estimated_cost: 0.0,
            strategy: ExplorationStrategy::RandomWalk,
        };

        // Create RNG if not already set
        let rng = self.rng.get_or_insert_with(StdRng::from_os_rng);

        let mut current_state = if !self.graph.initial_states.is_empty() {
            self.graph.initial_states[0].clone()
        } else if let Some(first) = self.graph.states.keys().next() {
            first.clone()
        } else {
            return path;
        };

        let mut visited: HashSet<String> = HashSet::new();
        let mut sequence = 0u32;
        let max_steps = if self.max_states > 0 {
            self.max_states
        } else {
            (self.graph.states.len() * 2) as u32
        };

        for _ in 0..max_steps {
            if let Some(state_info) = self.graph.states.get(&current_state) {
                let is_new = !visited.contains(&current_state);
                if is_new {
                    visited.insert(current_state.clone());
                    path.states.push(StateVisit {
                        state_id: current_state.clone(),
                        state_name: state_info.name.clone(),
                        arrived_via: None,
                        depth: sequence,
                        sequence_index: sequence,
                        is_critical: state_info.is_critical,
                        priority: state_info.priority,
                    });
                    sequence += 1;
                }

                // Pick a random outgoing transition
                let transitions = self.graph.get_outgoing_transitions(&current_state);
                if transitions.is_empty() {
                    break;
                }

                let transition = transitions[rng.random_range(0..transitions.len())];
                path.transitions.push(TransitionStep {
                    transition_id: transition.id.clone(),
                    from_state_id: current_state.clone(),
                    to_state_id: transition.to_state_id.clone(),
                    sequence_index: path.transitions.len() as u32,
                    verify: is_new, // Only verify transitions to new states
                });

                current_state = transition.to_state_id.clone();
            } else {
                break;
            }
        }

        path.estimated_cost = path.transitions.len() as f64;
        path
    }

    /// Targeted - explore only specific states/transitions
    fn targeted_path(&self) -> ExplorationPath {
        let mut path = ExplorationPath {
            states: Vec::new(),
            transitions: Vec::new(),
            estimated_cost: 0.0,
            strategy: ExplorationStrategy::Targeted,
        };

        let mut sequence = 0u32;

        // Add target states
        for state_id in &self.target_states {
            if let Some(state_info) = self.graph.states.get(state_id) {
                path.states.push(StateVisit {
                    state_id: state_id.clone(),
                    state_name: state_info.name.clone(),
                    arrived_via: None,
                    depth: 0,
                    sequence_index: sequence,
                    is_critical: true,
                    priority: 0,
                });
                sequence += 1;
            }
        }

        // Add target transitions
        let mut transition_sequence = 0u32;
        for transition_id in &self.target_transitions {
            if let Some(transition) = self.graph.transitions.get(transition_id) {
                path.transitions.push(TransitionStep {
                    transition_id: transition_id.clone(),
                    from_state_id: transition.from_state_id.clone(),
                    to_state_id: transition.to_state_id.clone(),
                    sequence_index: transition_sequence,
                    verify: true,
                });
                transition_sequence += 1;
            }
        }

        path.estimated_cost = (path.states.len() + path.transitions.len()) as f64;
        path
    }

    /// Seeded exploration: like Exhaustive but skips known states and penalizes
    /// transitions involving flaky elements. This reduces re-discovery time on
    /// subsequent runs while still discovering new states.
    fn seeded_path(&self) -> ExplorationPath {
        let mut path = ExplorationPath {
            states: Vec::new(),
            transitions: Vec::new(),
            estimated_cost: 0.0,
            strategy: ExplorationStrategy::Seeded,
        };

        let mut visited = HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        let mut sequence = 0u32;

        // Start with initial states (never skip those)
        for (state_id, state_info) in &self.graph.states {
            if state_info.is_initial {
                visited.insert(state_id.clone());
                path.states.push(StateVisit {
                    state_id: state_id.clone(),
                    state_name: state_info.name.clone(),
                    arrived_via: Some("initial".to_string()),
                    depth: 0,
                    sequence_index: sequence,
                    is_critical: state_info.is_critical,
                    priority: state_info.priority,
                });
                sequence += 1;
                queue.push_back((state_id.clone(), 0u32));
            }
        }

        // BFS, but skip known states from prior runs
        let mut transition_sequence = 0u32;
        while let Some((current_state, depth)) = queue.pop_front() {
            if self.max_states > 0 && path.states.len() as u32 >= self.max_states {
                break;
            }

            for (transition_id, transition) in &self.graph.transitions {
                if transition.from_state_id != current_state {
                    continue;
                }
                let next = &transition.to_state_id;
                if visited.contains(next) {
                    continue;
                }
                visited.insert(next.clone());

                // Skip states we already know from prior runs
                if self.known_states.contains(next) {
                    continue;
                }

                // Calculate cost — higher for transitions with flaky elements
                let flaky_penalty = if self.flaky_elements.iter().any(|e| {
                    transition.from_state_id.contains(e) || transition.to_state_id.contains(e)
                }) {
                    3.0 // Triple cost for flaky paths
                } else {
                    1.0
                };

                if let Some(state_info) = self.graph.states.get(next) {
                    path.states.push(StateVisit {
                        state_id: next.clone(),
                        state_name: state_info.name.clone(),
                        arrived_via: Some(transition_id.clone()),
                        depth: depth + 1,
                        sequence_index: sequence,
                        is_critical: state_info.is_critical,
                        priority: state_info.priority,
                    });
                    sequence += 1;
                }

                path.transitions.push(TransitionStep {
                    transition_id: transition_id.clone(),
                    from_state_id: transition.from_state_id.clone(),
                    to_state_id: transition.to_state_id.clone(),
                    sequence_index: transition_sequence,
                    verify: true,
                });
                transition_sequence += 1;
                path.estimated_cost += flaky_penalty;

                queue.push_back((next.clone(), depth + 1));
            }
        }

        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_graph() -> StateMachineGraph {
        let mut graph = StateMachineGraph::new();

        // Add states
        graph.states.insert(
            "s1".to_string(),
            StateInfo {
                id: "s1".to_string(),
                name: "State 1".to_string(),
                is_initial: true,
                is_critical: true,
                priority: 0,
                has_description: true,
                expected_elements: vec!["Login button".to_string()],
                unexpected_elements: vec![],
                ai_description: Some(StateDescription {
                    summary: "Login page with authentication form".to_string(),
                    expected_elements: Some(vec!["Login button".to_string()]),
                    unexpected_elements: None,
                    user_goal: Some("User is trying to authenticate".to_string()),
                    verification_prompt: None,
                }),
                assertions: vec![],
            },
        );

        graph.states.insert(
            "s2".to_string(),
            StateInfo {
                id: "s2".to_string(),
                name: "State 2".to_string(),
                is_initial: false,
                is_critical: false,
                priority: 2,
                has_description: false,
                expected_elements: vec![],
                unexpected_elements: vec![],
                ai_description: None,
                assertions: vec![],
            },
        );

        graph.states.insert(
            "s3".to_string(),
            StateInfo {
                id: "s3".to_string(),
                name: "State 3".to_string(),
                is_initial: false,
                is_critical: true,
                priority: 1,
                has_description: true,
                expected_elements: vec!["Dashboard".to_string()],
                unexpected_elements: vec!["Error dialog".to_string()],
                ai_description: Some(StateDescription {
                    summary: "Main dashboard after successful login".to_string(),
                    expected_elements: Some(vec!["Dashboard".to_string()]),
                    unexpected_elements: Some(vec!["Error dialog".to_string()]),
                    user_goal: Some("User has logged in and can access features".to_string()),
                    verification_prompt: None,
                }),
                assertions: vec![],
            },
        );

        graph.initial_states.push("s1".to_string());

        // Add transitions
        graph.transitions.insert(
            "t1".to_string(),
            TransitionInfo {
                id: "t1".to_string(),
                from_state_id: "s1".to_string(),
                to_state_id: "s2".to_string(),
                cost: 1.0,
                has_description: false,
                expected_duration_ms: None,
                ai_description: None,
            },
        );

        graph.transitions.insert(
            "t2".to_string(),
            TransitionInfo {
                id: "t2".to_string(),
                from_state_id: "s2".to_string(),
                to_state_id: "s3".to_string(),
                cost: 1.0,
                has_description: true,
                expected_duration_ms: Some(2000),
                ai_description: Some(TransitionDescription {
                    intent: "Navigate from intermediate state to dashboard".to_string(),
                    preconditions: Some(vec!["User is authenticated".to_string()]),
                    postconditions: Some(vec!["Dashboard is visible".to_string()]),
                    failure_modes: None,
                    expected_duration_ms: Some(2000),
                }),
            },
        );

        // Add adjacency
        graph
            .adjacency
            .insert("s1".to_string(), vec!["t1".to_string()]);
        graph
            .adjacency
            .insert("s2".to_string(), vec!["t2".to_string()]);
        graph.adjacency.insert("s3".to_string(), vec![]);

        graph
    }

    #[test]
    fn test_exhaustive_path() {
        let graph = make_test_graph();
        let mut explorer = StateExplorer::new(graph, ExplorationStrategy::Exhaustive);
        let path = explorer.generate_path();

        assert_eq!(path.states.len(), 3);
        assert_eq!(path.transitions.len(), 2);
        assert_eq!(path.states[0].state_id, "s1");
    }

    #[test]
    fn test_smoke_test_path() {
        let graph = make_test_graph();
        let mut explorer = StateExplorer::new(graph, ExplorationStrategy::SmokeTest);
        let path = explorer.generate_path();

        // Should include critical states (s1 and s3 have descriptions)
        assert!(path.states.iter().any(|s| s.state_id == "s1"));
        assert!(path.states.iter().any(|s| s.state_id == "s3"));
    }

    #[test]
    fn test_targeted_path() {
        let graph = make_test_graph();
        let mut explorer = StateExplorer::new(graph, ExplorationStrategy::Targeted)
            .with_targets(vec!["s2".to_string()], vec!["t2".to_string()]);
        let path = explorer.generate_path();

        assert_eq!(path.states.len(), 1);
        assert_eq!(path.states[0].state_id, "s2");
        assert_eq!(path.transitions.len(), 1);
        assert_eq!(path.transitions[0].transition_id, "t2");
    }

    #[test]
    fn test_max_states_limit() {
        let graph = make_test_graph();
        let mut explorer =
            StateExplorer::new(graph, ExplorationStrategy::Exhaustive).with_max_states(2);
        let path = explorer.generate_path();

        assert_eq!(path.states.len(), 2);
    }
}
