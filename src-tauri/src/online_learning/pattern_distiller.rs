//! Pattern distillation from the knowledge graph.
//!
//! Mines recurring Finding→Fix relationships across successful runs, stores them
//! as reusable `LearnedPattern` entries, and provides keyword-based retrieval
//! for injection into agentic prompts.

use std::collections::HashMap;
use std::sync::Arc;

use crate::database::pg::PgDb;

/// Minimum occurrences required for a pattern to be distilled.
const MIN_OCCURRENCES: u32 = 3;

/// Minimum success rate (0.0-1.0) for a pattern to be stored.
const MIN_SUCCESS_RATE: f64 = 0.7;

/// Minimum character length for a pattern description (quality gate).
const MIN_DESCRIPTION_LEN: usize = 20;

/// A distilled pattern representing a recurring problem→solution pair.
#[derive(Debug, Clone)]
pub struct LearnedPattern {
    pub id: String,
    /// Keywords that trigger matching against prompts.
    pub trigger_keywords: Vec<String>,
    /// Normalized description of the problem.
    pub problem_description: String,
    /// Description of the solution approach.
    pub solution_description: String,
    /// Confidence score (0.0-1.0) based on success rate and sample count.
    pub confidence: f64,
    /// Number of runs that contributed to this pattern.
    pub sample_count: u32,
    /// Project path for project-specific patterns (optional).
    pub project_path: Option<String>,
    /// Workflow name this pattern was distilled from.
    pub workflow_name: Option<String>,
}

pub struct PatternDistiller {
    pg: Arc<PgDb>,
}

impl PatternDistiller {
    pub fn new(pg: Arc<PgDb>) -> Self {
        Self { pg }
    }

    /// Distill patterns from successful runs of a workflow.
    ///
    /// Queries the knowledge base for Finding→Fix pairs, groups by normalized
    /// problem pattern, filters by occurrence count and success rate, applies
    /// quality gates, and persists qualifying patterns.
    pub async fn distill_for_workflow(&self, workflow_name: &str) -> Result<u32, String> {
        tracing::info!(
            "PatternDistiller: distilling patterns for workflow '{}'",
            workflow_name
        );

        // Query finding→fix pairs from successful runs
        let pairs = self
            .pg
            .get_finding_fix_pairs_for_workflow(workflow_name)
            .await?;
        if pairs.is_empty() {
            tracing::debug!(
                "PatternDistiller: no finding/fix pairs for '{}'",
                workflow_name
            );
            return Ok(0);
        }

        // Group by normalized problem pattern
        #[derive(Default)]
        struct PatternAgg {
            problem_description: String,
            solution_description: String,
            successful_count: u32,
            total_count: u32,
            sample_task_run_ids: Vec<String>,
        }
        let mut groups: HashMap<String, PatternAgg> = HashMap::new();

        for pair in pairs {
            let key = normalize_problem(&pair.finding_description);
            let entry = groups.entry(key).or_default();
            entry.problem_description = pair.finding_description.clone();
            entry.solution_description = pair.fix_description.clone();
            entry.total_count += 1;
            if pair.fix_applied_successfully {
                entry.successful_count += 1;
            }
            if entry.sample_task_run_ids.len() < 5 {
                entry.sample_task_run_ids.push(pair.task_run_id.clone());
            }
        }

        // Filter + persist
        let mut stored = 0u32;
        for (_, agg) in groups {
            if agg.total_count < MIN_OCCURRENCES {
                continue;
            }
            let success_rate = agg.successful_count as f64 / agg.total_count as f64;
            if success_rate < MIN_SUCCESS_RATE {
                continue;
            }
            // Quality gate: reject generic or short descriptions
            if !passes_quality_gate(&agg.problem_description, &agg.solution_description) {
                tracing::debug!(
                    "PatternDistiller: rejected generic pattern: {}",
                    agg.problem_description
                );
                continue;
            }

            let keywords = extract_keywords(&agg.problem_description);
            if keywords.is_empty() {
                continue;
            }

            let confidence = compute_confidence(success_rate, agg.total_count);
            let pattern = LearnedPattern {
                id: uuid::Uuid::new_v4().to_string(),
                trigger_keywords: keywords,
                problem_description: agg.problem_description,
                solution_description: agg.solution_description,
                confidence,
                sample_count: agg.total_count,
                project_path: None,
                workflow_name: Some(workflow_name.to_string()),
            };

            if let Err(e) = self.pg.upsert_learned_pattern(&pattern).await {
                tracing::warn!("PatternDistiller: failed to upsert pattern: {}", e);
            } else {
                stored += 1;
            }
        }

        tracing::info!(
            "PatternDistiller: stored {} patterns for workflow '{}'",
            stored,
            workflow_name
        );
        Ok(stored)
    }

    /// Match learned patterns against a prompt by keyword overlap.
    /// Returns up to 3 patterns sorted by confidence descending.
    pub async fn match_patterns(
        &self,
        prompt: &str,
        project_path: Option<&str>,
    ) -> Result<Vec<LearnedPattern>, String> {
        let prompt_tokens = tokenize_for_matching(prompt);
        if prompt_tokens.is_empty() {
            return Ok(Vec::new());
        }

        let all_patterns = self.pg.get_learned_patterns(project_path).await?;

        let mut scored: Vec<(f64, LearnedPattern)> = all_patterns
            .into_iter()
            .filter_map(|p| {
                // Count keyword overlap
                let overlap = p
                    .trigger_keywords
                    .iter()
                    .filter(|kw| prompt_tokens.contains(&kw.to_lowercase()))
                    .count();
                if overlap == 0 {
                    None
                } else {
                    // Score = confidence * overlap_ratio
                    let overlap_ratio = overlap as f64 / p.trigger_keywords.len() as f64;
                    let score = p.confidence * overlap_ratio;
                    Some((score, p))
                }
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().take(3).map(|(_, p)| p).collect())
    }

    /// Format matched patterns as a "## Known Solutions" section for prompt injection.
    pub fn format_for_prompt(patterns: &[LearnedPattern]) -> String {
        if patterns.is_empty() {
            return String::new();
        }
        let mut s = String::from(
            "\n## Known Solutions\n\nThe following patterns have been observed and resolved in similar situations:\n\n",
        );
        for (i, p) in patterns.iter().enumerate() {
            s.push_str(&format!(
                "### Pattern {} (confidence: {:.0}%, {} past occurrences)\n**Problem:** {}\n\n**Solution:** {}\n\n",
                i + 1,
                p.confidence * 100.0,
                p.sample_count,
                p.problem_description,
                p.solution_description,
            ));
        }
        s
    }
}

/// Normalize a problem description for grouping similar issues.
/// Removes specific identifiers, file paths, line numbers, and timestamps.
fn normalize_problem(desc: &str) -> String {
    let mut normalized = desc.to_lowercase();
    // Remove file paths
    normalized = regex_replace(
        &normalized,
        r"[a-z0-9_./\\-]+\.(rs|ts|tsx|js|jsx|py|go|java|cpp|c|h|hpp)",
        "<file>",
    );
    // Remove line numbers
    normalized = regex_replace(&normalized, r":\d+", ":<line>");
    // Remove hex addresses
    normalized = regex_replace(&normalized, r"0x[0-9a-f]+", "<addr>");
    // Remove UUIDs
    normalized = regex_replace(
        &normalized,
        r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
        "<uuid>",
    );
    // Collapse whitespace
    normalized = regex_replace(&normalized, r"\s+", " ");
    normalized.trim().to_string()
}

fn regex_replace(s: &str, pattern: &str, replacement: &str) -> String {
    use once_cell::sync::Lazy;
    use std::sync::Mutex;
    static CACHE: Lazy<Mutex<HashMap<String, regex::Regex>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    let mut cache = CACHE.lock().unwrap();
    let re = cache
        .entry(pattern.to_string())
        .or_insert_with(|| regex::Regex::new(pattern).unwrap());
    re.replace_all(s, replacement).to_string()
}

/// Quality gate: reject generic or too-short descriptions.
fn passes_quality_gate(problem: &str, solution: &str) -> bool {
    if problem.len() < MIN_DESCRIPTION_LEN || solution.len() < MIN_DESCRIPTION_LEN {
        return false;
    }
    // Reject common generic terms as the only content
    let generic_phrases = [
        "error occurred",
        "unknown error",
        "something went wrong",
        "fix the issue",
        "resolve the problem",
        "update the code",
    ];
    let lower_problem = problem.to_lowercase();
    let lower_solution = solution.to_lowercase();
    for phrase in generic_phrases {
        if lower_problem == phrase || lower_solution == phrase {
            return false;
        }
    }
    // Require at least 3 distinct "content" words in problem
    let content_words = lower_problem
        .split_whitespace()
        .filter(|w| w.len() > 3 && !STOP_WORDS.contains(w))
        .count();
    content_words >= 3
}

/// Extract trigger keywords from a problem description.
fn extract_keywords(description: &str) -> Vec<String> {
    let lower = description.to_lowercase();
    let mut keywords: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() >= 4 && !STOP_WORDS.contains(w))
        .map(|w| w.to_string())
        .collect();
    keywords.sort();
    keywords.dedup();
    keywords.truncate(10);
    keywords
}

fn tokenize_for_matching(prompt: &str) -> std::collections::HashSet<String> {
    prompt
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_string())
        .collect()
}

/// Confidence score based on success rate and sample count.
/// Higher samples increase confidence up to a cap.
fn compute_confidence(success_rate: f64, sample_count: u32) -> f64 {
    let sample_bonus = (sample_count as f64 / 10.0).min(1.0);
    let base = success_rate;
    (base * 0.7 + sample_bonus * 0.3).min(1.0)
}

const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "if", "then", "else", "for", "with", "to", "of", "in",
    "on", "at", "by", "from", "is", "was", "be", "been", "this", "that", "these", "those", "it",
    "its", "not", "no", "yes", "error", "issue", "problem", "fail", "failed", "failure",
];
