//! Context summarization for the orchestration loop.
//!
//! Manages iteration context and compresses older iterations into summaries
//! when the token budget threshold is exceeded, preserving the most recent
//! iterations in full detail.

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::types::SummarizationConfig;

/// Context from a single iteration, to be summarized when token budget is exceeded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationContext {
    pub iteration: u32,
    pub workflow_output: String,
    pub reflection_findings: Vec<String>,
    pub fixes_applied: Vec<String>,
    pub exit_check_reason: String,
    pub token_estimate: usize,
}

/// A compressed summary of one or more iterations.
///
/// Uses a structured narrative template inspired by hermes-agent's context compression:
/// Goal / Progress / Decisions / Files Modified / Next Steps
/// This preserves actionable context better than flat text during long-running workflows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizedContext {
    pub summary: String,
    pub iterations_summarized: Vec<u32>,
    pub key_findings: Vec<String>,
    pub successful_fixes: Vec<String>,
    pub root_causes: Vec<String>,
    /// Files that were modified during the summarized iterations
    #[serde(default)]
    pub files_modified: Vec<String>,
    /// Decisions made and their rationale
    #[serde(default)]
    pub decisions: Vec<String>,
    /// What needs to happen next (unfixed criteria, remaining work)
    #[serde(default)]
    pub next_steps: Vec<String>,
    pub original_token_count: usize,
    pub summary_token_count: usize,
}

/// Manages iteration contexts and triggers summarization when needed.
pub struct ContextSummarizer {
    config: SummarizationConfig,
    iteration_contexts: Vec<IterationContext>,
    prior_summaries: Vec<SummarizedContext>,
}

impl ContextSummarizer {
    pub fn new(config: SummarizationConfig) -> Self {
        Self {
            config,
            iteration_contexts: Vec::new(),
            prior_summaries: Vec::new(),
        }
    }

    /// Add context from a completed iteration.
    pub fn add_iteration_context(&mut self, ctx: IterationContext) {
        debug!(
            "Context summarizer: added iteration {} context (~{} tokens)",
            ctx.iteration, ctx.token_estimate
        );
        self.iteration_contexts.push(ctx);
    }

    /// Estimate total tokens across all stored iteration contexts and summaries.
    pub fn total_token_estimate(&self) -> usize {
        let iteration_tokens: usize =
            self.iteration_contexts.iter().map(|c| c.token_estimate).sum();
        let summary_tokens: usize = self
            .prior_summaries
            .iter()
            .map(|s| s.summary_token_count)
            .sum();
        iteration_tokens + summary_tokens
    }

    /// Check if we should summarize based on token threshold.
    pub fn should_summarize(&self) -> bool {
        if !self.config.enabled {
            return false;
        }
        let preserve = self.config.preserve_last_n_iterations as usize;
        if self.iteration_contexts.len() <= preserve {
            return false;
        }

        let total = self.total_token_estimate();
        let threshold =
            (self.config.max_tokens_budget as f32 * self.config.token_threshold_pct) as usize;
        let should = total >= threshold;
        if should {
            info!(
                "Context summarizer: token estimate {} exceeds threshold {} ({}% of {})",
                total,
                threshold,
                (self.config.token_threshold_pct * 100.0) as u32,
                self.config.max_tokens_budget
            );
        }
        should
    }

    /// Build the prompt for AI summarization of older iterations.
    pub fn build_summarization_prompt(&self) -> Option<String> {
        let preserve = self.config.preserve_last_n_iterations as usize;
        if self.iteration_contexts.len() <= preserve {
            return None;
        }

        let to_summarize = &self.iteration_contexts[..self.iteration_contexts.len() - preserve];
        if to_summarize.is_empty() {
            return None;
        }

        let mut context_text = String::new();
        for ctx in to_summarize {
            context_text.push_str(&format!("\n--- Iteration {} ---\n", ctx.iteration));
            context_text.push_str(&format!("Output: {}\n", ctx.workflow_output));
            if !ctx.reflection_findings.is_empty() {
                context_text.push_str(&format!(
                    "Findings: {}\n",
                    ctx.reflection_findings.join("; ")
                ));
            }
            if !ctx.fixes_applied.is_empty() {
                context_text.push_str(&format!("Fixes applied: {}\n", ctx.fixes_applied.join("; ")));
            }
            context_text.push_str(&format!("Exit reason: {}\n", ctx.exit_check_reason));
        }

        Some(format!(
            "Summarize the following iteration history from an automated workflow loop. \
            Use a structured narrative format that preserves actionable context.\n\
            \n\
            Respond with a JSON object using this template:\n\
            {{\n\
              \"summary\": \"One-paragraph narrative: what was the goal, what progress was made, what's left\",\n\
              \"key_findings\": [\"finding1\", \"finding2\"],\n\
              \"successful_fixes\": [\"fix that worked and why\"],\n\
              \"root_causes\": [\"underlying cause identified\"],\n\
              \"files_modified\": [\"path/to/file.ts\", \"path/to/other.rs\"],\n\
              \"decisions\": [\"chose approach X over Y because Z\"],\n\
              \"next_steps\": [\"remaining criterion to fix\", \"unresolved issue\"]\n\
            }}\n\
            \n\
            Guidelines:\n\
            - The summary should read as a continuous narrative, not a list\n\
            - Include file paths mentioned in outputs or fixes\n\
            - Capture WHY decisions were made, not just what was done\n\
            - next_steps should reflect failing criteria or unresolved issues\n\
            - If this is an update to a prior summary, merge new information rather than starting fresh\n\
            \n\
            Iteration history to summarize:\n{}",
            context_text
        ))
    }

    /// Apply a completed summarization, replacing old contexts with the summary.
    pub fn apply_summary(&mut self, summary: SummarizedContext) {
        let preserve = self.config.preserve_last_n_iterations as usize;
        let total = self.iteration_contexts.len();
        if total <= preserve {
            return;
        }

        info!(
            "Context summarizer: compressing iterations {:?} ({} -> {} tokens)",
            summary.iterations_summarized, summary.original_token_count, summary.summary_token_count
        );

        // Remove summarized iterations, keep the last N
        let kept = self.iteration_contexts.split_off(total - preserve);
        self.iteration_contexts = kept;
        self.prior_summaries.push(summary);
    }

    /// Build the full context string for the next iteration.
    ///
    /// Uses a structured narrative format (Goal/Progress/Decisions/Files/Next Steps)
    /// inspired by hermes-agent's context compression template. This gives the worker
    /// agent clear orientation after compression.
    pub fn build_context_for_next_iteration(&self) -> String {
        let mut parts = Vec::new();

        // Include prior summaries in structured narrative format
        for summary in &self.prior_summaries {
            let iter_range = summary
                .iterations_summarized
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(", ");

            let mut section = format!(
                "[Summary of iterations {}]\n\n**Progress:** {}\n",
                iter_range, summary.summary,
            );

            if !summary.key_findings.is_empty() {
                section.push_str(&format!(
                    "\n**Key findings:** {}\n",
                    summary.key_findings.join("; ")
                ));
            }
            if !summary.root_causes.is_empty() {
                section.push_str(&format!(
                    "\n**Root causes:** {}\n",
                    summary.root_causes.join("; ")
                ));
            }
            if !summary.successful_fixes.is_empty() {
                section.push_str(&format!(
                    "\n**Successful fixes:** {}\n",
                    summary.successful_fixes.join("; ")
                ));
            }
            if !summary.decisions.is_empty() {
                section.push_str(&format!(
                    "\n**Decisions:** {}\n",
                    summary.decisions.join("; ")
                ));
            }
            if !summary.files_modified.is_empty() {
                section.push_str(&format!(
                    "\n**Files modified:** {}\n",
                    summary.files_modified.join(", ")
                ));
            }
            if !summary.next_steps.is_empty() {
                section.push_str(&format!(
                    "\n**Next steps:** {}\n",
                    summary.next_steps.join("; ")
                ));
            }

            parts.push(section);
        }

        // Include recent full-detail iterations
        for ctx in &self.iteration_contexts {
            parts.push(format!(
                "[Iteration {} (full detail)]\nOutput: {}\nFindings: {}\nFixes: {}\nExit: {}",
                ctx.iteration,
                ctx.workflow_output,
                ctx.reflection_findings.join("; "),
                ctx.fixes_applied.join("; "),
                ctx.exit_check_reason,
            ));
        }

        parts.join("\n\n")
    }

    /// Parse an AI summarization response into a SummarizedContext.
    pub fn parse_summary_response(
        &self,
        response: &str,
        original_token_count: usize,
    ) -> SummarizedContext {
        let preserve = self.config.preserve_last_n_iterations as usize;
        let to_summarize =
            &self.iteration_contexts[..self.iteration_contexts.len().saturating_sub(preserve)];
        let iterations_summarized: Vec<u32> = to_summarize.iter().map(|c| c.iteration).collect();

        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(response) {
            let summary = parsed
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or(response)
                .to_string();
            let key_findings = Self::extract_string_array(&parsed, "key_findings");
            let successful_fixes = Self::extract_string_array(&parsed, "successful_fixes");
            let root_causes = Self::extract_string_array(&parsed, "root_causes");
            let files_modified = Self::extract_string_array(&parsed, "files_modified");
            let decisions = Self::extract_string_array(&parsed, "decisions");
            let next_steps = Self::extract_string_array(&parsed, "next_steps");
            let summary_token_count = estimate_tokens(&summary) + 300; // buffer for structured fields

            SummarizedContext {
                summary,
                iterations_summarized,
                key_findings,
                successful_fixes,
                root_causes,
                files_modified,
                decisions,
                next_steps,
                original_token_count,
                summary_token_count,
            }
        } else {
            SummarizedContext {
                summary: response.to_string(),
                iterations_summarized,
                key_findings: Vec::new(),
                successful_fixes: Vec::new(),
                root_causes: Vec::new(),
                files_modified: Vec::new(),
                decisions: Vec::new(),
                next_steps: Vec::new(),
                original_token_count,
                summary_token_count: estimate_tokens(response),
            }
        }
    }

    fn extract_string_array(value: &serde_json::Value, key: &str) -> Vec<String> {
        value
            .get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Simple token estimation: ~4 chars per token.
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(enabled: bool, threshold_pct: f32, budget: usize, preserve: u32) -> SummarizationConfig {
        SummarizationConfig {
            enabled,
            token_threshold_pct: threshold_pct,
            max_tokens_budget: budget,
            preserve_last_n_iterations: preserve,
            summary_max_tokens: 2000,
        }
    }

    fn make_iteration(iteration: u32, tokens: usize) -> IterationContext {
        IterationContext {
            iteration,
            workflow_output: format!("output for iteration {}", iteration),
            reflection_findings: vec![format!("finding_{}", iteration)],
            fixes_applied: vec![format!("fix_{}", iteration)],
            exit_check_reason: "not done yet".to_string(),
            token_estimate: tokens,
        }
    }

    #[test]
    fn should_summarize_returns_false_when_disabled() {
        let config = make_config(false, 0.75, 1000, 1);
        let mut summarizer = ContextSummarizer::new(config);
        // Add enough data to exceed threshold
        summarizer.add_iteration_context(make_iteration(1, 900));
        summarizer.add_iteration_context(make_iteration(2, 900));
        assert!(!summarizer.should_summarize());
    }

    #[test]
    fn should_summarize_returns_false_when_under_threshold() {
        let config = make_config(true, 0.75, 10000, 1);
        let mut summarizer = ContextSummarizer::new(config);
        summarizer.add_iteration_context(make_iteration(1, 100));
        summarizer.add_iteration_context(make_iteration(2, 100));
        // total=200, threshold=7500 => under
        assert!(!summarizer.should_summarize());
    }

    #[test]
    fn should_summarize_returns_true_when_over_threshold() {
        let config = make_config(true, 0.75, 1000, 1);
        let mut summarizer = ContextSummarizer::new(config);
        summarizer.add_iteration_context(make_iteration(1, 400));
        summarizer.add_iteration_context(make_iteration(2, 400));
        // total=800, threshold=750 => over, and 2 > preserve_last_n(1)
        assert!(summarizer.should_summarize());
    }

    #[test]
    fn should_summarize_returns_false_when_fewer_than_preserve_last_n() {
        let config = make_config(true, 0.01, 100, 5);
        let mut summarizer = ContextSummarizer::new(config);
        summarizer.add_iteration_context(make_iteration(1, 5000));
        summarizer.add_iteration_context(make_iteration(2, 5000));
        // Even though tokens exceed threshold, only 2 iterations <= preserve_last_n(5)
        assert!(!summarizer.should_summarize());
    }

    #[test]
    fn build_summarization_prompt_includes_iteration_data() {
        let config = make_config(true, 0.75, 1000, 1);
        let mut summarizer = ContextSummarizer::new(config);
        summarizer.add_iteration_context(make_iteration(1, 400));
        summarizer.add_iteration_context(make_iteration(2, 400));
        let prompt = summarizer.build_summarization_prompt();
        assert!(prompt.is_some());
        let prompt = prompt.unwrap();
        assert!(prompt.contains("Iteration 1"));
        assert!(prompt.contains("output for iteration 1"));
        assert!(prompt.contains("finding_1"));
        // Iteration 2 is preserved (last 1), so should NOT be in the prompt
        assert!(!prompt.contains("Iteration 2"));
    }

    #[test]
    fn build_summarization_prompt_returns_none_when_nothing_to_summarize() {
        let config = make_config(true, 0.75, 1000, 5);
        let mut summarizer = ContextSummarizer::new(config);
        summarizer.add_iteration_context(make_iteration(1, 400));
        assert!(summarizer.build_summarization_prompt().is_none());
    }

    #[test]
    fn parse_summary_response_with_valid_json() {
        let config = make_config(true, 0.75, 1000, 1);
        let mut summarizer = ContextSummarizer::new(config);
        summarizer.add_iteration_context(make_iteration(1, 400));
        summarizer.add_iteration_context(make_iteration(2, 400));

        let response = r#"{
            "summary": "Iterations showed progress",
            "key_findings": ["found bug A"],
            "successful_fixes": ["fix A applied"],
            "root_causes": ["misconfigured setting"],
            "files_modified": ["src/main.rs", "tests/test.rs"],
            "decisions": ["chose rewrite over patch"],
            "next_steps": ["fix lint errors"]
        }"#;

        let result = summarizer.parse_summary_response(response, 800);
        assert_eq!(result.summary, "Iterations showed progress");
        assert_eq!(result.key_findings, vec!["found bug A"]);
        assert_eq!(result.successful_fixes, vec!["fix A applied"]);
        assert_eq!(result.root_causes, vec!["misconfigured setting"]);
        assert_eq!(result.files_modified, vec!["src/main.rs", "tests/test.rs"]);
        assert_eq!(result.decisions, vec!["chose rewrite over patch"]);
        assert_eq!(result.next_steps, vec!["fix lint errors"]);
        assert_eq!(result.iterations_summarized, vec![1]);
        assert_eq!(result.original_token_count, 800);
    }

    #[test]
    fn parse_summary_response_with_invalid_json_falls_back() {
        let config = make_config(true, 0.75, 1000, 1);
        let mut summarizer = ContextSummarizer::new(config);
        summarizer.add_iteration_context(make_iteration(1, 400));
        summarizer.add_iteration_context(make_iteration(2, 400));

        let response = "Just a plain text summary of what happened";
        let result = summarizer.parse_summary_response(response, 800);
        assert_eq!(result.summary, response);
        assert!(result.key_findings.is_empty());
        assert!(result.successful_fixes.is_empty());
        assert!(result.root_causes.is_empty());
        assert!(result.files_modified.is_empty());
        assert!(result.decisions.is_empty());
        assert!(result.next_steps.is_empty());
    }

    #[test]
    fn apply_summary_removes_old_and_keeps_recent() {
        let config = make_config(true, 0.75, 1000, 2);
        let mut summarizer = ContextSummarizer::new(config);
        summarizer.add_iteration_context(make_iteration(1, 300));
        summarizer.add_iteration_context(make_iteration(2, 300));
        summarizer.add_iteration_context(make_iteration(3, 300));
        summarizer.add_iteration_context(make_iteration(4, 300));

        let summary = SummarizedContext {
            summary: "Summary of 1-2".to_string(),
            iterations_summarized: vec![1, 2],
            key_findings: vec!["finding".to_string()],
            successful_fixes: vec![],
            root_causes: vec![],
            files_modified: vec![],
            decisions: vec![],
            next_steps: vec![],
            original_token_count: 600,
            summary_token_count: 100,
        };

        summarizer.apply_summary(summary);
        // Should keep last 2 iterations (3 and 4), remove 1 and 2
        assert_eq!(summarizer.iteration_contexts.len(), 2);
        assert_eq!(summarizer.iteration_contexts[0].iteration, 3);
        assert_eq!(summarizer.iteration_contexts[1].iteration, 4);
        assert_eq!(summarizer.prior_summaries.len(), 1);
    }

    #[test]
    fn build_context_for_next_iteration_includes_summaries_and_recent() {
        let config = make_config(true, 0.75, 1000, 1);
        let mut summarizer = ContextSummarizer::new(config);
        summarizer.add_iteration_context(make_iteration(1, 300));
        summarizer.add_iteration_context(make_iteration(2, 300));
        summarizer.add_iteration_context(make_iteration(3, 300));

        let summary = SummarizedContext {
            summary: "Earlier iterations summary".to_string(),
            iterations_summarized: vec![1, 2],
            key_findings: vec!["key_find".to_string()],
            successful_fixes: vec!["good_fix".to_string()],
            root_causes: vec!["root_cause".to_string()],
            files_modified: vec!["src/main.rs".to_string()],
            decisions: vec!["chose approach A".to_string()],
            next_steps: vec!["fix criterion B".to_string()],
            original_token_count: 600,
            summary_token_count: 100,
        };
        summarizer.apply_summary(summary);

        let context = summarizer.build_context_for_next_iteration();
        assert!(context.contains("Summary of iterations 1, 2"));
        assert!(context.contains("Earlier iterations summary"));
        assert!(context.contains("key_find"));
        assert!(context.contains("Iteration 3 (full detail)"));
        // Verify structured narrative sections
        assert!(context.contains("**Progress:**"));
        assert!(context.contains("**Files modified:**"));
        assert!(context.contains("src/main.rs"));
        assert!(context.contains("**Decisions:**"));
        assert!(context.contains("chose approach A"));
        assert!(context.contains("**Next steps:**"));
        assert!(context.contains("fix criterion B"));
    }

    #[test]
    fn estimate_tokens_returns_approx_len_div_4() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        // 100 chars => 25 tokens
        let text = "a".repeat(100);
        assert_eq!(estimate_tokens(&text), 25);
    }
}
