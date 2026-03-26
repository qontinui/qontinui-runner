//! Brain/Actor Orchestrator
//!
//! Implements the two-phase LLM invocation pattern inspired by TuriX-CUA:
//!
//! - **Brain** (expensive vision model): Analyzes annotated screenshots + element
//!   indices, produces an action plan with screen analysis and context notes.
//! - **Actor** (cheap text model): Receives the Brain's plan (text only, no images)
//!   and translates it into concrete UI actions.
//!
//! This separation saves cost since the Actor never sees images (no vision tokens),
//! and allows using different model tiers per role.
//!
//! ## Flow
//!
//! 1. Brain is invoked with a multimodal prompt (screenshot + element index).
//! 2. Brain output is cached for `brain_cache_ttl_iterations` iterations.
//! 3. Actor receives Brain's plan + element index as text-only prompt.
//! 4. Actor outputs actions through the existing `WorkerOutput` system.
//! 5. If Actor signals `RequestContext`, Brain is re-invoked next iteration.
//! 6. Both agents can use `RecordStore` to persist/retrieve named data.

#![allow(dead_code)]

use crate::ai_provider::multimodal::MultimodalPrompt;
use crate::ai_provider::AiResponse;
use crate::orchestrator::agent_roles::{AgentRole, ModelTier};
use crate::orchestrator::structured_output::StructuredSignal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info, warn};

// =============================================================================
// Configuration
// =============================================================================

/// Configuration for the Brain/Actor orchestration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainActorConfig {
    /// Model tier for the Brain (vision analysis). Default: Complex.
    pub brain_model_tier: ModelTier,

    /// Model tier for the Actor (action execution). Default: Simple.
    pub actor_model_tier: ModelTier,

    /// Number of iterations to reuse cached Brain output before re-invoking.
    /// Default: 1 (re-invoke every iteration). Set higher to reduce cost.
    pub brain_cache_ttl_iterations: u32,

    /// Maximum number of Brain re-invocations per iteration (via RequestContext).
    /// Prevents infinite loops. Default: 2.
    pub max_brain_reinvocations: u32,

    /// Maximum image width for screenshots sent to Brain. Default: 1024.
    /// Resize reduces token cost (~75% savings at 1024 vs 1920).
    pub max_screenshot_width: u32,

    /// Whether Brain/Actor mode is enabled. When false, falls back to
    /// single-agent behavior.
    pub enabled: bool,
}

impl Default for BrainActorConfig {
    fn default() -> Self {
        Self {
            brain_model_tier: ModelTier::Complex,
            actor_model_tier: ModelTier::Simple,
            brain_cache_ttl_iterations: 1,
            max_brain_reinvocations: 2,
            max_screenshot_width: 1024,
            enabled: false, // Opt-in for now
        }
    }
}

// =============================================================================
// Brain Output
// =============================================================================

/// Structured output from the Brain agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainOutput {
    /// Analysis of the current screen state.
    pub screen_analysis: String,

    /// The element index text (carried through from annotation).
    pub element_index: String,

    /// Ordered list of action steps for the Actor to execute.
    pub suggested_plan: Vec<String>,

    /// Context notes and observations for the Actor.
    pub context_notes: String,

    /// Which iteration this Brain output was produced at.
    pub iteration_produced: u32,
}

impl BrainOutput {
    /// Format the Brain output as context text for the Actor prompt.
    pub fn to_actor_context(&self) -> String {
        let plan_text = if self.suggested_plan.is_empty() {
            "No specific actions planned.".to_string()
        } else {
            self.suggested_plan
                .iter()
                .enumerate()
                .map(|(i, step)| format!("{}. {}", i + 1, step))
                .collect::<Vec<_>>()
                .join("\n")
        };

        format!(
            "## Brain Analysis\n\
             {}\n\n\
             ## Action Plan\n\
             {}\n\n\
             ## Element Index\n\
             {}\n\n\
             ## Notes\n\
             {}",
            self.screen_analysis, plan_text, self.element_index, self.context_notes,
        )
    }
}

// =============================================================================
// Brain/Actor Orchestrator
// =============================================================================

/// Orchestrates the two-phase Brain/Actor LLM invocation pattern.
pub struct BrainActorOrchestrator {
    config: BrainActorConfig,
    last_brain_output: Option<BrainOutput>,
    record_store: HashMap<String, String>,
    brain_reinvocations_this_iteration: u32,
    force_brain_reinvocation: bool,
}

impl BrainActorOrchestrator {
    /// Create a new orchestrator with the given configuration.
    pub fn new(config: BrainActorConfig) -> Self {
        Self {
            config,
            last_brain_output: None,
            record_store: HashMap::new(),
            brain_reinvocations_this_iteration: 0,
            force_brain_reinvocation: false,
        }
    }

    /// Check whether the Brain should be (re-)invoked for the current iteration.
    ///
    /// Returns true if:
    /// - No cached Brain output exists
    /// - The cache has expired (iteration delta exceeds TTL)
    /// - A `RequestContext` signal forced re-invocation
    /// - The first iteration (always invoke Brain)
    pub fn should_invoke_brain(
        &self,
        current_iteration: u32,
    ) -> bool {
        // Forced by RequestContext signal
        if self.force_brain_reinvocation {
            if self.brain_reinvocations_this_iteration >= self.config.max_brain_reinvocations {
                warn!(
                    "Brain re-invocation limit reached ({}/{}), skipping",
                    self.brain_reinvocations_this_iteration,
                    self.config.max_brain_reinvocations
                );
                return false;
            }
            return true;
        }

        // No cached output
        let Some(ref brain_output) = self.last_brain_output else {
            return true;
        };

        // Cache expired
        let iterations_since = current_iteration.saturating_sub(brain_output.iteration_produced);
        iterations_since >= self.config.brain_cache_ttl_iterations
    }

    /// Record that the Brain was invoked and cache its output.
    pub fn set_brain_output(&mut self, output: BrainOutput) {
        info!(
            "Brain output cached (iteration {}, plan steps: {})",
            output.iteration_produced,
            output.suggested_plan.len()
        );
        self.last_brain_output = Some(output);
        self.brain_reinvocations_this_iteration += 1;
        self.force_brain_reinvocation = false;
    }

    /// Get the cached Brain output for building the Actor prompt.
    pub fn brain_output(&self) -> Option<&BrainOutput> {
        self.last_brain_output.as_ref()
    }

    /// Build the Actor's text-only prompt from the Brain's cached output.
    ///
    /// Returns None if no Brain output is cached.
    pub fn build_actor_prompt(&self, task_description: &str) -> Option<String> {
        let brain_output = self.last_brain_output.as_ref()?;

        let record_store_text = if self.record_store.is_empty() {
            String::new()
        } else {
            let entries: Vec<String> = self
                .record_store
                .iter()
                .map(|(k, v)| format!("- **{}**: {}", k, v))
                .collect();
            format!("\n\n## Stored Records\n{}", entries.join("\n"))
        };

        Some(format!(
            "## Task\n{}\n\n{}{}\n\n\
             Translate the action plan into concrete UI actions. Reference elements by \
             their index number. If the plan seems wrong or you need fresh screen analysis, \
             signal RequestContext.",
            task_description,
            brain_output.to_actor_context(),
            record_store_text,
        ))
    }

    /// Process signals from the Actor's output.
    ///
    /// Handles `RequestContext` (forces Brain re-invocation) and `RecordStore`
    /// (persists key-value data).
    pub fn process_signals(&mut self, signals: &[StructuredSignal]) {
        for signal in signals {
            match signal {
                StructuredSignal::RequestContext { reason } => {
                    info!("Actor requested Brain re-invocation: {}", reason);
                    self.force_brain_reinvocation = true;
                }
                StructuredSignal::RecordStore { key, value } => {
                    debug!("Storing record: {} = {}", key, &value[..value.len().min(100)]);
                    self.record_store.insert(key.clone(), value.clone());
                }
                _ => {} // Other signals handled by the main loop
            }
        }
    }

    /// Reset per-iteration state. Call at the start of each iteration.
    pub fn start_iteration(&mut self) {
        self.brain_reinvocations_this_iteration = 0;
    }

    /// Store a named record.
    pub fn store_record(&mut self, key: String, value: String) {
        self.record_store.insert(key, value);
    }

    /// Retrieve a named record.
    pub fn get_record(&self, key: &str) -> Option<&String> {
        self.record_store.get(key)
    }

    /// Get the record store contents as text for Brain context.
    pub fn records_context(&self) -> String {
        if self.record_store.is_empty() {
            return String::new();
        }
        let entries: Vec<String> = self
            .record_store
            .iter()
            .map(|(k, v)| format!("- **{}**: {}", k, v))
            .collect();
        format!("## Stored Records\n{}", entries.join("\n"))
    }

    /// Whether Brain/Actor mode is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Get the config.
    pub fn config(&self) -> &BrainActorConfig {
        &self.config
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> BrainActorConfig {
        BrainActorConfig {
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn test_should_invoke_brain_first_iteration() {
        let orch = BrainActorOrchestrator::new(default_config());
        assert!(orch.should_invoke_brain(0));
    }

    #[test]
    fn test_should_invoke_brain_cache_valid() {
        let mut orch = BrainActorOrchestrator::new(default_config());
        orch.set_brain_output(BrainOutput {
            screen_analysis: "Login page visible".into(),
            element_index: "[1] Login button".into(),
            suggested_plan: vec!["Click login".into()],
            context_notes: "".into(),
            iteration_produced: 0,
        });
        // TTL is 1, so at iteration 0 the cache is still valid
        assert!(!orch.should_invoke_brain(0));
        // At iteration 1 the cache expires
        assert!(orch.should_invoke_brain(1));
    }

    #[test]
    fn test_should_invoke_brain_request_context() {
        let mut orch = BrainActorOrchestrator::new(default_config());
        orch.set_brain_output(BrainOutput {
            screen_analysis: "test".into(),
            element_index: "".into(),
            suggested_plan: vec![],
            context_notes: "".into(),
            iteration_produced: 0,
        });

        // Signal RequestContext
        orch.process_signals(&[StructuredSignal::RequestContext {
            reason: "Screen changed".into(),
        }]);
        assert!(orch.should_invoke_brain(0));
    }

    #[test]
    fn test_brain_reinvocation_limit() {
        let mut config = default_config();
        config.max_brain_reinvocations = 2;
        let mut orch = BrainActorOrchestrator::new(config);

        // First two re-invocations should be allowed
        orch.force_brain_reinvocation = true;
        assert!(orch.should_invoke_brain(0));
        orch.brain_reinvocations_this_iteration = 1;
        assert!(orch.should_invoke_brain(0));

        // Third should be blocked
        orch.brain_reinvocations_this_iteration = 2;
        assert!(!orch.should_invoke_brain(0));
    }

    #[test]
    fn test_build_actor_prompt() {
        let mut orch = BrainActorOrchestrator::new(default_config());
        orch.set_brain_output(BrainOutput {
            screen_analysis: "Login page with email and password fields".into(),
            element_index: "[1] Email input\n[2] Password input\n[3] Login button".into(),
            suggested_plan: vec![
                "Type email in element [1]".into(),
                "Type password in element [2]".into(),
                "Click element [3]".into(),
            ],
            context_notes: "MFA may appear after login".into(),
            iteration_produced: 0,
        });

        let prompt = orch.build_actor_prompt("Log into the application").unwrap();
        assert!(prompt.contains("Log into the application"));
        assert!(prompt.contains("Login page with email and password fields"));
        assert!(prompt.contains("1. Type email in element [1]"));
        assert!(prompt.contains("[1] Email input"));
        assert!(prompt.contains("MFA may appear after login"));
    }

    #[test]
    fn test_record_store() {
        let mut orch = BrainActorOrchestrator::new(default_config());

        orch.process_signals(&[StructuredSignal::RecordStore {
            key: "login_url".into(),
            value: "https://app.example.com/login".into(),
        }]);

        assert_eq!(
            orch.get_record("login_url"),
            Some(&"https://app.example.com/login".to_string())
        );
        assert!(orch.records_context().contains("login_url"));
    }

    #[test]
    fn test_start_iteration_resets_counter() {
        let mut orch = BrainActorOrchestrator::new(default_config());
        orch.brain_reinvocations_this_iteration = 5;
        orch.start_iteration();
        assert_eq!(orch.brain_reinvocations_this_iteration, 0);
    }

    #[test]
    fn test_brain_output_to_actor_context() {
        let output = BrainOutput {
            screen_analysis: "Dashboard loaded".into(),
            element_index: "[1] Settings button".into(),
            suggested_plan: vec!["Click settings".into()],
            context_notes: "No errors visible".into(),
            iteration_produced: 0,
        };
        let ctx = output.to_actor_context();
        assert!(ctx.contains("Dashboard loaded"));
        assert!(ctx.contains("1. Click settings"));
        assert!(ctx.contains("[1] Settings button"));
    }
}
