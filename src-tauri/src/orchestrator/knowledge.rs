//! Knowledge Base for the task orchestrator.
//!
//! This module manages the shared knowledge accumulated during task execution:
//! - Findings from workers (bugs, root causes, observations, hypotheses, solutions)
//! - Verification results history
//! - Cross-iteration context for continuity
//!
//! The knowledge base enables:
//! - Workers to record and share findings
//! - Cross-iteration context preservation
//! - Agents to query accumulated knowledge

#![allow(dead_code)]

use std::sync::Arc;
use tracing::{debug, info};

use crate::database::{CheckpointDb, StoredTaskKnowledge, StoredVerificationResult};
use crate::orchestrator::compression::{CompressionConfig, CompressionResult, CompressionService};
use crate::orchestrator::types::{
    Confidence, CriterionOverride, Finding, OverrideCollection, WorkerSignal,
};
use crate::str_utils::truncate_str;

// ============================================================================
// Execution Span Types
// ============================================================================

/// A stored execution span from the database.
#[derive(Debug, Clone)]
pub struct ExecutionSpan {
    pub name: String,
    pub duration_ms: Option<u64>,
    pub success: bool,
    pub error: Option<String>,
}

// ============================================================================
// Knowledge Categories
// ============================================================================

/// Categories of knowledge that can be stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnowledgeCategory {
    /// A finding about the codebase or problem
    Finding,
    /// Identified root cause of an issue
    RootCause,
    /// General observation
    Observation,
    /// Hypothesis about the problem
    Hypothesis,
    /// Proposed or implemented solution
    Solution,
    /// General context information
    Context,
    /// Verification feedback from failed checks
    VerificationFeedback,
    /// A criterion override with justification
    CriterionOverride,
    /// Environment or infrastructure issue (PATH, disk space, tools not installed, etc.)
    /// These require user intervention and should NOT trigger automatic retries.
    Environment,
    /// Tool/API failure (UI Bridge errors, MCP failures, command timeouts)
    ToolFailure,
    /// AI couldn't find needed information during execution
    ContextGap,
    /// AI misinterpreted a workflow step or instruction
    InstructionAmbiguity,
    /// Same issue observed across multiple runs or iterations
    RecurringPattern,
}

impl KnowledgeCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Finding => "finding",
            Self::RootCause => "root_cause",
            Self::Observation => "observation",
            Self::Hypothesis => "hypothesis",
            Self::Solution => "solution",
            Self::Context => "context",
            Self::VerificationFeedback => "verification_feedback",
            Self::CriterionOverride => "criterion_override",
            Self::Environment => "environment",
            Self::ToolFailure => "tool_failure",
            Self::ContextGap => "context_gap",
            Self::InstructionAmbiguity => "instruction_ambiguity",
            Self::RecurringPattern => "recurring_pattern",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "finding" | "bug" => Some(Self::Finding),
            "root_cause" | "rootcause" | "root-cause" => Some(Self::RootCause),
            "observation" => Some(Self::Observation),
            "hypothesis" => Some(Self::Hypothesis),
            "solution" | "fix" => Some(Self::Solution),
            "context" => Some(Self::Context),
            "verification_feedback" | "feedback" => Some(Self::VerificationFeedback),
            "criterion_override" | "override" => Some(Self::CriterionOverride),
            "environment" | "infrastructure" | "env" => Some(Self::Environment),
            "tool_failure" | "tool-failure" => Some(Self::ToolFailure),
            "context_gap" | "context-gap" => Some(Self::ContextGap),
            "instruction_ambiguity" | "instruction-ambiguity" => Some(Self::InstructionAmbiguity),
            "recurring_pattern" | "recurring-pattern" | "pattern" => Some(Self::RecurringPattern),
            _ => None,
        }
    }

    /// Returns true if this category represents an issue requiring user intervention
    /// (should not trigger automatic retries).
    pub fn requires_user_intervention(&self) -> bool {
        matches!(self, Self::Environment)
    }
}

/// Agent types that can contribute knowledge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentType {
    Planning,
    Worker,
    Verification,
    System,
    Reflection,
}

impl AgentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Worker => "worker",
            Self::Verification => "verification",
            Self::System => "system",
            Self::Reflection => "reflection",
        }
    }
}

// ============================================================================
// Knowledge Base Service
// ============================================================================

/// Service for managing task knowledge.
///
/// Provides methods for storing, querying, and building context from
/// accumulated knowledge during task execution.
pub struct KnowledgeBase {
    db: Arc<CheckpointDb>,
}

impl KnowledgeBase {
    pub fn new(db: Arc<CheckpointDb>) -> Self {
        Self { db }
    }

    /// Compress knowledge if needed based on the provided configuration.
    ///
    /// This should be called before building iteration context to ensure
    /// the context doesn't exceed token limits.
    ///
    /// Returns Some(result) if compression was performed, None if not needed.
    pub fn compress_if_needed(
        &self,
        task_run_id: &str,
        config: &CompressionConfig,
    ) -> Result<Option<CompressionResult>, String> {
        let service = CompressionService::new(Arc::clone(&self.db), config.clone());
        service.compress_if_needed(task_run_id)
    }

    /// Record a finding from a worker.
    pub fn record_finding(
        &self,
        task_run_id: &str,
        finding: &Finding,
        iteration: u32,
    ) -> Result<String, String> {
        let confidence_str = match finding.confidence {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        };

        let category = KnowledgeCategory::from_str(&finding.finding_type)
            .unwrap_or(KnowledgeCategory::Finding);

        let stored = self.db.create_task_knowledge(
            task_run_id,
            category.as_str(),
            AgentType::Worker.as_str(),
            iteration,
            &finding.description,
            finding.evidence.as_deref(),
            confidence_str,
            &finding.related_files,
        )?;

        info!(
            "Recorded finding {} (type: {}) for task {} iteration {}",
            stored.id, finding.finding_type, task_run_id, iteration
        );

        Ok(stored.id)
    }

    /// Record verification feedback for a failed iteration.
    pub fn record_verification_feedback(
        &self,
        task_run_id: &str,
        iteration: u32,
        feedback: &str,
        failed_criteria: &[String],
    ) -> Result<String, String> {
        let stored = self.db.create_task_knowledge(
            task_run_id,
            KnowledgeCategory::VerificationFeedback.as_str(),
            AgentType::System.as_str(),
            iteration,
            feedback,
            None,
            "high", // Verification feedback is authoritative
            &failed_criteria
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )?;

        info!(
            "Recorded verification feedback {} for task {} iteration {}",
            stored.id, task_run_id, iteration
        );

        Ok(stored.id)
    }

    /// Record a general observation or context.
    pub fn record_observation(
        &self,
        task_run_id: &str,
        agent_type: AgentType,
        iteration: u32,
        content: &str,
        related_files: &[String],
    ) -> Result<String, String> {
        let stored = self.db.create_task_knowledge(
            task_run_id,
            KnowledgeCategory::Observation.as_str(),
            agent_type.as_str(),
            iteration,
            content,
            None,
            "medium",
            related_files,
        )?;

        Ok(stored.id)
    }

    /// Record an environment/infrastructure issue.
    ///
    /// Environment issues are problems with the execution environment (PATH, disk space,
    /// missing tools, etc.) rather than code issues. These require user intervention
    /// and should NOT trigger automatic retries.
    pub fn record_environment_issue(
        &self,
        task_run_id: &str,
        iteration: u32,
        description: &str,
        issue_type: &str, // e.g., "path_not_found", "disk_space", "tool_missing", "timeout"
        affected_checks: &[String],
    ) -> Result<String, String> {
        let evidence = format!(
            "Issue type: {}\nAffected checks: {}",
            issue_type,
            affected_checks.join(", ")
        );

        let stored = self.db.create_task_knowledge(
            task_run_id,
            KnowledgeCategory::Environment.as_str(),
            AgentType::System.as_str(),
            iteration,
            description,
            Some(&evidence),
            "high", // Environment issues are definitive
            affected_checks,
        )?;

        info!(
            "Recorded environment issue {} (type: {}) for task {} iteration {}: {}",
            stored.id, issue_type, task_run_id, iteration, description
        );

        Ok(stored.id)
    }

    /// Get all environment issues for a task.
    pub fn get_environment_issues(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<StoredTaskKnowledge>, String> {
        self.db.list_task_knowledge(
            task_run_id,
            Some(KnowledgeCategory::Environment.as_str()),
            false,
        )
    }

    /// Get all unresolved findings for a task.
    pub fn get_unresolved_findings(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<StoredTaskKnowledge>, String> {
        self.db
            .list_task_knowledge(task_run_id, Some("finding"), true)
    }

    /// Get all knowledge for a task.
    pub fn get_all_knowledge(&self, task_run_id: &str) -> Result<Vec<StoredTaskKnowledge>, String> {
        self.db.list_task_knowledge(task_run_id, None, false)
    }

    /// Get knowledge by category.
    pub fn get_knowledge_by_category(
        &self,
        task_run_id: &str,
        category: KnowledgeCategory,
    ) -> Result<Vec<StoredTaskKnowledge>, String> {
        self.db
            .list_task_knowledge(task_run_id, Some(category.as_str()), false)
    }

    // ========================================================================
    // Domain-Scoped Knowledge Queries (Phase 5)
    // ========================================================================

    /// Record a finding with domain association.
    pub fn record_domain_finding(
        &self,
        task_run_id: &str,
        finding: &Finding,
        iteration: u32,
        domain_id: &str,
    ) -> Result<String, String> {
        let confidence_str = match finding.confidence {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        };

        let category = KnowledgeCategory::from_str(&finding.finding_type)
            .unwrap_or(KnowledgeCategory::Finding);

        // Include domain in related_files as a special marker
        let mut related_files = finding.related_files.clone();
        related_files.push(format!("domain:{}", domain_id));

        let stored = self.db.create_task_knowledge(
            task_run_id,
            category.as_str(),
            AgentType::Worker.as_str(),
            iteration,
            &finding.description,
            finding.evidence.as_deref(),
            confidence_str,
            &related_files,
        )?;

        info!(
            "Recorded domain finding {} (type: {}, domain: {}) for task {} iteration {}",
            stored.id, finding.finding_type, domain_id, task_run_id, iteration
        );

        Ok(stored.id)
    }

    /// Get all knowledge for a specific domain.
    ///
    /// Filters knowledge entries that have the domain marker in their related_files.
    pub fn get_domain_knowledge(
        &self,
        task_run_id: &str,
        domain_id: &str,
    ) -> Result<Vec<StoredTaskKnowledge>, String> {
        let all_knowledge = self.get_all_knowledge(task_run_id)?;
        let domain_marker = format!("domain:{}", domain_id);

        Ok(all_knowledge
            .into_iter()
            .filter(|k| k.related_files.contains(&domain_marker))
            .collect())
    }

    /// Get domain knowledge by category.
    pub fn get_domain_knowledge_by_category(
        &self,
        task_run_id: &str,
        domain_id: &str,
        category: KnowledgeCategory,
    ) -> Result<Vec<StoredTaskKnowledge>, String> {
        let domain_knowledge = self.get_domain_knowledge(task_run_id, domain_id)?;

        Ok(domain_knowledge
            .into_iter()
            .filter(|k| k.category == category.as_str())
            .collect())
    }

    /// Get unresolved findings for a specific domain.
    pub fn get_domain_unresolved_findings(
        &self,
        task_run_id: &str,
        domain_id: &str,
    ) -> Result<Vec<StoredTaskKnowledge>, String> {
        let domain_knowledge = self.get_domain_knowledge(task_run_id, domain_id)?;

        Ok(domain_knowledge
            .into_iter()
            .filter(|k| k.category == "finding" && !k.is_resolved)
            .collect())
    }

    /// Get verification feedback for a specific domain.
    pub fn get_domain_verification_feedback(
        &self,
        task_run_id: &str,
        domain_id: &str,
    ) -> Result<Vec<StoredTaskKnowledge>, String> {
        self.get_domain_knowledge_by_category(
            task_run_id,
            domain_id,
            KnowledgeCategory::VerificationFeedback,
        )
    }

    /// Get all domains that have knowledge entries.
    pub fn get_domains_with_knowledge(&self, task_run_id: &str) -> Result<Vec<String>, String> {
        let all_knowledge = self.get_all_knowledge(task_run_id)?;
        let mut domains = Vec::new();

        for knowledge in all_knowledge {
            for file in &knowledge.related_files {
                if let Some(domain) = file.strip_prefix("domain:") {
                    if !domains.contains(&domain.to_string()) {
                        domains.push(domain.to_string());
                    }
                }
            }
        }

        Ok(domains)
    }

    /// Build domain-specific iteration context.
    ///
    /// Similar to build_iteration_context but filtered for a specific domain.
    pub fn build_domain_iteration_context(
        &self,
        task_run_id: &str,
        domain_id: &str,
        current_iteration: u32,
    ) -> Result<String, String> {
        let mut context = String::new();

        // Only add context if we're past the first iteration
        if current_iteration <= 1 {
            return Ok(context);
        }

        context.push_str(&format!("## Domain '{}' Context\n\n", domain_id));

        // Add domain-specific verification feedback
        let feedback = self.get_domain_verification_feedback(task_run_id, domain_id)?;
        if !feedback.is_empty() {
            if let Some(latest) = feedback.last() {
                context.push_str("### Last Verification Feedback for This Domain\n\n");
                context.push_str(&latest.content);
                context.push_str("\n\n");
            }
        }

        // Add domain-specific unresolved findings
        let findings = self.get_domain_unresolved_findings(task_run_id, domain_id)?;
        if !findings.is_empty() {
            context.push_str("### Unresolved Findings in This Domain\n\n");
            for finding in findings.iter().take(10) {
                context.push_str(&format!(
                    "- **[{}]** {}\n",
                    finding.category.to_uppercase(),
                    finding.content
                ));
                if let Some(evidence) = &finding.evidence {
                    context.push_str(&format!("  Evidence: {}\n", evidence));
                }
            }
            context.push('\n');
        }

        // Add domain-specific observations
        let observations = self.get_domain_knowledge_by_category(
            task_run_id,
            domain_id,
            KnowledgeCategory::Observation,
        )?;
        if !observations.is_empty() {
            let recent_obs: Vec<_> = observations.iter().rev().take(5).collect();
            if !recent_obs.is_empty() {
                context.push_str("### Recent Observations in This Domain\n\n");
                for obs in recent_obs {
                    context.push_str(&format!("- {}\n", obs.content));
                }
                context.push('\n');
            }
        }

        if context.len() > 30 {
            context.push_str("---\n\n");
            context.push_str("Focus on this domain's specific issues.\n\n");
        } else {
            context.clear();
        }

        Ok(context)
    }

    /// Mark a finding as resolved.
    pub fn resolve_finding(&self, finding_id: &str, notes: Option<&str>) -> Result<(), String> {
        self.db.resolve_task_knowledge(finding_id, notes)?;
        info!("Resolved finding {}", finding_id);
        Ok(())
    }

    // ========================================================================
    // Criterion Override Management
    // ========================================================================

    /// Record a criterion override.
    ///
    /// Overrides are stored as knowledge entries with structured content that
    /// includes the criterion ID, item, and justification.
    pub fn record_override(
        &self,
        task_run_id: &str,
        override_: &CriterionOverride,
    ) -> Result<String, String> {
        // Format content as structured text
        let content = format!(
            "Override for criterion '{}': {} - {}",
            override_.criterion_id, override_.item, override_.justification
        );

        // Store criterion_id as a related_file marker for easy filtering
        let related_files = vec![
            format!("criterion:{}", override_.criterion_id),
            format!("item:{}", override_.item),
        ];

        let stored = self.db.create_task_knowledge(
            task_run_id,
            KnowledgeCategory::CriterionOverride.as_str(),
            AgentType::Worker.as_str(),
            override_.iteration,
            &content,
            Some(&override_.justification),
            "high", // Overrides are explicit decisions
            &related_files,
        )?;

        info!(
            "Recorded override {} for criterion '{}' (item: {}) in task {}",
            stored.id, override_.criterion_id, override_.item, task_run_id
        );

        Ok(stored.id)
    }

    /// Record multiple overrides from worker output.
    pub fn record_overrides(
        &self,
        task_run_id: &str,
        overrides: &[CriterionOverride],
    ) -> Result<Vec<String>, String> {
        let mut ids = Vec::new();
        for override_ in overrides {
            let id = self.record_override(task_run_id, override_)?;
            ids.push(id);
        }
        Ok(ids)
    }

    /// Get all overrides for a task.
    pub fn get_all_overrides(&self, task_run_id: &str) -> Result<OverrideCollection, String> {
        let knowledge = self.db.list_task_knowledge(
            task_run_id,
            Some(KnowledgeCategory::CriterionOverride.as_str()),
            false,
        )?;

        let mut collection = OverrideCollection::new();

        for k in knowledge {
            // Parse the criterion_id and item from related_files
            let criterion_id = k
                .related_files
                .iter()
                .find_map(|f| f.strip_prefix("criterion:"))
                .map(|s| s.to_string())
                .unwrap_or_default();

            let item = k
                .related_files
                .iter()
                .find_map(|f| f.strip_prefix("item:"))
                .map(|s| s.to_string())
                .unwrap_or_default();

            let justification = k.evidence.unwrap_or_else(|| k.content.clone());

            if !criterion_id.is_empty() {
                collection.add(CriterionOverride::new(
                    &criterion_id,
                    &item,
                    &justification,
                    k.iteration,
                ));
            }
        }

        Ok(collection)
    }

    /// Get overrides for a specific criterion.
    pub fn get_criterion_overrides(
        &self,
        task_run_id: &str,
        criterion_id: &str,
    ) -> Result<Vec<CriterionOverride>, String> {
        let all_overrides = self.get_all_overrides(task_run_id)?;
        Ok(all_overrides
            .overrides
            .into_iter()
            .filter(|o| o.criterion_id == criterion_id)
            .collect())
    }

    /// Check if a criterion has any overrides.
    pub fn has_override(&self, task_run_id: &str, criterion_id: &str) -> Result<bool, String> {
        let overrides = self.get_criterion_overrides(task_run_id, criterion_id)?;
        Ok(!overrides.is_empty())
    }

    /// Get verification results for an iteration.
    pub fn get_verification_results(
        &self,
        task_run_id: &str,
        iteration: u32,
    ) -> Result<Vec<StoredVerificationResult>, String> {
        self.db
            .get_iteration_verification_results(task_run_id, iteration)
    }

    /// Get the latest verification results.
    pub fn get_latest_verification_results(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<StoredVerificationResult>, String> {
        self.db.get_latest_verification_results(task_run_id)
    }
}

// ============================================================================
// Findings Parser
// ============================================================================

/// Parse findings from worker output text.
///
/// Workers can emit findings using the format: `[FINDING:type] description`
/// where type is one of: bug, root_cause, observation, hypothesis, solution
///
/// Returns a list of findings found in the output.
pub fn parse_findings_from_output(output: &str) -> Vec<Finding> {
    let mut findings = Vec::new();

    // Look for [FINDING:type] markers
    let mut search_pos = 0;
    while let Some(start) = output[search_pos..].find("[FINDING:") {
        let abs_start = search_pos + start;

        // Find the closing bracket
        if let Some(end_bracket) = output[abs_start..].find(']') {
            let abs_end = abs_start + end_bracket;
            let finding_type = &output[abs_start + 9..abs_end];

            // Extract description (next line or content after marker)
            let after_marker = &output[abs_end + 1..];
            let description = extract_finding_description(after_marker);

            if !description.is_empty() {
                findings.push(Finding {
                    id: uuid::Uuid::new_v4().to_string(),
                    finding_type: finding_type.to_string(),
                    description,
                    evidence: None,
                    confidence: Confidence::Medium,
                    related_files: vec![],
                });

                debug!(
                    "Parsed finding: type={}, desc_len={}",
                    finding_type,
                    findings.last().map(|f| f.description.len()).unwrap_or(0)
                );
            }

            search_pos = abs_end + 1;
        } else {
            break;
        }
    }

    findings
}

/// Extract the description text after a finding marker.
fn extract_finding_description(text: &str) -> String {
    // Take content until next marker or reasonable end
    let trimmed = text.trim_start();

    // Find end: next marker, double newline, or max 500 chars
    let end = trimmed
        .find('[')
        .unwrap_or(trimmed.len())
        .min(trimmed.find("\n\n").unwrap_or(trimmed.len()))
        .min(500);

    trimmed[..end].trim().to_string()
}

/// Process worker output for signals and findings.
///
/// This is a convenience function that:
/// 1. Parses the worker signal (WORK_COMPLETE, NEED_REPLAN, CONTINUE)
/// 2. Extracts any findings from the output
///
/// Returns (signal, findings).
pub fn process_worker_output(output: &str) -> (Option<WorkerSignal>, Vec<Finding>) {
    let signal = WorkerSignal::parse_from_output(output);
    let findings = parse_findings_from_output(output);

    (signal, findings)
}

/// Process worker output for signals, findings, and criterion overrides.
///
/// This is a comprehensive function that:
/// 1. Parses the worker signal (WORK_COMPLETE, NEED_REPLAN, CONTINUE)
/// 2. Extracts any findings from the output
/// 3. Extracts any criterion overrides from the output
///
/// Returns (signal, findings, overrides).
pub fn process_worker_output_full(
    output: &str,
    iteration: u32,
) -> (Option<WorkerSignal>, Vec<Finding>, Vec<CriterionOverride>) {
    let signal = WorkerSignal::parse_from_output(output);
    let findings = parse_findings_from_output(output);
    let overrides = CriterionOverride::parse_from_output(output, iteration);

    debug!(
        "Processed worker output: signal={:?}, findings={}, overrides={}",
        signal.as_ref().map(|s| format!("{:?}", s)),
        findings.len(),
        overrides.len()
    );

    (signal, findings, overrides)
}

// ============================================================================
// Execution Span Helpers
// ============================================================================

/// Query execution spans for a task run from the database.
fn query_execution_spans(
    db: &CheckpointDb,
    task_run_id: &str,
) -> Result<Vec<ExecutionSpan>, String> {
    let conn = db.get_conn_string()?;

    let mut stmt = conn
        .prepare(
            "SELECT name, duration_ms, success, error
             FROM execution_spans
             WHERE execution_id = ?1
             ORDER BY start_ts ASC",
        )
        .map_err(|e| format!("Failed to prepare spans query: {}", e))?;

    let spans = stmt
        .query_map(rusqlite::params![task_run_id], |row| {
            Ok(ExecutionSpan {
                name: row.get(0)?,
                duration_ms: row.get::<_, Option<i64>>(1)?.map(|v| v as u64),
                success: row.get(2)?,
                error: row.get(3)?,
            })
        })
        .map_err(|e| format!("Failed to query spans: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(spans)
}

/// Build a timing summary section from execution spans.
///
/// Returns None if there are no spans for the execution.
fn build_timing_section(spans: &[ExecutionSpan]) -> Option<String> {
    if spans.is_empty() {
        return None;
    }

    let mut section = String::new();
    section.push_str("### Execution Timing\n\n");

    // Collect phase timings
    let phase_names = [
        "workflow.phase.setup",
        "workflow.phase.verification",
        "workflow.phase.agentic",
        "workflow.phase.completion",
    ];
    let mut phase_timings = Vec::new();
    for span in spans {
        if phase_names.contains(&span.name.as_str()) {
            if let Some(duration) = span.duration_ms {
                let phase_name = span
                    .name
                    .strip_prefix("workflow.phase.")
                    .unwrap_or(&span.name);
                phase_timings.push((phase_name.to_string(), duration, span.success));
            }
        }
    }

    if !phase_timings.is_empty() {
        section.push_str("**Phase Timings:**\n");
        for (name, duration, success) in &phase_timings {
            let status = if *success { "" } else { " (failed)" };
            section.push_str(&format!("- {}: {}ms{}\n", name, duration, status));
        }
        section.push('\n');
    }

    // Collect AI session durations
    let ai_sessions: Vec<_> = spans
        .iter()
        .filter(|s| s.name == "ai.session")
        .filter_map(|s| s.duration_ms.map(|d| (d, s.success)))
        .collect();

    if !ai_sessions.is_empty() {
        section.push_str("**AI Sessions:**\n");
        let total_ai_time: u64 = ai_sessions.iter().map(|(d, _)| d).sum();
        let count = ai_sessions.len();
        let avg = total_ai_time / count as u64;
        let failed = ai_sessions.iter().filter(|(_, s)| !s).count();

        section.push_str(&format!(
            "- Total: {} sessions, {}ms total\n",
            count, total_ai_time
        ));
        section.push_str(&format!("- Average: {}ms per session\n", avg));
        if failed > 0 {
            section.push_str(&format!("- Failed: {} sessions\n", failed));
        }
        section.push('\n');
    }

    // Identify slow operations (>5 seconds)
    let slow_threshold_ms = 5000u64;
    let slow_ops: Vec<_> = spans
        .iter()
        .filter_map(|s| {
            s.duration_ms.and_then(|d| {
                if d > slow_threshold_ms {
                    Some((s.name.clone(), d, s.success, s.error.clone()))
                } else {
                    None
                }
            })
        })
        .collect();

    if !slow_ops.is_empty() {
        section.push_str("**Slow Operations (>5s):**\n");
        for (name, duration, success, error) in slow_ops.iter().take(10) {
            let status = if *success {
                String::new()
            } else if let Some(err) = error {
                format!(" - FAILED: {}", err)
            } else {
                " - FAILED".to_string()
            };
            section.push_str(&format!("- {}: {}ms{}\n", name, duration, status));
        }
        section.push('\n');
    }

    // Only return if we added meaningful content
    if section.len() > 25 {
        Some(section)
    } else {
        None
    }
}

// ============================================================================
// Cross-Iteration Context Builder
// ============================================================================

/// Failure category for verification results
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureCategory {
    /// Environment/infrastructure issues (PATH, disk space, tools not found, timeouts)
    /// These require user intervention and should NOT trigger automatic retries.
    Environment,
    /// Code issues (actual lint/type/test failures)
    /// These can be fixed by the AI and should trigger retries.
    Code,
}

/// Categorize a verification result failure.
///
/// Uses heuristics to detect environment issues based on error messages and patterns.
pub fn categorize_failure(issues: &[String], raw_output: Option<&str>) -> FailureCategory {
    let combined_text = issues.join(" ").to_lowercase();
    let raw_lower = raw_output.map(|s| s.to_lowercase()).unwrap_or_default();

    // Environment issue patterns
    let env_patterns = [
        // Tool not found
        "program not found",
        "not found in path",
        "is not recognized as an internal or external command",
        "'npx' is not recognized",
        "'npm' is not recognized",
        "'node' is not recognized",
        "'python' is not recognized",
        "'cargo' is not recognized",
        "command not found",
        "no such file or directory",
        // Disk space
        "no space left on device",
        "disk space",
        "not enough space",
        "enospc",
        "[winerror 112]",
        // Permission issues
        "permission denied",
        "access denied",
        "operation not permitted",
        // Network issues
        "network is unreachable",
        "connection refused",
        "etimedout",
        "econnrefused",
        // Timeout (when it's a system-level timeout, not test timeout)
        "timed out after",
        "check timed out",
        "operation timed out",
    ];

    for pattern in env_patterns {
        if combined_text.contains(pattern) || raw_lower.contains(pattern) {
            return FailureCategory::Environment;
        }
    }

    FailureCategory::Code
}

/// Build a structured failure summary for AI context.
///
/// Separates failures into environment issues (require user intervention) and
/// code issues (can be fixed by AI). This helps AI understand what to focus on.
pub fn build_structured_failure_summary(
    failed_results: &[&crate::database::StoredVerificationResult],
) -> String {
    let mut summary = String::new();

    // Categorize failures
    let mut env_failures: Vec<&crate::database::StoredVerificationResult> = Vec::new();
    let mut code_failures: Vec<&crate::database::StoredVerificationResult> = Vec::new();

    for result in failed_results {
        let category = categorize_failure(&result.issues, result.raw_output.as_deref());
        match category {
            FailureCategory::Environment => env_failures.push(result),
            FailureCategory::Code => code_failures.push(result),
        }
    }

    // Build summary
    summary.push_str("## Verification Failure Summary\n\n");

    // Environment issues section
    if !env_failures.is_empty() {
        summary.push_str("### ⚠️ Environment Issues (Require User Intervention)\n\n");
        summary.push_str("**These issues are NOT code problems. They indicate missing tools, configuration issues, or resource constraints. Do NOT attempt to fix these with code changes.**\n\n");

        for result in &env_failures {
            summary.push_str(&format!(
                "- **{}** ({})\n",
                result.criterion_id, result.criterion_type
            ));
            for issue in &result.issues {
                summary.push_str(&format!("  - {}\n", issue));
            }
        }
        summary.push_str("\n**Action Required:** User should verify:\n");
        summary
            .push_str("1. Required tools are installed and in PATH (node, npm, python, cargo)\n");
        summary.push_str("2. Sufficient disk space is available\n");
        summary.push_str("3. Network connectivity for package downloads\n\n");
    }

    // Code issues section
    if !code_failures.is_empty() {
        summary.push_str(&format!(
            "### 🔧 Code Issues ({} checks to fix)\n\n",
            code_failures.len()
        ));
        summary.push_str("**These are actual code problems that you should fix:**\n\n");

        for result in &code_failures {
            let critical = if result.is_critical {
                " [CRITICAL]"
            } else {
                ""
            };
            summary.push_str(&format!(
                "- **{}**{} ({}): {} issues\n",
                result.criterion_id,
                critical,
                result.criterion_type,
                result.issues.len()
            ));
        }
        summary.push('\n');
    }

    // Summary stats
    summary.push_str(&format!(
        "**Total failures:** {} ({} environment, {} code)\n\n",
        failed_results.len(),
        env_failures.len(),
        code_failures.len()
    ));

    if env_failures.is_empty() && !code_failures.is_empty() {
        summary.push_str("All failures are code issues - proceed with fixes.\n\n");
    } else if !env_failures.is_empty() && code_failures.is_empty() {
        summary.push_str("All failures are environment issues - no code changes needed.\n");
        summary.push_str("Consider using `[FINDING:environment:high]` to report these issues.\n\n");
    } else if !env_failures.is_empty() && !code_failures.is_empty() {
        summary.push_str(
            "Fix the code issues. Environment issues will persist until user resolves them.\n\n",
        );
    }

    summary
}

/// Build context for a worker based on accumulated knowledge.
///
/// This creates a context string that can be prepended to the worker's prompt
/// to give them awareness of:
/// - Previous verification failures and feedback
/// - Unresolved findings
/// - Key observations from previous iterations
///
/// If a compression config is provided, compression will be attempted before
/// building context to prevent token overflow.
pub fn build_iteration_context(
    kb: &KnowledgeBase,
    task_run_id: &str,
    current_iteration: u32,
) -> Result<String, String> {
    build_iteration_context_with_compression(kb, task_run_id, current_iteration, None)
}

/// Build context with optional compression.
///
/// If compression_config is Some, runs compression before building context.
pub fn build_iteration_context_with_compression(
    kb: &KnowledgeBase,
    task_run_id: &str,
    current_iteration: u32,
    compression_config: Option<&CompressionConfig>,
) -> Result<String, String> {
    // Run compression if configured and past first iteration
    if let Some(config) = compression_config {
        if current_iteration > 1 {
            if let Some(result) = kb.compress_if_needed(task_run_id, config)? {
                info!(
                    "Compressed knowledge for task {}: {} -> {} tokens ({} items summarized)",
                    task_run_id,
                    result.original_tokens,
                    result.compressed_tokens,
                    result.items_summarized
                );
            }
        }
    }

    let mut context = String::new();

    // Only add context if we're past the first iteration
    if current_iteration <= 1 {
        return Ok(context);
    }

    context.push_str("## Previous Iteration Context\n\n");

    // Add latest verification feedback if any
    let feedback =
        kb.get_knowledge_by_category(task_run_id, KnowledgeCategory::VerificationFeedback)?;
    if !feedback.is_empty() {
        // Get the most recent feedback
        if let Some(latest) = feedback.last() {
            context.push_str("### Last Verification Feedback\n\n");
            context.push_str(&latest.content);
            context.push_str("\n\n");
        }
    }

    // Add detailed verification results from the database
    // This ensures AI has access to structured test results even after runner restart
    let verification_results = kb.get_latest_verification_results(task_run_id)?;
    if !verification_results.is_empty() {
        context.push_str("### Detailed Verification Test Results\n\n");

        // Group results by passed/failed for better readability
        let failed_results: Vec<_> = verification_results.iter().filter(|r| !r.passed).collect();
        let passed_results: Vec<_> = verification_results.iter().filter(|r| r.passed).collect();

        // Add structured failure summary FIRST (helps AI understand what to focus on)
        if !failed_results.is_empty() {
            let structured_summary = build_structured_failure_summary(&failed_results);
            context.push_str(&structured_summary);
        }

        // Show detailed failed results
        if !failed_results.is_empty() {
            context.push_str("#### Detailed Failed Check Results\n\n");
            for result in failed_results {
                let critical_marker = if result.is_critical {
                    " [CRITICAL]"
                } else {
                    ""
                };
                context.push_str(&format!(
                    "**{}**{} ({})\n",
                    result.criterion_id, critical_marker, result.criterion_type
                ));

                // Add issues (most important - what specifically failed)
                if !result.issues.is_empty() {
                    context.push_str("- **Issues:**\n");
                    for issue in &result.issues {
                        context.push_str(&format!("  - {}\n", issue));
                    }
                }

                // Add observations (context about the failure)
                if !result.observations.is_empty() {
                    context.push_str("- **Observations:**\n");
                    for obs in &result.observations {
                        context.push_str(&format!("  - {}\n", obs));
                    }
                }

                // Add suggestions (how to fix)
                if !result.suggestions.is_empty() {
                    context.push_str("- **Suggestions:**\n");
                    for suggestion in &result.suggestions {
                        context.push_str(&format!("  - {}\n", suggestion));
                    }
                }

                // Add raw output (truncated to avoid context bloat)
                if let Some(raw_output) = &result.raw_output {
                    let truncated = if raw_output.len() > 2000 {
                        format!(
                            "{}...\n[truncated, {} chars total]",
                            truncate_str(raw_output, 2000),
                            raw_output.len()
                        )
                    } else {
                        raw_output.clone()
                    };
                    context.push_str("- **Raw Output:**\n```\n");
                    context.push_str(&truncated);
                    context.push_str("\n```\n");
                }

                context.push('\n');
            }
        }

        // Show passed results summary (less detail needed)
        if !passed_results.is_empty() {
            context.push_str(&format!(
                "#### ✓ Passed Checks ({} total)\n\n",
                passed_results.len()
            ));
            for result in passed_results.iter().take(5) {
                context.push_str(&format!(
                    "- {} ({})\n",
                    result.criterion_id, result.criterion_type
                ));
            }
            if passed_results.len() > 5 {
                context.push_str(&format!("- ... and {} more\n", passed_results.len() - 5));
            }
            context.push('\n');
        }

        // Add instructions for accessing complete data via API
        context.push_str("### Available Data APIs\n\n");
        context
            .push_str("The runner database contains detailed execution data. Access via HTTP:\n\n");
        context.push_str("**Verification & Testing:**\n");
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}/verification-results` - Full test results (issues, observations, raw output)\n",
            task_run_id
        ));
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}/verification-results?failed_only=true` - Only failed checks\n",
            task_run_id
        ));
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}/playwright-results` - Playwright test results (status, console, snapshots)\n",
            task_run_id
        ));
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}/playwright-results?status=failed` - Only failed Playwright tests\n\n",
            task_run_id
        ));
        context.push_str("**Execution History:**\n");
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}/events` - All execution events (general, action, state changes)\n",
            task_run_id
        ));
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}/checkpoints` - Step completion checkpoints\n",
            task_run_id
        ));
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}/screenshots` - Screenshot paths from execution\n\n",
            task_run_id
        ));
        context.push_str("**Tool Calls & Integrations:**\n");
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}/mcp-calls` - MCP tool calls (server, tool, args, response)\n",
            task_run_id
        ));
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}/mcp-calls?success=false` - Only failed MCP calls\n",
            task_run_id
        ));
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}/api-requests` - HTTP API requests made\n",
            task_run_id
        ));
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}/awas-steps` - AWAS web automation steps\n\n",
            task_run_id
        ));
        context.push_str("**Knowledge & Findings:**\n");
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}/knowledge` - All findings, observations, solutions\n",
            task_run_id
        ));
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}/knowledge?category=finding` - Only findings/bugs\n",
            task_run_id
        ));
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}/knowledge?unresolved_only=true` - Unresolved issues\n\n",
            task_run_id
        ));
        context.push_str("**Task Run Info:**\n");
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}` - Task run metadata and status\n",
            task_run_id
        ));
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}/output` - Full AI output log\n",
            task_run_id
        ));
        context.push_str(&format!(
            "- `curl http://localhost:9876/task-runs/{}/full-state` - Complete workflow state\n\n",
            task_run_id
        ));
        context.push_str("**Tracing & Performance:**\n");
        context.push_str(&format!(
            "- `curl http://localhost:9876/execution-spans?execution_id={}` - Execution traces (timing, hierarchy)\n",
            task_run_id
        ));
        context.push_str(&format!(
            "- `curl \"http://localhost:9876/execution-spans?execution_id={}&name_pattern=workflow.%\"` - Workflow phase spans\n",
            task_run_id
        ));
        context.push_str(&format!(
            "- `curl \"http://localhost:9876/execution-spans?execution_id={}&min_duration_ms=5000\"` - Slow operations (>5s)\n\n",
            task_run_id
        ));
        context.push_str(
            "Use these APIs when you need more detail than provided in this context.\n\n",
        );

        // Add UI Bridge vs Playwright guidance
        context.push_str("### Verification Tool Priority\n\n");
        context.push_str(
            "**Prefer UI Bridge over Playwright** when the target app has the UI Bridge SDK integrated.\n\n",
        );
        context.push_str(
            "- **UI Bridge SDK** (`/ui-bridge/sdk/*`): Use for SDK-integrated apps. Provides structured element data, \
             fingerprinting, and state discovery. Connect via `POST /ui-bridge/sdk/connect`, then query elements \
             via `GET /ui-bridge/sdk/elements` or `GET /ui-bridge/sdk/snapshot`.\n",
        );
        context.push_str(
            "- **UI Bridge Control** (`/ui-bridge/control/*`): Use for the runner's own UI. \
             Always available, no connection needed.\n",
        );
        context.push_str(
            "- **Playwright**: Use only for non-SDK web apps, or when you need real browser behavior \
             (cookies, network interception, navigation). Playwright is a fallback, not the default.\n\n",
        );
        context.push_str(
            "Check `GET /ui-bridge/sdk/status` to see if an SDK app is already connected before resorting to Playwright.\n\n",
        );
    }

    // Add unresolved findings
    let findings = kb.get_unresolved_findings(task_run_id)?;
    if !findings.is_empty() {
        context.push_str("### Unresolved Findings\n\n");
        for finding in findings.iter().take(10) {
            // Limit to 10 findings
            context.push_str(&format!(
                "- **[{}]** {}\n",
                finding.category.to_uppercase(),
                finding.content
            ));
            if let Some(evidence) = &finding.evidence {
                context.push_str(&format!("  Evidence: {}\n", evidence));
            }
        }
        context.push('\n');
    }

    // Add key observations (limit to recent ones)
    let observations = kb.get_knowledge_by_category(task_run_id, KnowledgeCategory::Observation)?;
    if !observations.is_empty() {
        let recent_obs: Vec<_> = observations.iter().rev().take(5).collect();

        if !recent_obs.is_empty() {
            context.push_str("### Recent Observations\n\n");
            for obs in recent_obs {
                context.push_str(&format!("- {}\n", obs.content));
            }
            context.push('\n');
        }
    }

    // Add solution attempts if any
    let solutions = kb.get_knowledge_by_category(task_run_id, KnowledgeCategory::Solution)?;
    if !solutions.is_empty() {
        context.push_str("### Previous Solution Attempts\n\n");
        for sol in solutions.iter().take(5) {
            let status = if sol.is_resolved { "✓" } else { "✗" };
            context.push_str(&format!("- {} {}\n", status, sol.content));
        }
        context.push('\n');
    }

    // Add execution timing information if spans exist
    if let Ok(spans) = query_execution_spans(&kb.db, task_run_id) {
        if let Some(timing_section) = build_timing_section(&spans) {
            context.push_str(&timing_section);
        }
    }

    if context.len() > 30 {
        // Only add if we have meaningful content
        context.push_str("---\n\n");
        context.push_str(
            "Use this context to avoid repeating mistakes and build on previous progress.\n\n",
        );
    } else {
        context.clear();
    }

    Ok(context)
}

/// Build a summary of task progress for status updates.
pub fn build_progress_summary(
    kb: &KnowledgeBase,
    task_run_id: &str,
    current_iteration: u32,
) -> Result<String, String> {
    let all_knowledge = kb.get_all_knowledge(task_run_id)?;
    let latest_results = kb.get_latest_verification_results(task_run_id)?;

    let findings_count = all_knowledge
        .iter()
        .filter(|k| k.category == "finding")
        .count();

    let resolved_count = all_knowledge.iter().filter(|k| k.is_resolved).count();

    let verification_status = if latest_results.is_empty() {
        "Not yet verified".to_string()
    } else {
        let passed = latest_results.iter().filter(|r| r.passed).count();
        let total = latest_results.len();
        format!("{}/{} checks passed", passed, total)
    };

    Ok(format!(
        "Iteration {}: {} findings ({} resolved), {}",
        current_iteration, findings_count, resolved_count, verification_status
    ))
}

// ============================================================================
// Knowledge Export
// ============================================================================

/// Export all knowledge for a task as structured data.
///
/// Useful for debugging, reporting, or external processing.
pub fn export_task_knowledge(
    kb: &KnowledgeBase,
    task_run_id: &str,
) -> Result<TaskKnowledgeExport, String> {
    let all_knowledge = kb.get_all_knowledge(task_run_id)?;
    let latest_results = kb.get_latest_verification_results(task_run_id)?;

    let findings: Vec<_> = all_knowledge
        .iter()
        .filter(|k| k.category == "finding" || k.category == "root_cause")
        .cloned()
        .collect();

    let observations: Vec<_> = all_knowledge
        .iter()
        .filter(|k| k.category == "observation")
        .cloned()
        .collect();

    let solutions: Vec<_> = all_knowledge
        .iter()
        .filter(|k| k.category == "solution")
        .cloned()
        .collect();

    let feedback: Vec<_> = all_knowledge
        .iter()
        .filter(|k| k.category == "verification_feedback")
        .cloned()
        .collect();

    Ok(TaskKnowledgeExport {
        task_run_id: task_run_id.to_string(),
        total_entries: all_knowledge.len(),
        findings,
        observations,
        solutions,
        verification_feedback: feedback,
        latest_verification_results: latest_results,
    })
}

/// Exported knowledge for a task.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskKnowledgeExport {
    pub task_run_id: String,
    pub total_entries: usize,
    pub findings: Vec<StoredTaskKnowledge>,
    pub observations: Vec<StoredTaskKnowledge>,
    pub solutions: Vec<StoredTaskKnowledge>,
    pub verification_feedback: Vec<StoredTaskKnowledge>,
    pub latest_verification_results: Vec<StoredVerificationResult>,
}

// ============================================================================
// Cross-task knowledge search (hybrid RAG)
// ============================================================================

/// Search for similar knowledge entries across ALL task runs.
///
/// Uses hybrid search: SQL filter by category + vector re-rank by content_embedding.
/// Enables questions like "Has this root cause been identified before?"
pub fn search_similar_knowledge(
    conn: &rusqlite::Connection,
    query_embedding: &[f32],
    category: Option<&str>,
    limit: usize,
) -> Result<
    Vec<
        crate::database::hybrid_search::SearchResult<
            crate::database::hybrid_search::KnowledgeResult,
        >,
    >,
    String,
> {
    let config = crate::database::hybrid_search::HybridSearchConfig {
        limit,
        ..Default::default()
    };

    crate::database::hybrid_search::hybrid_search_knowledge(
        conn,
        query_embedding,
        category,
        &config,
    )
}

/// Format cross-task knowledge results for injection into AI context.
pub fn format_cross_task_knowledge(
    results: &[crate::database::hybrid_search::SearchResult<
        crate::database::hybrid_search::KnowledgeResult,
    >],
) -> String {
    if results.is_empty() {
        return String::new();
    }

    let mut output = String::from("## Related Knowledge from Previous Tasks\n\n");

    for (i, result) in results.iter().take(5).enumerate() {
        output.push_str(&format!(
            "{}. **[{}]** (similarity: {:.0}%, task: {})\n   {}\n\n",
            i + 1,
            result.item.category,
            result.similarity * 100.0,
            &result.item.task_run_id[..8.min(result.item.task_run_id.len())],
            result.item.content,
        ));
    }

    output
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_findings_single() {
        let output = r#"
I found an issue with the login form.

[FINDING:bug] The email validation regex is missing the + character

Let me fix this.
"#;

        let findings = parse_findings_from_output(output);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, "bug");
        assert!(findings[0].description.contains("email validation"));
    }

    #[test]
    fn test_parse_findings_multiple() {
        let output = r#"
After analyzing the code:

[FINDING:observation] The form component uses React Hook Form
[FINDING:root_cause] The validation schema is not exported correctly
[FINDING:solution] Added the missing export to index.ts

Done with analysis.
"#;

        let findings = parse_findings_from_output(output);
        assert_eq!(findings.len(), 3);
        assert_eq!(findings[0].finding_type, "observation");
        assert_eq!(findings[1].finding_type, "root_cause");
        assert_eq!(findings[2].finding_type, "solution");
    }

    #[test]
    fn test_parse_findings_none() {
        let output = "Just some regular output without any findings.";
        let findings = parse_findings_from_output(output);
        assert!(findings.is_empty());
    }

    #[test]
    fn test_knowledge_category_from_str() {
        assert_eq!(
            KnowledgeCategory::from_str("bug"),
            Some(KnowledgeCategory::Finding)
        );
        assert_eq!(
            KnowledgeCategory::from_str("root_cause"),
            Some(KnowledgeCategory::RootCause)
        );
        assert_eq!(
            KnowledgeCategory::from_str("OBSERVATION"),
            Some(KnowledgeCategory::Observation)
        );
        assert_eq!(KnowledgeCategory::from_str("invalid"), None);
    }

    #[test]
    fn test_process_worker_output_complete() {
        let output = r#"
I've fixed the issue.

[FINDING:solution] Updated the regex to include + character

[WORK_COMPLETE] Validation now works correctly
"#;

        let (signal, findings) = process_worker_output(output);

        assert!(matches!(signal, Some(WorkerSignal::WorkComplete { .. })));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, "solution");
    }

    #[test]
    fn test_process_worker_output_replan() {
        let output = r#"
After investigation, I found this is a backend issue.

[FINDING:root_cause] The API is returning wrong error codes

[NEED_REPLAN] Need to modify backend validation instead of frontend
"#;

        let (signal, findings) = process_worker_output(output);

        assert!(matches!(signal, Some(WorkerSignal::NeedReplan { .. })));
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn test_parse_criterion_override_single() {
        let output = r#"
After analyzing the god class detection results:

[CRITERION_OVERRIDE:god_class_detection]
Item: ConfigurationManager
Justification: This class is a facade that delegates to specialized subsystems.
Splitting would create unnecessary indirection without improving cohesion.
[/CRITERION_OVERRIDE]

Continuing with other fixes.
"#;

        let overrides = CriterionOverride::parse_from_output(output, 1);
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].criterion_id, "god_class_detection");
        assert_eq!(overrides[0].item, "ConfigurationManager");
        assert!(overrides[0].justification.contains("facade"));
        assert_eq!(overrides[0].iteration, 1);
    }

    #[test]
    fn test_parse_criterion_override_multiple() {
        let output = r#"
Analysis complete. The following classes should remain as-is:

[CRITERION_OVERRIDE:god_class_detection]
Class: EventDispatcher
Reason: Central coordinator pattern - splitting would fragment event handling.
[/CRITERION_OVERRIDE]

[CRITERION_OVERRIDE:god_class_detection]
Class: DatabaseConnection
Reason: Connection pooling requires centralized state management.
[/CRITERION_OVERRIDE]

[CRITERION_OVERRIDE:security_scan]
Item: test_credentials.json
Justification: Test file with mock credentials, not used in production.
[/CRITERION_OVERRIDE]
"#;

        let overrides = CriterionOverride::parse_from_output(output, 2);
        assert_eq!(overrides.len(), 3);

        assert_eq!(overrides[0].criterion_id, "god_class_detection");
        assert_eq!(overrides[0].item, "EventDispatcher");

        assert_eq!(overrides[1].criterion_id, "god_class_detection");
        assert_eq!(overrides[1].item, "DatabaseConnection");

        assert_eq!(overrides[2].criterion_id, "security_scan");
        assert_eq!(overrides[2].item, "test_credentials.json");
    }

    #[test]
    fn test_parse_criterion_override_no_overrides() {
        let output = "Just some regular output without any overrides.";
        let overrides = CriterionOverride::parse_from_output(output, 1);
        assert!(overrides.is_empty());
    }

    #[test]
    fn test_process_worker_output_full_with_overrides() {
        let output = r#"
I've fixed most issues but need to skip one god class:

[FINDING:solution] Fixed type errors in auth module

[CRITERION_OVERRIDE:god_class_detection]
Item: LegacyAdapter
Justification: Third-party integration requires maintaining API surface.
[/CRITERION_OVERRIDE]

[WORK_COMPLETE] All actionable issues resolved
"#;

        let (signal, findings, overrides) = process_worker_output_full(output, 3);

        assert!(matches!(signal, Some(WorkerSignal::WorkComplete { .. })));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, "solution");
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].criterion_id, "god_class_detection");
        assert_eq!(overrides[0].item, "LegacyAdapter");
        assert_eq!(overrides[0].iteration, 3);
    }

    #[test]
    fn test_override_collection() {
        let mut collection = OverrideCollection::new();

        collection.add(CriterionOverride::new(
            "god_class",
            "ClassA",
            "Justification A",
            1,
        ));
        collection.add(CriterionOverride::new(
            "god_class",
            "ClassB",
            "Justification B",
            1,
        ));
        collection.add(CriterionOverride::new(
            "security",
            "file.json",
            "Test file",
            2,
        ));

        assert!(collection.has_override("god_class"));
        assert!(collection.has_override("security"));
        assert!(!collection.has_override("lint_check"));

        let god_class_overrides = collection.get_overrides("god_class");
        assert_eq!(god_class_overrides.len(), 2);

        let criteria = collection.overridden_criteria();
        assert_eq!(criteria.len(), 2);
        assert!(criteria.contains(&"god_class".to_string()));
        assert!(criteria.contains(&"security".to_string()));
    }
}
