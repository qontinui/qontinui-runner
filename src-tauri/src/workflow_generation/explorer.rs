//! AB-MCTS Explorer
//!
//! Adaptive Branching Monte Carlo Tree Search for verification step generation.
//! Generates multiple candidate step sets with different strategies, evaluates
//! them, and selects the best candidate using UCB1 selection.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Instant;
use tracing::{debug, info, warn};

use super::domain_generators::{
    get_domain_generator, DomainContext, DomainGenerationResult, DomainGenerator,
};
use super::domain_routing::VerificationDomain;
use super::evaluation::{evaluate_workflow, ScoringStrategy};
use super::specification::AcceptanceCriterion;
use super::template_library::StepTemplate;
use crate::doctor::DoctorHandle;

// ============================================================================
// Configuration
// ============================================================================

/// How to score exploration candidates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum CandidateScorer {
    /// Fast heuristic scoring (no AI calls)
    #[default]
    QuickHeuristic,
    /// Tier 1 deterministic evaluation only
    DeterministicEval,
    /// Full evaluation engine (Tiers 1+2, slower but accurate)
    FullEval,
}

/// Configuration for the exploration engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationConfig {
    /// Whether AB-MCTS exploration is enabled (false = single-shot generation).
    pub enabled: bool,
    /// Maximum number of candidates to generate before selecting.
    pub max_candidates: usize,
    /// Maximum tree depth for refinement chains.
    pub max_depth: usize,
    /// Minimum score below which a candidate is discarded entirely.
    pub min_score_threshold: f64,
    /// Target score at which exploration stops early (good enough).
    pub target_score: f64,
    /// UCB1 exploration constant (higher = more exploration, lower = more exploitation).
    pub exploration_constant: f64,
    /// Scoring method used to evaluate candidates.
    #[serde(default)]
    pub scorer: CandidateScorer,
}

impl Default for ExplorationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_candidates: 5,
            max_depth: 3,
            min_score_threshold: 0.3,
            target_score: 0.85,
            exploration_constant: 1.414,
            scorer: CandidateScorer::default(),
        }
    }
}

impl ExplorationConfig {
    /// Config tuned for a specific domain.
    ///
    /// Compilation/CiCd domains use fewer candidates with lower targets because
    /// their verification patterns are more deterministic. UiContent/Security
    /// domains explore more aggressively because they need greater variety.
    pub fn for_domain(domain: VerificationDomain) -> Self {
        match domain {
            VerificationDomain::Compilation | VerificationDomain::CiCd => Self {
                enabled: true,
                max_candidates: 3,
                max_depth: 2,
                min_score_threshold: 0.3,
                target_score: 0.75,
                exploration_constant: 1.0,
                scorer: CandidateScorer::QuickHeuristic,
            },
            VerificationDomain::UiContent | VerificationDomain::Security => Self {
                enabled: true,
                max_candidates: 6,
                max_depth: 4,
                min_score_threshold: 0.25,
                target_score: 0.85,
                exploration_constant: 1.6,
                scorer: CandidateScorer::DeterministicEval,
            },
            VerificationDomain::Performance => Self {
                enabled: true,
                max_candidates: 4,
                max_depth: 3,
                min_score_threshold: 0.3,
                target_score: 0.80,
                exploration_constant: 1.414,
                scorer: CandidateScorer::DeterministicEval,
            },
            _ => Self::default(),
        }
    }
}

// ============================================================================
// Strategy Variants
// ============================================================================

/// Strategy variant used to generate a candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationVariant {
    /// Normal generation with no special emphasis.
    Standard,
    /// Inject high-confidence templates prominently into the prompt.
    TemplateFirst,
    /// Prefer command steps over prompt steps; every step must be deterministic.
    StrictDeterminism,
    /// Generate one step per criterion with no shared steps.
    MaxCoverage,
    /// Generate the fewest steps that cover all critical criteria.
    Minimal,
    /// Refine a previous candidate by regenerating only weak steps.
    Refinement { weak_step_ids: Vec<String> },
}

impl GenerationVariant {
    /// Strategy label for deduplication and observability.
    fn label(&self) -> &str {
        match self {
            GenerationVariant::Standard => "standard",
            GenerationVariant::TemplateFirst => "template_first",
            GenerationVariant::StrictDeterminism => "strict_determinism",
            GenerationVariant::MaxCoverage => "max_coverage",
            GenerationVariant::Minimal => "minimal",
            GenerationVariant::Refinement { .. } => "refinement",
        }
    }

    /// Supplemental system instruction injected into the generation prompt.
    fn system_instruction(&self) -> Option<String> {
        match self {
            GenerationVariant::Standard => None,
            GenerationVariant::TemplateFirst => Some(
                "Prioritize using the provided step templates as-is where they match a criterion. \
                 Only generate custom steps when no template fits."
                    .to_string(),
            ),
            GenerationVariant::StrictDeterminism => Some(
                "Prefer command steps over prompt steps. Every verification step MUST be \
                 deterministic: use concrete commands, exit codes, and stdout/stderr checks. \
                 Do not generate any step whose outcome depends on AI judgment."
                    .to_string(),
            ),
            GenerationVariant::MaxCoverage => Some(
                "Generate exactly one verification step per acceptance criterion. Do not share \
                 steps between criteria. Each step should be narrowly targeted at its criterion."
                    .to_string(),
            ),
            GenerationVariant::Minimal => Some(
                "Generate the fewest verification steps that cover ALL critical criteria. Combine \
                 checks where possible. Favor multi-assertion steps over single-assertion ones."
                    .to_string(),
            ),
            GenerationVariant::Refinement { weak_step_ids } => Some(format!(
                "The following step IDs scored poorly and must be regenerated with improved \
                 quality: {}. Keep all other steps unchanged. Focus on determinism and entailment \
                 for the regenerated steps.",
                weak_step_ids.join(", ")
            )),
        }
    }

    /// The initial set of strategies to seed the search tree (Phase 1).
    fn initial_strategies() -> Vec<GenerationVariant> {
        vec![
            GenerationVariant::Standard,
            GenerationVariant::TemplateFirst,
            GenerationVariant::StrictDeterminism,
        ]
    }

    /// Strategies to try when going wider (Phase 2, diversification).
    fn wider_strategies() -> Vec<GenerationVariant> {
        vec![GenerationVariant::MaxCoverage, GenerationVariant::Minimal]
    }
}

// ============================================================================
// Search Tree
// ============================================================================

/// A node in the search tree representing a single candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchNode {
    /// Unique node identifier.
    pub id: String,
    /// Generated verification steps as JSON values.
    pub steps: Vec<Value>,
    /// Evaluation score (None if not yet scored).
    pub score: Option<f64>,
    /// Parent node ID (None for root-level candidates).
    pub parent_id: Option<String>,
    /// Child node IDs.
    pub children: Vec<String>,
    /// Number of times this node has been visited/evaluated.
    pub visits: u32,
    /// Strategy used to generate this candidate.
    pub strategy: GenerationVariant,
    /// Depth in the search tree (0 = root).
    pub depth: u32,
}

/// The AB-MCTS search tree structure.
#[derive(Debug, Clone)]
pub struct SearchTree {
    nodes: HashMap<String, SearchNode>,
    root_ids: Vec<String>,
}

impl SearchTree {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            root_ids: Vec::new(),
        }
    }

    /// Add a root-level candidate.
    pub fn add_root(&mut self, node: SearchNode) {
        let id = node.id.clone();
        self.root_ids.push(id.clone());
        self.nodes.insert(id, node);
    }

    /// Add a child to an existing node.
    pub fn add_child(&mut self, parent_id: &str, node: SearchNode) {
        let child_id = node.id.clone();
        self.nodes.insert(child_id.clone(), node);
        if let Some(parent) = self.nodes.get_mut(parent_id) {
            parent.children.push(child_id);
        }
    }

    /// Total number of scored nodes.
    pub fn total_visits(&self) -> usize {
        self.nodes.values().filter(|n| n.score.is_some()).count()
    }

    /// Select the most promising node using the UCB1 formula.
    ///
    /// UCB1 = mean_score + C * sqrt(ln(total_visits) / node_visits)
    ///
    /// Nodes that have not been scored yet are skipped. Returns the node
    /// with the highest UCB1 value, which balances exploitation (high score)
    /// with exploration (few visits).
    pub fn select_ucb1(&self, exploration_constant: f64) -> Option<&SearchNode> {
        let total = self.total_visits();
        if total == 0 {
            return None;
        }
        let ln_total = (total as f64).ln();

        let mut best_node: Option<&SearchNode> = None;
        let mut best_ucb1 = f64::NEG_INFINITY;

        for node in self.nodes.values() {
            let score = match node.score {
                Some(s) => s,
                None => continue,
            };
            if node.visits == 0 {
                continue;
            }
            let ucb1 = score + exploration_constant * (ln_total / node.visits as f64).sqrt();
            if ucb1 > best_ucb1 {
                best_ucb1 = ucb1;
                best_node = Some(node);
            }
        }

        best_node
    }

    /// Get the best and runner-up nodes by score.
    pub fn select_best_and_runner_up(&self) -> (Option<&SearchNode>, Option<&SearchNode>) {
        let mut scored: Vec<&SearchNode> =
            self.nodes.values().filter(|n| n.score.is_some()).collect();
        scored.sort_by(|a, b| {
            b.score
                .unwrap_or(0.0)
                .partial_cmp(&a.score.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let best = scored.first().copied();
        let runner_up = scored.get(1).copied();
        (best, runner_up)
    }

    /// Score variance across all scored nodes.
    pub fn score_variance(&self) -> f64 {
        let scores: Vec<f64> = self.nodes.values().filter_map(|n| n.score).collect();
        if scores.len() < 2 {
            return 0.0;
        }
        let mean = scores.iter().sum::<f64>() / scores.len() as f64;
        let sum_sq_diff: f64 = scores.iter().map(|s| (s - mean).powi(2)).sum();
        sum_sq_diff / scores.len() as f64
    }

    /// Best score across all nodes.
    pub fn best_score(&self) -> f64 {
        self.nodes
            .values()
            .filter_map(|n| n.score)
            .fold(0.0_f64, f64::max)
    }

    /// Maximum depth reached in the tree.
    pub fn max_depth(&self) -> usize {
        self.nodes
            .values()
            .map(|n| n.depth as usize)
            .max()
            .unwrap_or(0)
    }

    /// Number of distinct strategies explored.
    pub fn strategies_explored(&self) -> usize {
        let labels: std::collections::HashSet<&str> =
            self.nodes.values().map(|n| n.strategy.label()).collect();
        labels.len()
    }

    /// Score progression (node index, score) for observability.
    ///
    /// Returns pairs of (evaluation order, score) so callers can plot
    /// how scores evolved over the search.
    pub fn score_progression(&self) -> Vec<(usize, f64)> {
        let mut scored: Vec<(&str, f64)> = self
            .nodes
            .values()
            .filter_map(|n| n.score.map(|s| (n.id.as_str(), s)))
            .collect();
        // Stable order: sort by id (UUIDs are monotonically increasing within
        // a process because uuid v4 uses random bytes, but we just need a
        // consistent ordering for the progression view).
        scored.sort_by(|a, b| a.0.cmp(b.0));
        scored
            .into_iter()
            .enumerate()
            .map(|(i, (_, s))| (i, s))
            .collect()
    }

    /// Get weak steps (composite score below threshold) from a node.
    ///
    /// Since we store steps as JSON values, we extract step names/IDs that
    /// scored poorly. This is used by the Refinement strategy.
    pub fn get_weak_steps(&self, node_id: &str, threshold: f64) -> Vec<String> {
        let node = match self.nodes.get(node_id) {
            Some(n) => n,
            None => return Vec::new(),
        };
        let mut weak = Vec::new();
        for (i, step) in node.steps.iter().enumerate() {
            // Check if the step has a "score" or "composite_score" field
            // from a previous evaluation pass. If not, use heuristics.
            let step_score = step
                .get("composite_score")
                .and_then(|v| v.as_f64())
                .or_else(|| step.get("score").and_then(|v| v.as_f64()));

            if let Some(s) = step_score {
                if s < threshold {
                    let step_id = step
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            step.get("name")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_else(|| format!("step_{}", i));
                    weak.push(step_id);
                }
            } else {
                // No score attached: check heuristic signals for weakness
                let has_expected =
                    step.get("expected").is_some() || step.get("expected_output").is_some();
                let has_check = step.get("check_type").is_some() || step.get("check").is_some();
                let is_command = step
                    .get("step_type")
                    .and_then(|v| v.as_str())
                    .map(|t| t == "command")
                    .unwrap_or(false);

                // Heuristic: if it lacks expected value AND is not a command, it is weak
                if !has_expected && !is_command && !has_check {
                    let step_id = step
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            step.get("name")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_else(|| format!("step_{}", i));
                    weak.push(step_id);
                }
            }
        }
        weak
    }
}

// ============================================================================
// Exploration Result
// ============================================================================

/// Result of the exploration process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationResult {
    /// The best candidate found during exploration.
    pub best_candidate: SearchNode,
    /// Second-best candidate for fixer backtracking.
    pub runner_up: Option<SearchNode>,
    /// Total number of candidates generated and scored.
    pub total_candidates_explored: usize,
    /// Maximum search depth reached.
    pub search_depth_reached: usize,
    /// Wall-clock duration of exploration in milliseconds.
    pub search_duration_ms: u64,
    /// Score progression for observability.
    pub score_progression: Vec<(usize, f64)>,
}

// ============================================================================
// Adaptive Branching
// ============================================================================

/// Whether to explore wider (new strategy) or deeper (refine existing).
#[derive(Debug)]
enum BranchingAction {
    /// Generate a new root candidate with a different strategy.
    Wider,
    /// Refine an existing candidate (create child node).
    Deeper(String),
}

/// Bayesian adaptive branching decision.
///
/// Decides whether the next candidate should diversify (new strategy) or
/// exploit (refine the best existing candidate). The heuristics are:
///
/// 1. If fewer than 3 strategies have been explored, go wider for diversity.
/// 2. If score variance is very low (< 0.05), all candidates are similar so
///    go wider to break out of a local optimum.
/// 3. If the best score exceeds 0.7 and the selected node has room for depth,
///    go deeper to polish it.
/// 4. Otherwise, go deeper on the best node.
fn adaptive_branching_decision(
    tree: &SearchTree,
    selected: &SearchNode,
    config: &ExplorationConfig,
) -> BranchingAction {
    let strategies_explored = tree.strategies_explored();

    // Too few strategies explored -- diversify first
    if strategies_explored < 3 {
        debug!(
            strategies_explored,
            "AB-MCTS: fewer than 3 strategies explored, going wider"
        );
        return BranchingAction::Wider;
    }

    // All scores are clustered -- need diversity
    let variance = tree.score_variance();
    if variance < 0.05 {
        debug!(
            variance,
            "AB-MCTS: low score variance, going wider for diversity"
        );
        return BranchingAction::Wider;
    }

    // Best score is promising and depth budget remains -- refine
    let best = tree.best_score();
    if best > 0.7 && (selected.depth as usize) < config.max_depth {
        debug!(
            best_score = best,
            selected_depth = selected.depth,
            "AB-MCTS: best score above 0.7 with depth budget, going deeper"
        );
        return BranchingAction::Deeper(selected.id.clone());
    }

    // Default: refine the selected node
    debug!("AB-MCTS: defaulting to deeper on selected node");
    BranchingAction::Deeper(selected.id.clone())
}

// ============================================================================
// Quick Scoring (Heuristic)
// ============================================================================

/// Compute a heuristic score for a DomainGenerationResult without running
/// the full evaluation engine.
///
/// Used for rapid candidate scoring during exploration. The heuristic checks:
/// - Step count roughly matches criteria count (within +/-2): +0.3
/// - All steps have step_type == "command" (deterministic): +0.2
/// - Steps have check_type fields: +0.2
/// - Steps have expected/expected_output fields: +0.2
/// - No duplicate step names: +0.1
///
/// Score is clamped to [0.0, 1.0].
fn quick_score(result: &DomainGenerationResult, criteria: &[AcceptanceCriterion]) -> f64 {
    let steps = &result.steps;
    if steps.is_empty() {
        return 0.0;
    }

    let criteria_count = criteria.len();
    let step_count = steps.len();
    let mut score = 0.0;

    // +0.3 if step count roughly matches criteria count (within +/-2)
    let diff = (step_count as isize - criteria_count as isize).unsigned_abs();
    if diff <= 2 {
        score += 0.3;
    } else if diff <= 4 {
        // Partial credit for being in the ballpark
        score += 0.15;
    }

    // +0.2 if all steps have step_type == "command"
    let command_count = steps
        .iter()
        .filter(|s| {
            s.get("step_type")
                .and_then(|v| v.as_str())
                .map(|t| t == "command")
                .unwrap_or(false)
        })
        .count();
    if command_count == step_count {
        score += 0.2;
    } else if command_count > 0 {
        // Partial credit proportional to command ratio
        score += 0.2 * (command_count as f64 / step_count as f64);
    }

    // +0.2 if steps have check_type fields
    let check_count = steps
        .iter()
        .filter(|s| s.get("check_type").is_some() || s.get("check").is_some())
        .count();
    if check_count == step_count {
        score += 0.2;
    } else if check_count > 0 {
        score += 0.2 * (check_count as f64 / step_count as f64);
    }

    // +0.2 if steps have expected/expected_output fields
    let expected_count = steps
        .iter()
        .filter(|s| s.get("expected").is_some() || s.get("expected_output").is_some())
        .count();
    if expected_count == step_count {
        score += 0.2;
    } else if expected_count > 0 {
        score += 0.2 * (expected_count as f64 / step_count as f64);
    }

    // +0.1 if no duplicate step names
    let names: Vec<&str> = steps
        .iter()
        .filter_map(|s| s.get("name").and_then(|v| v.as_str()))
        .collect();
    let unique_names: std::collections::HashSet<&str> = names.iter().copied().collect();
    if !names.is_empty() && unique_names.len() == names.len() {
        score += 0.1;
    }

    score.clamp(0.0, 1.0)
}

// ============================================================================
// Score Dispatch
// ============================================================================

/// Score a candidate's steps using the configured scorer.
///
/// For QuickHeuristic: uses the fast heuristic (no AI calls).
/// For DeterministicEval: builds a temporary UnifiedWorkflow and runs Tier 1 evaluation.
/// For FullEval: builds a temporary UnifiedWorkflow and runs Tier 1+2 evaluation.
fn score_candidate(
    steps: &[Value],
    criteria: &[AcceptanceCriterion],
    config: &ExplorationConfig,
    doctor_handle: Option<&DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
) -> f64 {
    match &config.scorer {
        CandidateScorer::QuickHeuristic => quick_score_from_steps(steps, criteria),
        CandidateScorer::DeterministicEval | CandidateScorer::FullEval => {
            let strategy = match &config.scorer {
                CandidateScorer::DeterministicEval => ScoringStrategy::FastOnly,
                CandidateScorer::FullEval => ScoringStrategy::Standard,
                _ => unreachable!(),
            };

            // Build a temporary workflow from the steps
            let workflow_json = serde_json::json!({
                "id": uuid::Uuid::new_v4().to_string(),
                "name": "exploration-candidate",
                "description": "Temporary workflow for evaluation",
                "category": "generated",
                "tags": [],
                "verification_steps": steps,
                "setup_steps": [],
                "agentic_steps": [],
                "completion_steps": [],
                "max_iterations": 3,
            });

            match serde_json::from_value::<crate::unified_workflows::UnifiedWorkflow>(workflow_json)
            {
                Ok(workflow) => {
                    let acceptance = super::specification::AcceptanceCriteria {
                        goal_summary: String::new(),
                        criteria: criteria.to_vec(),
                        assumptions: Vec::new(),
                        bugfix_context: None,
                    };

                    let eval = evaluate_workflow(
                        &workflow,
                        Some(&acceptance),
                        strategy,
                        doctor_handle,
                        model_override,
                        provider_override,
                        None, // no PRM endpoint
                        None, // target_family — auto-detect
                        None, // runner_port — auto-detect from env
                    );

                    debug!(
                        overall_score = eval.overall_score,
                        duration_ms = eval.evaluation_duration_ms,
                        steps_evaluated = eval.step_evaluations.len(),
                        "Evaluation engine scored candidate"
                    );

                    eval.overall_score
                }
                Err(e) => {
                    warn!("Failed to build temporary workflow for evaluation: {}", e);
                    quick_score_from_steps(steps, criteria)
                }
            }
        }
    }
}

/// Wrapper for quick_score that takes steps directly (for the dispatch).
fn quick_score_from_steps(steps: &[Value], criteria: &[AcceptanceCriterion]) -> f64 {
    let result = DomainGenerationResult {
        domain: VerificationDomain::General,
        steps: steps.to_vec(),
        success: true,
        error: None,
        duration_ms: 0,
    };
    quick_score(&result, criteria)
}

// ============================================================================
// Strategy-Driven Generation
// ============================================================================

/// Generate a candidate with a specific strategy variant.
///
/// Modifies the prompt/template injection based on the strategy before
/// delegating to the domain generator.
fn generate_with_strategy(
    generator: &dyn DomainGenerator,
    criteria: &[&AcceptanceCriterion],
    context: &DomainContext,
    templates: &[StepTemplate],
    strategy: &GenerationVariant,
    doctor_handle: Option<&DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
) -> DomainGenerationResult {
    // Build a modified context with the strategy instruction injected
    let mut ctx = context.clone();

    // Inject strategy-specific system instruction into the context
    if let Some(instruction) = strategy.system_instruction() {
        ctx.resolved_contexts
            .push_str(&format!("\n\n[Strategy Instruction]: {}", instruction));
    }

    // For TemplateFirst, promote templates to the front of the hint list
    let effective_templates: Vec<StepTemplate> = match strategy {
        GenerationVariant::TemplateFirst => {
            // Sort templates by confidence descending so the best ones appear first
            let mut sorted: Vec<StepTemplate> = templates.to_vec();
            sorted.sort_by(|a, b| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            sorted
        }
        _ => templates.to_vec(),
    };

    // For Refinement, filter criteria to only those mapping to weak steps
    let effective_criteria: Vec<&AcceptanceCriterion> = match strategy {
        GenerationVariant::Refinement { weak_step_ids } => {
            // Include all criteria (the instruction tells the model which steps to fix)
            // but also include the weak step IDs in the context for reference
            if !weak_step_ids.is_empty() {
                ctx.resolved_contexts.push_str(&format!(
                    "\n\n[Strategy Instruction]: Weak steps to regenerate: {}",
                    weak_step_ids.join(", ")
                ));
            }
            criteria.to_vec()
        }
        _ => criteria.to_vec(),
    };

    generator.generate(
        &effective_criteria,
        &ctx,
        effective_templates.as_slice(),
        doctor_handle,
        model_override,
        provider_override,
    )
}

// ============================================================================
// Main Entry Point
// ============================================================================

/// Main exploration entry point.
///
/// Generates multiple candidates using different strategies, scores them,
/// and returns the best candidate with an optional runner-up for backtracking.
///
/// The search proceeds in three phases:
/// 1. **Seeding**: Generate initial candidates with Standard, TemplateFirst,
///    and StrictDeterminism strategies.
/// 2. **AB-MCTS loop**: Use UCB1 selection and adaptive branching to decide
///    whether to explore wider (new strategy) or deeper (refine existing).
/// 3. **Selection**: Pick the best and runner-up candidates.
pub fn explore_candidates(
    criteria: &[AcceptanceCriterion],
    domain: VerificationDomain,
    context: &DomainContext,
    templates: &[StepTemplate],
    config: &ExplorationConfig,
    doctor_handle: Option<&DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
) -> ExplorationResult {
    let start = Instant::now();
    let mut tree = SearchTree::new();
    let generator = get_domain_generator(domain);

    // Collect criterion references for the generator interface
    let criteria_refs: Vec<&AcceptanceCriterion> = criteria.iter().collect();

    info!(
        domain = ?domain,
        criteria_count = criteria.len(),
        max_candidates = config.max_candidates,
        "AB-MCTS exploration starting"
    );

    // ---- Phase 1: Seed initial candidates with diverse strategies ----------

    let initial_strategies = GenerationVariant::initial_strategies();
    let phase1_count = initial_strategies.len().min(config.max_candidates);

    for strategy in initial_strategies.into_iter().take(phase1_count) {
        let result = generate_with_strategy(
            generator.as_ref(),
            &criteria_refs,
            context,
            templates,
            &strategy,
            doctor_handle,
            model_override,
            provider_override,
        );

        let score = score_candidate(
            &result.steps,
            criteria,
            config,
            doctor_handle,
            model_override,
            provider_override,
        );
        let node = SearchNode {
            id: uuid::Uuid::new_v4().to_string(),
            steps: result.steps,
            score: Some(score),
            parent_id: None,
            children: Vec::new(),
            visits: 1,
            strategy,
            depth: 0,
        };

        debug!(
            node_id = %node.id,
            strategy = node.strategy.label(),
            score,
            step_count = node.steps.len(),
            "Phase 1: seeded candidate"
        );

        tree.add_root(node);
    }

    // ---- Phase 2: AB-MCTS exploration loop ---------------------------------

    let mut wider_strategies = GenerationVariant::wider_strategies().into_iter();

    while tree.total_visits() < config.max_candidates {
        // Check early termination: if best score meets target, stop
        let current_best = tree.best_score();
        if current_best >= config.target_score {
            info!(
                best_score = current_best,
                target = config.target_score,
                "AB-MCTS: target score reached, stopping early"
            );
            break;
        }

        // Select most promising node via UCB1
        let selected = match tree.select_ucb1(config.exploration_constant) {
            Some(node) => node.clone(),
            None => {
                warn!("AB-MCTS: no scored nodes available for UCB1 selection");
                break;
            }
        };

        // Decide: go wider or go deeper?
        let action = adaptive_branching_decision(&tree, &selected, config);

        match action {
            BranchingAction::Wider => {
                // Pick the next unused wider strategy, or fall back to MaxCoverage
                let strategy = wider_strategies
                    .next()
                    .unwrap_or(GenerationVariant::MaxCoverage);

                let result = generate_with_strategy(
                    generator.as_ref(),
                    &criteria_refs,
                    context,
                    templates,
                    &strategy,
                    doctor_handle,
                    model_override,
                    provider_override,
                );

                let score = score_candidate(
                    &result.steps,
                    criteria,
                    config,
                    doctor_handle,
                    model_override,
                    provider_override,
                );
                let node = SearchNode {
                    id: uuid::Uuid::new_v4().to_string(),
                    steps: result.steps,
                    score: Some(score),
                    parent_id: None,
                    children: Vec::new(),
                    visits: 1,
                    strategy,
                    depth: 0,
                };

                debug!(
                    node_id = %node.id,
                    strategy = node.strategy.label(),
                    score,
                    "Phase 2: wider candidate"
                );

                tree.add_root(node);
            }
            BranchingAction::Deeper(parent_id) => {
                // Refine the selected node by regenerating its weak steps
                let weak_steps = tree.get_weak_steps(&parent_id, config.min_score_threshold);

                let strategy = if weak_steps.is_empty() {
                    // No identifiably weak steps -- use MaxCoverage as fallback
                    GenerationVariant::MaxCoverage
                } else {
                    GenerationVariant::Refinement {
                        weak_step_ids: weak_steps,
                    }
                };

                let parent_depth = tree.nodes.get(&parent_id).map(|n| n.depth).unwrap_or(0);

                if (parent_depth as usize) >= config.max_depth {
                    debug!(
                        parent_id = %parent_id,
                        depth = parent_depth,
                        "AB-MCTS: max depth reached, skipping deeper"
                    );
                    // Fall back to wider if we still have budget
                    let fallback_strategy = wider_strategies
                        .next()
                        .unwrap_or(GenerationVariant::Minimal);

                    let result = generate_with_strategy(
                        generator.as_ref(),
                        &criteria_refs,
                        context,
                        templates,
                        &fallback_strategy,
                        doctor_handle,
                        model_override,
                        provider_override,
                    );

                    let score = score_candidate(
                        &result.steps,
                        criteria,
                        config,
                        doctor_handle,
                        model_override,
                        provider_override,
                    );
                    let node = SearchNode {
                        id: uuid::Uuid::new_v4().to_string(),
                        steps: result.steps,
                        score: Some(score),
                        parent_id: None,
                        children: Vec::new(),
                        visits: 1,
                        strategy: fallback_strategy,
                        depth: 0,
                    };

                    tree.add_root(node);
                    continue;
                }

                let result = generate_with_strategy(
                    generator.as_ref(),
                    &criteria_refs,
                    context,
                    templates,
                    &strategy,
                    doctor_handle,
                    model_override,
                    provider_override,
                );

                let score = score_candidate(
                    &result.steps,
                    criteria,
                    config,
                    doctor_handle,
                    model_override,
                    provider_override,
                );
                let child = SearchNode {
                    id: uuid::Uuid::new_v4().to_string(),
                    steps: result.steps,
                    score: Some(score),
                    parent_id: Some(parent_id.clone()),
                    children: Vec::new(),
                    visits: 1,
                    strategy,
                    depth: parent_depth + 1,
                };

                debug!(
                    node_id = %child.id,
                    parent_id = %parent_id,
                    strategy = child.strategy.label(),
                    score,
                    depth = child.depth,
                    "Phase 2: deeper candidate"
                );

                tree.add_child(&parent_id, child);
            }
        }
    }

    // ---- Phase 3: Select best + runner-up ----------------------------------

    let (best, runner_up) = tree.select_best_and_runner_up();

    let best_candidate = best.cloned().unwrap_or_else(|| {
        // Should never happen since Phase 1 always generates at least one,
        // but produce a safe fallback.
        warn!("AB-MCTS: no scored candidates found, returning empty fallback");
        SearchNode {
            id: uuid::Uuid::new_v4().to_string(),
            steps: Vec::new(),
            score: Some(0.0),
            parent_id: None,
            children: Vec::new(),
            visits: 0,
            strategy: GenerationVariant::Standard,
            depth: 0,
        }
    });

    let runner_up_candidate = runner_up.cloned();
    let total_explored = tree.total_visits();
    let depth_reached = tree.max_depth();
    let progression = tree.score_progression();
    let elapsed = start.elapsed().as_millis() as u64;

    info!(
        best_score = best_candidate.score.unwrap_or(0.0),
        runner_up_score = runner_up_candidate
            .as_ref()
            .and_then(|n| n.score)
            .unwrap_or(0.0),
        total_explored,
        depth_reached,
        elapsed_ms = elapsed,
        "AB-MCTS exploration complete"
    );

    ExplorationResult {
        best_candidate,
        runner_up: runner_up_candidate,
        total_candidates_explored: total_explored,
        search_depth_reached: depth_reached,
        search_duration_ms: elapsed,
        score_progression: progression,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(id: &str, score: f64, depth: u32, strategy: GenerationVariant) -> SearchNode {
        SearchNode {
            id: id.to_string(),
            steps: Vec::new(),
            score: Some(score),
            parent_id: None,
            children: Vec::new(),
            visits: 1,
            strategy,
            depth,
        }
    }

    #[test]
    fn test_search_tree_best_score() {
        let mut tree = SearchTree::new();
        tree.add_root(make_node("a", 0.5, 0, GenerationVariant::Standard));
        tree.add_root(make_node("b", 0.8, 0, GenerationVariant::TemplateFirst));
        tree.add_root(make_node("c", 0.6, 0, GenerationVariant::StrictDeterminism));
        assert!((tree.best_score() - 0.8).abs() < 1e-9);
    }

    #[test]
    fn test_search_tree_select_best_and_runner_up() {
        let mut tree = SearchTree::new();
        tree.add_root(make_node("a", 0.5, 0, GenerationVariant::Standard));
        tree.add_root(make_node("b", 0.8, 0, GenerationVariant::TemplateFirst));
        tree.add_root(make_node("c", 0.6, 0, GenerationVariant::Minimal));

        let (best, runner_up) = tree.select_best_and_runner_up();
        assert_eq!(best.unwrap().id, "b");
        assert_eq!(runner_up.unwrap().id, "c");
    }

    #[test]
    fn test_search_tree_ucb1_prefers_less_visited() {
        let mut tree = SearchTree::new();
        // Node with high score but many visits
        let mut high_visited = make_node("hv", 0.8, 0, GenerationVariant::Standard);
        high_visited.visits = 10;
        tree.add_root(high_visited);

        // Node with lower score but one visit -- UCB1 exploration bonus should help
        tree.add_root(make_node("lv", 0.7, 0, GenerationVariant::Minimal));

        let selected = tree.select_ucb1(1.414).unwrap();
        // With C=1.414 and 2 total visits:
        // hv: 0.8 + 1.414 * sqrt(ln(2)/10) = 0.8 + 1.414 * 0.263 = 0.8 + 0.372 = 1.172
        // lv: 0.7 + 1.414 * sqrt(ln(2)/1) = 0.7 + 1.414 * 0.832 = 0.7 + 1.177 = 1.877
        assert_eq!(selected.id, "lv");
    }

    #[test]
    fn test_search_tree_variance() {
        let mut tree = SearchTree::new();
        tree.add_root(make_node("a", 0.5, 0, GenerationVariant::Standard));
        tree.add_root(make_node("b", 0.5, 0, GenerationVariant::Minimal));
        // Identical scores -> variance = 0
        assert!(tree.score_variance() < 1e-9);
    }

    #[test]
    fn test_search_tree_add_child() {
        let mut tree = SearchTree::new();
        tree.add_root(make_node("parent", 0.6, 0, GenerationVariant::Standard));
        let child = SearchNode {
            id: "child".to_string(),
            steps: Vec::new(),
            score: Some(0.75),
            parent_id: Some("parent".to_string()),
            children: Vec::new(),
            visits: 1,
            strategy: GenerationVariant::Refinement {
                weak_step_ids: vec!["s1".to_string()],
            },
            depth: 1,
        };
        tree.add_child("parent", child);
        assert_eq!(tree.max_depth(), 1);
        assert_eq!(tree.total_visits(), 2);
        assert_eq!(tree.nodes.get("parent").unwrap().children.len(), 1);
    }

    #[test]
    fn test_search_tree_strategies_explored() {
        let mut tree = SearchTree::new();
        tree.add_root(make_node("a", 0.5, 0, GenerationVariant::Standard));
        tree.add_root(make_node("b", 0.6, 0, GenerationVariant::Standard));
        tree.add_root(make_node("c", 0.7, 0, GenerationVariant::Minimal));
        assert_eq!(tree.strategies_explored(), 2);
    }

    #[test]
    fn test_quick_score_perfect() {
        use serde_json::json;

        let steps = vec![
            json!({
                "name": "check-build",
                "step_type": "command",
                "check_type": "exit_code",
                "expected": "0"
            }),
            json!({
                "name": "check-tests",
                "step_type": "command",
                "check_type": "exit_code",
                "expected": "0"
            }),
        ];

        let result = DomainGenerationResult {
            steps,
            domain: VerificationDomain::Compilation,
            success: true,
            error: None,
            duration_ms: 100,
        };

        let criteria = vec![
            AcceptanceCriterion {
                id: "c1".to_string(),
                description: "Build passes".to_string(),
                method: super::super::specification::VerificationMethod::Command,
                priority: super::super::specification::CriterionPriority::Critical,
                verification_hint: "".to_string(),
                category: "compilation".to_string(),
                ..Default::default()
            },
            AcceptanceCriterion {
                id: "c2".to_string(),
                description: "Tests pass".to_string(),
                method: super::super::specification::VerificationMethod::Command,
                priority: super::super::specification::CriterionPriority::Critical,
                verification_hint: "".to_string(),
                category: "compilation".to_string(),
                ..Default::default()
            },
        ];

        let score = quick_score(&result, &criteria);
        // 2 steps, 2 criteria (within ±2): +0.3
        // Both command: +0.2
        // Both have check_type: +0.2
        // Both have expected: +0.2
        // Unique names: +0.1
        assert!((score - 1.0).abs() < 1e-9, "Expected 1.0, got {}", score);
    }

    #[test]
    fn test_quick_score_empty() {
        let result = DomainGenerationResult {
            steps: Vec::new(),
            domain: VerificationDomain::General,
            success: true,
            error: None,
            duration_ms: 0,
        };
        let score = quick_score(&result, &[]);
        assert!((score - 0.0).abs() < 1e-9);
    }

    #[test]
    fn test_exploration_config_for_domain() {
        let comp = ExplorationConfig::for_domain(VerificationDomain::Compilation);
        assert_eq!(comp.max_candidates, 3);
        assert!((comp.target_score - 0.75).abs() < 1e-9);

        let ui = ExplorationConfig::for_domain(VerificationDomain::UiContent);
        assert_eq!(ui.max_candidates, 6);
        assert!((ui.target_score - 0.85).abs() < 1e-9);

        let general = ExplorationConfig::for_domain(VerificationDomain::General);
        assert_eq!(general.max_candidates, 5);
    }

    #[test]
    fn test_adaptive_branching_few_strategies() {
        let mut tree = SearchTree::new();
        tree.add_root(make_node("a", 0.5, 0, GenerationVariant::Standard));
        tree.add_root(make_node("b", 0.6, 0, GenerationVariant::Standard));
        // Only 1 strategy explored
        let config = ExplorationConfig::default();
        let selected = tree.select_ucb1(config.exploration_constant).unwrap();
        let action = adaptive_branching_decision(&tree, selected, &config);
        assert!(matches!(action, BranchingAction::Wider));
    }

    #[test]
    fn test_get_weak_steps_from_scored() {
        use serde_json::json;
        let mut tree = SearchTree::new();
        let node = SearchNode {
            id: "n1".to_string(),
            steps: vec![
                json!({"id": "s1", "composite_score": 0.8}),
                json!({"id": "s2", "composite_score": 0.1}),
                json!({"name": "s3", "composite_score": 0.9}),
            ],
            score: Some(0.6),
            parent_id: None,
            children: Vec::new(),
            visits: 1,
            strategy: GenerationVariant::Standard,
            depth: 0,
        };
        tree.add_root(node);

        let weak = tree.get_weak_steps("n1", 0.3);
        assert_eq!(weak, vec!["s2".to_string()]);
    }

    #[test]
    fn test_get_weak_steps_heuristic() {
        use serde_json::json;
        let mut tree = SearchTree::new();
        let node = SearchNode {
            id: "n1".to_string(),
            steps: vec![
                json!({"name": "good-step", "step_type": "command", "expected": "0"}),
                json!({"name": "bad-step", "step_type": "prompt"}), // no expected, no check, not command
            ],
            score: Some(0.5),
            parent_id: None,
            children: Vec::new(),
            visits: 1,
            strategy: GenerationVariant::Standard,
            depth: 0,
        };
        tree.add_root(node);

        let weak = tree.get_weak_steps("n1", 0.3);
        assert_eq!(weak, vec!["bad-step".to_string()]);
    }

    #[test]
    fn test_generation_variant_labels() {
        assert_eq!(GenerationVariant::Standard.label(), "standard");
        assert_eq!(GenerationVariant::TemplateFirst.label(), "template_first");
        assert_eq!(
            GenerationVariant::Refinement {
                weak_step_ids: vec![]
            }
            .label(),
            "refinement"
        );
    }

    #[test]
    fn test_generation_variant_instructions() {
        assert!(GenerationVariant::Standard.system_instruction().is_none());
        assert!(GenerationVariant::StrictDeterminism
            .system_instruction()
            .unwrap()
            .contains("deterministic"));
        assert!(GenerationVariant::MaxCoverage
            .system_instruction()
            .unwrap()
            .contains("one step per criterion"));
    }

    #[test]
    fn test_score_progression() {
        let mut tree = SearchTree::new();
        tree.add_root(make_node("aaa", 0.5, 0, GenerationVariant::Standard));
        tree.add_root(make_node("bbb", 0.7, 0, GenerationVariant::Minimal));
        let prog = tree.score_progression();
        assert_eq!(prog.len(), 2);
        assert_eq!(prog[0].0, 0);
        assert_eq!(prog[1].0, 1);
    }
}
