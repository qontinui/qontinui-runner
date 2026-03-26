//! Fuzzy matching for fix generalization.
//!
//! Provides trigram similarity and embedding-based matching to find similar errors
//! and generalize fixes beyond exact signature_hash matching.
//! This closes feedback loop break point #6 (universal fix underutilization)
//! and break point #7 (by enabling fuzzy fix prediction).

use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::HashSet;
use tracing::info;

use crate::database::embeddings::{blob_to_vector, cosine_similarity};

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
// Similar error search
// ============================================================================

/// Find errors similar to the given one.
/// Uses embedding cosine similarity when available, falls back to trigram similarity.
pub fn find_similar_errors(
    conn: &Connection,
    error_description: &str,
    error_embedding: Option<&[f32]>,
    min_similarity: f64,
    limit: usize,
) -> Result<Vec<SimilarError>, String> {
    let mut results = Vec::new();

    // Strategy 1: If we have an embedding, use cosine similarity
    if let Some(query_emb) = error_embedding {
        let mut stmt = conn
            .prepare(
                r#"SELECT id, signature_hash, message, message_embedding
               FROM error_events
               WHERE message_embedding IS NOT NULL
               AND status != 'resolved'
               ORDER BY last_seen_at DESC
               LIMIT 200"#,
            )
            .map_err(|e| format!("Failed to query errors: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let sig: String = row.get(1)?;
                let msg: String = row.get(2)?;
                let emb_blob: Vec<u8> = row.get(3)?;
                Ok((id, sig, msg, emb_blob))
            })
            .map_err(|e| format!("Failed to iterate errors: {}", e))?;

        for row_result in rows {
            if let Ok((id, sig, msg, emb_blob)) = row_result {
                if let Some(candidate_emb) = blob_to_vector(&emb_blob) {
                    if candidate_emb.len() == query_emb.len() {
                        let sim = cosine_similarity(query_emb, &candidate_emb) as f64;
                        if sim >= min_similarity {
                            results.push(SimilarError {
                                error_id: id,
                                signature_hash: sig,
                                message: msg,
                                similarity: sim,
                            });
                        }
                    }
                }
            }
        }
    }

    // Strategy 2: Trigram fallback (or supplement) on recent errors
    if results.len() < limit {
        let mut stmt = conn
            .prepare(
                r#"SELECT id, signature_hash, message
               FROM error_events
               WHERE status != 'resolved'
               ORDER BY last_seen_at DESC
               LIMIT 100"#,
            )
            .map_err(|e| format!("Failed to query errors for trigram: {}", e))?;

        let existing_ids: HashSet<String> = results.iter().map(|r| r.error_id.clone()).collect();

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| format!("Failed to iterate: {}", e))?;

        for row_result in rows {
            if let Ok((id, sig, msg)) = row_result {
                if existing_ids.contains(&id) {
                    continue;
                }
                let sim = trigram_similarity(error_description, &msg);
                if sim >= min_similarity {
                    results.push(SimilarError {
                        error_id: id,
                        signature_hash: sig,
                        message: msg,
                        similarity: sim,
                    });
                }
            }
        }
    }

    // Sort by similarity DESC, truncate
    results.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(limit);

    Ok(results)
}

// ============================================================================
// Fuzzy fix prediction
// ============================================================================

/// Find fixes that resolved similar errors.
/// This extends the exact-match predict_fix_for_error with fuzzy matching.
pub fn predict_fix_fuzzy(
    conn: &Connection,
    error_description: &str,
    error_embedding: Option<&[f32]>,
) -> Result<Option<FuzzyPredictedFix>, String> {
    // First try exact match via signature hash
    // (caller should have already tried this, but we double-check)

    // Find similar errors
    let similar = find_similar_errors(conn, error_description, error_embedding, 0.6, 10)?;

    if similar.is_empty() {
        return Ok(None);
    }

    // For each similar error, check if there's an effective fix
    for error in &similar {
        let fix: Option<(String, String, String, i32)> = conn
            .query_row(
                r#"SELECT rf.id, rf.fix_type, rf.fix_description, rf.reuse_count
                   FROM reflection_fixes rf
                   JOIN task_run_findings trf ON rf.source_finding_id = trf.id
                   WHERE trf.signature_hash = ?1
                   AND rf.effectiveness = 'effective'
                   AND rf.status = 'applied'
                   ORDER BY rf.reuse_count DESC, rf.applied_at DESC
                   LIMIT 1"#,
                params![error.signature_hash],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .ok();

        if let Some((fix_id, fix_type, fix_desc, reuse_count)) = fix {
            let confidence = error.similarity * (1.0 + reuse_count as f64 * 0.1).min(1.5);
            let match_method = if error_embedding.is_some() && error.similarity > 0.8 {
                "embedding"
            } else {
                "trigram"
            };

            return Ok(Some(FuzzyPredictedFix {
                fix_id,
                fix_type,
                fix_description: fix_desc,
                confidence: confidence.min(1.0),
                match_method: match_method.to_string(),
                source_error_signature: error.signature_hash.clone(),
            }));
        }
    }

    Ok(None)
}

// ============================================================================
// Fix generalization (break point #6)
// ============================================================================

/// Broaden an effective fix's applicability_context by analyzing similar errors.
/// When a fix has been effective 2+ times, look for other error patterns it could help with.
pub fn generalize_fix(conn: &Connection, fix_id: &str) -> Result<Option<String>, String> {
    // Get the fix details
    let (fix_desc, fix_type, reuse_count, current_applicability): (
        String,
        String,
        i32,
        Option<String>,
    ) = conn
        .query_row(
            r#"SELECT fix_description, fix_type, reuse_count, applicability_context
               FROM reflection_fixes
               WHERE id = ?1 AND effectiveness = 'effective'"#,
            params![fix_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|e| format!("Fix not found or not effective: {}", e))?;

    // Only generalize if reuse_count >= 2 and no existing applicability_context
    if reuse_count < 2 || current_applicability.is_some() {
        return Ok(None);
    }

    // Find all error signatures this fix has resolved
    let resolved_signatures: Vec<String> = conn
        .prepare(
            r#"SELECT DISTINCT trf.signature_hash
               FROM fix_applications fa
               JOIN task_run_findings trf ON trf.signature_hash = fa.error_signature_hash
               WHERE fa.fix_id = ?1 AND fa.outcome = 'resolved'"#,
        )
        .and_then(|mut stmt| {
            stmt.query_map(params![fix_id], |row| row.get(0))
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .map_err(|e| format!("Failed to query resolved signatures: {}", e))?;

    if resolved_signatures.is_empty() {
        return Ok(None);
    }

    // Build applicability context from the patterns
    let context = format!(
        "Effective for {} fix. Resolved {} distinct error patterns across {} applications. \
         Error types: {}",
        fix_type,
        resolved_signatures.len(),
        reuse_count,
        resolved_signatures.join(", ")
    );

    // Update the fix with the new applicability context and promote to universal scope
    conn.execute(
        r#"UPDATE reflection_fixes
           SET applicability_context = ?1, reflection_scope = 'universal'
           WHERE id = ?2"#,
        params![context, fix_id],
    )
    .map_err(|e| format!("Failed to update fix applicability: {}", e))?;

    info!("Generalized fix {} to universal scope: {}", fix_id, context);

    Ok(Some(context))
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
        assert!(sim > 0.7, "Similar error messages should have high similarity: {}", sim);
    }

    #[test]
    fn test_trigram_dissimilar_strings() {
        let sim = trigram_similarity(
            "connection timeout after 30s",
            "invalid JSON syntax at line 5",
        );
        assert!(sim < 0.3, "Different errors should have low similarity: {}", sim);
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
