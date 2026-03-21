//! Convergence detector for the verification-agentic loop.
//!
//! Analyzes verification history across iterations to detect patterns that indicate
//! the AI is stuck, diverging, oscillating, or making no progress. Returns actionable
//! responses that the loop controller uses to adjust execution strategy.
//!
//! This module formalizes what was previously inline heuristics (stall counters,
//! flaky detection) into a proper pattern detection engine with configurable
//! thresholds and composable actions.
//!
//! ## Design Principles
//!
//! 1. **Actions are feedback, not termination.** A convergence action never kills
//!    the workflow — it adjusts the AI's context, model, or approach. The existing
//!    termination paths (max_iterations, unfixable_errors, critical_failure) remain
//!    the only ways a workflow ends.
//!
//! 2. **Patterns are detected from verification_history.** The loop controller
//!    already tracks per-step pass/fail across iterations. The convergence detector
//!    reads this data and returns a diagnosis.
//!
//! 3. **Actions compose.** Multiple patterns can fire simultaneously, and their
//!    actions are merged into a single `ConvergenceReport`.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Tunable thresholds for convergence detection.
///
/// These can be overridden per-workflow in the future. Defaults are chosen to
/// be conservative — better to detect a pattern late than to fire false positives.
#[derive(Debug, Clone)]
pub struct ConvergenceConfig {
    /// Number of consecutive iterations with identical failure sets before
    /// declaring "stuck". Default: 3.
    pub stuck_threshold: u32,

    /// Number of iterations over which total failures must not increase.
    /// If failures increase over this window, we declare "divergence". Default: 3.
    pub divergence_window: u32,

    /// Minimum number of alternations (pass→fail or fail→pass) in the last
    /// `oscillation_window` iterations to flag a step as oscillating. Default: 3.
    pub oscillation_min_flips: u32,

    /// Window size (iterations) for oscillation detection. Default: 4.
    pub oscillation_window: u32,

    /// Number of iterations where total failure count doesn't change (but the
    /// failing steps may differ) before declaring "plateau". Default: 4.
    pub plateau_threshold: u32,

    /// Maximum constraint retries within a single iteration before consuming
    /// the iteration and moving on. Default: 2.
    pub max_constraint_retries: u32,
}

impl Default for ConvergenceConfig {
    fn default() -> Self {
        Self {
            stuck_threshold: 3,
            divergence_window: 3,
            oscillation_min_flips: 3,
            oscillation_window: 4,
            plateau_threshold: 4,
            max_constraint_retries: 2,
        }
    }
}

// ---------------------------------------------------------------------------
// Patterns
// ---------------------------------------------------------------------------

/// A detected convergence pattern.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConvergencePattern {
    /// The exact same set of checks has failed for N consecutive iterations.
    /// The AI is making changes that don't affect the failing checks at all.
    Stuck {
        /// Number of consecutive identical-failure iterations.
        consecutive: u32,
        /// Names of the checks that keep failing.
        stuck_checks: Vec<String>,
    },

    /// Total failure count is increasing over the detection window.
    /// The AI's fixes are introducing new failures.
    Diverging {
        /// Failure counts over the window (oldest to newest).
        failure_trend: Vec<u32>,
    },

    /// Specific checks are flip-flopping between pass and fail.
    /// Indicates an unstable fix or a flaky test.
    Oscillating {
        /// Steps exhibiting the oscillation pattern.
        steps: Vec<String>,
    },

    /// Total failure count is stable but the failing checks are shifting.
    /// The AI is fixing one thing but breaking another.
    Plateau {
        /// Number of consecutive iterations at the same failure count.
        duration: u32,
        /// The stable failure count.
        failure_count: u32,
    },

    /// Making progress — failure count is strictly decreasing.
    /// This is the "no action needed" signal.
    Converging,
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// An action the loop controller should take in response to a convergence pattern.
///
/// Actions are **feedback**, not termination. They adjust the AI's next attempt
/// rather than killing the workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConvergenceAction {
    /// Inject additional context into the AI's failure prompt.
    /// This is the most common action — tell the AI what pattern was detected
    /// so it can adjust its approach.
    InjectContext {
        /// Markdown-formatted context to prepend to the failure context.
        context: String,
        /// Priority level for token budgeting (lower = higher priority).
        priority: u32,
    },

    /// Request model escalation for the agentic phase.
    /// The loop controller should update the routing context so that
    /// conditional routing rules can trigger a stronger model.
    EscalateModel {
        /// Human-readable reason for escalation.
        reason: String,
    },

    /// Force reflection mode for the next agentic iteration, even if
    /// it's not enabled at the workflow level.
    ForceReflection {
        /// Specific investigation prompt to inject.
        investigation_hint: String,
    },

    /// Suggest that the errors may be unfixable. The AI still gets a chance
    /// to try, but the context includes a strong hint to consider declaring
    /// unfixable if the pattern continues.
    SuggestUnfixable {
        /// Human-readable reason.
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// The output of a convergence analysis for a single iteration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergenceReport {
    /// The iteration this report was generated for.
    pub iteration: u32,

    /// All detected patterns (may be empty if converging normally).
    pub patterns: Vec<ConvergencePattern>,

    /// Recommended actions derived from the detected patterns.
    pub actions: Vec<ConvergenceAction>,

    /// Whether the detector considers the loop "healthy" (converging or early iterations).
    pub is_healthy: bool,

    /// Formatted context string ready for injection into the AI's failure prompt.
    /// Empty if no context injection is needed.
    pub context_injection: String,
}

impl ConvergenceReport {
    /// Returns true if model escalation was recommended.
    pub fn should_escalate_model(&self) -> bool {
        self.actions
            .iter()
            .any(|a| matches!(a, ConvergenceAction::EscalateModel { .. }))
    }

    /// Returns true if forced reflection was recommended.
    pub fn should_force_reflection(&self) -> bool {
        self.actions
            .iter()
            .any(|a| matches!(a, ConvergenceAction::ForceReflection { .. }))
    }

    /// Returns the reflection investigation hint, if any.
    pub fn reflection_hint(&self) -> Option<&str> {
        self.actions.iter().find_map(|a| match a {
            ConvergenceAction::ForceReflection {
                investigation_hint, ..
            } => Some(investigation_hint.as_str()),
            _ => None,
        })
    }

    /// Returns true if any non-healthy pattern was detected.
    pub fn has_concerns(&self) -> bool {
        !self.is_healthy
    }
}

// ---------------------------------------------------------------------------
// Detector State
// ---------------------------------------------------------------------------

/// Per-iteration snapshot used for trend analysis.
#[derive(Debug, Clone)]
struct IterationSnapshot {
    iteration: u32,
    failed_count: u32,
    failed_step_names: HashSet<String>,
}

/// The convergence detector. Maintains state across iterations and produces
/// a `ConvergenceReport` after each verification phase.
pub struct ConvergenceDetector {
    config: ConvergenceConfig,
    /// Snapshots of each iteration's verification results.
    snapshots: Vec<IterationSnapshot>,
}

impl ConvergenceDetector {
    pub fn new(config: ConvergenceConfig) -> Self {
        Self {
            config,
            snapshots: Vec::new(),
        }
    }

    /// Record a verification result and analyze convergence patterns.
    ///
    /// Call this after each verification phase completes (before the agentic phase).
    /// The returned `ConvergenceReport` tells the loop controller what actions to take.
    ///
    /// # Arguments
    /// * `iteration` - Current iteration number (1-indexed)
    /// * `verification_history` - Per-step pass/fail history across all iterations
    /// * `failed_step_names` - Names of steps that failed this iteration
    /// * `failed_count` - Total number of failed steps this iteration
    /// * `passed_count` - Total number of passed steps this iteration
    pub fn analyze(
        &mut self,
        iteration: u32,
        verification_history: &HashMap<String, Vec<bool>>,
        failed_step_names: HashSet<String>,
        failed_count: u32,
        _passed_count: u32,
    ) -> ConvergenceReport {
        // Record snapshot
        self.snapshots.push(IterationSnapshot {
            iteration,
            failed_count,
            failed_step_names: failed_step_names.clone(),
        });

        // Don't analyze first 2 iterations — not enough data for patterns
        if iteration <= 2 {
            debug!(
                "CONVERGENCE-DETECTOR: iteration {} — too early for pattern detection",
                iteration
            );
            return ConvergenceReport {
                iteration,
                patterns: vec![ConvergencePattern::Converging],
                actions: vec![],
                is_healthy: true,
                context_injection: String::new(),
            };
        }

        let mut patterns = Vec::new();
        let mut actions = Vec::new();

        // --- Pattern 1: Stuck detection ---
        if let Some(stuck) = self.detect_stuck(&failed_step_names) {
            info!(
                "CONVERGENCE-DETECTOR: STUCK pattern detected — {} checks failing for {} consecutive iterations: {:?}",
                stuck.1.len(), stuck.0, stuck.1
            );
            let stuck_checks = stuck.1.clone();
            let consecutive = stuck.0;

            patterns.push(ConvergencePattern::Stuck {
                consecutive,
                stuck_checks: stuck_checks.clone(),
            });

            // After stuck_threshold: inject strong context
            if consecutive >= self.config.stuck_threshold {
                actions.push(ConvergenceAction::InjectContext {
                    context: format!(
                        "### Convergence Warning: STUCK\n\n\
                        The following checks have failed identically for **{} consecutive iterations**:\n{}\n\n\
                        Your previous attempts did not affect these checks at all. \
                        You MUST take a fundamentally different approach:\n\
                        - Re-read the failing test/check code to understand exactly what it asserts\n\
                        - Trace the code path from the assertion back to the root cause\n\
                        - Consider whether the fix belongs in a different file or layer\n\
                        - If you've been editing the same file repeatedly, look elsewhere\n",
                        consecutive,
                        stuck_checks.iter().map(|s| format!("- `{}`", s)).collect::<Vec<_>>().join("\n"),
                    ),
                    priority: 0,
                });

                // Escalate model after stuck_threshold + 1
                if consecutive > self.config.stuck_threshold {
                    actions.push(ConvergenceAction::EscalateModel {
                        reason: format!(
                            "Stuck for {} iterations on: {}",
                            consecutive,
                            stuck_checks.join(", ")
                        ),
                    });
                }

                // Force reflection after stuck_threshold + 2
                if consecutive > self.config.stuck_threshold + 1 {
                    actions.push(ConvergenceAction::ForceReflection {
                        investigation_hint: format!(
                            "CRITICAL: You have been stuck on the same {} failing check(s) for {} iterations. \
                            Before attempting any fix, perform a thorough root-cause investigation:\n\
                            1. Read the FULL source of each failing check end-to-end\n\
                            2. Read the code being tested, not just the file you've been editing\n\
                            3. Check if the failure is in a dependency, configuration, or environment\n\
                            4. Document your root-cause hypothesis before writing any code",
                            stuck_checks.len(),
                            consecutive,
                        ),
                    });
                }

                // Suggest unfixable after stuck_threshold + 3
                if consecutive > self.config.stuck_threshold + 2 {
                    actions.push(ConvergenceAction::SuggestUnfixable {
                        reason: format!(
                            "Stuck on {} check(s) for {} iterations with no progress. \
                            Consider whether these failures are caused by an external dependency, \
                            environment issue, or constraint that cannot be resolved through code changes.",
                            stuck_checks.len(),
                            consecutive,
                        ),
                    });
                }
            }
        }

        // --- Pattern 2: Divergence detection ---
        if let Some(trend) = self.detect_divergence() {
            info!(
                "CONVERGENCE-DETECTOR: DIVERGING pattern detected — failure trend: {:?}",
                trend
            );
            patterns.push(ConvergencePattern::Diverging {
                failure_trend: trend.clone(),
            });

            actions.push(ConvergenceAction::InjectContext {
                context: format!(
                    "### Convergence Warning: DIVERGING\n\n\
                    Failure count is **increasing** over recent iterations: {}\n\n\
                    Your fixes are introducing new failures. Before making more changes:\n\
                    - Review what you changed in the last iteration\n\
                    - Check if your changes broke something that was previously passing\n\
                    - Consider reverting your last change and taking a more targeted approach\n\
                    - Focus on fixing ONE thing at a time rather than making broad changes\n",
                    trend
                        .iter()
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(" → "),
                ),
                priority: 0,
            });

            actions.push(ConvergenceAction::EscalateModel {
                reason: format!(
                    "Diverging: failures increasing over {} iterations ({} → {})",
                    trend.len(),
                    trend.first().unwrap_or(&0),
                    trend.last().unwrap_or(&0),
                ),
            });
        }

        // --- Pattern 3: Oscillation detection ---
        let oscillating_steps = self.detect_oscillation(verification_history);
        if !oscillating_steps.is_empty() {
            info!(
                "CONVERGENCE-DETECTOR: OSCILLATING pattern detected — {} steps flip-flopping: {:?}",
                oscillating_steps.len(),
                oscillating_steps
            );
            patterns.push(ConvergencePattern::Oscillating {
                steps: oscillating_steps.clone(),
            });

            actions.push(ConvergenceAction::InjectContext {
                context: format!(
                    "### Convergence Warning: OSCILLATING\n\n\
                    The following checks are **flip-flopping** between pass and fail across iterations:\n{}\n\n\
                    This usually means:\n\
                    - Your fix addresses the symptom but introduces a side effect that breaks on the next run\n\
                    - There's a shared dependency between these checks and the ones you're fixing\n\
                    - The test may have non-deterministic behavior (timing, ordering, external state)\n\n\
                    Investigate the root cause of the instability before attempting another fix.\n",
                    oscillating_steps.iter().map(|s| format!("- `{}`", s)).collect::<Vec<_>>().join("\n"),
                ),
                priority: 1,
            });
        }

        // --- Pattern 4: Plateau detection ---
        if let Some((duration, count)) = self.detect_plateau() {
            info!(
                "CONVERGENCE-DETECTOR: PLATEAU pattern detected — {} failures for {} consecutive iterations",
                count, duration
            );
            patterns.push(ConvergencePattern::Plateau {
                duration,
                failure_count: count,
            });

            actions.push(ConvergenceAction::InjectContext {
                context: format!(
                    "### Convergence Warning: PLATEAU\n\n\
                    The total failure count has been **stable at {}** for {} consecutive iterations, \
                    but the specific failing checks are changing.\n\n\
                    This means you're fixing one issue but introducing another — net zero progress. \
                    You need to:\n\
                    - Run the checks locally before committing to verify your fix doesn't break other checks\n\
                    - Look for shared state or dependencies between the checks\n\
                    - Consider whether the checks need to be fixed together as a group\n",
                    count, duration,
                ),
                priority: 1,
            });

            // Escalate if plateau persists beyond threshold + 2
            if duration >= self.config.plateau_threshold + 2 {
                actions.push(ConvergenceAction::EscalateModel {
                    reason: format!(
                        "Plateau: {} failures unchanged for {} iterations (fixing one, breaking another)",
                        count, duration,
                    ),
                });
            }
        }

        // If no concerning patterns detected, mark as converging
        if patterns.is_empty() {
            patterns.push(ConvergencePattern::Converging);
        }

        let is_healthy = patterns
            .iter()
            .all(|p| matches!(p, ConvergencePattern::Converging));

        // Build the combined context injection string
        let context_injection = self.build_context_injection(&actions);

        let report = ConvergenceReport {
            iteration,
            patterns,
            actions,
            is_healthy,
            context_injection,
        };

        if report.has_concerns() {
            warn!(
                "CONVERGENCE-DETECTOR: iteration {} — {} pattern(s) detected, {} action(s) recommended",
                iteration,
                report.patterns.len(),
                report.actions.len(),
            );
        } else {
            debug!(
                "CONVERGENCE-DETECTOR: iteration {} — converging normally",
                iteration
            );
        }

        report
    }

    // -----------------------------------------------------------------------
    // Pattern detection methods
    // -----------------------------------------------------------------------

    /// Detect if the same set of checks has been failing consecutively.
    /// Returns (consecutive_count, stuck_check_names) if stuck.
    fn detect_stuck(&self, current_failed: &HashSet<String>) -> Option<(u32, Vec<String>)> {
        if current_failed.is_empty() {
            return None;
        }

        let mut consecutive = 1u32;

        // Walk backwards through snapshots comparing failure sets
        for snapshot in self.snapshots.iter().rev().skip(1) {
            if snapshot.failed_step_names == *current_failed {
                consecutive += 1;
            } else {
                break;
            }
        }

        if consecutive >= 2 {
            let mut names: Vec<String> = current_failed.iter().cloned().collect();
            names.sort();
            Some((consecutive, names))
        } else {
            None
        }
    }

    /// Detect if failure count is strictly increasing over the divergence window.
    /// Returns the failure count trend if diverging.
    fn detect_divergence(&self) -> Option<Vec<u32>> {
        let window = self.config.divergence_window as usize;
        if self.snapshots.len() < window {
            return None;
        }

        let recent: Vec<u32> = self
            .snapshots
            .iter()
            .rev()
            .take(window)
            .map(|s| s.failed_count)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        // Check if strictly increasing
        let is_increasing = recent.windows(2).all(|w| w[1] > w[0]);

        if is_increasing {
            Some(recent)
        } else {
            None
        }
    }

    /// Detect checks that oscillate between pass and fail.
    /// Uses the full verification_history (not just snapshots) for per-step analysis.
    fn detect_oscillation(&self, verification_history: &HashMap<String, Vec<bool>>) -> Vec<String> {
        let window = self.config.oscillation_window as usize;
        let min_flips = self.config.oscillation_min_flips;

        let mut oscillating = Vec::new();

        for (name, results) in verification_history {
            if results.len() < window {
                continue;
            }

            let tail = &results[results.len() - window..];
            let alternations = tail.windows(2).filter(|w| w[0] != w[1]).count() as u32;

            if alternations >= min_flips {
                oscillating.push(name.clone());
            }
        }

        oscillating.sort();
        oscillating
    }

    /// Detect if failure count is stable but the specific failing checks differ.
    /// Returns (duration, failure_count) if plateau detected.
    fn detect_plateau(&self) -> Option<(u32, u32)> {
        if self.snapshots.len() < 3 {
            return None;
        }

        let current = self.snapshots.last()?;
        let current_count = current.failed_count;

        if current_count == 0 {
            return None;
        }

        let mut duration = 1u32;
        let mut has_different_sets = false;

        for snapshot in self.snapshots.iter().rev().skip(1) {
            if snapshot.failed_count == current_count {
                duration += 1;
                // Check if the actual failing steps differ (not just count)
                if snapshot.failed_step_names != current.failed_step_names {
                    has_different_sets = true;
                }
            } else {
                break;
            }
        }

        // Only report plateau if count is stable AND the sets are changing
        // (if sets are identical, that's a "stuck" pattern instead)
        if duration >= self.config.plateau_threshold && has_different_sets {
            Some((duration, current_count))
        } else {
            None
        }
    }

    // -----------------------------------------------------------------------
    // Context building
    // -----------------------------------------------------------------------

    /// Build a single context injection string from all InjectContext actions.
    fn build_context_injection(&self, actions: &[ConvergenceAction]) -> String {
        let mut sections: Vec<(u32, &str)> = Vec::new();

        for action in actions {
            if let ConvergenceAction::InjectContext { context, priority } = action {
                sections.push((*priority, context.as_str()));
            }
        }

        if sections.is_empty() {
            return String::new();
        }

        // Sort by priority (lower = first)
        sections.sort_by_key(|(p, _)| *p);

        let mut result = String::from("\n## Convergence Analysis\n\n");
        for (_, section) in sections {
            result.push_str(section);
            result.push('\n');
        }
        result
    }

    /// Get the current stall count (for backward compatibility with metrics emission).
    pub fn stall_count(&self) -> u32 {
        if self.snapshots.len() < 2 {
            return 0;
        }

        let mut count = 0u32;
        let mut iter = self.snapshots.iter().rev();
        let current = match iter.next() {
            Some(s) => s.failed_count,
            None => return 0,
        };

        for snapshot in iter {
            if snapshot.failed_count <= current {
                count += 1;
            } else {
                break;
            }
        }

        count
    }
}

// ---------------------------------------------------------------------------
// Resource Tracker
// ---------------------------------------------------------------------------

/// Configuration for resource limits.
///
/// All limits are optional — `None` means no limit for that resource.
/// When a limit is approached (within the warning threshold), the tracker
/// emits `InjectContext` actions. When exceeded, it emits stronger actions.
#[derive(Debug, Clone)]
pub struct ResourceLimits {
    /// Maximum wall-clock time for the entire workflow (seconds).
    pub max_wall_time_secs: Option<u64>,

    /// Maximum number of unique files modified across all iterations.
    /// Guards against scope creep where a focused bug fix turns into a refactor.
    pub max_files_modified: Option<u32>,

    /// Maximum number of agentic phase durations summed (milliseconds).
    /// Proxy for token/cost budget — longer sessions use more tokens.
    pub max_agentic_time_ms: Option<u64>,

    /// Warning threshold as a fraction (0.0-1.0). When resource usage
    /// exceeds this fraction of the limit, a warning is injected.
    /// Default: 0.75 (warn at 75% of limit).
    pub warning_threshold: f64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_wall_time_secs: None,
            max_files_modified: None,
            max_agentic_time_ms: None,
            warning_threshold: 0.75,
        }
    }
}

/// Tracks resource usage across iterations and produces feedback actions
/// when limits are approached or exceeded.
///
/// The resource tracker is separate from the convergence detector because
/// it operates on different data (time, file counts) rather than verification
/// history patterns. But its output uses the same `ConvergenceAction` type
/// so the loop controller can handle both uniformly.
pub struct ResourceTracker {
    limits: ResourceLimits,

    /// Workflow start time.
    workflow_start: std::time::Instant,

    /// Cumulative agentic phase time across all iterations (milliseconds).
    total_agentic_time_ms: u64,

    /// All unique file paths modified across all iterations.
    all_files_modified: HashSet<String>,

    /// Per-iteration file modification counts (for trend display).
    files_per_iteration: Vec<u32>,
}

impl ResourceTracker {
    pub fn new(limits: ResourceLimits, workflow_start: std::time::Instant) -> Self {
        Self {
            limits,
            workflow_start,
            total_agentic_time_ms: 0,
            all_files_modified: HashSet::new(),
            files_per_iteration: Vec::new(),
        }
    }

    /// Record the result of an agentic phase iteration.
    ///
    /// Call this after each agentic phase completes (before the next verification).
    ///
    /// # Arguments
    /// * `agentic_duration_ms` - How long the agentic phase took (milliseconds)
    /// * `files_modified` - File paths modified in this iteration
    pub fn record_iteration(&mut self, agentic_duration_ms: u64, files_modified: &[String]) {
        self.total_agentic_time_ms += agentic_duration_ms;
        for path in files_modified {
            self.all_files_modified.insert(path.clone());
        }
        self.files_per_iteration.push(files_modified.len() as u32);

        debug!(
            "RESOURCE-TRACKER: recorded iteration — agentic_time={}ms (total: {}ms), \
            files_this_iter={}, unique_files_total={}",
            agentic_duration_ms,
            self.total_agentic_time_ms,
            files_modified.len(),
            self.all_files_modified.len(),
        );
    }

    /// Check all resource limits and return any actions needed.
    ///
    /// Call this at the same point as `ConvergenceDetector::analyze` — after
    /// verification but before the next agentic phase. The wall time check is
    /// real-time so it doesn't need explicit recording.
    pub fn check_limits(&self, iteration: u32) -> Vec<ConvergenceAction> {
        let mut actions = Vec::new();

        // --- Wall time check ---
        if let Some(max_secs) = self.limits.max_wall_time_secs {
            let elapsed_secs = self.workflow_start.elapsed().as_secs();
            let ratio = elapsed_secs as f64 / max_secs as f64;

            if ratio >= 1.0 {
                warn!(
                    "RESOURCE-LIMIT: Wall time exceeded ({}s / {}s limit)",
                    elapsed_secs, max_secs
                );
                actions.push(ConvergenceAction::InjectContext {
                    context: format!(
                        "### Resource Limit: TIME EXCEEDED\n\n\
                        This workflow has been running for **{} seconds** (limit: {}s).\n\
                        You must complete your fix in this iteration. Focus on the single \
                        most impactful change and avoid broad refactoring.\n\
                        If you cannot fix the issue in this iteration, output `[UNFIXABLE_ERRORS]`.\n",
                        elapsed_secs, max_secs,
                    ),
                    priority: 0,
                });
                actions.push(ConvergenceAction::SuggestUnfixable {
                    reason: format!(
                        "Workflow time limit exceeded ({}s / {}s). \
                        Consider declaring unfixable if the fix requires more time.",
                        elapsed_secs, max_secs,
                    ),
                });
            } else if ratio >= self.limits.warning_threshold {
                info!(
                    "RESOURCE-WARNING: Wall time at {:.0}% ({}s / {}s limit)",
                    ratio * 100.0,
                    elapsed_secs,
                    max_secs
                );
                actions.push(ConvergenceAction::InjectContext {
                    context: format!(
                        "### Resource Warning: TIME\n\n\
                        This workflow has been running for **{} seconds** ({:.0}% of the {}s limit).\n\
                        Prioritize the most impactful fix and avoid unnecessary changes.\n",
                        elapsed_secs,
                        ratio * 100.0,
                        max_secs,
                    ),
                    priority: 2,
                });
            }
        }

        // --- Files modified check ---
        if let Some(max_files) = self.limits.max_files_modified {
            let file_count = self.all_files_modified.len() as u32;
            let ratio = file_count as f64 / max_files as f64;

            if ratio >= 1.0 {
                warn!(
                    "RESOURCE-LIMIT: Files modified exceeded ({} / {} limit)",
                    file_count, max_files
                );
                actions.push(ConvergenceAction::InjectContext {
                    context: format!(
                        "### Resource Limit: SCOPE EXCEEDED\n\n\
                        You have modified **{} unique files** across all iterations (limit: {}).\n\
                        This suggests scope creep — a focused fix should not touch this many files.\n\n\
                        **Do NOT modify any new files.** Only make changes to files you've already \
                        touched. If the fix genuinely requires more files, explain why.\n",
                        file_count, max_files,
                    ),
                    priority: 0,
                });
                actions.push(ConvergenceAction::ForceReflection {
                    investigation_hint: format!(
                        "SCOPE CHECK: You've modified {} files for this task (limit: {}). \
                        Before making any more changes, review whether all your modifications \
                        are truly necessary for the fix. List which files are essential and \
                        which could be reverted.",
                        file_count, max_files,
                    ),
                });
            } else if ratio >= self.limits.warning_threshold {
                info!(
                    "RESOURCE-WARNING: Files modified at {:.0}% ({} / {} limit)",
                    ratio * 100.0,
                    file_count,
                    max_files
                );
                actions.push(ConvergenceAction::InjectContext {
                    context: format!(
                        "### Resource Warning: SCOPE\n\n\
                        You have modified **{} unique files** ({:.0}% of the {} file limit).\n\
                        Be mindful of scope creep — keep changes focused on the specific issue.\n",
                        file_count,
                        ratio * 100.0,
                        max_files,
                    ),
                    priority: 2,
                });
            }
        }

        // --- Agentic time check ---
        if let Some(max_ms) = self.limits.max_agentic_time_ms {
            let ratio = self.total_agentic_time_ms as f64 / max_ms as f64;

            if ratio >= 1.0 {
                warn!(
                    "RESOURCE-LIMIT: Agentic time exceeded ({}ms / {}ms limit)",
                    self.total_agentic_time_ms, max_ms
                );
                actions.push(ConvergenceAction::InjectContext {
                    context: format!(
                        "### Resource Limit: AI TIME EXCEEDED\n\n\
                        Total AI processing time: **{}ms** (limit: {}ms).\n\
                        Make your response concise and focused. Apply the minimal fix needed.\n",
                        self.total_agentic_time_ms, max_ms,
                    ),
                    priority: 0,
                });
            } else if ratio >= self.limits.warning_threshold {
                actions.push(ConvergenceAction::InjectContext {
                    context: format!(
                        "### Resource Warning: AI TIME\n\n\
                        Total AI processing time: **{}ms** ({:.0}% of the {}ms limit).\n\
                        Keep responses concise to stay within budget.\n",
                        self.total_agentic_time_ms,
                        ratio * 100.0,
                        max_ms,
                    ),
                    priority: 2,
                });
            }
        }

        if !actions.is_empty() {
            info!(
                "RESOURCE-TRACKER: iteration {} — {} action(s) from resource checks",
                iteration,
                actions.len(),
            );
        }

        actions
    }

    /// Get the total number of unique files modified across all iterations.
    pub fn total_files_modified(&self) -> usize {
        self.all_files_modified.len()
    }

    /// Get all unique file paths modified across all iterations.
    pub fn files_modified(&self) -> Vec<String> {
        self.all_files_modified.iter().cloned().collect()
    }

    /// Get the total agentic phase time in milliseconds.
    pub fn total_agentic_time_ms(&self) -> u64 {
        self.total_agentic_time_ms
    }

    /// Get the wall time elapsed since workflow start in seconds.
    pub fn wall_time_secs(&self) -> u64 {
        self.workflow_start.elapsed().as_secs()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_history(data: &[(&str, &[bool])]) -> HashMap<String, Vec<bool>> {
        data.iter()
            .map(|(name, results)| (name.to_string(), results.to_vec()))
            .collect()
    }

    fn make_failed_set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_early_iterations_are_healthy() {
        let mut detector = ConvergenceDetector::new(ConvergenceConfig::default());

        let report = detector.analyze(1, &HashMap::new(), make_failed_set(&["check_a"]), 1, 2);
        assert!(report.is_healthy);
        assert!(report.context_injection.is_empty());

        let report = detector.analyze(2, &HashMap::new(), make_failed_set(&["check_a"]), 1, 2);
        assert!(report.is_healthy);
    }

    #[test]
    fn test_stuck_detection() {
        let mut detector = ConvergenceDetector::new(ConvergenceConfig {
            stuck_threshold: 3,
            ..Default::default()
        });
        let history = HashMap::new();
        let failed = make_failed_set(&["check_a", "check_b"]);

        // Iterations 1-2: too early
        detector.analyze(1, &history, failed.clone(), 2, 1);
        detector.analyze(2, &history, failed.clone(), 2, 1);

        // Iteration 3: now we have 3 consecutive identical failures
        let report = detector.analyze(3, &history, failed.clone(), 2, 1);
        assert!(!report.is_healthy);
        assert!(report
            .patterns
            .iter()
            .any(|p| matches!(p, ConvergencePattern::Stuck { consecutive: 3, .. })));
        assert!(!report.context_injection.is_empty());
        assert!(report.context_injection.contains("STUCK"));
    }

    #[test]
    fn test_stuck_resets_on_different_failures() {
        let mut detector = ConvergenceDetector::new(ConvergenceConfig {
            stuck_threshold: 3,
            ..Default::default()
        });
        let history = HashMap::new();

        detector.analyze(1, &history, make_failed_set(&["check_a"]), 1, 2);
        detector.analyze(2, &history, make_failed_set(&["check_a"]), 1, 2);
        // Different failure set breaks the streak
        detector.analyze(3, &history, make_failed_set(&["check_b"]), 1, 2);
        let report = detector.analyze(4, &history, make_failed_set(&["check_b"]), 1, 2);

        // Only 2 consecutive for check_b, below threshold
        let stuck_pattern = report
            .patterns
            .iter()
            .find(|p| matches!(p, ConvergencePattern::Stuck { .. }));
        match stuck_pattern {
            Some(ConvergencePattern::Stuck { consecutive, .. }) => {
                assert!(*consecutive < 3, "Should not have reached threshold");
            }
            _ => {} // No stuck pattern is also fine
        }
    }

    #[test]
    fn test_divergence_detection() {
        let mut detector = ConvergenceDetector::new(ConvergenceConfig {
            divergence_window: 3,
            ..Default::default()
        });
        let history = HashMap::new();

        // Strictly increasing failures
        detector.analyze(1, &history, make_failed_set(&["a"]), 1, 5);
        detector.analyze(2, &history, make_failed_set(&["a", "b"]), 2, 4);
        let report = detector.analyze(3, &history, make_failed_set(&["a", "b", "c"]), 3, 3);

        assert!(report
            .patterns
            .iter()
            .any(|p| matches!(p, ConvergencePattern::Diverging { .. })));
        assert!(report.context_injection.contains("DIVERGING"));
        assert!(report.should_escalate_model());
    }

    #[test]
    fn test_no_divergence_when_decreasing() {
        let mut detector = ConvergenceDetector::new(ConvergenceConfig {
            divergence_window: 3,
            ..Default::default()
        });
        let history = HashMap::new();

        detector.analyze(1, &history, make_failed_set(&["a", "b", "c"]), 3, 3);
        detector.analyze(2, &history, make_failed_set(&["a", "b"]), 2, 4);
        let report = detector.analyze(3, &history, make_failed_set(&["a"]), 1, 5);

        assert!(report.is_healthy);
    }

    #[test]
    fn test_oscillation_detection() {
        let mut detector = ConvergenceDetector::new(ConvergenceConfig {
            oscillation_window: 4,
            oscillation_min_flips: 3,
            ..Default::default()
        });

        // Build history with a flip-flopping step
        let history = make_history(&[
            ("stable_check", &[true, true, true, true]),
            ("flaky_check", &[true, false, true, false]),
        ]);

        // Need 3 iterations of snapshots for analyze to proceed
        detector.analyze(1, &history, make_failed_set(&["flaky_check"]), 1, 1);
        detector.analyze(2, &history, make_failed_set(&[]), 0, 2);
        let report = detector.analyze(3, &history, make_failed_set(&["flaky_check"]), 1, 1);

        assert!(report
            .patterns
            .iter()
            .any(|p| matches!(p, ConvergencePattern::Oscillating { .. })));
        assert!(report.context_injection.contains("OSCILLATING"));
    }

    #[test]
    fn test_plateau_detection() {
        let mut detector = ConvergenceDetector::new(ConvergenceConfig {
            plateau_threshold: 3,
            ..Default::default()
        });
        let history = HashMap::new();

        // Same failure count, different checks
        detector.analyze(1, &history, make_failed_set(&["a", "b"]), 2, 4);
        detector.analyze(2, &history, make_failed_set(&["b", "c"]), 2, 4);
        detector.analyze(3, &history, make_failed_set(&["c", "d"]), 2, 4);
        let report = detector.analyze(4, &history, make_failed_set(&["d", "e"]), 2, 4);

        assert!(report.patterns.iter().any(|p| matches!(
            p,
            ConvergencePattern::Plateau {
                failure_count: 2,
                ..
            }
        )));
        assert!(report.context_injection.contains("PLATEAU"));
    }

    #[test]
    fn test_plateau_not_triggered_for_identical_sets() {
        let mut detector = ConvergenceDetector::new(ConvergenceConfig {
            plateau_threshold: 3,
            stuck_threshold: 3,
            ..Default::default()
        });
        let history = HashMap::new();
        let same_set = make_failed_set(&["a", "b"]);

        // Same count AND same checks → stuck, not plateau
        detector.analyze(1, &history, same_set.clone(), 2, 4);
        detector.analyze(2, &history, same_set.clone(), 2, 4);
        detector.analyze(3, &history, same_set.clone(), 2, 4);
        let report = detector.analyze(4, &history, same_set.clone(), 2, 4);

        // Should be stuck, not plateau
        assert!(report
            .patterns
            .iter()
            .any(|p| matches!(p, ConvergencePattern::Stuck { .. })));
        assert!(!report
            .patterns
            .iter()
            .any(|p| matches!(p, ConvergencePattern::Plateau { .. })));
    }

    #[test]
    fn test_escalation_ladder() {
        let mut detector = ConvergenceDetector::new(ConvergenceConfig {
            stuck_threshold: 3,
            ..Default::default()
        });
        let history = HashMap::new();
        let failed = make_failed_set(&["check_a"]);

        // Build up iterations: 1 through 7
        for i in 1..=7 {
            let report = detector.analyze(i, &history, failed.clone(), 1, 2);

            if i <= 2 {
                // Too early
                assert!(report.is_healthy);
            } else if i == 3 {
                // Hit stuck_threshold (3): context injection
                assert!(!report.context_injection.is_empty());
                assert!(!report.should_escalate_model());
                assert!(!report.should_force_reflection());
            } else if i == 4 {
                // stuck_threshold + 1 (4): escalate model
                assert!(report.should_escalate_model());
                assert!(!report.should_force_reflection());
            } else if i == 5 {
                // stuck_threshold + 2 (5): force reflection
                assert!(report.should_escalate_model());
                assert!(report.should_force_reflection());
            } else if i >= 6 {
                // stuck_threshold + 3 (6+): suggest unfixable
                assert!(report
                    .actions
                    .iter()
                    .any(|a| matches!(a, ConvergenceAction::SuggestUnfixable { .. })));
            }
        }
    }

    #[test]
    fn test_converging_is_healthy() {
        let mut detector = ConvergenceDetector::new(ConvergenceConfig::default());
        let history = HashMap::new();

        // Decreasing failures each iteration
        detector.analyze(1, &history, make_failed_set(&["a", "b", "c"]), 3, 3);
        detector.analyze(2, &history, make_failed_set(&["a", "b"]), 2, 4);
        let report = detector.analyze(3, &history, make_failed_set(&["a"]), 1, 5);

        assert!(report.is_healthy);
        assert!(report.context_injection.is_empty());
        assert!(!report.should_escalate_model());
    }

    #[test]
    fn test_multiple_patterns_compose() {
        let mut detector = ConvergenceDetector::new(ConvergenceConfig {
            stuck_threshold: 3,
            oscillation_window: 4,
            oscillation_min_flips: 3,
            ..Default::default()
        });

        // History with a flaky step
        let history = make_history(&[
            ("stuck_check", &[false, false, false, false]),
            ("flaky_check", &[true, false, true, false]),
        ]);

        let failed = make_failed_set(&["stuck_check", "flaky_check"]);

        detector.analyze(1, &history, failed.clone(), 2, 1);
        detector.analyze(2, &history, failed.clone(), 2, 1);
        let report = detector.analyze(3, &history, failed.clone(), 2, 1);

        // Both stuck and oscillating should be detected
        assert!(report
            .patterns
            .iter()
            .any(|p| matches!(p, ConvergencePattern::Stuck { .. })));
        assert!(report
            .patterns
            .iter()
            .any(|p| matches!(p, ConvergencePattern::Oscillating { .. })));
        assert!(report.context_injection.contains("STUCK"));
        assert!(report.context_injection.contains("OSCILLATING"));
    }

    // -----------------------------------------------------------------------
    // Resource tracker tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_resource_tracker_no_limits() {
        let tracker = ResourceTracker::new(ResourceLimits::default(), std::time::Instant::now());
        let actions = tracker.check_limits(1);
        assert!(actions.is_empty(), "No limits set → no actions");
    }

    #[test]
    fn test_resource_tracker_files_warning() {
        let mut tracker = ResourceTracker::new(
            ResourceLimits {
                max_files_modified: Some(10),
                warning_threshold: 0.75,
                ..Default::default()
            },
            std::time::Instant::now(),
        );

        // Record 8 unique files (80% of limit = above 75% threshold)
        let files: Vec<String> = (0..8).map(|i| format!("src/file_{}.rs", i)).collect();
        tracker.record_iteration(1000, &files);

        let actions = tracker.check_limits(1);
        assert!(!actions.is_empty(), "Should warn at 80% of file limit");
        assert!(actions.iter().any(|a| matches!(
            a,
            ConvergenceAction::InjectContext { context, .. } if context.contains("SCOPE")
        )));
    }

    #[test]
    fn test_resource_tracker_files_exceeded() {
        let mut tracker = ResourceTracker::new(
            ResourceLimits {
                max_files_modified: Some(5),
                ..Default::default()
            },
            std::time::Instant::now(),
        );

        // Record 6 unique files (exceeds limit of 5)
        let files: Vec<String> = (0..6).map(|i| format!("src/file_{}.rs", i)).collect();
        tracker.record_iteration(1000, &files);

        let actions = tracker.check_limits(1);
        // Should have both InjectContext and ForceReflection
        assert!(actions.iter().any(|a| matches!(
            a,
            ConvergenceAction::InjectContext { context, .. } if context.contains("SCOPE EXCEEDED")
        )));
        assert!(actions
            .iter()
            .any(|a| matches!(a, ConvergenceAction::ForceReflection { .. })));
    }

    #[test]
    fn test_resource_tracker_deduplicates_files() {
        let mut tracker = ResourceTracker::new(
            ResourceLimits {
                max_files_modified: Some(10),
                ..Default::default()
            },
            std::time::Instant::now(),
        );

        // Record the same files across multiple iterations
        let files = vec!["src/main.rs".to_string(), "src/lib.rs".to_string()];
        tracker.record_iteration(500, &files);
        tracker.record_iteration(500, &files);
        tracker.record_iteration(500, &files);

        assert_eq!(
            tracker.total_files_modified(),
            2,
            "Should deduplicate files"
        );
    }

    #[test]
    fn test_resource_tracker_accumulates_time() {
        let mut tracker = ResourceTracker::new(
            ResourceLimits {
                max_agentic_time_ms: Some(10000),
                warning_threshold: 0.75,
                ..Default::default()
            },
            std::time::Instant::now(),
        );

        tracker.record_iteration(3000, &[]);
        tracker.record_iteration(3000, &[]);
        tracker.record_iteration(3000, &[]);

        assert_eq!(tracker.total_agentic_time_ms(), 9000);

        let actions = tracker.check_limits(3);
        assert!(!actions.is_empty(), "Should warn at 90% of time limit");
    }

    #[test]
    fn test_resource_tracker_agentic_time_exceeded() {
        let mut tracker = ResourceTracker::new(
            ResourceLimits {
                max_agentic_time_ms: Some(5000),
                ..Default::default()
            },
            std::time::Instant::now(),
        );

        tracker.record_iteration(3000, &[]);
        tracker.record_iteration(3000, &[]);

        let actions = tracker.check_limits(2);
        assert!(actions.iter().any(|a| matches!(
            a,
            ConvergenceAction::InjectContext { context, .. } if context.contains("AI TIME EXCEEDED")
        )));
    }
}
