//! Learning Loop with Post-Task Analysis
//!
//! Implements continuous improvement through post-task analysis, pattern recognition,
//! and feedback loops. Inspired by CrewAI's learning mechanisms and AutoGen's
//! conversation history analysis.
//!
//! # Key Features
//!
//! - **Post-Task Analysis**: Evaluates task outcomes to identify successes and failures
//! - **Pattern Recognition**: Identifies successful strategies and common failure modes
//! - **Knowledge Extraction**: Extracts reusable insights from completed tasks
//! - **Feedback Integration**: Applies learned patterns to future task execution
//!
//! # Example
//!
//! ```rust
//! use crate::orchestrator::learning::{LearningSystem, TaskOutcome, AnalysisConfig};
//!
//! let mut learning = LearningSystem::new();

#![allow(dead_code)]
//!
//! // Record task outcome
//! let outcome = TaskOutcome::success("task-1")
//!     .with_duration_secs(120)
//!     .with_iterations(5)
//!     .with_strategy("incremental_verification");
//!
//! learning.record_outcome(outcome);
//!
//! // Analyze patterns
//! let insights = learning.analyze_patterns();
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Outcome status of a completed task
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutcomeStatus {
    /// Task completed successfully
    Success,
    /// Task partially completed
    PartialSuccess,
    /// Task failed
    Failure,
    /// Task was abandoned or cancelled
    Abandoned,
}

/// Represents the outcome of a completed task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOutcome {
    /// Unique identifier for the task
    pub task_id: String,
    /// Outcome status
    pub status: OutcomeStatus,
    /// Duration in seconds
    pub duration_secs: Option<u64>,
    /// Number of iterations taken
    pub iterations: Option<u32>,
    /// Strategy used for the task
    pub strategy: Option<String>,
    /// Agent roles involved
    pub agents_involved: Vec<String>,
    /// Tools used during execution
    pub tools_used: Vec<String>,
    /// Error messages if failed
    pub errors: Vec<String>,
    /// Key decisions made during execution
    pub decisions: Vec<Decision>,
    /// Metrics collected during execution
    pub metrics: HashMap<String, f64>,
    /// Timestamp when task completed
    pub completed_at: String,
    /// Tags for categorization
    pub tags: Vec<String>,
}

impl TaskOutcome {
    /// Create a successful task outcome
    pub fn success(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            status: OutcomeStatus::Success,
            duration_secs: None,
            iterations: None,
            strategy: None,
            agents_involved: Vec::new(),
            tools_used: Vec::new(),
            errors: Vec::new(),
            decisions: Vec::new(),
            metrics: HashMap::new(),
            completed_at: chrono::Utc::now().to_rfc3339(),
            tags: Vec::new(),
        }
    }

    /// Create a failed task outcome
    pub fn failure(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            status: OutcomeStatus::Failure,
            duration_secs: None,
            iterations: None,
            strategy: None,
            agents_involved: Vec::new(),
            tools_used: Vec::new(),
            errors: Vec::new(),
            decisions: Vec::new(),
            metrics: HashMap::new(),
            completed_at: chrono::Utc::now().to_rfc3339(),
            tags: Vec::new(),
        }
    }

    /// Set duration
    pub fn with_duration_secs(mut self, secs: u64) -> Self {
        self.duration_secs = Some(secs);
        self
    }

    /// Set iterations
    pub fn with_iterations(mut self, iterations: u32) -> Self {
        self.iterations = Some(iterations);
        self
    }

    /// Set strategy
    pub fn with_strategy(mut self, strategy: impl Into<String>) -> Self {
        self.strategy = Some(strategy.into());
        self
    }

    /// Add an agent involved
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agents_involved.push(agent.into());
        self
    }

    /// Add a tool used
    pub fn with_tool(mut self, tool: impl Into<String>) -> Self {
        self.tools_used.push(tool.into());
        self
    }

    /// Add an error
    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.errors.push(error.into());
        self
    }

    /// Add a decision
    pub fn with_decision(mut self, decision: Decision) -> Self {
        self.decisions.push(decision);
        self
    }

    /// Add a metric
    pub fn with_metric(mut self, name: impl Into<String>, value: f64) -> Self {
        self.metrics.insert(name.into(), value);
        self
    }

    /// Add a tag
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }
}

/// A decision made during task execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    /// What was decided
    pub description: String,
    /// Why this decision was made
    pub rationale: Option<String>,
    /// Alternatives considered
    pub alternatives: Vec<String>,
    /// Outcome of the decision
    pub outcome: DecisionOutcome,
    /// Context at the time of decision
    pub context: HashMap<String, String>,
}

impl Decision {
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            rationale: None,
            alternatives: Vec::new(),
            outcome: DecisionOutcome::Unknown,
            context: HashMap::new(),
        }
    }

    pub fn with_rationale(mut self, rationale: impl Into<String>) -> Self {
        self.rationale = Some(rationale.into());
        self
    }

    pub fn with_alternative(mut self, alt: impl Into<String>) -> Self {
        self.alternatives.push(alt.into());
        self
    }

    pub fn with_outcome(mut self, outcome: DecisionOutcome) -> Self {
        self.outcome = outcome;
        self
    }
}

/// Outcome of a decision
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DecisionOutcome {
    /// Decision led to positive outcome
    Positive,
    /// Decision led to negative outcome
    Negative,
    /// Decision had neutral impact
    Neutral,
    /// Outcome not yet determined
    #[default]
    Unknown,
}

/// A pattern identified from task analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pattern {
    /// Unique identifier
    pub id: String,
    /// Pattern name
    pub name: String,
    /// Description of the pattern
    pub description: String,
    /// Pattern type
    pub pattern_type: PatternType,
    /// Confidence score (0.0 - 1.0)
    pub confidence: f64,
    /// Number of occurrences observed
    pub occurrences: u32,
    /// Success rate when this pattern is observed
    pub success_rate: f64,
    /// Conditions that trigger this pattern
    pub triggers: Vec<String>,
    /// Recommended actions when pattern is detected
    pub recommendations: Vec<String>,
    /// Tasks where this pattern was observed
    pub observed_in: Vec<String>,
    /// When pattern was first identified
    pub identified_at: String,
    /// When pattern was last updated
    pub updated_at: String,
}

impl Pattern {
    pub fn new(id: impl Into<String>, name: impl Into<String>, pattern_type: PatternType) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            pattern_type,
            confidence: 0.0,
            occurrences: 0,
            success_rate: 0.0,
            triggers: Vec::new(),
            recommendations: Vec::new(),
            observed_in: Vec::new(),
            identified_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    pub fn with_confidence(mut self, confidence: f64) -> Self {
        self.confidence = confidence.clamp(0.0, 1.0);
        self
    }

    pub fn with_trigger(mut self, trigger: impl Into<String>) -> Self {
        self.triggers.push(trigger.into());
        self
    }

    pub fn with_recommendation(mut self, rec: impl Into<String>) -> Self {
        self.recommendations.push(rec.into());
        self
    }

    pub fn with_occurrences(mut self, count: u32) -> Self {
        self.occurrences = count;
        self
    }

    pub fn with_success_rate(mut self, rate: f64) -> Self {
        self.success_rate = rate.clamp(0.0, 1.0);
        self
    }

    /// Update pattern with new observation
    pub fn record_observation(&mut self, task_id: &str, was_successful: bool) {
        self.occurrences += 1;
        self.observed_in.push(task_id.to_string());

        // Update success rate with exponential moving average
        let alpha = 0.3; // Weight for new observations
        let success_value = if was_successful { 1.0 } else { 0.0 };
        self.success_rate = alpha * success_value + (1.0 - alpha) * self.success_rate;

        // Update confidence based on number of observations
        self.confidence = (self.occurrences as f64 / 10.0).min(1.0);

        self.updated_at = chrono::Utc::now().to_rfc3339();
    }
}

/// Type of pattern
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatternType {
    /// Pattern that leads to success
    SuccessStrategy,
    /// Pattern that leads to failure
    FailureMode,
    /// Pattern in tool usage
    ToolUsage,
    /// Pattern in agent collaboration
    Collaboration,
    /// Pattern in iteration behavior
    IterationBehavior,
    /// Pattern in error recovery
    ErrorRecovery,
    /// Pattern in decision making
    DecisionMaking,
}

/// Insight extracted from task analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Insight {
    /// Unique identifier
    pub id: String,
    /// Insight category
    pub category: InsightCategory,
    /// The insight content
    pub content: String,
    /// Confidence score
    pub confidence: f64,
    /// Supporting evidence
    pub evidence: Vec<String>,
    /// Actionable recommendations
    pub recommendations: Vec<String>,
    /// Related patterns
    pub related_patterns: Vec<String>,
    /// When insight was generated
    pub generated_at: String,
}

impl Insight {
    pub fn new(category: InsightCategory, content: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            category,
            content: content.into(),
            confidence: 0.5,
            evidence: Vec::new(),
            recommendations: Vec::new(),
            related_patterns: Vec::new(),
            generated_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    pub fn with_confidence(mut self, confidence: f64) -> Self {
        self.confidence = confidence.clamp(0.0, 1.0);
        self
    }

    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence.push(evidence.into());
        self
    }

    pub fn with_recommendation(mut self, rec: impl Into<String>) -> Self {
        self.recommendations.push(rec.into());
        self
    }
}

/// Category of insight
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InsightCategory {
    /// Performance-related insight
    Performance,
    /// Quality-related insight
    Quality,
    /// Efficiency-related insight
    Efficiency,
    /// Reliability-related insight
    Reliability,
    /// Strategy-related insight
    Strategy,
    /// Tool-usage insight
    ToolUsage,
    /// Collaboration insight
    Collaboration,
}

/// Configuration for analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisConfig {
    /// Minimum confidence threshold for patterns
    pub min_pattern_confidence: f64,
    /// Minimum occurrences for pattern recognition
    pub min_pattern_occurrences: u32,
    /// Maximum number of patterns to track
    pub max_patterns: usize,
    /// Whether to analyze decisions
    pub analyze_decisions: bool,
    /// Whether to analyze tool usage
    pub analyze_tool_usage: bool,
    /// Whether to analyze collaboration
    pub analyze_collaboration: bool,
    /// Window size for trend analysis (number of tasks)
    pub trend_window_size: usize,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            min_pattern_confidence: 0.3,
            min_pattern_occurrences: 3,
            max_patterns: 100,
            analyze_decisions: true,
            analyze_tool_usage: true,
            analyze_collaboration: true,
            trend_window_size: 20,
        }
    }
}

/// Results from pattern analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResult {
    /// Identified patterns
    pub patterns: Vec<Pattern>,
    /// Generated insights
    pub insights: Vec<Insight>,
    /// Overall success rate
    pub success_rate: f64,
    /// Average duration
    pub avg_duration_secs: Option<f64>,
    /// Average iterations
    pub avg_iterations: Option<f64>,
    /// Most used tools
    pub top_tools: Vec<(String, u32)>,
    /// Most effective strategies
    pub top_strategies: Vec<(String, f64)>,
    /// Trend direction
    pub trend: Trend,
    /// Timestamp
    pub analyzed_at: String,
}

/// Trend direction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Trend {
    /// Performance improving
    Improving,
    /// Performance declining
    Declining,
    /// Performance stable
    #[default]
    Stable,
    /// Not enough data
    Insufficient,
}

/// Feedback to apply to future tasks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feedback {
    /// Feedback type
    pub feedback_type: FeedbackType,
    /// Priority (higher = more important)
    pub priority: u32,
    /// The feedback content
    pub content: String,
    /// Conditions when to apply
    pub apply_when: Vec<String>,
    /// Source pattern or insight
    pub source: String,
    /// Confidence score
    pub confidence: f64,
}

/// Type of feedback
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeedbackType {
    /// Recommend a strategy
    RecommendStrategy,
    /// Avoid a strategy
    AvoidStrategy,
    /// Recommend a tool
    RecommendTool,
    /// Avoid a tool
    AvoidTool,
    /// Warning about potential issue
    Warning,
    /// Optimization suggestion
    Optimization,
}

/// The main learning system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningSystem {
    /// Configuration
    pub config: AnalysisConfig,
    /// Recorded task outcomes
    pub outcomes: Vec<TaskOutcome>,
    /// Identified patterns
    pub patterns: HashMap<String, Pattern>,
    /// Generated insights
    pub insights: Vec<Insight>,
    /// Active feedback items
    pub feedback: Vec<Feedback>,
    /// Strategy statistics
    strategy_stats: HashMap<String, StrategyStats>,
    /// Tool statistics
    tool_stats: HashMap<String, ToolStats>,
    /// Created timestamp
    pub created_at: String,
    /// Last analysis timestamp
    pub last_analysis_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StrategyStats {
    total_uses: u32,
    successes: u32,
    total_duration_secs: u64,
    total_iterations: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ToolStats {
    total_uses: u32,
    in_successful_tasks: u32,
    in_failed_tasks: u32,
}

impl LearningSystem {
    /// Create a new learning system
    pub fn new() -> Self {
        Self {
            config: AnalysisConfig::default(),
            outcomes: Vec::new(),
            patterns: HashMap::new(),
            insights: Vec::new(),
            feedback: Vec::new(),
            strategy_stats: HashMap::new(),
            tool_stats: HashMap::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
            last_analysis_at: None,
        }
    }

    /// Create with custom configuration
    pub fn with_config(config: AnalysisConfig) -> Self {
        Self {
            config,
            ..Self::new()
        }
    }

    /// Record a task outcome
    pub fn record_outcome(&mut self, outcome: TaskOutcome) {
        // Update strategy stats
        if let Some(strategy) = &outcome.strategy {
            let stats = self.strategy_stats.entry(strategy.clone()).or_default();
            stats.total_uses += 1;
            if outcome.status == OutcomeStatus::Success {
                stats.successes += 1;
            }
            if let Some(duration) = outcome.duration_secs {
                stats.total_duration_secs += duration;
            }
            if let Some(iterations) = outcome.iterations {
                stats.total_iterations += iterations;
            }
        }

        // Update tool stats
        let is_success = outcome.status == OutcomeStatus::Success;
        for tool in &outcome.tools_used {
            let stats = self.tool_stats.entry(tool.clone()).or_default();
            stats.total_uses += 1;
            if is_success {
                stats.in_successful_tasks += 1;
            } else {
                stats.in_failed_tasks += 1;
            }
        }

        // Store outcome
        self.outcomes.push(outcome);
    }

    /// Analyze patterns from recorded outcomes
    pub fn analyze_patterns(&mut self) -> AnalysisResult {
        let now = chrono::Utc::now().to_rfc3339();
        self.last_analysis_at = Some(now.clone());

        // Calculate basic metrics
        let total = self.outcomes.len();
        let successes = self
            .outcomes
            .iter()
            .filter(|o| o.status == OutcomeStatus::Success)
            .count();
        let success_rate = if total > 0 {
            successes as f64 / total as f64
        } else {
            0.0
        };

        // Calculate averages
        let durations: Vec<u64> = self
            .outcomes
            .iter()
            .filter_map(|o| o.duration_secs)
            .collect();
        let avg_duration = if !durations.is_empty() {
            Some(durations.iter().sum::<u64>() as f64 / durations.len() as f64)
        } else {
            None
        };

        let iterations: Vec<u32> = self.outcomes.iter().filter_map(|o| o.iterations).collect();
        let avg_iterations = if !iterations.is_empty() {
            Some(iterations.iter().sum::<u32>() as f64 / iterations.len() as f64)
        } else {
            None
        };

        // Find top tools
        let mut top_tools: Vec<(String, u32)> = self
            .tool_stats
            .iter()
            .map(|(k, v)| (k.clone(), v.total_uses))
            .collect();
        top_tools.sort_by(|a, b| b.1.cmp(&a.1));
        top_tools.truncate(5);

        // Find top strategies
        let mut top_strategies: Vec<(String, f64)> = self
            .strategy_stats
            .iter()
            .filter(|(_, v)| v.total_uses >= self.config.min_pattern_occurrences)
            .map(|(k, v)| {
                let success_rate = v.successes as f64 / v.total_uses as f64;
                (k.clone(), success_rate)
            })
            .collect();
        top_strategies.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        top_strategies.truncate(5);

        // Identify patterns
        self.identify_patterns();

        // Generate insights
        self.generate_insights(success_rate, avg_duration, avg_iterations);

        // Calculate trend
        let trend = self.calculate_trend();

        // Generate feedback
        self.generate_feedback();

        AnalysisResult {
            patterns: self.patterns.values().cloned().collect(),
            insights: self.insights.clone(),
            success_rate,
            avg_duration_secs: avg_duration,
            avg_iterations,
            top_tools,
            top_strategies,
            trend,
            analyzed_at: now,
        }
    }

    fn identify_patterns(&mut self) {
        // Identify success strategies
        for (strategy, stats) in &self.strategy_stats {
            if stats.total_uses >= self.config.min_pattern_occurrences {
                let success_rate = stats.successes as f64 / stats.total_uses as f64;

                if success_rate >= 0.7 {
                    let pattern_id = format!("strategy_success_{}", strategy);
                    let pattern = self.patterns.entry(pattern_id.clone()).or_insert_with(|| {
                        Pattern::new(
                            &pattern_id,
                            format!("Successful strategy: {}", strategy),
                            PatternType::SuccessStrategy,
                        )
                        .with_description(format!(
                            "Strategy '{}' has a {}% success rate",
                            strategy,
                            (success_rate * 100.0) as u32
                        ))
                    });
                    pattern.success_rate = success_rate;
                    pattern.occurrences = stats.total_uses;
                    pattern.confidence = (stats.total_uses as f64 / 10.0).min(1.0);
                } else if success_rate <= 0.3 {
                    let pattern_id = format!("strategy_failure_{}", strategy);
                    let pattern = self.patterns.entry(pattern_id.clone()).or_insert_with(|| {
                        Pattern::new(
                            &pattern_id,
                            format!("Problematic strategy: {}", strategy),
                            PatternType::FailureMode,
                        )
                        .with_description(format!(
                            "Strategy '{}' has only {}% success rate",
                            strategy,
                            (success_rate * 100.0) as u32
                        ))
                        .with_recommendation(format!("Consider avoiding '{}'", strategy))
                    });
                    pattern.success_rate = success_rate;
                    pattern.occurrences = stats.total_uses;
                }
            }
        }

        // Identify tool usage patterns
        if self.config.analyze_tool_usage {
            for (tool, stats) in &self.tool_stats {
                if stats.total_uses >= self.config.min_pattern_occurrences {
                    let success_rate = stats.in_successful_tasks as f64 / stats.total_uses as f64;

                    let pattern_id = format!("tool_usage_{}", tool);
                    let pattern = self.patterns.entry(pattern_id.clone()).or_insert_with(|| {
                        Pattern::new(
                            &pattern_id,
                            format!("Tool usage: {}", tool),
                            PatternType::ToolUsage,
                        )
                    });
                    pattern.success_rate = success_rate;
                    pattern.occurrences = stats.total_uses;
                }
            }
        }

        // Identify iteration behavior patterns
        let high_iteration_outcomes: Vec<&TaskOutcome> = self
            .outcomes
            .iter()
            .filter(|o| o.iterations.map(|i| i > 10).unwrap_or(false))
            .collect();

        if high_iteration_outcomes.len() >= self.config.min_pattern_occurrences as usize {
            let success_count = high_iteration_outcomes
                .iter()
                .filter(|o| o.status == OutcomeStatus::Success)
                .count();
            let success_rate = success_count as f64 / high_iteration_outcomes.len() as f64;

            let pattern_id = "high_iteration_tasks".to_string();
            let pattern = self.patterns.entry(pattern_id.clone()).or_insert_with(|| {
                Pattern::new(
                    &pattern_id,
                    "High iteration tasks",
                    PatternType::IterationBehavior,
                )
                .with_description("Tasks requiring more than 10 iterations")
            });
            pattern.success_rate = success_rate;
            pattern.occurrences = high_iteration_outcomes.len() as u32;
        }
    }

    fn generate_insights(
        &mut self,
        success_rate: f64,
        avg_duration: Option<f64>,
        avg_iterations: Option<f64>,
    ) {
        // Clear old insights
        self.insights.clear();

        // Success rate insight
        if self.outcomes.len() >= 5 {
            let insight = if success_rate >= 0.8 {
                Insight::new(
                    InsightCategory::Quality,
                    "High success rate indicates effective task execution",
                )
                .with_confidence(0.8)
                .with_evidence(format!(
                    "{}% success rate across {} tasks",
                    (success_rate * 100.0) as u32,
                    self.outcomes.len()
                ))
            } else if success_rate <= 0.4 {
                Insight::new(
                    InsightCategory::Quality,
                    "Low success rate suggests need for strategy improvements",
                )
                .with_confidence(0.8)
                .with_evidence(format!(
                    "Only {}% success rate across {} tasks",
                    (success_rate * 100.0) as u32,
                    self.outcomes.len()
                ))
                .with_recommendation("Review failed tasks for common patterns")
                .with_recommendation("Consider adjusting verification criteria")
            } else {
                Insight::new(
                    InsightCategory::Quality,
                    "Moderate success rate - room for improvement",
                )
                .with_confidence(0.6)
                .with_evidence(format!("{}% success rate", (success_rate * 100.0) as u32))
            };
            self.insights.push(insight);
        }

        // Duration insight
        if let Some(avg_dur) = avg_duration {
            if avg_dur > 300.0 {
                let insight = Insight::new(
                    InsightCategory::Performance,
                    "Tasks are taking longer than expected",
                )
                .with_confidence(0.7)
                .with_evidence(format!("Average duration: {:.1} seconds", avg_dur))
                .with_recommendation("Consider parallel execution for independent subtasks")
                .with_recommendation("Review context strategies for efficiency");
                self.insights.push(insight);
            }
        }

        // Iteration insight
        if let Some(avg_iter) = avg_iterations {
            if avg_iter > 8.0 {
                let insight = Insight::new(
                    InsightCategory::Efficiency,
                    "High average iterations may indicate unclear goals",
                )
                .with_confidence(0.6)
                .with_evidence(format!("Average iterations: {:.1}", avg_iter))
                .with_recommendation("Ensure verification criteria are well-defined")
                .with_recommendation("Consider breaking complex tasks into subtasks");
                self.insights.push(insight);
            }
        }

        // Pattern-based insights
        for pattern in self.patterns.values() {
            if pattern.confidence >= self.config.min_pattern_confidence {
                match pattern.pattern_type {
                    PatternType::SuccessStrategy if pattern.success_rate >= 0.8 => {
                        let insight = Insight::new(
                            InsightCategory::Strategy,
                            format!("Strategy '{}' is highly effective", pattern.name),
                        )
                        .with_confidence(pattern.confidence)
                        .with_evidence(format!(
                            "{}% success rate over {} uses",
                            (pattern.success_rate * 100.0) as u32,
                            pattern.occurrences
                        ))
                        .with_recommendation("Prioritize using this strategy".to_string());
                        self.insights.push(insight);
                    }
                    PatternType::FailureMode => {
                        let insight = Insight::new(
                            InsightCategory::Reliability,
                            format!("Pattern '{}' is associated with failures", pattern.name),
                        )
                        .with_confidence(pattern.confidence)
                        .with_recommendation("Avoid conditions that trigger this pattern");
                        self.insights.push(insight);
                    }
                    _ => {}
                }
            }
        }
    }

    fn calculate_trend(&self) -> Trend {
        let window_size = self.config.trend_window_size;

        if self.outcomes.len() < window_size {
            return Trend::Insufficient;
        }

        let recent: Vec<&TaskOutcome> = self.outcomes.iter().rev().take(window_size).collect();

        let older: Vec<&TaskOutcome> = self
            .outcomes
            .iter()
            .rev()
            .skip(window_size)
            .take(window_size)
            .collect();

        if older.is_empty() {
            return Trend::Insufficient;
        }

        let recent_success_rate = recent
            .iter()
            .filter(|o| o.status == OutcomeStatus::Success)
            .count() as f64
            / recent.len() as f64;

        let older_success_rate = older
            .iter()
            .filter(|o| o.status == OutcomeStatus::Success)
            .count() as f64
            / older.len() as f64;

        let diff = recent_success_rate - older_success_rate;

        if diff > 0.1 {
            Trend::Improving
        } else if diff < -0.1 {
            Trend::Declining
        } else {
            Trend::Stable
        }
    }

    fn generate_feedback(&mut self) {
        self.feedback.clear();

        // Strategy recommendations
        for (strategy, stats) in &self.strategy_stats {
            if stats.total_uses >= self.config.min_pattern_occurrences {
                let success_rate = stats.successes as f64 / stats.total_uses as f64;

                if success_rate >= 0.8 {
                    self.feedback.push(Feedback {
                        feedback_type: FeedbackType::RecommendStrategy,
                        priority: ((success_rate * 100.0) as u32).min(100),
                        content: format!(
                            "Strategy '{}' has {}% success rate",
                            strategy,
                            (success_rate * 100.0) as u32
                        ),
                        apply_when: vec!["task_start".to_string()],
                        source: format!("pattern:strategy_success_{}", strategy),
                        confidence: (stats.total_uses as f64 / 10.0).min(1.0),
                    });
                } else if success_rate <= 0.3 {
                    self.feedback.push(Feedback {
                        feedback_type: FeedbackType::AvoidStrategy,
                        priority: ((1.0 - success_rate) * 100.0) as u32,
                        content: format!(
                            "Strategy '{}' has only {}% success rate - consider alternatives",
                            strategy,
                            (success_rate * 100.0) as u32
                        ),
                        apply_when: vec!["task_start".to_string()],
                        source: format!("pattern:strategy_failure_{}", strategy),
                        confidence: (stats.total_uses as f64 / 10.0).min(1.0),
                    });
                }
            }
        }

        // Tool recommendations
        for (tool, stats) in &self.tool_stats {
            if stats.total_uses >= self.config.min_pattern_occurrences {
                let success_rate = stats.in_successful_tasks as f64 / stats.total_uses as f64;

                if success_rate >= 0.8 {
                    self.feedback.push(Feedback {
                        feedback_type: FeedbackType::RecommendTool,
                        priority: 50,
                        content: format!("Tool '{}' is associated with successful tasks", tool),
                        apply_when: vec!["tool_selection".to_string()],
                        source: format!("pattern:tool_usage_{}", tool),
                        confidence: success_rate,
                    });
                }
            }
        }
    }

    /// Get feedback applicable to a given context
    pub fn get_applicable_feedback(&self, context: &str) -> Vec<&Feedback> {
        self.feedback
            .iter()
            .filter(|f| f.apply_when.iter().any(|c| c == context))
            .collect()
    }

    /// Get the most effective strategy
    pub fn get_best_strategy(&self) -> Option<(String, f64)> {
        self.strategy_stats
            .iter()
            .filter(|(_, stats)| stats.total_uses >= self.config.min_pattern_occurrences)
            .map(|(name, stats)| {
                let success_rate = stats.successes as f64 / stats.total_uses as f64;
                (name.clone(), success_rate)
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// Export learning data for persistence
    pub fn export(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Import learning data from persistence
    pub fn import(data: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(data)
    }

    /// Get summary statistics
    pub fn get_summary(&self) -> LearningSummary {
        let total_tasks = self.outcomes.len();
        let successful_tasks = self
            .outcomes
            .iter()
            .filter(|o| o.status == OutcomeStatus::Success)
            .count();

        LearningSummary {
            total_tasks,
            successful_tasks,
            failed_tasks: total_tasks - successful_tasks,
            patterns_identified: self.patterns.len(),
            insights_generated: self.insights.len(),
            feedback_items: self.feedback.len(),
            strategies_tracked: self.strategy_stats.len(),
            tools_tracked: self.tool_stats.len(),
        }
    }
}

impl Default for LearningSystem {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of learning system state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningSummary {
    pub total_tasks: usize,
    pub successful_tasks: usize,
    pub failed_tasks: usize,
    pub patterns_identified: usize,
    pub insights_generated: usize,
    pub feedback_items: usize,
    pub strategies_tracked: usize,
    pub tools_tracked: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_outcome_builder() {
        let outcome = TaskOutcome::success("task-1")
            .with_duration_secs(120)
            .with_iterations(5)
            .with_strategy("incremental")
            .with_agent("developer")
            .with_tool("grep")
            .with_metric("complexity", 0.8);

        assert_eq!(outcome.task_id, "task-1");
        assert_eq!(outcome.status, OutcomeStatus::Success);
        assert_eq!(outcome.duration_secs, Some(120));
        assert_eq!(outcome.iterations, Some(5));
        assert_eq!(outcome.strategy, Some("incremental".to_string()));
        assert!(outcome.agents_involved.contains(&"developer".to_string()));
        assert!(outcome.tools_used.contains(&"grep".to_string()));
        assert_eq!(outcome.metrics.get("complexity"), Some(&0.8));
    }

    #[test]
    fn test_record_outcome() {
        let mut learning = LearningSystem::new();

        learning.record_outcome(TaskOutcome::success("task-1").with_strategy("fast"));
        learning.record_outcome(TaskOutcome::success("task-2").with_strategy("fast"));
        learning.record_outcome(TaskOutcome::failure("task-3").with_strategy("slow"));

        assert_eq!(learning.outcomes.len(), 3);
        assert_eq!(learning.strategy_stats.get("fast").unwrap().total_uses, 2);
        assert_eq!(learning.strategy_stats.get("fast").unwrap().successes, 2);
        assert_eq!(learning.strategy_stats.get("slow").unwrap().successes, 0);
    }

    #[test]
    fn test_analyze_patterns() {
        let mut learning = LearningSystem::new();
        learning.config.min_pattern_occurrences = 2;

        // Record enough outcomes to identify patterns
        for i in 0..5 {
            learning.record_outcome(
                TaskOutcome::success(format!("task-{}", i))
                    .with_strategy("effective")
                    .with_tool("tool_a"),
            );
        }
        for i in 5..8 {
            learning.record_outcome(
                TaskOutcome::failure(format!("task-{}", i))
                    .with_strategy("ineffective")
                    .with_tool("tool_b"),
            );
        }

        let result = learning.analyze_patterns();

        assert!(result.success_rate > 0.5);
        assert!(!result.patterns.is_empty());
        assert!(result.top_strategies.iter().any(|(s, _)| s == "effective"));
    }

    #[test]
    fn test_pattern_observation() {
        let mut pattern =
            Pattern::new("test-pattern", "Test Pattern", PatternType::SuccessStrategy);

        pattern.record_observation("task-1", true);
        assert_eq!(pattern.occurrences, 1);
        assert!(pattern.success_rate > 0.0);

        pattern.record_observation("task-2", true);
        pattern.record_observation("task-3", false);

        assert_eq!(pattern.occurrences, 3);
        assert!(pattern.observed_in.contains(&"task-1".to_string()));
    }

    #[test]
    fn test_decision_builder() {
        let decision = Decision::new("Use caching")
            .with_rationale("Improves performance")
            .with_alternative("No caching")
            .with_outcome(DecisionOutcome::Positive);

        assert_eq!(decision.description, "Use caching");
        assert_eq!(decision.rationale, Some("Improves performance".to_string()));
        assert!(decision.alternatives.contains(&"No caching".to_string()));
        assert_eq!(decision.outcome, DecisionOutcome::Positive);
    }

    #[test]
    fn test_insight_builder() {
        let insight = Insight::new(InsightCategory::Performance, "System is slow")
            .with_confidence(0.9)
            .with_evidence("Average response time > 5s")
            .with_recommendation("Add caching");

        assert_eq!(insight.category, InsightCategory::Performance);
        assert_eq!(insight.confidence, 0.9);
        assert!(!insight.evidence.is_empty());
        assert!(!insight.recommendations.is_empty());
    }

    #[test]
    fn test_get_best_strategy() {
        let mut learning = LearningSystem::new();
        learning.config.min_pattern_occurrences = 2;

        // Strategy A: 100% success
        for i in 0..3 {
            learning.record_outcome(TaskOutcome::success(format!("a-{}", i)).with_strategy("A"));
        }

        // Strategy B: 50% success
        learning.record_outcome(TaskOutcome::success("b-1").with_strategy("B"));
        learning.record_outcome(TaskOutcome::failure("b-2").with_strategy("B"));

        let best = learning.get_best_strategy();
        assert!(best.is_some());
        let (name, rate) = best.unwrap();
        assert_eq!(name, "A");
        assert_eq!(rate, 1.0);
    }

    #[test]
    fn test_trend_calculation() {
        let mut learning = LearningSystem::new();
        learning.config.trend_window_size = 3;

        // First 3 tasks: all failures
        for i in 0..3 {
            learning.record_outcome(TaskOutcome::failure(format!("old-{}", i)));
        }

        // Next 3 tasks: all successes
        for i in 0..3 {
            learning.record_outcome(TaskOutcome::success(format!("new-{}", i)));
        }

        let result = learning.analyze_patterns();
        assert_eq!(result.trend, Trend::Improving);
    }

    #[test]
    fn test_feedback_generation() {
        let mut learning = LearningSystem::new();
        learning.config.min_pattern_occurrences = 2;

        // Create outcomes with good strategy
        for i in 0..5 {
            learning.record_outcome(
                TaskOutcome::success(format!("task-{}", i)).with_strategy("good_strategy"),
            );
        }

        learning.analyze_patterns();

        let feedback = learning.get_applicable_feedback("task_start");
        assert!(!feedback.is_empty());
        assert!(feedback
            .iter()
            .any(|f| f.feedback_type == FeedbackType::RecommendStrategy));
    }

    #[test]
    fn test_export_import() {
        let mut learning = LearningSystem::new();
        learning.record_outcome(TaskOutcome::success("task-1").with_strategy("test"));

        let exported = learning.export();
        let imported = LearningSystem::import(&exported).unwrap();

        assert_eq!(imported.outcomes.len(), 1);
        assert_eq!(imported.outcomes[0].task_id, "task-1");
    }

    #[test]
    fn test_summary() {
        let mut learning = LearningSystem::new();
        learning.config.min_pattern_occurrences = 1;

        learning.record_outcome(TaskOutcome::success("task-1").with_strategy("a"));
        learning.record_outcome(TaskOutcome::failure("task-2").with_strategy("b"));
        learning.analyze_patterns();

        let summary = learning.get_summary();
        assert_eq!(summary.total_tasks, 2);
        assert_eq!(summary.successful_tasks, 1);
        assert_eq!(summary.failed_tasks, 1);
        assert_eq!(summary.strategies_tracked, 2);
    }
}
