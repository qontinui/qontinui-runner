//! Mutation strategies for generating the next experiment configuration.

use super::agentic_verification::WorkflowArchitecture;
use super::types::{ExperimentConfig, ExperimentResult, SearchDimension};
use rand::prelude::*;

/// Sequential (grid-search) mutation strategy.
///
/// Cycles through dimensions one at a time. For each dimension, tries each candidate.
/// Keeps the best, moves to the next dimension, repeats forever.
pub struct SequentialMutator {
    /// Which dimension we're currently exploring.
    dimension_index: usize,
    /// Which candidate within the current dimension.
    candidate_index: usize,
}

impl SequentialMutator {
    pub fn new() -> Self {
        Self {
            dimension_index: 0,
            candidate_index: 0,
        }
    }

    /// Generate the next experiment config by mutating one dimension of the control.
    /// Returns None if there are no dimensions to search.
    pub fn next_experiment(
        &mut self,
        control: &ExperimentConfig,
        dimensions: &[SearchDimension],
        _history: &[(u32, ExperimentResult)],
    ) -> Option<ExperimentConfig> {
        if dimensions.is_empty() {
            return None;
        }

        // Wrap dimension index
        self.dimension_index %= dimensions.len();
        let dim = &dimensions[self.dimension_index];

        let mut config = control.clone();
        let applied = self.apply_candidate(&mut config, dim);

        if !applied {
            // Exhausted candidates for this dimension — advance to next
            self.dimension_index += 1;
            self.candidate_index = 0;
            if self.dimension_index >= dimensions.len() {
                self.dimension_index = 0;
            }
            // Try again with next dimension
            let dim = &dimensions[self.dimension_index];
            self.apply_candidate(&mut config, dim);
        }

        // Advance candidate for next call
        self.candidate_index += 1;
        let current_dim = &dimensions[self.dimension_index];
        if self.candidate_index >= current_dim.candidate_count() {
            self.candidate_index = 0;
            self.dimension_index += 1;
        }

        Some(config)
    }

    /// Apply the current candidate to config. Returns false if the candidate index is out of range.
    fn apply_candidate(&self, config: &mut ExperimentConfig, dim: &SearchDimension) -> bool {
        match dim {
            SearchDimension::Model { candidates } => {
                if self.candidate_index >= candidates.len() {
                    return false;
                }
                config.model = Some(candidates[self.candidate_index].clone());
                true
            }
            SearchDimension::MultiAgentMode => {
                if self.candidate_index >= 2 {
                    return false;
                }
                config.multi_agent_mode = Some(self.candidate_index == 1);
                true
            }
            SearchDimension::MaxIterations { range } => {
                let count = (range[1] - range[0] + 1) as usize;
                if self.candidate_index >= count {
                    return false;
                }
                config.max_iterations = Some(range[0] + self.candidate_index as u32);
                true
            }
            SearchDimension::ContextTokens { candidates } => {
                if self.candidate_index >= candidates.len() {
                    return false;
                }
                config.max_context_tokens = Some(candidates[self.candidate_index]);
                true
            }
            SearchDimension::WorkflowArchitecture => {
                if self.candidate_index >= 3 {
                    return false;
                }
                config.workflow_architecture = Some(match self.candidate_index {
                    0 => WorkflowArchitecture::Traditional,
                    1 => WorkflowArchitecture::AgenticVerification,
                    2 => WorkflowArchitecture::MultiAgentPipeline,
                    _ => return false,
                });
                true
            }
            SearchDimension::Custom { name, candidates } => {
                if self.candidate_index >= candidates.len() {
                    return false;
                }
                config
                    .extra
                    .insert(name.clone(), candidates[self.candidate_index].clone());
                true
            }
        }
    }
}

/// Random perturbation mutation strategy.
///
/// Each call picks 1-2 random dimensions and perturbs them:
/// - Numeric dimensions (max_iterations, context_tokens): ±10-30% perturbation
/// - Categorical dimensions (model, multi_agent_mode): uniform random selection
pub struct RandomPerturbator {
    rng: StdRng,
}

impl RandomPerturbator {
    pub fn new() -> Self {
        Self {
            rng: StdRng::from_entropy(),
        }
    }

    /// Generate the next experiment config by randomly perturbing 1-2 dimensions.
    /// Returns None if there are no dimensions to search.
    pub fn next_experiment(
        &mut self,
        control: &ExperimentConfig,
        dimensions: &[SearchDimension],
        _history: &[(u32, ExperimentResult)],
    ) -> Option<ExperimentConfig> {
        if dimensions.is_empty() {
            return None;
        }

        let mut config = control.clone();

        // Pick how many dimensions to perturb (1 or 2)
        let n_perturb = if dimensions.len() == 1 {
            1
        } else {
            self.rng.gen_range(1..=2usize.min(dimensions.len()))
        };

        // Pick which dimensions to perturb (without replacement)
        let mut indices: Vec<usize> = (0..dimensions.len()).collect();
        indices.shuffle(&mut self.rng);
        let selected = &indices[..n_perturb];

        for &dim_idx in selected {
            self.perturb_dimension(&mut config, &dimensions[dim_idx]);
        }

        Some(config)
    }

    /// Apply a random perturbation to one dimension.
    fn perturb_dimension(&mut self, config: &mut ExperimentConfig, dim: &SearchDimension) {
        match dim {
            SearchDimension::Model { candidates } => {
                if !candidates.is_empty() {
                    let idx = self.rng.gen_range(0..candidates.len());
                    config.model = Some(candidates[idx].clone());
                }
            }
            SearchDimension::MultiAgentMode => {
                config.multi_agent_mode = Some(self.rng.gen_bool(0.5));
            }
            SearchDimension::MaxIterations { range } => {
                let current = config.max_iterations.unwrap_or(range[0]);
                let perturbation = self.rng.gen_range(0.7..1.3);
                let new_val = ((current as f64) * perturbation).round() as u32;
                config.max_iterations = Some(new_val.clamp(range[0], range[1]));
            }
            SearchDimension::ContextTokens { candidates } => {
                if !candidates.is_empty() {
                    let idx = self.rng.gen_range(0..candidates.len());
                    config.max_context_tokens = Some(candidates[idx]);
                }
            }
            SearchDimension::WorkflowArchitecture => {
                let choices = [
                    WorkflowArchitecture::Traditional,
                    WorkflowArchitecture::AgenticVerification,
                    WorkflowArchitecture::MultiAgentPipeline,
                ];
                let idx = self.rng.gen_range(0..choices.len());
                config.workflow_architecture = Some(choices[idx].clone());
            }
            SearchDimension::Custom { name, candidates } => {
                if !candidates.is_empty() {
                    let idx = self.rng.gen_range(0..candidates.len());
                    config.extra.insert(name.clone(), candidates[idx].clone());
                }
            }
        }
    }
}

/// AI-guided mutation strategy.
///
/// Uses a fallback strategy (RandomPerturbation) between LLM calls.
/// Every `batch_size` experiments, consults an LLM with full history
/// to get a recommendation for the next config to try.
pub struct AiGuidedMutator {
    /// Fallback strategy for experiments between LLM calls.
    fallback: RandomPerturbator,
    /// How many experiments between LLM guidance calls.
    batch_size: u32,
    /// Counter since last LLM call.
    since_last_guidance: u32,
    /// Last LLM recommendation (used for the next experiment after guidance).
    pending_recommendation: Option<ExperimentConfig>,
    /// Model to use for guidance (None = use default).
    guidance_model: Option<String>,
    /// Human-readable summary of the last AI recommendation.
    pub last_recommendation_text: Option<String>,
}

impl AiGuidedMutator {
    pub fn new(batch_size: u32, model: Option<String>) -> Self {
        Self {
            fallback: RandomPerturbator::new(),
            batch_size: batch_size.max(1),
            since_last_guidance: 0,
            pending_recommendation: None,
            guidance_model: model,
            last_recommendation_text: None,
        }
    }

    /// Generate the next experiment, consulting the LLM when due.
    pub fn next_experiment(
        &mut self,
        control: &ExperimentConfig,
        dimensions: &[SearchDimension],
        history: &[(u32, ExperimentResult)],
    ) -> Option<ExperimentConfig> {
        if dimensions.is_empty() {
            return None;
        }

        // If we have a pending recommendation from the LLM, use it
        if let Some(rec) = self.pending_recommendation.take() {
            self.since_last_guidance = 0;
            return Some(rec);
        }

        self.since_last_guidance += 1;

        // Check if it's time to consult the LLM
        if self.since_last_guidance >= self.batch_size && !history.is_empty() {
            if let Some(config) = self.ask_llm_for_recommendation(control, dimensions, history) {
                self.since_last_guidance = 0;
                return Some(config);
            }
            // LLM failed — fall through to fallback
        }

        // Use fallback strategy between LLM calls
        self.fallback.next_experiment(control, dimensions, history)
    }

    /// Consult the LLM for the next experiment config.
    /// This is a blocking call — runs synchronously within the async engine
    /// via spawn_blocking in the caller.
    fn ask_llm_for_recommendation(
        &mut self,
        control: &ExperimentConfig,
        dimensions: &[SearchDimension],
        history: &[(u32, ExperimentResult)],
    ) -> Option<ExperimentConfig> {
        let prompt = build_guidance_prompt(control, dimensions, history);

        let context = crate::ai_router::TaskContext::from_prompt(&prompt);
        let response = crate::ai_provider::run_prompt_with_model_override(
            &prompt,
            &context,
            None,
            self.guidance_model.as_deref(),
            None,  // provider_override
            Some(0.7), // temperature — some creativity
            Some(2048), // max_tokens
            None,  // fallback_model
            None,  // fallback_provider
        );

        if !response.success {
            tracing::warn!("AI guidance failed: {:?}", response.error);
            return None;
        }

        self.last_recommendation_text = Some(response.output.clone());

        // Parse the LLM's JSON recommendation
        parse_llm_recommendation(&response.output, control, dimensions)
    }
}

/// Build the prompt for the LLM guidance call.
fn build_guidance_prompt(
    control: &ExperimentConfig,
    dimensions: &[SearchDimension],
    history: &[(u32, ExperimentResult)],
) -> String {
    let mut prompt = String::new();

    prompt.push_str("You are an optimization advisor for an autonomous workflow experiment loop.\n\n");

    // Current control
    prompt.push_str(&format!(
        "## Current Best Config (Control)\n{}\n\n",
        control.summary()
    ));

    // Search dimensions
    prompt.push_str("## Search Dimensions\n");
    for dim in dimensions {
        match dim {
            SearchDimension::Model { candidates } => {
                prompt.push_str(&format!("- model: {:?}\n", candidates));
            }
            SearchDimension::MultiAgentMode => {
                prompt.push_str("- multi_agent_mode: [true, false]\n");
            }
            SearchDimension::MaxIterations { range } => {
                prompt.push_str(&format!("- max_iterations: [{}, {}]\n", range[0], range[1]));
            }
            SearchDimension::ContextTokens { candidates } => {
                prompt.push_str(&format!("- context_tokens: {:?}\n", candidates));
            }
            SearchDimension::WorkflowArchitecture => {
                prompt.push_str("- workflow_architecture: [traditional, agentic_verification, multi_agent_pipeline]\n");
            }
            SearchDimension::Custom { name, candidates } => {
                prompt.push_str(&format!("- {}: {:?}\n", name, candidates));
            }
        }
    }

    // Experiment history (last 20 max)
    prompt.push_str("\n## Experiment History (most recent)\n");
    prompt.push_str("| # | Config | Pass Rate | Iterations | Duration | Accepted |\n");
    prompt.push_str("|---|--------|-----------|------------|----------|----------|\n");
    let start = if history.len() > 20 { history.len() - 20 } else { 0 };
    for (num, result) in &history[start..] {
        prompt.push_str(&format!(
            "| {} | {} | {:.1}% | {:.1} | {:.0}ms | {} |\n",
            num,
            result.config.summary(),
            result.aggregate.pass_rate * 100.0,
            result.aggregate.mean_iterations,
            result.aggregate.mean_duration_ms,
            if result.accepted { "YES" } else { "no" },
        ));
    }

    // Summary stats
    let total = history.len();
    let accepted = history.iter().filter(|(_, r)| r.accepted).count();
    prompt.push_str(&format!(
        "\n## Summary\n- Total experiments: {}\n- Accepted: {} ({:.1}%)\n",
        total,
        accepted,
        if total > 0 { accepted as f64 / total as f64 * 100.0 } else { 0.0 }
    ));

    prompt.push_str(r#"
## Task
Based on the experiment history, recommend the SINGLE best config to try next.
Consider:
1. Which dimensions show the most variance in outcomes
2. Combinations that haven't been tried yet
3. Promising directions from partially successful experiments
4. Whether the current best can be improved incrementally

Respond with ONLY a JSON object:
```json
{
  "model": "model-name-or-null",
  "max_iterations": null_or_number,
  "multi_agent_mode": null_or_boolean,
  "max_context_tokens": null_or_number,
  "workflow_architecture": "traditional_or_agentic_verification_or_multi_agent_pipeline_or_null",
  "reasoning": "brief explanation of why this config"
}
```

Use null for dimensions you don't want to change from the control.
"#);

    prompt
}

/// Parse the LLM's recommendation output into an ExperimentConfig.
fn parse_llm_recommendation(
    output: &str,
    control: &ExperimentConfig,
    _dimensions: &[SearchDimension],
) -> Option<ExperimentConfig> {
    // Try to extract JSON from the output (may be wrapped in markdown code blocks)
    let json_str = extract_json_from_output(output)?;

    let parsed: serde_json::Value = serde_json::from_str(&json_str).ok()?;

    let mut config = control.clone();

    if let Some(model) = parsed.get("model").and_then(|v| v.as_str()) {
        config.model = Some(model.to_string());
    }
    if let Some(max_iter) = parsed.get("max_iterations").and_then(|v| v.as_u64()) {
        config.max_iterations = Some(max_iter as u32);
    }
    if let Some(multi) = parsed.get("multi_agent_mode").and_then(|v| v.as_bool()) {
        config.multi_agent_mode = Some(multi);
    }
    if let Some(tokens) = parsed.get("max_context_tokens").and_then(|v| v.as_u64()) {
        config.max_context_tokens = Some(tokens as usize);
    }
    if let Some(arch) = parsed.get("workflow_architecture").and_then(|v| v.as_str()) {
        match arch {
            "traditional" => {
                config.workflow_architecture = Some(WorkflowArchitecture::Traditional);
            }
            "agentic_verification" => {
                config.workflow_architecture = Some(WorkflowArchitecture::AgenticVerification);
            }
            "multi_agent_pipeline" => {
                config.workflow_architecture = Some(WorkflowArchitecture::MultiAgentPipeline);
            }
            _ => {} // Ignore unknown values
        }
    }

    Some(config)
}

/// Extract JSON from LLM output that may contain markdown code fences.
fn extract_json_from_output(output: &str) -> Option<String> {
    // Try extracting from ```json ... ``` code block
    if let Some(start) = output.find("```json") {
        let after_fence = &output[start + 7..];
        if let Some(end) = after_fence.find("```") {
            return Some(after_fence[..end].trim().to_string());
        }
    }
    // Try extracting from ``` ... ```
    if let Some(start) = output.find("```") {
        let after_fence = &output[start + 3..];
        if let Some(end) = after_fence.find("```") {
            let content = after_fence[..end].trim();
            if content.starts_with('{') {
                return Some(content.to_string());
            }
        }
    }
    // Try the whole output as JSON
    let trimmed = output.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sequential_cycles_through_model_candidates() {
        let control = ExperimentConfig {
            model: Some("sonnet".into()),
            max_iterations: Some(10),
            multi_agent_mode: None,
            max_context_tokens: None,
            workflow_architecture: None,
            agentic_verification_config: None,
            multi_agent_pipeline_config: None,
            extra: Default::default(),
        };
        let dims = vec![SearchDimension::Model {
            candidates: vec!["sonnet".into(), "opus".into()],
        }];

        let mut mutator = SequentialMutator::new();

        let e1 = mutator.next_experiment(&control, &dims, &[]).unwrap();
        assert_eq!(e1.model, Some("sonnet".into()));

        let e2 = mutator.next_experiment(&control, &dims, &[]).unwrap();
        assert_eq!(e2.model, Some("opus".into()));

        // Wraps around
        let e3 = mutator.next_experiment(&control, &dims, &[]).unwrap();
        assert_eq!(e3.model, Some("sonnet".into()));
    }

    #[test]
    fn test_random_perturbator_produces_different_configs() {
        let control = ExperimentConfig {
            model: Some("sonnet".into()),
            max_iterations: Some(10),
            multi_agent_mode: Some(false),
            max_context_tokens: None,
            workflow_architecture: None,
            agentic_verification_config: None,
            multi_agent_pipeline_config: None,
            extra: Default::default(),
        };
        let dims = vec![
            SearchDimension::Model {
                candidates: vec!["sonnet".into(), "opus".into(), "haiku".into()],
            },
            SearchDimension::MaxIterations { range: [3, 15] },
            SearchDimension::MultiAgentMode,
        ];

        let mut mutator = RandomPerturbator::new();
        let mut configs = Vec::new();
        for _ in 0..10 {
            configs.push(mutator.next_experiment(&control, &dims, &[]).unwrap());
        }

        // At least some should differ from control
        let different = configs.iter().any(|c| c.summary() != control.summary());
        assert!(different, "RandomPerturbator should produce at least one different config");
    }

    #[test]
    fn test_random_perturbator_max_iterations_stays_in_range() {
        let control = ExperimentConfig {
            model: None,
            max_iterations: Some(10),
            multi_agent_mode: None,
            max_context_tokens: None,
            workflow_architecture: None,
            agentic_verification_config: None,
            multi_agent_pipeline_config: None,
            extra: Default::default(),
        };
        let dims = vec![SearchDimension::MaxIterations { range: [3, 15] }];

        let mut mutator = RandomPerturbator::new();
        for _ in 0..50 {
            let config = mutator.next_experiment(&control, &dims, &[]).unwrap();
            let val = config.max_iterations.unwrap();
            assert!(val >= 3 && val <= 15, "max_iterations {} out of range [3, 15]", val);
        }
    }
}
