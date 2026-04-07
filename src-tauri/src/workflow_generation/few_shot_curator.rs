//! Few-Shot Curation types.
//!
//! The distilabel-inspired extraction/storage pipeline was backed by SQLite
//! and has not been ported to PG. Only the configuration struct and shared
//! result type remain; the `LearningOrchestrator` still owns a curator
//! instance but never invokes the (removed) extraction hooks.

use serde::{Deserialize, Serialize};

/// A curated verification-step example suitable for few-shot injection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuratedExample {
    pub id: String,
    /// Verification domain (e.g. "accessibility", "performance").
    pub domain: String,
    /// Human-readable description of the verification criterion.
    pub criterion_description: String,
    /// The verification steps JSON (gold standard).
    pub steps_json: String,
    /// Quality score from the evaluation engine.
    pub quality_score: f64,
    /// Whether these steps actually passed at runtime.
    pub execution_verified: bool,
    /// How often this example has been injected as a few-shot prompt.
    pub times_used: u32,
    pub created_at: String,
}

/// Curator configuration that manages a bank of high-quality verification step examples.
pub struct FewShotCurator {
    /// Maximum number of curated examples to keep per domain.
    pub max_examples_per_domain: usize,
    /// Minimum evaluation quality score required for extraction.
    pub min_quality_score: f64,
    /// Minimum text distance between criterion descriptions (diversity gate).
    pub diversity_threshold: f64,
}

impl FewShotCurator {
    pub fn new() -> Self {
        Self {
            max_examples_per_domain: 20,
            min_quality_score: 0.8,
            diversity_threshold: 0.3,
        }
    }

    /// Format curated examples as a few-shot section for prompt injection.
    pub fn format_few_shot_section(examples: &[CuratedExample]) -> String {
        if examples.is_empty() {
            return String::new();
        }

        let mut out = String::from("## Verified High-Quality Examples\n\n");
        out.push_str(
            "The following examples were extracted from successful first-attempt runs \
             and verified at runtime. Use them as reference for style and structure.\n\n",
        );

        for (i, ex) in examples.iter().enumerate() {
            out.push_str(&format!(
                "### Example {} (score: {:.2})\n\n",
                i + 1,
                ex.quality_score
            ));
            out.push_str(&format!("**Criterion:** {}\n\n", ex.criterion_description));
            out.push_str(&format!("**Steps:**\n```json\n{}\n```\n\n", ex.steps_json));
        }

        out
    }
}

impl Default for FewShotCurator {
    fn default() -> Self {
        Self::new()
    }
}
