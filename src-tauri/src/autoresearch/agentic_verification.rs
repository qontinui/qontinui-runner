//! Agentic Verification Loop — an alternative workflow architecture for autoresearch.
//!
//! Instead of a fixed workflow with deterministic verification steps and agentic fix steps,
//! this architecture uses a continuous loop of two agents:
//!
//! 1. **Worker Agent** — executes one focused action toward the goal (navigate, click,
//!    type, run command, modify code, etc.)
//! 2. **Verification Agent** — observes the current state, validates whether the worker's
//!    action achieved its intent, and reports status + next priority to the next iteration.
//!
//! ```text
//! loop {
//!     1. Verification Agent assesses current state against goal
//!        → status (pass/partial/fail), observations, next_priority
//!     2. If pass → exit loop (success)
//!     3. Worker Agent receives observations + next_priority
//!        → executes one focused action
//!     4. Check max_iterations / stop signal
//! }
//! ```
//!
//! ## Design Rationale
//!
//! The traditional architecture requires pre-defined verification steps that anticipate
//! every possible failure mode. When the environment diverges from expectations (unexpected
//! UI, changed layouts, novel error states), deterministic checks break silently.
//!
//! The agentic verification approach replaces those checks with a reasoning agent that can:
//! - Assess whether the *intent* was achieved, not just whether specific assertions pass
//! - Adapt to novel states without workflow changes
//! - Provide rich, contextual feedback to the worker agent
//! - Gracefully handle environment drift (e.g., "button moved but action still completed")
//!
//! ## Tradeoffs vs Traditional
//!
//! | Aspect               | Traditional (Deterministic) | Agentic Verification      |
//! |----------------------|-----------------------------|---------------------------|
//! | Reliability          | High (no hallucination)     | Depends on verifier model |
//! | Adaptability         | Low (brittle to change)     | High (reasons about intent)|
//! | Cost per iteration   | Low (fast checks)           | Higher (LLM call)         |
//! | Authoring effort     | High (craft each check)     | Low (describe the goal)   |
//! | Convergence signal   | Binary (pass/fail)          | Rich (partial progress)   |

use serde::{Deserialize, Serialize};

/// The workflow execution architecture to use.
///
/// This is a first-class search dimension in autoresearch, allowing direct comparison
/// between traditional deterministic verification and agentic verification approaches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowArchitecture {
    /// Traditional: Setup → [Deterministic Verification ↔ Agentic Fix]* → Completion.
    /// Pre-defined verification steps run deterministically; agentic phase fixes failures.
    #[default]
    Traditional,
    /// Agentic Verification: [Verification Agent → Worker Agent]* loop.
    /// No pre-defined verification steps — a verification agent reasons about success.
    AgenticVerification,
    /// Multi-Agent Pipeline: Specialized agents in a DAG-structured pipeline.
    ///
    /// Instead of a single worker fixing all failures, this architecture decomposes
    /// spec fulfillment into phases handled by specialized agents:
    ///
    /// 1. **Spec Analyst** — parses specs into acceptance criteria with dependency ordering
    /// 2. **DAG Builder** — (deterministic) creates an execution DAG from criteria dependencies
    /// 3. **Snapshot Agent** — captures and normalizes UI state via UI Bridge
    /// 4. **Locator Agent** — maps criteria to code locations (files, components)
    /// 5. **Implementer Agent(s)** — makes code changes for assigned DAG subtrees (parallel)
    /// 6. **Verifier Agent(s)** — verifies criteria after each implementer (1:1, isolated)
    ///
    /// Each agent is independently configurable and measurable via autoresearch,
    /// enabling per-agent optimization instead of whole-loop tuning.
    MultiAgentPipeline,
}

impl std::fmt::Display for WorkflowArchitecture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Traditional => write!(f, "traditional"),
            Self::AgenticVerification => write!(f, "agentic_verification"),
            Self::MultiAgentPipeline => write!(f, "multi_agent_pipeline"),
        }
    }
}

/// Configuration for the verification agent in the agentic verification loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationAgentConfig {
    /// Model to use for the verification agent (None = use workflow default).
    /// A smaller/faster model is often sufficient for verification.
    #[serde(default)]
    pub model: Option<String>,

    /// Provider override for the verification agent.
    #[serde(default)]
    pub provider: Option<String>,

    /// System prompt preamble for the verification agent.
    /// Injected before the goal description. Use this to tune verification behavior
    /// (e.g., "Be strict about visual layout matching" or "Focus on functional correctness").
    #[serde(default)]
    pub system_preamble: Option<String>,

    /// Maximum tokens for the verification agent's response.
    #[serde(default = "default_verifier_max_tokens")]
    pub max_tokens: u32,

    /// Whether the verification agent can request screenshots/snapshots.
    /// When true, the verifier receives the current UI state as context.
    #[serde(default = "default_true")]
    pub use_screenshots: bool,

    /// Whether to include console errors/logs in verifier context.
    #[serde(default = "default_true")]
    pub include_console_errors: bool,

    /// Whether to include app health status in verifier context.
    #[serde(default = "default_true")]
    pub include_app_health: bool,
}

fn default_verifier_max_tokens() -> u32 {
    2048
}

fn default_true() -> bool {
    true
}

impl Default for VerificationAgentConfig {
    fn default() -> Self {
        Self {
            model: None,
            provider: None,
            system_preamble: None,
            max_tokens: default_verifier_max_tokens(),
            use_screenshots: true,
            include_console_errors: true,
            include_app_health: true,
        }
    }
}

/// Configuration for the worker agent in the agentic verification loop.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkerAgentConfig {
    /// Model to use for the worker agent (None = use workflow default).
    #[serde(default)]
    pub model: Option<String>,

    /// Provider override for the worker agent.
    #[serde(default)]
    pub provider: Option<String>,

    /// Whether the worker should execute only one focused action per iteration,
    /// or may take multiple actions in sequence.
    ///
    /// Single-action mode produces more granular verification at higher cost.
    /// Multi-action mode is faster but verification covers more changes at once.
    #[serde(default)]
    pub single_action_mode: bool,
}

/// Full configuration for the agentic verification loop architecture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticVerificationConfig {
    /// The goal to achieve, described in natural language.
    /// The verification agent judges success against this goal.
    /// When empty, falls back to the workflow's base_prompt.
    #[serde(default)]
    pub goal: String,

    /// Configuration for the verification agent.
    #[serde(default)]
    pub verifier: VerificationAgentConfig,

    /// Configuration for the worker agent.
    #[serde(default)]
    pub worker: WorkerAgentConfig,

    /// Maximum iterations (worker + verifier = 1 iteration).
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,

    /// Whether to run the verification agent first (before any worker action).
    /// Useful to establish a baseline understanding of the current state.
    #[serde(default = "default_true")]
    pub verify_first: bool,

    /// Confidence threshold (0.0-1.0) at which the verification agent's "pass"
    /// judgment is accepted. Lower values allow earlier exit but risk false positives.
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,

    /// Number of consecutive "pass" verdicts required before exiting.
    /// Setting this > 1 reduces false-positive exits at the cost of extra iterations.
    #[serde(default = "default_consecutive_passes")]
    pub required_consecutive_passes: u32,
}

fn default_max_iterations() -> u32 {
    10
}

fn default_confidence_threshold() -> f64 {
    0.6
}

fn default_consecutive_passes() -> u32 {
    1
}

impl Default for AgenticVerificationConfig {
    fn default() -> Self {
        Self {
            goal: String::new(),
            verifier: VerificationAgentConfig::default(),
            worker: WorkerAgentConfig::default(),
            max_iterations: default_max_iterations(),
            verify_first: true,
            confidence_threshold: default_confidence_threshold(),
            required_consecutive_passes: default_consecutive_passes(),
        }
    }
}

/// The structured output from the verification agent after assessing the current state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationVerdict {
    /// Overall status of the verification.
    pub status: VerificationStatus,

    /// Confidence in this verdict (0.0 = no confidence, 1.0 = certain).
    pub confidence: f64,

    /// Free-form observations about the current state.
    /// Passed to the worker agent as context for its next action.
    pub observations: String,

    /// What the worker agent should focus on next.
    /// Only relevant when status is not Pass.
    pub next_priority: Option<String>,

    /// Specific issues found during verification.
    pub issues: Vec<VerificationIssue>,

    /// Whether the verifier believes the goal is unreachable from the current state.
    #[serde(default)]
    pub unreachable: bool,

    /// Optional reason why the goal is unreachable.
    #[serde(default)]
    pub unreachable_reason: Option<String>,
}

/// Status reported by the verification agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    /// Goal fully achieved — exit the loop.
    Pass,
    /// Some progress made, but goal not yet achieved.
    Partial,
    /// No progress or regression — worker needs to change approach.
    Fail,
    /// The goal appears unreachable from the current state.
    Unreachable,
}

impl std::fmt::Display for VerificationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pass => write!(f, "pass"),
            Self::Partial => write!(f, "partial"),
            Self::Fail => write!(f, "fail"),
            Self::Unreachable => write!(f, "unreachable"),
        }
    }
}

/// A specific issue found during agentic verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationIssue {
    /// What went wrong or is missing.
    pub description: String,
    /// Severity: "critical", "warning", "info".
    #[serde(default = "default_severity")]
    pub severity: String,
    /// Optional suggestion for the worker agent.
    pub suggestion: Option<String>,
}

fn default_severity() -> String {
    "warning".to_string()
}

/// Result of one iteration of the agentic verification loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticVerificationIterationResult {
    /// Iteration number (1-indexed).
    pub iteration: u32,
    /// The verification verdict for this iteration.
    pub verdict: VerificationVerdict,
    /// Whether the worker agent ran in this iteration.
    pub worker_ran: bool,
    /// Summary of what the worker agent did (if it ran).
    pub worker_summary: Option<String>,
    /// Duration of the verification agent call in ms.
    pub verifier_duration_ms: u64,
    /// Duration of the worker agent call in ms (0 if worker didn't run).
    pub worker_duration_ms: u64,
}

/// Final result of the agentic verification loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticVerificationResult {
    /// Total iterations executed.
    pub iterations_run: u32,
    /// Whether the goal was achieved (verification passed).
    pub goal_achieved: bool,
    /// Whether the loop exited because the goal was deemed unreachable.
    pub unreachable: bool,
    /// Whether the loop was stopped externally.
    pub was_stopped: bool,
    /// Whether max iterations were reached.
    pub max_iterations_reached: bool,
    /// Per-iteration results.
    pub iteration_results: Vec<AgenticVerificationIterationResult>,
    /// The final verification verdict (from the last iteration).
    pub final_verdict: Option<VerificationVerdict>,
}

impl AgenticVerificationResult {
    /// Convert to a LoopResult for compatibility with the unified workflow executor.
    pub fn to_loop_result(&self) -> crate::unified_workflow_executor::LoopResult {
        crate::unified_workflow_executor::LoopResult {
            iterations_run: self.iterations_run,
            verification_passed: self.goal_achieved,
            max_iterations_reached: self.max_iterations_reached,
            critical_failure: self.unreachable,
            was_stopped: self.was_stopped,
            unfixable_errors: self.unreachable,
            iteration_results: self
                .iteration_results
                .iter()
                .map(|ir| crate::unified_workflow_executor::IterationResult {
                    iteration: ir.iteration,
                    verification_passed: ir.verdict.status == VerificationStatus::Pass,
                    critical_failure: ir.verdict.status == VerificationStatus::Unreachable,
                    passed_checks: if ir.verdict.status == VerificationStatus::Pass {
                        1
                    } else {
                        0
                    },
                    failed_checks: if ir.verdict.status == VerificationStatus::Pass {
                        0
                    } else {
                        ir.verdict.issues.len()
                    },
                    failure_context: ir.verdict.observations.clone(),
                    agentic_phase_ran: ir.worker_ran,
                    agentic_phase_success: if ir.worker_ran {
                        Some(ir.verdict.status != VerificationStatus::Fail)
                    } else {
                        None
                    },
                })
                .collect(),
            total_tokens: None,
            total_cost_usd: None,
        }
    }

    /// Get a human-readable summary.
    pub fn summary(&self) -> String {
        if self.was_stopped {
            format!("STOPPED by user after {} iteration(s)", self.iterations_run)
        } else if self.goal_achieved {
            format!("Goal ACHIEVED after {} iteration(s)", self.iterations_run)
        } else if self.unreachable {
            format!(
                "Goal deemed UNREACHABLE after {} iteration(s)",
                self.iterations_run
            )
        } else if self.max_iterations_reached {
            format!(
                "Max iterations ({}) reached — goal not achieved",
                self.iterations_run
            )
        } else {
            format!("Loop ended after {} iteration(s)", self.iterations_run)
        }
    }
}

/// Prompt templates for the verification and worker agents.
pub struct AgenticVerificationPrompts;

impl AgenticVerificationPrompts {
    /// Build the verification agent's system prompt.
    pub fn verifier_system_prompt(goal: &str, preamble: Option<&str>) -> String {
        let mut prompt = String::new();

        if let Some(pre) = preamble {
            prompt.push_str(pre);
            prompt.push_str("\n\n");
        }

        prompt.push_str(&format!(
            r#"You are a verification agent. Your job is to assess whether the following goal has been achieved:

<goal>
{}
</goal>

Examine the current state carefully and respond with a JSON object:

```json
{{
  "status": "pass" | "partial" | "fail" | "unreachable",
  "confidence": 0.0-1.0,
  "observations": "What you observe about the current state",
  "next_priority": "What the worker should focus on next (omit if pass)",
  "issues": [
    {{
      "description": "What went wrong or is missing",
      "severity": "critical" | "warning" | "info",
      "suggestion": "Optional suggestion for the worker"
    }}
  ],
  "unreachable": false,
  "unreachable_reason": null
}}
```

Rules:
- "pass" means the goal is fully achieved. Only use this when you are confident.
- "partial" means some progress has been made but more work is needed.
- "fail" means no meaningful progress, or a regression from previous state.
- "unreachable" means the goal cannot be achieved from the current state.
- Be specific in observations — the worker agent relies on your feedback.
- Report your true confidence level. Don't inflate it.
- Focus on whether the INTENT of the goal was achieved, not just surface-level checks."#,
            goal
        ));

        prompt
    }

    /// Build the worker agent's context from the verification verdict.
    pub fn worker_context_from_verdict(
        goal: &str,
        verdict: &VerificationVerdict,
        iteration: u32,
        max_iterations: u32,
    ) -> String {
        let mut context = format!(
            "## Goal\n{}\n\n## Verification Feedback (iteration {}/{})\n\nStatus: {}\nConfidence: {:.0}%\n\nObservations:\n{}\n",
            goal,
            iteration,
            max_iterations,
            verdict.status,
            verdict.confidence * 100.0,
            verdict.observations,
        );

        if let Some(ref priority) = verdict.next_priority {
            context.push_str(&format!("\n## Next Priority\n{}\n", priority));
        }

        if !verdict.issues.is_empty() {
            context.push_str("\n## Issues Found\n");
            for (i, issue) in verdict.issues.iter().enumerate() {
                context.push_str(&format!(
                    "{}. [{}] {}\n",
                    i + 1,
                    issue.severity,
                    issue.description
                ));
                if let Some(ref suggestion) = issue.suggestion {
                    context.push_str(&format!("   Suggestion: {}\n", suggestion));
                }
            }
        }

        let remaining = max_iterations - iteration;
        if remaining <= 2 {
            context.push_str(&format!(
                "\n⚠️ Only {} iteration(s) remaining. Focus on the most critical action.\n",
                remaining
            ));
        }

        context
    }
}

/// Parse a structured VerificationVerdict from AI output.
/// Looks for a JSON block in the output matching the verdict schema.
pub fn parse_verification_verdict(output: &str) -> Option<VerificationVerdict> {
    // Try to find JSON block (with or without markdown code fences)
    let json_str = if let Some(start) = output.find("```json") {
        let content_start = start + 7;
        let end = output[content_start..]
            .find("```")
            .map(|e| content_start + e)?;
        &output[content_start..end]
    } else if let Some(start) = output.find("```") {
        let content_start = start + 3;
        // Skip optional language tag on same line
        let line_end = output[content_start..]
            .find('\n')
            .map(|e| content_start + e + 1)
            .unwrap_or(content_start);
        let end = output[line_end..].find("```").map(|e| line_end + e)?;
        &output[line_end..end]
    } else {
        // Try to find raw JSON object
        let start = output.find('{')?;
        let mut depth = 0;
        let mut end = start;
        for (i, ch) in output[start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        if depth != 0 {
            return None;
        }
        &output[start..end]
    };

    serde_json::from_str::<VerificationVerdict>(json_str.trim()).ok()
}

/// Heuristic verdict parsing when structured JSON is not available.
/// Uses keyword analysis to approximate the verification status.
pub fn heuristic_verdict(output: &str) -> VerificationVerdict {
    let lower = output.to_lowercase();

    let status = if lower.contains("goal achieved")
        || lower.contains("all checks pass")
        || lower.contains("all checks passing")
        || lower.contains("all verification checks pass")
        || lower.contains("all verification checks are passing")
        || lower.contains("verification checks passing")
        || lower.contains("successfully completed")
        || lower.contains("\"status\": \"pass\"")
        || lower.contains("status: pass")
        || lower.contains("all_passed: true")
        || lower.contains("\"all_passed\": true")
        || lower.contains("\"all_passed\":true")
        || lower.contains("no action needed")
        || lower.contains("no further action")
        || lower.contains("no fixes needed")
        || lower.contains("0 failures")
        || lower.contains("already passing")
        || lower.contains("already fixed")
    {
        VerificationStatus::Pass
    } else if lower.contains("unreachable")
        || lower.contains("impossible")
        || lower.contains("cannot be achieved")
    {
        VerificationStatus::Unreachable
    } else if lower.contains("progress")
        || lower.contains("partial")
        || lower.contains("some improvement")
    {
        VerificationStatus::Partial
    } else {
        VerificationStatus::Fail
    };

    // Count how many pass-related keywords match for confidence scaling.
    // Multiple matching keywords indicate higher certainty of a pass.
    let pass_keyword_count = [
        "goal achieved",
        "all checks pass",
        "all checks passing",
        "all verification checks pass",
        "verification checks passing",
        "successfully completed",
        "all_passed: true",
        "\"all_passed\": true",
        "0 failures",
        "no action needed",
        "already passing",
        "already fixed",
    ]
    .iter()
    .filter(|kw| lower.contains(**kw))
    .count();

    let confidence = match status {
        VerificationStatus::Pass => {
            // Scale confidence by number of matching keywords:
            // 1 keyword  → 0.8 (meets default threshold)
            // 2 keywords → 0.85
            // 3+ keywords → 0.9
            if pass_keyword_count >= 3 {
                0.9
            } else if pass_keyword_count >= 2 {
                0.85
            } else {
                0.8
            }
        }
        VerificationStatus::Unreachable => 0.5,
        VerificationStatus::Partial => 0.5,
        VerificationStatus::Fail => 0.5,
    };

    VerificationVerdict {
        status,
        confidence,
        observations: output.chars().take(1000).collect(),
        next_priority: None,
        issues: vec![],
        unreachable: false,
        unreachable_reason: None,
    }
}

// ── Multi-Agent Pipeline Types ──────────────────────────────────────────────

/// Configuration for an individual agent within the multi-agent pipeline.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PipelineAgentConfig {
    /// Model to use for this agent (None = use workflow default).
    #[serde(default)]
    pub model: Option<String>,

    /// Provider override for this agent.
    #[serde(default)]
    pub provider: Option<String>,

    /// Maximum tokens for this agent's response.
    #[serde(default)]
    pub max_tokens: Option<u32>,

    /// Named prompt variant for this agent (allows A/B testing prompt strategies).
    /// Maps to prompt templates registered in the pipeline executor.
    #[serde(default)]
    pub prompt_variant: Option<String>,

    /// Temperature override (0.0-1.0). Lower = more deterministic.
    #[serde(default)]
    pub temperature: Option<f64>,
}

// ── Handoff & Guardrail Types (inspired by openai-agents-python) ─────────────

/// Typed context passed between pipeline agents during handoffs.
/// Formalizes inter-agent data transfer with schema validation support.
/// Mirrors openai-agents-python's `HandoffInputData` pattern.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HandoffContext {
    /// Source agent type (e.g., "locator").
    #[serde(default)]
    pub from_agent: String,
    /// Target agent type (e.g., "implementer").
    #[serde(default)]
    pub to_agent: String,
    /// Structured payload — schema depends on the handoff pair.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Items from the source agent's conversation history to forward.
    #[serde(default)]
    pub forwarded_items: Vec<String>,
    /// Whether this handoff passed guardrail validation.
    #[serde(default)]
    pub validated: bool,
}

/// Result of a guardrail check on agent input or output.
/// Mirrors openai-agents-python's `GuardrailFunctionOutput` with tripwire semantics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardrailResult {
    /// Name of the guardrail that ran.
    pub guardrail_name: String,
    /// Whether this guardrail tripped (halts pipeline progression if true).
    pub tripwire_triggered: bool,
    /// Human-readable explanation of what was checked.
    pub check_description: String,
    /// Optional structured metadata about the check.
    #[serde(default)]
    pub output_info: Option<serde_json::Value>,
}

// ── Built-in Guardrail Functions ─────────────────────────────────────────────

/// Validates locator agent output has the expected structure.
/// Trips if the output is not valid JSON or lacks criteria_locations.
pub fn guardrail_locator_output_schema(output: &serde_json::Value) -> GuardrailResult {
    let has_locations = output.get("located_criteria_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) > 0;

    GuardrailResult {
        guardrail_name: "locator_output_schema".to_string(),
        tripwire_triggered: !has_locations,
        check_description: if has_locations {
            "Locator produced criteria-to-code mappings".to_string()
        } else {
            "Locator produced no criteria-to-code mappings — implementer will receive no guidance".to_string()
        },
        output_info: None,
    }
}

/// Validates verifier output contains a parseable pass/fail verdict.
pub fn guardrail_verifier_verdict(output: &serde_json::Value) -> GuardrailResult {
    let has_verdict = output.get("passed").is_some()
        || output.get("all_passed").is_some();

    GuardrailResult {
        guardrail_name: "verifier_verdict_parseable".to_string(),
        tripwire_triggered: !has_verdict,
        check_description: if has_verdict {
            "Verifier produced a clear pass/fail verdict".to_string()
        } else {
            "Verifier output lacks pass/fail verdict — cannot determine success".to_string()
        },
        output_info: None,
    }
}

/// Guards against exceeding the remaining token budget before an agent call.
pub fn guardrail_token_budget(
    tokens_used: u64,
    max_tokens: u64,
    agent_min_requirement: u64,
) -> GuardrailResult {
    let remaining = max_tokens.saturating_sub(tokens_used);
    let sufficient = remaining >= agent_min_requirement;

    GuardrailResult {
        guardrail_name: "token_budget_guard".to_string(),
        tripwire_triggered: !sufficient,
        check_description: format!(
            "Token budget: {} remaining of {} total (need {} for next agent)",
            remaining, max_tokens, agent_min_requirement
        ),
        output_info: Some(serde_json::json!({
            "remaining": remaining,
            "max": max_tokens,
            "required": agent_min_requirement,
        })),
    }
}

/// Validates a handoff payload is not empty/null.
pub fn guardrail_handoff_payload_present(handoff: &HandoffContext) -> GuardrailResult {
    let has_payload = !handoff.payload.is_null()
        && handoff.payload != serde_json::Value::Object(serde_json::Map::new());

    GuardrailResult {
        guardrail_name: "handoff_payload_present".to_string(),
        tripwire_triggered: !has_payload,
        check_description: format!(
            "Handoff from {} → {}: payload {}",
            handoff.from_agent,
            handoff.to_agent,
            if has_payload { "present" } else { "empty or null" }
        ),
        output_info: None,
    }
}

/// Full configuration for the multi-agent pipeline architecture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiAgentPipelineConfig {
    /// Configuration for the Spec Analyst agent.
    /// Parses spec files into structured acceptance criteria with dependency ordering.
    #[serde(default)]
    pub spec_analyst: PipelineAgentConfig,

    /// Configuration for the Locator agent.
    /// Maps acceptance criteria to code locations (files, components, lines).
    #[serde(default)]
    pub locator: PipelineAgentConfig,

    /// Configuration for Implementer agents.
    /// Makes code changes for assigned DAG subtrees. Runs in parallel across subtrees.
    #[serde(default)]
    pub implementer: PipelineAgentConfig,

    /// Configuration for Verifier agents.
    /// Runs spec assertions after each implementer. One per implementer, isolated context.
    #[serde(default)]
    pub verifier: PipelineAgentConfig,

    /// Maximum parallel implementer agents (each handles one independent DAG subtree).
    #[serde(default = "default_max_parallel_implementers")]
    pub max_parallel_implementers: u32,

    /// Maximum retries per DAG subtree before marking it as failed.
    #[serde(default = "default_max_retries_per_subtree")]
    pub max_retries_per_subtree: u32,

    /// DAG construction strategy.
    /// - "strict": only analyst-declared dependencies
    /// - "permissive": infer additional deps from element hierarchy
    /// - "flat": no deps, all criteria at level 0 (baseline for comparison)
    #[serde(default = "default_dag_strategy")]
    pub dag_strategy: String,

    /// Level execution strategy within each subtree.
    /// - "level_by_level": complete all criteria at level N before starting N+1
    /// - "greedy": start level N+1 as soon as its deps are satisfied
    /// - "critical_first": reorder by severity (critical criteria first)
    #[serde(default = "default_level_strategy")]
    pub level_strategy: String,

    /// Whether to run a full integration verification after all subtrees complete.
    /// Catches cross-subtree regressions.
    #[serde(default = "default_true")]
    pub integration_verification: bool,

    /// Maximum total iterations across all subtrees (budget cap).
    #[serde(default = "default_max_total_iterations")]
    pub max_total_iterations: u32,
}

fn default_max_parallel_implementers() -> u32 {
    3
}

fn default_max_retries_per_subtree() -> u32 {
    3
}

fn default_dag_strategy() -> String {
    "strict".to_string()
}

fn default_level_strategy() -> String {
    "level_by_level".to_string()
}

fn default_max_total_iterations() -> u32 {
    20
}

impl Default for MultiAgentPipelineConfig {
    fn default() -> Self {
        Self {
            spec_analyst: PipelineAgentConfig::default(),
            locator: PipelineAgentConfig::default(),
            implementer: PipelineAgentConfig::default(),
            verifier: PipelineAgentConfig::default(),
            max_parallel_implementers: default_max_parallel_implementers(),
            max_retries_per_subtree: default_max_retries_per_subtree(),
            dag_strategy: default_dag_strategy(),
            level_strategy: default_level_strategy(),
            integration_verification: true,
            max_total_iterations: default_max_total_iterations(),
        }
    }
}

/// An acceptance criterion produced by the Spec Analyst agent.
/// Extends the spec assertion with dependency and location metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineAcceptanceCriterion {
    /// Unique ID: "{spec_group_id}::{assertion_id}"
    pub id: String,
    /// Back-pointer to the source spec assertion ID.
    pub spec_assertion_id: String,
    /// Source spec group ID.
    pub spec_group_id: String,
    /// Human-readable description of what should be true.
    pub description: String,
    /// Whether this can be verified deterministically or requires AI evaluation.
    pub criterion_type: String, // "deterministic" | "ai_evaluated"
    /// Verification method (maps to existing VerificationMethod).
    pub verification_method: String,
    /// IDs of criteria that must pass before this one is attempted.
    pub depends_on: Vec<String>,
    /// UI Bridge element IDs or search criteria targeted by this assertion.
    pub target_elements: Vec<serde_json::Value>,
    /// Estimated complexity: "trivial", "simple", "moderate", "complex".
    pub estimated_complexity: String,
    /// Severity from the source assertion.
    pub severity: String,
    /// Whether this criterion is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// A node in the execution DAG, produced by the DAG Builder (deterministic).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DAGNode {
    /// The acceptance criterion at this node.
    pub criterion_id: String,
    /// Direct dependencies (must pass before this node executes).
    pub dependencies: Vec<String>,
    /// Nodes that depend on this one.
    pub dependents: Vec<String>,
    /// Topological level (0 = root, no dependencies).
    pub level: u32,
    /// Which independent subtree this node belongs to.
    pub subtree_id: String,
}

/// An independent subtree of the execution DAG.
/// Each subtree can be assigned to a separate implementer agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DAGSubtree {
    /// Unique subtree identifier.
    pub id: String,
    /// Root criteria in this subtree (level 0 within the subtree).
    pub root_criteria: Vec<String>,
    /// All criteria IDs in this subtree.
    pub all_criteria: Vec<String>,
    /// Maximum level depth in this subtree.
    pub max_level: u32,
    /// Aggregate estimated complexity.
    pub estimated_complexity: String,
}

/// The full execution DAG produced by the DAG Builder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionDAG {
    /// All nodes, keyed by criterion ID.
    pub nodes: std::collections::HashMap<String, DAGNode>,
    /// Root criteria (no dependencies).
    pub roots: Vec<String>,
    /// Criteria grouped by topological level.
    pub levels: Vec<Vec<String>>,
    /// Independent subtrees (can be processed in parallel).
    pub subtrees: Vec<DAGSubtree>,
}

/// A code location identified by the Locator agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeLocation {
    /// File path relative to project root.
    pub path: String,
    /// Optional line range.
    pub line_range: Option<(u32, u32)>,
    /// React component name (if applicable).
    pub component_name: Option<String>,
    /// How relevant this file is: "primary", "supporting", "type_definition".
    pub relevance: String,
}

/// A criterion with its located code targets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocatedCriterion {
    /// The acceptance criterion.
    pub criterion: PipelineAcceptanceCriterion,
    /// Primary files to modify.
    pub target_files: Vec<CodeLocation>,
    /// Related files that may need changes.
    pub related_files: Vec<CodeLocation>,
    /// Locator's confidence in this mapping (0.0-1.0).
    pub confidence: f64,
}

/// Trace record for any agent invocation in the pipeline.
/// Enables per-agent autoresearch benchmarking and replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineAgentTrace {
    /// Which agent type produced this trace.
    pub agent_type: String, // "spec_analyst", "locator", "implementer", "verifier", "snapshot"
    /// Unique agent instance ID (for parallel implementers).
    pub agent_id: String,
    /// Pipeline run ID (links to campaign).
    pub run_id: String,
    /// Serialized input (for replay).
    pub input_snapshot: serde_json::Value,
    /// Serialized output (for evaluation).
    pub output_snapshot: serde_json::Value,
    /// Agent config used.
    pub config: PipelineAgentConfig,
    /// Wall-clock duration in ms.
    pub duration_ms: u64,
    /// Input tokens consumed.
    pub tokens_in: u32,
    /// Output tokens produced.
    pub tokens_out: u32,
    /// Estimated cost in USD.
    pub cost_usd: f64,
    /// Whether the downstream pipeline succeeded (filled post-hoc).
    #[serde(default)]
    pub downstream_success: Option<bool>,
    /// Quality score from automated or human review (filled post-hoc).
    #[serde(default)]
    pub output_quality_score: Option<f64>,

    // ── Span hierarchy fields (openai-agents-python tracing pattern) ──

    /// Parent span ID for hierarchical trace nesting (None = root span).
    #[serde(default)]
    pub parent_span_id: Option<String>,
    /// Span type for categorization: "agent", "guardrail", "handoff", "verification".
    #[serde(default = "default_span_type_agent")]
    pub span_type: String,
    /// Guardrail results collected during this agent's execution.
    #[serde(default)]
    pub guardrail_results: Vec<GuardrailResult>,
    /// Handoff context received (if this agent was handed off to).
    #[serde(default)]
    pub handoff_received: Option<HandoffContext>,
}

fn default_span_type_agent() -> String {
    "agent".to_string()
}

/// Result of one subtree's implementation + verification cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtreeResult {
    /// Which subtree.
    pub subtree_id: String,
    /// Per-level results.
    pub level_results: Vec<SubtreeLevelResult>,
    /// Total retries used.
    pub retries_used: u32,
    /// Whether all criteria in this subtree passed.
    pub all_passed: bool,
    /// Criteria that regressed (were passing, now failing).
    pub regressions: Vec<String>,
}

/// Result of processing one level within a subtree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtreeLevelResult {
    /// DAG level number.
    pub level: u32,
    /// Implementer agent trace.
    pub implementer_trace: PipelineAgentTrace,
    /// Verifier agent trace.
    pub verifier_trace: PipelineAgentTrace,
    /// Number of retries at this level.
    pub retries: u32,
    /// Whether all criteria at this level passed.
    pub passed: bool,
    /// Per-criterion results.
    pub criterion_results: Vec<PipelineCriterionResult>,
}

/// Verification result for a single criterion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineCriterionResult {
    /// Criterion ID.
    pub criterion_id: String,
    /// Whether it passed.
    pub passed: bool,
    /// Verification method used.
    pub method_used: String,
    /// Verifier's confidence (0.0-1.0).
    pub confidence: f64,
    /// What was found vs expected.
    pub details: String,
    /// Duration in ms.
    pub duration_ms: u64,
}

/// Final result of the entire multi-agent pipeline execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiAgentPipelineResult {
    /// Total iterations across all subtrees.
    pub total_iterations: u32,
    /// Whether all acceptance criteria were satisfied.
    pub goal_achieved: bool,
    /// Whether the pipeline was stopped externally.
    pub was_stopped: bool,
    /// Whether the iteration budget was exhausted.
    pub max_iterations_reached: bool,
    /// Per-subtree results.
    pub subtree_results: Vec<SubtreeResult>,
    /// Integration verification result (cross-subtree regression check).
    pub integration_result: Option<Vec<PipelineCriterionResult>>,
    /// All agent traces for the entire pipeline run.
    pub agent_traces: Vec<PipelineAgentTrace>,
    /// The execution DAG that was used.
    pub dag: ExecutionDAG,
    /// Total criteria count.
    pub total_criteria: u32,
    /// Passed criteria count.
    pub passed_criteria: u32,
    /// Total tokens consumed across all agents.
    pub total_tokens: u64,
    /// Total cost in USD.
    pub total_cost_usd: f64,
}

impl MultiAgentPipelineResult {
    /// Convert to a LoopResult for compatibility with the unified workflow executor.
    pub fn to_loop_result(&self) -> crate::unified_workflow_executor::LoopResult {
        crate::unified_workflow_executor::LoopResult {
            iterations_run: self.total_iterations,
            verification_passed: self.goal_achieved,
            max_iterations_reached: self.max_iterations_reached,
            critical_failure: false,
            was_stopped: self.was_stopped,
            unfixable_errors: false,
            iteration_results: self
                .subtree_results
                .iter()
                .flat_map(|st| {
                    st.level_results.iter().map(|lr| {
                        crate::unified_workflow_executor::IterationResult {
                            iteration: lr.level,
                            verification_passed: lr.passed,
                            critical_failure: false,
                            passed_checks: lr.criterion_results.iter().filter(|c| c.passed).count(),
                            failed_checks: lr
                                .criterion_results
                                .iter()
                                .filter(|c| !c.passed)
                                .count(),
                            failure_context: lr
                                .criterion_results
                                .iter()
                                .filter(|c| !c.passed)
                                .map(|c| format!("{}: {}", c.criterion_id, c.details))
                                .collect::<Vec<_>>()
                                .join("; "),
                            agentic_phase_ran: true,
                            agentic_phase_success: Some(lr.passed),
                        }
                    })
                })
                .collect(),
            total_tokens: if self.total_tokens > 0 { Some(self.total_tokens) } else { None },
            total_cost_usd: if self.total_cost_usd > 0.0 { Some(self.total_cost_usd) } else { None },
        }
    }

    /// Human-readable summary.
    pub fn summary(&self) -> String {
        if self.was_stopped {
            format!(
                "STOPPED after {} iteration(s) ({}/{} criteria passed)",
                self.total_iterations, self.passed_criteria, self.total_criteria
            )
        } else if self.goal_achieved {
            format!(
                "All {} criteria PASSED after {} iteration(s) across {} subtree(s)",
                self.total_criteria,
                self.total_iterations,
                self.subtree_results.len()
            )
        } else if self.max_iterations_reached {
            format!(
                "Budget exhausted after {} iteration(s) — {}/{} criteria passed",
                self.total_iterations, self.passed_criteria, self.total_criteria
            )
        } else {
            format!(
                "Pipeline ended: {}/{} criteria passed after {} iteration(s)",
                self.passed_criteria, self.total_iterations, self.total_criteria
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_verification_verdict ──────────────────────────────────────

    #[test]
    fn parse_verdict_json_code_fence() {
        let input = r#"Here is my assessment:

```json
{
  "status": "pass",
  "confidence": 0.95,
  "observations": "Everything looks good",
  "next_priority": null,
  "issues": [],
  "unreachable": false,
  "unreachable_reason": null
}
```
"#;
        let verdict = parse_verification_verdict(input).expect("should parse json code fence");
        assert_eq!(verdict.status, VerificationStatus::Pass);
        assert!((verdict.confidence - 0.95).abs() < f64::EPSILON);
        assert_eq!(verdict.observations, "Everything looks good");
    }

    #[test]
    fn parse_verdict_plain_code_fence() {
        let input = r#"Here is the result:

```
{
  "status": "partial",
  "confidence": 0.7,
  "observations": "Some progress made",
  "next_priority": "Fix the header",
  "issues": [],
  "unreachable": false,
  "unreachable_reason": null
}
```
"#;
        let verdict = parse_verification_verdict(input).expect("should parse plain code fence");
        assert_eq!(verdict.status, VerificationStatus::Partial);
        assert!((verdict.confidence - 0.7).abs() < f64::EPSILON);
        assert_eq!(verdict.next_priority.as_deref(), Some("Fix the header"));
    }

    #[test]
    fn parse_verdict_raw_json_without_fences() {
        let input = r#"{"status":"fail","confidence":0.8,"observations":"Nothing works","next_priority":"Start over","issues":[],"unreachable":false,"unreachable_reason":null}"#;
        let verdict = parse_verification_verdict(input).expect("should parse raw JSON");
        assert_eq!(verdict.status, VerificationStatus::Fail);
        assert!((verdict.confidence - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_verdict_empty_input_returns_none() {
        assert!(parse_verification_verdict("").is_none());
    }

    #[test]
    fn parse_verdict_malformed_json_returns_none() {
        let input = r#"```json
{ "status": "pass", confidence: INVALID }
```"#;
        assert!(parse_verification_verdict(input).is_none());
    }

    #[test]
    fn parse_verdict_unbalanced_braces_returns_none() {
        let input = r#"{ "status": "pass", "confidence": 0.9, "observations": "ok" "#;
        assert!(parse_verification_verdict(input).is_none());
    }

    #[test]
    fn parse_verdict_json_with_extra_text_around_it() {
        let input = r#"The system is working well. Here is my verdict: {"status":"pass","confidence":0.85,"observations":"All checks pass","next_priority":null,"issues":[],"unreachable":false,"unreachable_reason":null} That concludes my review."#;
        let verdict =
            parse_verification_verdict(input).expect("should parse JSON surrounded by text");
        assert_eq!(verdict.status, VerificationStatus::Pass);
        assert!((verdict.confidence - 0.85).abs() < f64::EPSILON);
        assert_eq!(verdict.observations, "All checks pass");
    }

    // ── heuristic_verdict ───────────────────────────────────────────────

    #[test]
    fn heuristic_goal_achieved_returns_pass() {
        let verdict = heuristic_verdict("The goal achieved successfully and everything is fine.");
        assert_eq!(verdict.status, VerificationStatus::Pass);
    }

    #[test]
    fn heuristic_unreachable_returns_unreachable() {
        let verdict = heuristic_verdict("This task is unreachable given the current constraints.");
        assert_eq!(verdict.status, VerificationStatus::Unreachable);
    }

    #[test]
    fn heuristic_progress_returns_partial() {
        let verdict = heuristic_verdict("We made some progress toward the solution.");
        assert_eq!(verdict.status, VerificationStatus::Partial);
    }

    #[test]
    fn heuristic_generic_output_returns_fail() {
        let verdict = heuristic_verdict("The button is red and the page loaded.");
        assert_eq!(verdict.status, VerificationStatus::Fail);
    }

    #[test]
    fn heuristic_empty_output_returns_fail() {
        let verdict = heuristic_verdict("");
        assert_eq!(verdict.status, VerificationStatus::Fail);
    }

    #[test]
    fn heuristic_pass_confidence_is_0_6() {
        let verdict = heuristic_verdict("goal achieved");
        assert!((verdict.confidence - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn heuristic_non_pass_confidence_is_0_5() {
        let unreachable = heuristic_verdict("unreachable state");
        assert!((unreachable.confidence - 0.5).abs() < f64::EPSILON);

        let partial = heuristic_verdict("some progress here");
        assert!((partial.confidence - 0.5).abs() < f64::EPSILON);

        let fail = heuristic_verdict("nothing relevant");
        assert!((fail.confidence - 0.5).abs() < f64::EPSILON);
    }
}
