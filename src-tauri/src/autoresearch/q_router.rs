//! Q-learning-based architecture router for autoresearch.
//!
//! Learns which `WorkflowArchitecture` (Traditional, AgenticVerification,
//! MultiAgentPipeline) works best for which type of task, replacing global
//! random/grid-search candidate selection with contextual, feature-based routing.
//!
//! # State Space
//!
//! Tasks are discretized into a `TaskState` based on:
//! - Primary domain (frontend, backend, database, testing, infra)
//! - Complexity tier (trivial, simple, moderate, complex, highly_complex)
//! - Whether the task has a UI component (react/css/html in technology tags)
//!
//! Total: 5 × 5 × 2 = 50 states.
//!
//! # Action Space
//!
//! The 3 workflow architectures: Traditional, AgenticVerification, MultiAgentPipeline.
//!
//! # Policy
//!
//! Epsilon-greedy: with probability ε pick randomly; otherwise pick argmax_a Q(s,a).
//! Epsilon auto-decays per state as visits accumulate.

use super::agentic_verification::WorkflowArchitecture;
use rand::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// =============================================================================
// State representation
// =============================================================================

/// Primary domain of a task, inferred from `domain_tags`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Frontend,
    Backend,
    Database,
    Testing,
    Infra,
}

impl Domain {
    /// All domain variants for iteration.
    pub const ALL: [Domain; 5] = [
        Domain::Frontend,
        Domain::Backend,
        Domain::Database,
        Domain::Testing,
        Domain::Infra,
    ];

    /// Infer primary domain from domain tags (comma-separated or JSON array).
    pub fn from_domain_tags(tags: &str) -> Self {
        let lower = tags.to_lowercase();
        // Priority order: more specific first
        if lower.contains("frontend") || lower.contains("ui-bridge") {
            Domain::Frontend
        } else if lower.contains("database") {
            Domain::Database
        } else if lower.contains("testing") {
            Domain::Testing
        } else if lower.contains("infra") || lower.contains("orchestration") || lower.contains("meta-optimizer") {
            Domain::Infra
        } else if lower.contains("backend") || lower.contains("api") {
            Domain::Backend
        } else {
            // Default: backend (most common)
            Domain::Backend
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Domain::Frontend => "frontend",
            Domain::Backend => "backend",
            Domain::Database => "database",
            Domain::Testing => "testing",
            Domain::Infra => "infra",
        }
    }
}

impl std::fmt::Display for Domain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Complexity tier of a task (matches learning_outcomes.complexity_tier).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComplexityTier {
    Trivial,
    Simple,
    Moderate,
    Complex,
    HighlyComplex,
}

impl ComplexityTier {
    /// All complexity variants for iteration.
    pub const ALL: [ComplexityTier; 5] = [
        ComplexityTier::Trivial,
        ComplexityTier::Simple,
        ComplexityTier::Moderate,
        ComplexityTier::Complex,
        ComplexityTier::HighlyComplex,
    ];

    /// Parse from the complexity_tier string in learning_outcomes.
    pub fn from_str_lossy(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "trivial" => ComplexityTier::Trivial,
            "simple" => ComplexityTier::Simple,
            "moderate" => ComplexityTier::Moderate,
            "complex" => ComplexityTier::Complex,
            "highly_complex" => ComplexityTier::HighlyComplex,
            _ => ComplexityTier::Moderate, // fallback
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ComplexityTier::Trivial => "trivial",
            ComplexityTier::Simple => "simple",
            ComplexityTier::Moderate => "moderate",
            ComplexityTier::Complex => "complex",
            ComplexityTier::HighlyComplex => "highly_complex",
        }
    }
}

impl std::fmt::Display for ComplexityTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Discretized task state for Q-learning.
///
/// 5 domains × 5 complexity tiers × 2 (has_ui) = 50 possible states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskState {
    pub primary_domain: Domain,
    pub complexity: ComplexityTier,
    pub has_ui_component: bool,
}

impl TaskState {
    /// Build a TaskState from learning_outcome fields.
    pub fn from_outcome_tags(
        technology_tags: &str,
        domain_tags: &str,
        complexity_tier: &str,
    ) -> Self {
        let lower_tech = technology_tags.to_lowercase();
        let has_ui = lower_tech.contains("react")
            || lower_tech.contains("css")
            || lower_tech.contains("html")
            || lower_tech.contains("nextjs");

        TaskState {
            primary_domain: Domain::from_domain_tags(domain_tags),
            complexity: ComplexityTier::from_str_lossy(complexity_tier),
            has_ui_component: has_ui,
        }
    }

    /// Serialize to a stable string key for database storage.
    pub fn to_key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.primary_domain,
            self.complexity,
            if self.has_ui_component { "ui" } else { "no_ui" }
        )
    }

    /// Parse from a serialized key string.
    pub fn from_key(key: &str) -> Option<Self> {
        let parts: Vec<&str> = key.splitn(3, ':').collect();
        if parts.len() != 3 {
            return None;
        }
        let primary_domain = match parts[0] {
            "frontend" => Domain::Frontend,
            "backend" => Domain::Backend,
            "database" => Domain::Database,
            "testing" => Domain::Testing,
            "infra" => Domain::Infra,
            _ => return None,
        };
        let complexity = match parts[1] {
            "trivial" => ComplexityTier::Trivial,
            "simple" => ComplexityTier::Simple,
            "moderate" => ComplexityTier::Moderate,
            "complex" => ComplexityTier::Complex,
            "highly_complex" => ComplexityTier::HighlyComplex,
            _ => return None,
        };
        let has_ui_component = match parts[2] {
            "ui" => true,
            "no_ui" => false,
            _ => return None,
        };
        Some(TaskState {
            primary_domain,
            complexity,
            has_ui_component,
        })
    }
}

impl std::fmt::Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{{{}, {}, has_ui={}}}",
            self.primary_domain, self.complexity, self.has_ui_component
        )
    }
}

// =============================================================================
// Q-table entry
// =============================================================================

/// A single entry in the Q-table: the learned value for a (state, action) pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QEntry {
    pub q_value: f64,
    pub visit_count: u32,
}

impl Default for QEntry {
    fn default() -> Self {
        Self {
            q_value: 0.0,
            visit_count: 0,
        }
    }
}

// =============================================================================
// Q-Router
// =============================================================================

/// Default learning rate for Q-value updates.
const DEFAULT_ALPHA: f64 = 0.1;
/// Base exploration rate (decays per state).
const DEFAULT_EPSILON_BASE: f64 = 0.3;
/// Minimum exploration rate.
const DEFAULT_EPSILON_MIN: f64 = 0.05;
/// Decay rate for epsilon per visit.
const EPSILON_DECAY_RATE: f64 = 20.0;
/// Minimum visits per state before Q-routing activates.
const MIN_VISITS_FOR_ROUTING: u32 = 3;

/// Cost penalty weight applied to total_cost_usd in reward calculation.
/// reward = composite_agentic_score - COST_PENALTY * total_cost_usd + CACHE_SAVINGS_WEIGHT * cache_savings
const COST_PENALTY: f64 = 0.01;

/// Cache savings bonus weight in reward calculation.
/// Incentivizes architectures/models that maximize prompt cache reuse.
const CACHE_SAVINGS_WEIGHT: f64 = 0.005;

/// Q-learning router for workflow architecture selection.
///
/// Maintains a Q-table mapping (TaskState, WorkflowArchitecture) → QEntry.
/// Supports loading/saving from the database, epsilon-greedy selection,
/// and incremental Q-value updates after each workflow run.
pub struct QRouter {
    /// In-memory Q-table: (state_key, architecture_str) → QEntry.
    table: HashMap<(String, String), QEntry>,
    /// Manual overrides: state_key → forced architecture.
    overrides: HashMap<String, String>,
    /// Learning rate.
    alpha: f64,
    /// RNG for exploration.
    rng: StdRng,
}

impl QRouter {
    /// Create an empty QRouter with default hyperparameters.
    pub fn new() -> Self {
        Self {
            table: HashMap::new(),
            overrides: HashMap::new(),
            alpha: DEFAULT_ALPHA,
            rng: StdRng::from_os_rng(),
        }
    }

    /// Create a QRouter with a specific learning rate.
    pub fn with_alpha(alpha: f64) -> Self {
        Self {
            table: HashMap::new(),
            overrides: HashMap::new(),
            alpha,
            rng: StdRng::from_os_rng(),
        }
    }

    /// Load Q-table entries from database rows.
    ///
    /// Each row should have (state_key, action, q_value, visit_count).
    pub fn load_from_rows(&mut self, rows: Vec<(String, String, f64, u32)>) {
        for (state_key, action, q_value, visit_count) in rows {
            self.table.insert(
                (state_key, action),
                QEntry {
                    q_value,
                    visit_count,
                },
            );
        }
    }

    /// Load manual overrides from database rows.
    ///
    /// Each row should have (state_key, forced_action).
    pub fn load_overrides(&mut self, overrides: Vec<(String, String)>) {
        for (state_key, forced_action) in overrides {
            self.overrides.insert(state_key, forced_action);
        }
    }

    /// Set a manual override: lock a state to a specific architecture.
    pub fn set_override(&mut self, state: &TaskState, arch: &WorkflowArchitecture) {
        self.overrides.insert(state.to_key(), arch.to_string());
    }

    /// Remove a manual override for a state.
    pub fn remove_override(&mut self, state: &TaskState) -> bool {
        self.overrides.remove(&state.to_key()).is_some()
    }

    /// Get the override for a state, if any.
    pub fn get_override(&self, state: &TaskState) -> Option<WorkflowArchitecture> {
        self.overrides.get(&state.to_key()).and_then(|s| parse_architecture(s))
    }

    /// Get all overrides as serializable entries.
    pub fn overrides_snapshot(&self) -> Vec<OverrideEntry> {
        self.overrides
            .iter()
            .map(|(state_key, action)| OverrideEntry {
                state_key: state_key.clone(),
                forced_action: action.clone(),
            })
            .collect()
    }

    /// Get the Q-value for a (state, action) pair.
    pub fn get_q(&self, state: &TaskState, arch: &WorkflowArchitecture) -> QEntry {
        let key = (state.to_key(), arch.to_string());
        self.table.get(&key).cloned().unwrap_or_default()
    }

    /// Update Q-value after observing a reward for (state, action).
    ///
    /// Uses simple temporal-difference update (no next-state since this is
    /// a contextual bandit, not a sequential MDP):
    ///
    ///   Q(s,a) ← Q(s,a) + α * (reward - Q(s,a))
    ///
    /// Returns the updated QEntry for database persistence.
    pub fn update(
        &mut self,
        state: &TaskState,
        arch: &WorkflowArchitecture,
        reward: f64,
    ) -> QEntry {
        let key = (state.to_key(), arch.to_string());
        let entry = self.table.entry(key).or_default();

        entry.q_value += self.alpha * (reward - entry.q_value);
        entry.visit_count += 1;

        entry.clone()
    }

    /// Compute reward from a learning outcome's composite score, cost, and cache savings.
    pub fn compute_reward(composite_agentic_score: f64, total_cost_usd: Option<f64>) -> f64 {
        Self::compute_reward_with_cache(composite_agentic_score, total_cost_usd, None)
    }

    /// Compute reward with optional cache savings bonus.
    ///
    /// reward = composite_score - 0.01 * cost + 0.005 * cache_savings, clamped to [0, 1]
    pub fn compute_reward_with_cache(
        composite_agentic_score: f64,
        total_cost_usd: Option<f64>,
        cache_savings_usd: Option<f64>,
    ) -> f64 {
        let cost_penalty = total_cost_usd.unwrap_or(0.0) * COST_PENALTY;
        let cache_bonus = cache_savings_usd.unwrap_or(0.0) * CACHE_SAVINGS_WEIGHT;
        (composite_agentic_score - cost_penalty + cache_bonus).clamp(0.0, 1.0)
    }

    /// Select an architecture using epsilon-greedy policy.
    ///
    /// Manual overrides take precedence. Otherwise: with probability ε, pick
    /// randomly; else pick argmax_a Q(s,a).
    pub fn select_architecture(&mut self, task_state: &TaskState) -> WorkflowArchitecture {
        // Check manual override first
        if let Some(forced) = self.get_override(task_state) {
            return forced;
        }

        let epsilon = self.effective_epsilon(task_state);

        if self.rng.random_bool(epsilon.min(1.0)) {
            // Explore: random
            let choices = [
                WorkflowArchitecture::Traditional,
                WorkflowArchitecture::AgenticVerification,
                WorkflowArchitecture::MultiAgentPipeline,
            ];
            let idx = self.rng.random_range(0..choices.len());
            choices[idx].clone()
        } else {
            // Exploit: argmax
            self.greedy_architecture(task_state)
        }
    }

    /// Get the greedy (best) architecture for a state.
    /// Uses min_by with reversed comparison so that on tie the earlier
    /// (cheaper) architecture wins — same pattern as model_q_router.rs.
    pub fn greedy_architecture(&self, task_state: &TaskState) -> WorkflowArchitecture {
        let architectures = [
            WorkflowArchitecture::Traditional,
            WorkflowArchitecture::AgenticVerification,
            WorkflowArchitecture::MultiAgentPipeline,
        ];

        architectures
            .iter()
            .min_by(|a, b| {
                let qa = self.get_q(task_state, a).q_value;
                let qb = self.get_q(task_state, b).q_value;
                // Higher Q sorts first; on tie, earlier (cheaper) wins
                qb.partial_cmp(&qa).unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
            .unwrap_or(WorkflowArchitecture::Traditional)
    }

    /// Compute effective epsilon for a task state.
    ///
    /// Auto-decays: ε = max(0.05, 0.3 * exp(-total_visits / 20))
    /// Starts exploring heavily, converges to exploitation.
    pub fn effective_epsilon(&self, task_state: &TaskState) -> f64 {
        let total_visits: u32 = [
            WorkflowArchitecture::Traditional,
            WorkflowArchitecture::AgenticVerification,
            WorkflowArchitecture::MultiAgentPipeline,
        ]
        .iter()
        .map(|a| self.get_q(task_state, a).visit_count)
        .sum();

        let epsilon =
            DEFAULT_EPSILON_BASE * (-(total_visits as f64) / EPSILON_DECAY_RATE).exp();
        epsilon.max(DEFAULT_EPSILON_MIN)
    }

    /// Check if we have enough data for a state to use Q-routing.
    ///
    /// Requires at least `MIN_VISITS_FOR_ROUTING` total visits (one per architecture minimum).
    pub fn has_sufficient_data(&self, task_state: &TaskState) -> bool {
        let architectures = [
            WorkflowArchitecture::Traditional,
            WorkflowArchitecture::AgenticVerification,
            WorkflowArchitecture::MultiAgentPipeline,
        ];

        // Each architecture must have at least 1 visit
        architectures
            .iter()
            .all(|a| self.get_q(task_state, a).visit_count >= 1)
    }

    /// Get the full Q-table as a serializable structure.
    pub fn table_snapshot(&self) -> Vec<QTableRow> {
        self.table
            .iter()
            .map(|((state_key, action), entry)| QTableRow {
                state_key: state_key.clone(),
                action: action.clone(),
                q_value: entry.q_value,
                visit_count: entry.visit_count,
            })
            .collect()
    }

    /// Get the greedy policy: best architecture per state.
    pub fn policy_snapshot(&self) -> Vec<PolicyEntry> {
        // Collect unique state keys
        let mut state_keys: Vec<String> = self
            .table
            .keys()
            .map(|(sk, _)| sk.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        state_keys.sort();

        state_keys
            .into_iter()
            .filter_map(|state_key| {
                let task_state = TaskState::from_key(&state_key)?;
                let best = self.greedy_architecture(&task_state);
                let best_q = self.get_q(&task_state, &best);
                let epsilon = self.effective_epsilon(&task_state);
                let total_visits: u32 = [
                    WorkflowArchitecture::Traditional,
                    WorkflowArchitecture::AgenticVerification,
                    WorkflowArchitecture::MultiAgentPipeline,
                ]
                .iter()
                .map(|a| self.get_q(&task_state, a).visit_count)
                .sum();

                Some(PolicyEntry {
                    state_key,
                    recommended_architecture: best.to_string(),
                    q_value: best_q.q_value,
                    confidence: 1.0 - epsilon,
                    total_visits,
                })
            })
            .collect()
    }

    /// Get convergence stats.
    pub fn stats(&self) -> QRoutingStats {
        let total_entries = self.table.len();
        let total_visits: u64 = self.table.values().map(|e| e.visit_count as u64).sum();
        let states_with_data: usize = {
            let mut keys: std::collections::HashSet<&String> = std::collections::HashSet::new();
            for (sk, _) in self.table.keys() {
                keys.insert(sk);
            }
            keys.len()
        };

        // Count states with sufficient data for routing
        let routable_states: usize = {
            let mut keys: std::collections::HashSet<String> = std::collections::HashSet::new();
            for (sk, _) in self.table.keys() {
                keys.insert(sk.clone());
            }
            keys.into_iter()
                .filter(|sk| {
                    TaskState::from_key(sk)
                        .map(|ts| self.has_sufficient_data(&ts))
                        .unwrap_or(false)
                })
                .count()
        };

        let avg_q_value = if total_entries > 0 {
            self.table.values().map(|e| e.q_value).sum::<f64>() / total_entries as f64
        } else {
            0.0
        };

        QRoutingStats {
            total_state_action_pairs: total_entries,
            states_with_data,
            routable_states,
            total_possible_states: 50, // 5 × 5 × 2
            total_visits,
            avg_q_value,
        }
    }
}

// =============================================================================
// Serialization types for API responses
// =============================================================================

/// One row of the Q-table (for API/frontend).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QTableRow {
    pub state_key: String,
    pub action: String,
    pub q_value: f64,
    pub visit_count: u32,
}

/// Greedy policy entry (for API/frontend).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyEntry {
    pub state_key: String,
    pub recommended_architecture: String,
    pub q_value: f64,
    pub confidence: f64,
    pub total_visits: u32,
}

/// Aggregate stats about the Q-routing system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QRoutingStats {
    pub total_state_action_pairs: usize,
    pub states_with_data: usize,
    pub routable_states: usize,
    pub total_possible_states: usize,
    pub total_visits: u64,
    pub avg_q_value: f64,
}

/// Manual override entry (for API/frontend).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverrideEntry {
    pub state_key: String,
    pub forced_action: String,
}

/// Parse an architecture string back to the enum.
fn parse_architecture(s: &str) -> Option<WorkflowArchitecture> {
    match s {
        "traditional" => Some(WorkflowArchitecture::Traditional),
        "agentic_verification" => Some(WorkflowArchitecture::AgenticVerification),
        "multi_agent_pipeline" => Some(WorkflowArchitecture::MultiAgentPipeline),
        _ => None,
    }
}

// =============================================================================
// SQLite helpers (shared by commands.rs and engine.rs)
// =============================================================================

/// Load all Q-table entries via PG.
pub async fn load_q_table_pg(
    pg: &crate::database::pg::PgDb,
) -> Result<Vec<(String, String, f64, u32)>, String> {
    pg.load_q_table().await
}

/// Load all Q-routing overrides via PG.
pub async fn load_overrides_pg(
    pg: &crate::database::pg::PgDb,
) -> Result<Vec<(String, String)>, String> {
    pg.load_q_overrides().await
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state(domain: Domain, complexity: ComplexityTier, has_ui: bool) -> TaskState {
        TaskState {
            primary_domain: domain,
            complexity,
            has_ui_component: has_ui,
        }
    }

    #[test]
    fn test_task_state_from_outcome_tags() {
        let state = TaskState::from_outcome_tags(
            r#"["rust","typescript","css"]"#,
            r#"["frontend","testing"]"#,
            "moderate",
        );
        assert_eq!(state.primary_domain, Domain::Frontend);
        assert_eq!(state.complexity, ComplexityTier::Moderate);
        assert!(state.has_ui_component); // css detected
    }

    #[test]
    fn test_task_state_from_outcome_tags_backend() {
        let state = TaskState::from_outcome_tags(
            r#"["rust","sql"]"#,
            r#"["backend","api"]"#,
            "complex",
        );
        assert_eq!(state.primary_domain, Domain::Backend);
        assert_eq!(state.complexity, ComplexityTier::Complex);
        assert!(!state.has_ui_component);
    }

    #[test]
    fn test_task_state_key_roundtrip() {
        let state = make_state(Domain::Frontend, ComplexityTier::Moderate, true);
        let key = state.to_key();
        assert_eq!(key, "frontend:moderate:ui");
        let parsed = TaskState::from_key(&key).unwrap();
        assert_eq!(parsed, state);
    }

    #[test]
    fn test_task_state_key_no_ui() {
        let state = make_state(Domain::Database, ComplexityTier::Simple, false);
        let key = state.to_key();
        assert_eq!(key, "database:simple:no_ui");
        let parsed = TaskState::from_key(&key).unwrap();
        assert_eq!(parsed, state);
    }

    #[test]
    fn test_q_update_converges() {
        let mut router = QRouter::with_alpha(0.1);
        let state = make_state(Domain::Backend, ComplexityTier::Moderate, false);
        let arch = WorkflowArchitecture::AgenticVerification;

        // Repeatedly observe reward of 0.8 — Q-value should converge toward 0.8
        for _ in 0..100 {
            router.update(&state, &arch, 0.8);
        }

        let q = router.get_q(&state, &arch);
        assert!(
            (q.q_value - 0.8).abs() < 0.01,
            "Q-value {} should converge to ~0.8",
            q.q_value
        );
        assert_eq!(q.visit_count, 100);
    }

    #[test]
    fn test_q_update_tracks_average() {
        let mut router = QRouter::with_alpha(0.1);
        let state = make_state(Domain::Frontend, ComplexityTier::Simple, true);
        let arch = WorkflowArchitecture::Traditional;

        // Mixed rewards: should settle between them
        for _ in 0..50 {
            router.update(&state, &arch, 1.0);
        }
        for _ in 0..50 {
            router.update(&state, &arch, 0.0);
        }

        let q = router.get_q(&state, &arch);
        // With recency bias from α=0.1, recent 0.0 rewards pull it down
        assert!(q.q_value < 0.5, "Q-value {} should be pulled below 0.5", q.q_value);
    }

    #[test]
    fn test_epsilon_decay() {
        let mut router = QRouter::new();
        let state = make_state(Domain::Backend, ComplexityTier::Moderate, false);

        // No visits → full exploration rate
        let eps0 = router.effective_epsilon(&state);
        assert!((eps0 - DEFAULT_EPSILON_BASE).abs() < 0.001);

        // Add visits → epsilon should decrease
        for _ in 0..20 {
            router.update(&state, &WorkflowArchitecture::Traditional, 0.5);
        }
        let eps20 = router.effective_epsilon(&state);
        assert!(eps20 < eps0, "ε should decay: {} < {}", eps20, eps0);

        // Many visits → near minimum
        for _ in 0..100 {
            router.update(&state, &WorkflowArchitecture::AgenticVerification, 0.5);
        }
        let eps120 = router.effective_epsilon(&state);
        assert!(eps120 < 0.06, "ε should approach minimum: {}", eps120);
    }

    #[test]
    fn test_greedy_selects_best() {
        let mut router = QRouter::with_alpha(1.0); // α=1 for instant convergence
        let state = make_state(Domain::Testing, ComplexityTier::Complex, false);

        router.update(&state, &WorkflowArchitecture::Traditional, 0.3);
        router.update(&state, &WorkflowArchitecture::AgenticVerification, 0.9);
        router.update(&state, &WorkflowArchitecture::MultiAgentPipeline, 0.5);

        let best = router.greedy_architecture(&state);
        assert_eq!(best, WorkflowArchitecture::AgenticVerification);
    }

    #[test]
    fn test_has_sufficient_data() {
        let mut router = QRouter::new();
        let state = make_state(Domain::Backend, ComplexityTier::Moderate, false);

        assert!(!router.has_sufficient_data(&state));

        router.update(&state, &WorkflowArchitecture::Traditional, 0.5);
        assert!(!router.has_sufficient_data(&state)); // only 1 of 3

        router.update(&state, &WorkflowArchitecture::AgenticVerification, 0.6);
        assert!(!router.has_sufficient_data(&state)); // only 2 of 3

        router.update(&state, &WorkflowArchitecture::MultiAgentPipeline, 0.4);
        assert!(router.has_sufficient_data(&state)); // all 3 visited
    }

    #[test]
    fn test_compute_reward() {
        assert_eq!(QRouter::compute_reward(0.8, None), 0.8);
        assert_eq!(QRouter::compute_reward(0.8, Some(0.0)), 0.8);

        // With cost penalty
        let reward = QRouter::compute_reward(0.8, Some(5.0));
        assert!((reward - 0.75).abs() < 0.001); // 0.8 - 0.01 * 5.0 = 0.75

        // Clamped to 0
        let reward = QRouter::compute_reward(0.1, Some(100.0));
        assert_eq!(reward, 0.0);
    }

    #[test]
    fn test_cold_state_returns_traditional() {
        let router = QRouter::new();
        let state = make_state(Domain::Frontend, ComplexityTier::Moderate, true);
        // No data → greedy should return Traditional (all Q=0, Traditional is first)
        let best = router.greedy_architecture(&state);
        assert_eq!(best, WorkflowArchitecture::Traditional);
    }

    #[test]
    fn test_table_snapshot() {
        let mut router = QRouter::new();
        let state = make_state(Domain::Backend, ComplexityTier::Simple, false);
        router.update(&state, &WorkflowArchitecture::Traditional, 0.7);

        let snapshot = router.table_snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].state_key, "backend:simple:no_ui");
        assert_eq!(snapshot[0].action, "traditional");
        assert!(snapshot[0].q_value > 0.0);
        assert_eq!(snapshot[0].visit_count, 1);
    }

    #[test]
    fn test_policy_snapshot() {
        let mut router = QRouter::with_alpha(1.0);
        let state = make_state(Domain::Frontend, ComplexityTier::Complex, true);

        router.update(&state, &WorkflowArchitecture::Traditional, 0.3);
        router.update(&state, &WorkflowArchitecture::AgenticVerification, 0.9);
        router.update(&state, &WorkflowArchitecture::MultiAgentPipeline, 0.5);

        let policy = router.policy_snapshot();
        assert_eq!(policy.len(), 1);
        assert_eq!(policy[0].recommended_architecture, "agentic_verification");
        assert!((policy[0].q_value - 0.9).abs() < 0.001);
    }

    #[test]
    fn test_stats() {
        let mut router = QRouter::new();
        let state = make_state(Domain::Backend, ComplexityTier::Simple, false);
        router.update(&state, &WorkflowArchitecture::Traditional, 0.7);
        router.update(&state, &WorkflowArchitecture::AgenticVerification, 0.5);

        let stats = router.stats();
        assert_eq!(stats.total_state_action_pairs, 2);
        assert_eq!(stats.states_with_data, 1);
        assert_eq!(stats.routable_states, 0); // only 2 of 3 architectures visited
        assert_eq!(stats.total_visits, 2);
        assert_eq!(stats.total_possible_states, 50);
    }

    #[test]
    fn test_domain_inference() {
        assert_eq!(Domain::from_domain_tags("frontend,testing"), Domain::Frontend);
        assert_eq!(Domain::from_domain_tags("database"), Domain::Database);
        assert_eq!(Domain::from_domain_tags("api,backend"), Domain::Backend);
        assert_eq!(Domain::from_domain_tags("orchestration"), Domain::Infra);
        assert_eq!(Domain::from_domain_tags("testing"), Domain::Testing);
        assert_eq!(Domain::from_domain_tags(""), Domain::Backend); // fallback
    }

    #[test]
    fn test_load_from_rows() {
        let mut router = QRouter::new();
        router.load_from_rows(vec![
            ("backend:moderate:no_ui".into(), "traditional".into(), 0.7, 10),
            ("backend:moderate:no_ui".into(), "agentic_verification".into(), 0.85, 15),
        ]);

        let state = make_state(Domain::Backend, ComplexityTier::Moderate, false);
        let q_trad = router.get_q(&state, &WorkflowArchitecture::Traditional);
        assert_eq!(q_trad.q_value, 0.7);
        assert_eq!(q_trad.visit_count, 10);

        let q_av = router.get_q(&state, &WorkflowArchitecture::AgenticVerification);
        assert_eq!(q_av.q_value, 0.85);
        assert_eq!(q_av.visit_count, 15);
    }

    #[test]
    fn test_override_takes_precedence() {
        let mut router = QRouter::with_alpha(1.0);
        let state = make_state(Domain::Backend, ComplexityTier::Moderate, false);

        // AgenticVerification has the best Q-value
        router.update(&state, &WorkflowArchitecture::Traditional, 0.3);
        router.update(&state, &WorkflowArchitecture::AgenticVerification, 0.9);
        router.update(&state, &WorkflowArchitecture::MultiAgentPipeline, 0.5);

        // But we override to Traditional
        router.set_override(&state, &WorkflowArchitecture::Traditional);

        // select_architecture should always return Traditional now
        for _ in 0..20 {
            let selected = router.select_architecture(&state);
            assert_eq!(selected, WorkflowArchitecture::Traditional);
        }
    }

    #[test]
    fn test_override_remove() {
        let mut router = QRouter::new();
        let state = make_state(Domain::Frontend, ComplexityTier::Simple, true);

        router.set_override(&state, &WorkflowArchitecture::MultiAgentPipeline);
        assert!(router.get_override(&state).is_some());

        let removed = router.remove_override(&state);
        assert!(removed);
        assert!(router.get_override(&state).is_none());

        // Remove again should return false
        assert!(!router.remove_override(&state));
    }

    #[test]
    fn test_overrides_snapshot() {
        let mut router = QRouter::new();
        let state = make_state(Domain::Database, ComplexityTier::Complex, false);
        router.set_override(&state, &WorkflowArchitecture::AgenticVerification);

        let snapshot = router.overrides_snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].state_key, "database:complex:no_ui");
        assert_eq!(snapshot[0].forced_action, "agentic_verification");
    }

    #[test]
    fn test_load_overrides() {
        let mut router = QRouter::new();
        router.load_overrides(vec![
            ("frontend:moderate:ui".into(), "multi_agent_pipeline".into()),
        ]);

        let state = make_state(Domain::Frontend, ComplexityTier::Moderate, true);
        let forced = router.get_override(&state);
        assert_eq!(forced, Some(WorkflowArchitecture::MultiAgentPipeline));
    }

    // ─── Integration-style tests (exercise full API response paths) ──────

    #[test]
    fn test_table_snapshot_returns_valid_data_after_updates() {
        let mut router = QRouter::with_alpha(0.5);

        // Simulate 3 different states with different architectures
        let s1 = make_state(Domain::Frontend, ComplexityTier::Simple, true);
        let s2 = make_state(Domain::Backend, ComplexityTier::Complex, false);
        let s3 = make_state(Domain::Database, ComplexityTier::Moderate, false);

        router.update(&s1, &WorkflowArchitecture::Traditional, 0.7);
        router.update(&s1, &WorkflowArchitecture::AgenticVerification, 0.9);
        router.update(&s2, &WorkflowArchitecture::MultiAgentPipeline, 0.5);
        router.update(&s3, &WorkflowArchitecture::Traditional, 0.3);
        router.update(&s3, &WorkflowArchitecture::AgenticVerification, 0.6);
        router.update(&s3, &WorkflowArchitecture::MultiAgentPipeline, 0.8);

        let snapshot = router.table_snapshot();
        assert_eq!(snapshot.len(), 6); // 2 + 1 + 3

        // All entries should have valid state keys and actions
        for row in &snapshot {
            assert!(TaskState::from_key(&row.state_key).is_some(), "Invalid state_key: {}", row.state_key);
            assert!(
                ["traditional", "agentic_verification", "multi_agent_pipeline"].contains(&row.action.as_str()),
                "Invalid action: {}", row.action
            );
            assert!(row.q_value >= 0.0 && row.q_value <= 1.0, "Q-value out of range: {}", row.q_value);
            assert!(row.visit_count >= 1);
        }
    }

    #[test]
    fn test_policy_snapshot_returns_valid_data_with_overrides() {
        let mut router = QRouter::with_alpha(1.0);

        let s1 = make_state(Domain::Frontend, ComplexityTier::Simple, true);
        router.update(&s1, &WorkflowArchitecture::Traditional, 0.3);
        router.update(&s1, &WorkflowArchitecture::AgenticVerification, 0.9);
        router.update(&s1, &WorkflowArchitecture::MultiAgentPipeline, 0.5);

        let policy = router.policy_snapshot();
        assert_eq!(policy.len(), 1);

        let entry = &policy[0];
        assert!(TaskState::from_key(&entry.state_key).is_some());
        assert_eq!(entry.recommended_architecture, "agentic_verification");
        assert!(entry.confidence >= 0.0 && entry.confidence <= 1.0);
        assert_eq!(entry.total_visits, 3);
    }

    #[test]
    fn test_stats_returns_valid_data() {
        let mut router = QRouter::new();

        // Start empty
        let stats = router.stats();
        assert_eq!(stats.total_state_action_pairs, 0);
        assert_eq!(stats.states_with_data, 0);
        assert_eq!(stats.routable_states, 0);
        assert_eq!(stats.total_possible_states, 50);
        assert_eq!(stats.total_visits, 0);

        // Add data for one fully-explored state
        let s = make_state(Domain::Backend, ComplexityTier::Simple, false);
        router.update(&s, &WorkflowArchitecture::Traditional, 0.5);
        router.update(&s, &WorkflowArchitecture::AgenticVerification, 0.6);
        router.update(&s, &WorkflowArchitecture::MultiAgentPipeline, 0.4);

        let stats = router.stats();
        assert_eq!(stats.total_state_action_pairs, 3);
        assert_eq!(stats.states_with_data, 1);
        assert_eq!(stats.routable_states, 1); // all 3 visited
        assert_eq!(stats.total_visits, 3);
        assert!(stats.avg_q_value > 0.0);
    }

    #[test]
    fn test_convergence_with_mixed_outcomes() {
        let mut router = QRouter::with_alpha(0.1);
        let state = make_state(Domain::Frontend, ComplexityTier::Moderate, true);

        // Simulate realistic outcomes: AgenticVerification consistently best
        for _ in 0..30 {
            router.update(&state, &WorkflowArchitecture::Traditional, 0.4 + rand_offset());
            router.update(&state, &WorkflowArchitecture::AgenticVerification, 0.8 + rand_offset());
            router.update(&state, &WorkflowArchitecture::MultiAgentPipeline, 0.6 + rand_offset());
        }

        // After 30 rounds, the policy should converge to AgenticVerification
        let best = router.greedy_architecture(&state);
        assert_eq!(best, WorkflowArchitecture::AgenticVerification);

        // Q-values should be in expected order
        let q_trad = router.get_q(&state, &WorkflowArchitecture::Traditional).q_value;
        let q_av = router.get_q(&state, &WorkflowArchitecture::AgenticVerification).q_value;
        let q_map = router.get_q(&state, &WorkflowArchitecture::MultiAgentPipeline).q_value;
        assert!(q_av > q_map, "AV ({}) should > MAP ({})", q_av, q_map);
        assert!(q_map > q_trad, "MAP ({}) should > Traditional ({})", q_map, q_trad);

        // Epsilon should be near minimum after 90 total visits
        let eps = router.effective_epsilon(&state);
        assert!(eps < 0.06, "Epsilon should be near minimum: {}", eps);
    }
}

/// Deterministic small offset for convergence test (no randomness in tests).
fn rand_offset() -> f64 {
    0.0 // Keep tests deterministic
}
