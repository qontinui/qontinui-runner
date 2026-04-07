//! Fuzzy matching types and trigram similarity helper.
//!
//! The actual similar-error search lives on `PgDb::find_similar_errors`
//! in `database/pg/reflection.rs`. This module keeps only the shared
//! result types and the pure-Rust trigram similarity scorer, which is
//! used both by that PG query and by the meta-optimizer parser.

use serde::Serialize;
use std::collections::HashSet;

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct SimilarError {
    pub error_id: String,
    pub signature_hash: String,
    pub message: String,
    pub similarity: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FuzzyPredictedFix {
    pub fix_id: String,
    pub fix_type: String,
    pub fix_description: String,
    pub confidence: f64,
    /// How the match was found: "embedding", "trigram", or "exact".
    pub match_method: String,
    pub source_error_signature: String,
}

// ============================================================================
// Trigram similarity
// ============================================================================

/// Compute trigram similarity between two strings using Dice coefficient.
/// Returns 0.0-1.0 where 1.0 is identical.
pub fn trigram_similarity(a: &str, b: &str) -> f64 {
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();

    if a_lower == b_lower {
        return 1.0;
    }
    if a_lower.len() < 3 || b_lower.len() < 3 {
        return 0.0;
    }

    let trigrams_a: HashSet<&str> = a_lower
        .as_bytes()
        .windows(3)
        .map(|w| std::str::from_utf8(w).unwrap_or(""))
        .collect();
    let trigrams_b: HashSet<&str> = b_lower
        .as_bytes()
        .windows(3)
        .map(|w| std::str::from_utf8(w).unwrap_or(""))
        .collect();

    let intersection = trigrams_a.intersection(&trigrams_b).count();
    let total = trigrams_a.len() + trigrams_b.len();

    if total == 0 {
        return 0.0;
    }

    (2.0 * intersection as f64) / total as f64
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trigram_identical() {
        assert_eq!(trigram_similarity("hello world", "hello world"), 1.0);
    }

    #[test]
    fn test_trigram_case_insensitive() {
        assert_eq!(trigram_similarity("Hello World", "hello world"), 1.0);
    }

    #[test]
    fn test_trigram_short_strings() {
        // Identical strings return 1.0 regardless of length
        assert_eq!(trigram_similarity("ab", "ab"), 1.0);
        // Different strings shorter than 3 chars return 0.0 (no trigrams)
        assert_eq!(trigram_similarity("a", "abc"), 0.0);
    }

    #[test]
    fn test_trigram_similar_strings() {
        let sim = trigram_similarity(
            "TypeError: cannot read property 'foo' of undefined",
            "TypeError: cannot read property 'bar' of undefined",
        );
        assert!(
            sim > 0.7,
            "Similar error messages should have high similarity: {}",
            sim
        );
    }

    #[test]
    fn test_trigram_dissimilar_strings() {
        let sim = trigram_similarity(
            "connection timeout after 30s",
            "invalid JSON syntax at line 5",
        );
        assert!(
            sim < 0.3,
            "Different errors should have low similarity: {}",
            sim
        );
    }

    #[test]
    fn test_trigram_empty_strings() {
        // Identical empty strings return 1.0 (equality check)
        assert_eq!(trigram_similarity("", ""), 1.0);
        // Empty vs non-empty returns 0.0
        assert_eq!(trigram_similarity("", "hello"), 0.0);
    }

    #[test]
    fn test_trigram_symmetry() {
        let a = "timeout error in database query";
        let b = "database query timeout";
        let sim_ab = trigram_similarity(a, b);
        let sim_ba = trigram_similarity(b, a);
        assert!(
            (sim_ab - sim_ba).abs() < 1e-10,
            "Trigram similarity should be symmetric"
        );
    }
}
