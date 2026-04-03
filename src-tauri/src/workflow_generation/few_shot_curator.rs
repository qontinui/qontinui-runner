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

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_table(&conn).unwrap();
        conn
    }

    #[test]
    fn test_insert_and_query() {
        let conn = setup_db();
        let ex = CuratedExample {
            id: "ex-1".into(),
            domain: "accessibility".into(),
            criterion_description: "Check color contrast".into(),
            steps_json: r#"[{"action":"check_contrast"}]"#.into(),
            quality_score: 0.95,
            execution_verified: true,
            times_used: 0,
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        insert_example(&conn, &ex).unwrap();

        let results = query_by_domain(&conn, "accessibility", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].criterion_description, "Check color contrast");
    }

    #[test]
    fn test_skips_non_first_attempt() {
        let conn = setup_db();
        let curator = FewShotCurator::new();
        let evals = vec![("s1".into(), "criterion".into(), 0.95, "[]".into())];
        let count = curator
            .consider_extraction("run-1", 1, &evals, "perf", &conn)
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_extracts_high_quality() {
        let conn = setup_db();
        let curator = FewShotCurator::new();
        let evals = vec![(
            "s1".into(),
            "Check load time".into(),
            0.92,
            r#"[{"action":"measure"}]"#.into(),
        )];
        let count = curator
            .consider_extraction("run-1", 0, &evals, "performance", &conn)
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(count_by_domain(&conn, "performance").unwrap(), 1);
    }

    #[test]
    fn test_skips_low_quality() {
        let conn = setup_db();
        let curator = FewShotCurator::new();
        let evals = vec![("s1".into(), "criterion".into(), 0.5, "[]".into())];
        let count = curator
            .consider_extraction("run-1", 0, &evals, "perf", &conn)
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_diversity_gate() {
        let conn = setup_db();
        let curator = FewShotCurator::new();

        // Insert first
        let evals1 = vec![(
            "s1".into(),
            "Check color contrast ratio".into(),
            0.9,
            "[{}]".into(),
        )];
        curator
            .consider_extraction("r1", 0, &evals1, "a11y", &conn)
            .unwrap();

        // Try near-duplicate criterion
        let evals2 = vec![(
            "s2".into(),
            "Check color contrast ratio".into(),
            0.91,
            "[{}]".into(),
        )];
        let count = curator
            .consider_extraction("r2", 0, &evals2, "a11y", &conn)
            .unwrap();
        assert_eq!(count, 0, "Should reject duplicate criterion");
    }

    #[test]
    fn test_format_few_shot_section_empty() {
        let section = FewShotCurator::format_few_shot_section(&[]);
        assert!(section.is_empty());
    }

    #[test]
    fn test_format_few_shot_section() {
        let examples = vec![CuratedExample {
            id: "ex-1".into(),
            domain: "perf".into(),
            criterion_description: "Page loads under 3s".into(),
            steps_json: r#"[{"action":"navigate"},{"action":"measure"}]"#.into(),
            quality_score: 0.95,
            execution_verified: true,
            times_used: 2,
            created_at: "2026-01-01T00:00:00Z".into(),
        }];
        let section = FewShotCurator::format_few_shot_section(&examples);
        assert!(section.contains("Example 1"));
        assert!(section.contains("Page loads under 3s"));
        assert!(section.contains("0.95"));
    }

    #[test]
    fn test_normalized_char_distance() {
        assert!((normalized_char_distance("abc", "abc") - 0.0).abs() < f64::EPSILON);
        assert!(normalized_char_distance("abc", "xyz") > 0.9);
        assert!((normalized_char_distance("", "") - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_replace_weakest_at_capacity() {
        let conn = setup_db();
        let mut curator = FewShotCurator::new();
        curator.max_examples_per_domain = 2;

        // Fill to capacity with lower scores
        for i in 0..2 {
            let ex = CuratedExample {
                id: format!("ex-{}", i),
                domain: "dom".into(),
                criterion_description: format!("criterion {}", i),
                steps_json: "[]".into(),
                quality_score: 0.8,
                execution_verified: true,
                times_used: 0,
                created_at: "2026-01-01T00:00:00Z".into(),
            };
            insert_example(&conn, &ex).unwrap();
        }

        // New example with score 0.95 should replace weakest (0.8 + 0.1 < 0.95)
        let evals = vec![(
            "s1".into(),
            "much better criterion".into(),
            0.95,
            "[{\"better\":true}]".into(),
        )];
        let count = curator
            .consider_extraction("r1", 0, &evals, "dom", &conn)
            .unwrap();
        assert_eq!(count, 1);
        // Still at capacity
        assert_eq!(count_by_domain(&conn, "dom").unwrap(), 2);
    }

    #[test]
    fn test_select_increments_usage() {
        let conn = setup_db();
        let ex = CuratedExample {
            id: "ex-1".into(),
            domain: "d".into(),
            criterion_description: "c".into(),
            steps_json: "[]".into(),
            quality_score: 0.9,
            execution_verified: true,
            times_used: 0,
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        insert_example(&conn, &ex).unwrap();

        let curator = FewShotCurator::new();
        curator.select_examples("d", 5, &conn).unwrap();

        let after = query_by_domain(&conn, "d", 1).unwrap();
        assert_eq!(after[0].times_used, 1);
    }
}
