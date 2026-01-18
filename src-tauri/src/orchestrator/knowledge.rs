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

use std::sync::Arc;
use tracing::{debug, info};

use crate::database::{CheckpointDb, StoredTaskKnowledge, StoredVerificationResult};
use crate::orchestrator::compression::{CompressionConfig, CompressionResult, CompressionService};
use crate::orchestrator::types::{Confidence, CriterionOverride, Finding, OverrideCollection, WorkerSignal};

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
            _ => None,
        }
    }
}

/// Agent types that can contribute knowledge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentType {
    Planning,
    Worker,
    Verification,
    System,
}

impl AgentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Worker => "worker",
            Self::Verification => "verification",
            Self::System => "system",
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
            &failed_criteria.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
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

    /// Get all unresolved findings for a task.
    pub fn get_unresolved_findings(&self, task_run_id: &str) -> Result<Vec<StoredTaskKnowledge>, String> {
        self.db.list_task_knowledge(task_run_id, Some("finding"), true)
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
        self.db.list_task_knowledge(task_run_id, Some(category.as_str()), false)
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
    pub fn get_domains_with_knowledge(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<String>, String> {
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
            let criterion_id = k.related_files
                .iter()
                .find_map(|f| f.strip_prefix("criterion:"))
                .map(|s| s.to_string())
                .unwrap_or_default();

            let item = k.related_files
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
                    k.iteration as u32,
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
        self.db.get_iteration_verification_results(task_run_id, iteration)
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

                debug!("Parsed finding: type={}, desc_len={}", finding_type, findings.last().map(|f| f.description.len()).unwrap_or(0));
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
// Cross-Iteration Context Builder
// ============================================================================

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
    let feedback = kb.get_knowledge_by_category(task_run_id, KnowledgeCategory::VerificationFeedback)?;
    if !feedback.is_empty() {
        // Get the most recent feedback
        if let Some(latest) = feedback.last() {
            context.push_str("### Last Verification Feedback\n\n");
            context.push_str(&latest.content);
            context.push_str("\n\n");
        }
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
        let recent_obs: Vec<_> = observations
            .iter()
            .rev()
            .take(5)
            .collect();

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

    if context.len() > 30 {
        // Only add if we have meaningful content
        context.push_str("---\n\n");
        context.push_str("Use this context to avoid repeating mistakes and build on previous progress.\n\n");
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
        assert_eq!(KnowledgeCategory::from_str("bug"), Some(KnowledgeCategory::Finding));
        assert_eq!(KnowledgeCategory::from_str("root_cause"), Some(KnowledgeCategory::RootCause));
        assert_eq!(KnowledgeCategory::from_str("OBSERVATION"), Some(KnowledgeCategory::Observation));
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
