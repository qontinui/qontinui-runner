//! Few-Shot Curation Pipeline
//!
//! Distilabel-inspired pipeline for curating high-quality verification step
//! examples. Extracts gold-standard examples from successful first-attempt
//! workflow runs and manages a per-domain example bank that can be injected
//! into generation prompts as few-shot demonstrations.

use rusqlite::{params, Connection};
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
fn ensure_table(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS curated_examples (
            id TEXT PRIMARY KEY,
            domain TEXT NOT NULL,
            criterion_description TEXT NOT NULL,
            steps_json TEXT NOT NULL,
            quality_score REAL NOT NULL DEFAULT 0.0,
            execution_verified INTEGER NOT NULL DEFAULT 0,
            times_used INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_curated_examples_domain
            ON curated_examples(domain);
        CREATE INDEX IF NOT EXISTS idx_curated_examples_quality
            ON curated_examples(quality_score);",
    )
    .map_err(|e| format!("Failed to create curated_examples table: {}", e))
}

/// Insert a new curated example.
fn insert_example(conn: &Connection, example: &CuratedExample) -> Result<(), String> {
    conn.execute(
        "INSERT INTO curated_examples (id, domain, criterion_description, steps_json, quality_score, execution_verified, times_used, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            example.id,
            example.domain,
            example.criterion_description,
            example.steps_json,
            example.quality_score,
            example.execution_verified as i32,
            example.times_used,
            example.created_at,
        ],
    )
    .map_err(|e| format!("Failed to insert curated example: {}", e))?;
    Ok(())
}

/// Query examples for a given domain, ordered by quality descending.
fn query_by_domain(
    conn: &Connection,
    domain: &str,
    limit: usize,
) -> Result<Vec<CuratedExample>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, domain, criterion_description, steps_json, quality_score,
                    execution_verified, times_used, created_at
             FROM curated_examples
             WHERE domain = ?1
             ORDER BY quality_score DESC
             LIMIT ?2",
        )
        .map_err(|e| format!("Failed to prepare query_by_domain: {}", e))?;

    let rows = stmt
        .query_map(params![domain, limit as i64], |row| {
            Ok(CuratedExample {
                id: row.get(0)?,
                domain: row.get(1)?,
                criterion_description: row.get(2)?,
                steps_json: row.get(3)?,
                quality_score: row.get(4)?,
                execution_verified: row.get::<_, i32>(5)? != 0,
                times_used: row.get(6)?,
                created_at: row.get(7)?,
            })
        })
        .map_err(|e| format!("Failed to query curated examples: {}", e))?;

    let mut examples = Vec::new();
    for row in rows {
        match row {
            Ok(ex) => examples.push(ex),
            Err(e) => warn!("Skipping malformed curated example row: {}", e),
        }
    }
    Ok(examples)
}

/// Count how many curated examples exist for a domain.
fn count_by_domain(conn: &Connection, domain: &str) -> Result<usize, String> {
    conn.query_row(
        "SELECT COUNT(*) FROM curated_examples WHERE domain = ?1",
        params![domain],
        |row| row.get::<_, i64>(0),
    )
    .map(|c| c as usize)
    .map_err(|e| format!("Failed to count curated examples: {}", e))
}

/// Get the weakest (lowest quality_score) example for a domain.
fn get_weakest_by_domain(
    conn: &Connection,
    domain: &str,
) -> Result<Option<CuratedExample>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, domain, criterion_description, steps_json, quality_score,
                    execution_verified, times_used, created_at
             FROM curated_examples
             WHERE domain = ?1
             ORDER BY quality_score ASC
             LIMIT 1",
        )
        .map_err(|e| format!("Failed to prepare get_weakest_by_domain: {}", e))?;

    let mut rows = stmt
        .query_map(params![domain], |row| {
            Ok(CuratedExample {
                id: row.get(0)?,
                domain: row.get(1)?,
                criterion_description: row.get(2)?,
                steps_json: row.get(3)?,
                quality_score: row.get(4)?,
                execution_verified: row.get::<_, i32>(5)? != 0,
                times_used: row.get(6)?,
                created_at: row.get(7)?,
            })
        })
        .map_err(|e| format!("Failed to query weakest curated example: {}", e))?;

    match rows.next() {
        Some(Ok(ex)) => Ok(Some(ex)),
        Some(Err(e)) => Err(format!("Failed to read weakest example: {}", e)),
        None => Ok(None),
    }
}

/// Replace an existing example (by id) with a new one.
fn replace_example(
    conn: &Connection,
    old_id: &str,
    new_example: &CuratedExample,
) -> Result<(), String> {
    conn.execute(
        "DELETE FROM curated_examples WHERE id = ?1",
        params![old_id],
    )
    .map_err(|e| format!("Failed to delete old curated example: {}", e))?;
    insert_example(conn, new_example)
}

/// Increment the times_used counter for an example.
fn increment_times_used(conn: &Connection, id: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE curated_examples SET times_used = times_used + 1 WHERE id = ?1",
        params![id],
    )
    .map_err(|e| format!("Failed to increment times_used: {}", e))?;
    Ok(())
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
        conn: &Connection,
    ) -> Result<usize, String> {
        // Gate 1: only first-attempt successes are gold-standard quality
        if iterations > 0 {
            debug!(
                run_id,
                iterations, "Skipping example extraction — not first-attempt"
            );
            return Ok(0);
        }

        ensure_table(conn)?;

        let mut extracted = 0usize;

        for (step_id, criterion_desc, score, steps_json) in step_evaluations {
            // Gate 2: quality score must meet threshold
            if *score < self.min_quality_score {
                debug!(
                    step_id,
                    score,
                    min = self.min_quality_score,
                    "Step score below minimum — skipping"
                );
                continue;
            }

            let existing_count = count_by_domain(conn, domain)?;

            if existing_count >= self.max_examples_per_domain {
                // At capacity — only replace if meaningfully better than weakest
                let weakest = get_weakest_by_domain(conn, domain)?;
                if let Some(ref weak) = weakest {
                    if *score < weak.quality_score + 0.1 {
                        debug!(
                            step_id,
                            score,
                            weakest_score = weak.quality_score,
                            "Not enough improvement over weakest — skipping"
                        );
                        continue;
                    }
                    // Build replacement
                    let new_example = CuratedExample {
                        id: Uuid::new_v4().to_string(),
                        domain: domain.to_string(),
                        criterion_description: criterion_desc.clone(),
                        steps_json: steps_json.clone(),
                        quality_score: *score,
                        execution_verified: true,
                        times_used: 0,
                        created_at: chrono::Utc::now().to_rfc3339(),
                    };
                    info!(
                        domain,
                        new_score = score,
                        replaced_id = %weak.id,
                        "Replacing weakest curated example"
                    );
                    replace_example(conn, &weak.id, &new_example)?;
                    extracted += 1;
                }
            } else {
                // Under capacity — check diversity against existing examples
                let existing = query_by_domain(conn, domain, self.max_examples_per_domain)?;
                let dominated = existing.iter().any(|ex| {
                    let dist =
                        normalized_char_distance(&ex.criterion_description, criterion_desc);
                    dist < self.diversity_threshold
                });

                if dominated {
                    debug!(
                        step_id,
                        criterion_desc,
                        "Too similar to existing example — skipping"
                    );
                    continue;
                }

                let new_example = CuratedExample {
                    id: Uuid::new_v4().to_string(),
                    domain: domain.to_string(),
                    criterion_description: criterion_desc.clone(),
                    steps_json: steps_json.clone(),
                    quality_score: *score,
                    execution_verified: true,
                    times_used: 0,
                    created_at: chrono::Utc::now().to_rfc3339(),
                };
                info!(
                    domain,
                    score,
                    criterion_desc,
                    "Inserting new curated example"
                );
                insert_example(conn, &new_example)?;
                extracted += 1;
            }
        }

        if extracted > 0 {
            info!(
                run_id,
                domain, extracted, "Curated examples extracted from run"
            );
        }

        Ok(extracted)
    }

    /// Select relevant examples for a generation prompt.
    ///
    /// Returns examples from the given domain sorted by quality_score descending.
    /// Increments each selected example's times_used counter.
    pub fn select_examples(
        &self,
        domain: &str,
        max_examples: usize,
        conn: &Connection,
    ) -> Result<Vec<CuratedExample>, String> {
        ensure_table(conn)?;

        let examples = query_by_domain(conn, domain, max_examples)?;

        // Track usage
        for ex in &examples {
            if let Err(e) = increment_times_used(conn, &ex.id) {
                warn!(id = %ex.id, error = %e, "Failed to increment times_used");
            }
        }

        debug!(
            domain,
            count = examples.len(),
            "Selected curated examples for few-shot injection"
        );
        Ok(examples)
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
            out.push_str(&format!("### Example {} (score: {:.2})\n\n", i + 1, ex.quality_score));
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
        let evals = vec![(
            "s1".into(),
            "criterion".into(),
            0.95,
            "[]".into(),
        )];
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
        let evals = vec![(
            "s1".into(),
            "criterion".into(),
            0.5,
            "[]".into(),
        )];
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
