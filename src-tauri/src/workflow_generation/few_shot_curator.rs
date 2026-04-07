//! Few-Shot Curation Pipeline
//!
//! Distilabel-inspired pipeline for curating high-quality verification step
//! examples. Extracts gold-standard examples from successful first-attempt
//! workflow runs and manages a per-domain example bank that can be injected
//! into generation prompts as few-shot demonstrations.

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use uuid::Uuid;

// ============================================================================
// Types
// ============================================================================

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

// ============================================================================
// Storage helpers
// ============================================================================

/// Ensure the curated_examples table and indices exist.
fn ensure_table() -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Insert a new curated example.
fn insert_example(example: &CuratedExample) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Query examples for a given domain, ordered by quality descending.
fn query_by_domain(domain: &str, limit: usize) -> Result<Vec<CuratedExample>, String> {
    Err("SQLite removed".to_string())
}

/// Count how many curated examples exist for a domain.
fn count_by_domain(domain: &str) -> Result<usize, String> {
    Err("SQLite removed".to_string())
}

/// Get the weakest (lowest quality_score) example for a domain.
fn get_weakest_by_domain(domain: &str) -> Result<Option<CuratedExample>, String> {
    Err("SQLite removed".to_string())
}

/// Replace an existing example (by id) with a new one.
fn replace_example(old_id: &str, new_example: &CuratedExample) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Increment the times_used counter for an example.
fn increment_times_used(id: &str) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

// ============================================================================
// Diversity check
// ============================================================================

/// Simple character-level normalized distance between two strings.
/// Returns a value in [0.0, 1.0] where 0.0 means identical.
fn normalized_char_distance(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let max_len = a.len().max(b.len());
    if max_len == 0 {
        return 0.0;
    }

    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let min_len = a_chars.len().min(b_chars.len());

    let mut matches = 0usize;
    for i in 0..min_len {
        if a_chars[i] == b_chars[i] {
            matches += 1;
        }
    }

    let total = a_chars.len().max(b_chars.len());
    1.0 - (matches as f64 / total as f64)
}

// ============================================================================
// FewShotCurator
// ============================================================================

/// Curator that manages a bank of high-quality verification step examples.
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

    /// After a successful first-attempt workflow run, consider extracting examples.
    ///
    /// Only extracts from runs that:
    /// 1. Passed on first attempt (iterations == 0)
    /// 2. Have evaluation scores >= min_quality_score
    /// 3. Steps actually ran and passed (execution_verified)
    ///
    /// Returns the number of examples extracted.
    pub fn consider_extraction(
        &self,
        run_id: &str,
        iterations: u32,
        step_evaluations: &[(String, String, f64, String)], // (step_id, criterion_desc, score, steps_json)
        domain: &str,
    ) -> Result<usize, String> {
        Err("SQLite removed".to_string())
    }

    /// Select relevant examples for a generation prompt.
    ///
    /// Returns examples from the given domain sorted by quality_score descending.
    /// Increments each selected example's times_used counter.
    pub fn select_examples(
        &self,
        domain: &str,
        max_examples: usize,
    ) -> Result<Vec<CuratedExample>, String> {
        Err("SQLite removed".to_string())
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

// ============================================================================
// Tests
// ============================================================================

